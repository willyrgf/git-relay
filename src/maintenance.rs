use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::audit::{new_structured_log_event, record_structured_log};
use crate::config::{
    AppConfig, HumanDuration, RepositoryDescriptor, RepositoryLifecycle, RepositoryMode,
};
use crate::read_path::{cache_retention_status, evict_cache_repository, CacheControlError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPolicyReport {
    pub maintenance_interval: HumanDuration,
    pub cache_idle_ttl: HumanDuration,
    pub terminal_run_ttl: HumanDuration,
    pub terminal_run_keep_count: usize,
    pub authoritative_reflog_ttl: HumanDuration,
    pub authoritative_prune_ttl: HumanDuration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalEvidenceCounts {
    pub reconcile_runs: usize,
    pub upstream_probe_runs: usize,
    pub matrix_probe_runs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalEvidencePruneReport {
    pub reconcile_runs_removed: usize,
    pub upstream_probe_runs_removed: usize,
    pub matrix_probe_runs_removed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheMaintenanceOutcome {
    NotApplicable,
    PinnedRetained,
    RecentRetained,
    NoActivityEvidenceRetained,
    EmptyRetained,
    Evicted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheMaintenanceReport {
    pub outcome: CacheMaintenanceOutcome,
    pub pinned: bool,
    pub repo_accessible: bool,
    pub has_visible_refs: Option<bool>,
    pub last_activity_at_ms: Option<u128>,
    pub removed_visible_ref_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthoritativeMaintenanceReport {
    pub applied: bool,
    pub reflog_ttl: HumanDuration,
    pub prune_ttl: HumanDuration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryMaintenanceReport {
    pub repo_id: String,
    pub repo_path: PathBuf,
    pub repo_mode: RepositoryMode,
    pub started_at_ms: u128,
    pub completed_at_ms: u128,
    pub evidence_pruned: TerminalEvidencePruneReport,
    pub cache: Option<CacheMaintenanceReport>,
    pub authoritative: Option<AuthoritativeMaintenanceReport>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRetentionStatus {
    pub policy: RetentionPolicyReport,
    pub due_now: bool,
    pub next_due_at_ms: Option<u128>,
    pub evidence_counts: TerminalEvidenceCounts,
    pub last_maintenance: Option<RepositoryMaintenanceReport>,
}

#[derive(Debug, Error)]
pub enum MaintenanceError {
    #[error("failed to create directory {path}: {error}", path = path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to read {path}: {error}", path = path.display())]
    Read {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to write {path}: {error}", path = path.display())]
    Write {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to remove {path}: {error}", path = path.display())]
    Remove {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to parse JSON {path}: {error}", path = path.display())]
    ParseJson {
        path: PathBuf,
        #[source]
        error: serde_json::Error,
    },
    #[error("failed to run git {args:?}: {error}")]
    SpawnGit {
        args: Vec<String>,
        #[source]
        error: std::io::Error,
    },
    #[error("git failed for args {args:?} with status {status:?}: {detail}")]
    Git {
        args: Vec<String>,
        status: Option<i32>,
        detail: String,
    },
    #[error(transparent)]
    CacheControl(#[from] CacheControlError),
    #[error("repository {repo_id} at {repo_path} is not accessible for maintenance")]
    RepositoryUnavailable { repo_id: String, repo_path: PathBuf },
}

pub fn retention_policy_report(config: &AppConfig) -> RetentionPolicyReport {
    RetentionPolicyReport {
        maintenance_interval: config.retention.maintenance_interval,
        cache_idle_ttl: config.retention.cache_idle_ttl,
        terminal_run_ttl: config.retention.terminal_run_ttl,
        terminal_run_keep_count: config.retention.terminal_run_keep_count,
        authoritative_reflog_ttl: config.retention.authoritative_reflog_ttl,
        authoritative_prune_ttl: config.retention.authoritative_prune_ttl,
    }
}

pub fn retention_status_for_repo(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
) -> Result<RepoRetentionStatus, MaintenanceError> {
    let policy = retention_policy_report(config);
    let last_maintenance = read_json_optional::<RepositoryMaintenanceReport>(&last_report_path(
        &config.paths.state_root,
        &descriptor.repo_id,
    ))?;
    let due_now = maintenance_due(
        last_maintenance.as_ref(),
        config.retention.maintenance_interval,
        current_time_ms(),
    );
    let next_due_at_ms = last_maintenance.as_ref().map(|report| {
        report.completed_at_ms
            + config
                .retention
                .maintenance_interval
                .as_duration()
                .as_millis()
    });
    Ok(RepoRetentionStatus {
        policy,
        due_now,
        next_due_at_ms,
        evidence_counts: terminal_evidence_counts(&config.paths.state_root, &descriptor.repo_id)?,
        last_maintenance,
    })
}

pub fn run_retention_maintenance(
    config: &AppConfig,
    descriptors: &[RepositoryDescriptor],
) -> Vec<RepositoryMaintenanceReport> {
    let mut reports = Vec::new();
    for descriptor in descriptors {
        if descriptor.lifecycle != RepositoryLifecycle::Ready {
            continue;
        }
        let due_now = retention_status_for_repo(config, descriptor)
            .map(|status| status.due_now)
            .unwrap_or(true);
        if !due_now {
            continue;
        }

        let report = match execute_maintenance(config, descriptor) {
            Ok(report) => report,
            Err(error) => RepositoryMaintenanceReport {
                repo_id: descriptor.repo_id.clone(),
                repo_path: descriptor.repo_path.clone(),
                repo_mode: descriptor.mode,
                started_at_ms: current_time_ms(),
                completed_at_ms: current_time_ms(),
                evidence_pruned: TerminalEvidencePruneReport {
                    reconcile_runs_removed: 0,
                    upstream_probe_runs_removed: 0,
                    matrix_probe_runs_removed: 0,
                },
                cache: None,
                authoritative: None,
                error: Some(error.to_string()),
            },
        };

        if let Err(error) = persist_report(config, &report) {
            let mut degraded = report.clone();
            degraded.error = Some(match degraded.error.take() {
                Some(existing) => {
                    format!("{existing} | failed to persist maintenance report: {error}")
                }
                None => format!("failed to persist maintenance report: {error}"),
            });
            record_report_event(config, &degraded);
            reports.push(degraded);
            continue;
        }

        record_report_event(config, &report);
        reports.push(report);
    }
    reports.sort_by(|left, right| left.repo_id.cmp(&right.repo_id));
    reports
}

fn execute_maintenance(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
) -> Result<RepositoryMaintenanceReport, MaintenanceError> {
    let started_at_ms = current_time_ms();
    let evidence_pruned = prune_terminal_evidence(config, descriptor, started_at_ms)?;
    let cache = match descriptor.mode {
        RepositoryMode::CacheOnly => Some(maintain_cache_repository(
            config,
            descriptor,
            started_at_ms,
        )?),
        RepositoryMode::Authoritative => None,
    };
    let authoritative = match descriptor.mode {
        RepositoryMode::Authoritative => Some(apply_authoritative_maintenance(config, descriptor)?),
        RepositoryMode::CacheOnly => None,
    };

    Ok(RepositoryMaintenanceReport {
        repo_id: descriptor.repo_id.clone(),
        repo_path: descriptor.repo_path.clone(),
        repo_mode: descriptor.mode,
        started_at_ms,
        completed_at_ms: current_time_ms(),
        evidence_pruned,
        cache,
        authoritative,
        error: None,
    })
}

fn maintain_cache_repository(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
    now_ms: u128,
) -> Result<CacheMaintenanceReport, MaintenanceError> {
    let status = cache_retention_status(config, descriptor)?;
    if !status.repo_accessible {
        return Err(MaintenanceError::RepositoryUnavailable {
            repo_id: descriptor.repo_id.clone(),
            repo_path: descriptor.repo_path.clone(),
        });
    }

    if status.pinned {
        return Ok(CacheMaintenanceReport {
            outcome: CacheMaintenanceOutcome::PinnedRetained,
            pinned: true,
            repo_accessible: status.repo_accessible,
            has_visible_refs: status.has_visible_refs,
            last_activity_at_ms: status.last_activity_at_ms,
            removed_visible_ref_count: 0,
        });
    }

    if status.has_visible_refs != Some(true) {
        return Ok(CacheMaintenanceReport {
            outcome: CacheMaintenanceOutcome::EmptyRetained,
            pinned: false,
            repo_accessible: status.repo_accessible,
            has_visible_refs: status.has_visible_refs,
            last_activity_at_ms: status.last_activity_at_ms,
            removed_visible_ref_count: 0,
        });
    }

    let Some(last_activity_at_ms) = status.last_activity_at_ms else {
        return Ok(CacheMaintenanceReport {
            outcome: CacheMaintenanceOutcome::NoActivityEvidenceRetained,
            pinned: false,
            repo_accessible: status.repo_accessible,
            has_visible_refs: status.has_visible_refs,
            last_activity_at_ms: None,
            removed_visible_ref_count: 0,
        });
    };

    if now_ms.saturating_sub(last_activity_at_ms)
        < config.retention.cache_idle_ttl.as_duration().as_millis()
    {
        return Ok(CacheMaintenanceReport {
            outcome: CacheMaintenanceOutcome::RecentRetained,
            pinned: false,
            repo_accessible: status.repo_accessible,
            has_visible_refs: status.has_visible_refs,
            last_activity_at_ms: Some(last_activity_at_ms),
            removed_visible_ref_count: 0,
        });
    }

    let eviction = evict_cache_repository(config, descriptor)?;
    Ok(CacheMaintenanceReport {
        outcome: CacheMaintenanceOutcome::Evicted,
        pinned: eviction.status.pinned,
        repo_accessible: eviction.status.repo_accessible,
        has_visible_refs: eviction.status.has_visible_refs,
        last_activity_at_ms: eviction.status.last_activity_at_ms,
        removed_visible_ref_count: eviction.removed_visible_ref_count,
    })
}

fn apply_authoritative_maintenance(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
) -> Result<AuthoritativeMaintenanceReport, MaintenanceError> {
    if !descriptor.repo_path.exists() {
        return Err(MaintenanceError::RepositoryUnavailable {
            repo_id: descriptor.repo_id.clone(),
            repo_path: descriptor.repo_path.clone(),
        });
    }

    let reflog_ttl = config.retention.authoritative_reflog_ttl;
    let prune_ttl = config.retention.authoritative_prune_ttl;
    run_git_expect_success(
        Some(&descriptor.repo_path),
        &[
            "reflog".to_owned(),
            "expire".to_owned(),
            format!("--expire={}", git_approxidate(reflog_ttl)),
            format!("--expire-unreachable={}", git_approxidate(prune_ttl)),
            "--all".to_owned(),
        ],
    )?;
    run_git_expect_success(
        Some(&descriptor.repo_path),
        &[
            "gc".to_owned(),
            format!("--prune={}", git_approxidate(prune_ttl)),
        ],
    )?;

    Ok(AuthoritativeMaintenanceReport {
        applied: true,
        reflog_ttl,
        prune_ttl,
    })
}

fn prune_terminal_evidence(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
    now_ms: u128,
) -> Result<TerminalEvidencePruneReport, MaintenanceError> {
    Ok(TerminalEvidencePruneReport {
        reconcile_runs_removed: prune_json_records(
            &reconcile_run_directory(&config.paths.state_root, &descriptor.repo_id),
            config.retention.terminal_run_keep_count,
            config.retention.terminal_run_ttl,
            now_ms,
        )?,
        upstream_probe_runs_removed: prune_json_records(
            &upstream_probe_run_directory(&config.paths.state_root, &descriptor.repo_id),
            config.retention.terminal_run_keep_count,
            config.retention.terminal_run_ttl,
            now_ms,
        )?,
        matrix_probe_runs_removed: prune_json_records(
            &matrix_probe_run_directory(&config.paths.state_root, &descriptor.repo_id),
            config.retention.terminal_run_keep_count,
            config.retention.terminal_run_ttl,
            now_ms,
        )?,
    })
}

fn persist_report(
    config: &AppConfig,
    report: &RepositoryMaintenanceReport,
) -> Result<(), MaintenanceError> {
    write_json(
        &last_report_path(&config.paths.state_root, &report.repo_id),
        report,
    )
}

fn record_report_event(config: &AppConfig, report: &RepositoryMaintenanceReport) {
    let mut event = new_structured_log_event("maintenance.retention");
    event.repo_id = Some(report.repo_id.clone());
    event.payload = serde_json::json!({
        "repo_mode": report.repo_mode,
        "evidence_pruned": report.evidence_pruned,
        "cache": report.cache,
        "authoritative": report.authoritative,
        "error": report.error,
    });
    let _ = record_structured_log(&config.paths.state_root, &event);
}

fn maintenance_due(
    report: Option<&RepositoryMaintenanceReport>,
    interval: HumanDuration,
    now_ms: u128,
) -> bool {
    match report {
        Some(report) => {
            now_ms.saturating_sub(report.completed_at_ms) >= interval.as_duration().as_millis()
        }
        None => true,
    }
}

fn terminal_evidence_counts(
    state_root: &Path,
    repo_id: &str,
) -> Result<TerminalEvidenceCounts, MaintenanceError> {
    Ok(TerminalEvidenceCounts {
        reconcile_runs: count_json_files(&reconcile_run_directory(state_root, repo_id))?,
        upstream_probe_runs: count_json_files(&upstream_probe_run_directory(state_root, repo_id))?,
        matrix_probe_runs: count_json_files(&matrix_probe_run_directory(state_root, repo_id))?,
    })
}

fn count_json_files(directory: &Path) -> Result<usize, MaintenanceError> {
    if !directory.exists() {
        return Ok(0);
    }
    let mut count = 0usize;
    for entry in fs::read_dir(directory).map_err(|error| MaintenanceError::Read {
        path: directory.to_path_buf(),
        error,
    })? {
        let entry = entry.map_err(|error| MaintenanceError::Read {
            path: directory.to_path_buf(),
            error,
        })?;
        if entry.path().extension().and_then(|value| value.to_str()) == Some("json") {
            count += 1;
        }
    }
    Ok(count)
}

fn prune_json_records(
    directory: &Path,
    keep_count: usize,
    ttl: HumanDuration,
    now_ms: u128,
) -> Result<usize, MaintenanceError> {
    if !directory.exists() {
        return Ok(0);
    }

    let mut records = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| MaintenanceError::Read {
        path: directory.to_path_buf(),
        error,
    })? {
        let entry = entry.map_err(|error| MaintenanceError::Read {
            path: directory.to_path_buf(),
            error,
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        records.push((terminal_record_timestamp(&path)?, path));
    }

    records.sort_by(|left, right| right.cmp(left));
    let cutoff = now_ms.saturating_sub(ttl.as_duration().as_millis());
    let mut removed = 0usize;
    for (index, (timestamp, path)) in records.into_iter().enumerate() {
        if index < keep_count || timestamp >= cutoff {
            continue;
        }
        fs::remove_file(&path).map_err(|error| MaintenanceError::Remove {
            path: path.clone(),
            error,
        })?;
        removed += 1;
    }
    Ok(removed)
}

fn terminal_record_timestamp(path: &Path) -> Result<u128, MaintenanceError> {
    let source = fs::read_to_string(path).map_err(|error| MaintenanceError::Read {
        path: path.to_path_buf(),
        error,
    })?;
    let value =
        serde_json::from_str::<Value>(&source).map_err(|error| MaintenanceError::ParseJson {
            path: path.to_path_buf(),
            error,
        })?;
    Ok(value
        .get("completed_at_ms")
        .and_then(Value::as_u64)
        .map(u128::from)
        .or_else(|| {
            value
                .get("started_at_ms")
                .and_then(Value::as_u64)
                .map(u128::from)
        })
        .unwrap_or_default())
}

fn last_report_path(state_root: &Path, repo_id: &str) -> PathBuf {
    state_root
        .join("retention")
        .join("maintenance")
        .join(format!("{}.json", sanitize_component(repo_id)))
}

fn reconcile_run_directory(state_root: &Path, repo_id: &str) -> PathBuf {
    state_root
        .join("reconcile")
        .join("runs")
        .join(sanitize_component(repo_id))
}

fn upstream_probe_run_directory(state_root: &Path, repo_id: &str) -> PathBuf {
    state_root
        .join("upstream-probes")
        .join("runs")
        .join(sanitize_component(repo_id))
}

fn matrix_probe_run_directory(state_root: &Path, repo_id: &str) -> PathBuf {
    state_root
        .join("upstream-probes")
        .join("matrix-runs")
        .join(sanitize_component(repo_id))
}

fn sanitize_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "unknown".to_owned()
    } else {
        sanitized
    }
}

fn git_approxidate(duration: HumanDuration) -> String {
    let seconds = duration.as_duration().as_secs();
    if seconds == 0 {
        return "now".to_owned();
    }
    if seconds % (24 * 60 * 60) == 0 {
        format!("{}.days.ago", seconds / (24 * 60 * 60))
    } else if seconds % (60 * 60) == 0 {
        format!("{}.hours.ago", seconds / (60 * 60))
    } else if seconds % 60 == 0 {
        format!("{}.minutes.ago", seconds / 60)
    } else {
        format!("{seconds}.seconds.ago")
    }
}

fn read_json_optional<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<Option<T>, MaintenanceError> {
    if !path.exists() {
        return Ok(None);
    }
    let source = fs::read_to_string(path).map_err(|error| MaintenanceError::Read {
        path: path.to_path_buf(),
        error,
    })?;
    let value = serde_json::from_str(&source).map_err(|error| MaintenanceError::ParseJson {
        path: path.to_path_buf(),
        error,
    })?;
    Ok(Some(value))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), MaintenanceError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| MaintenanceError::CreateDir {
            path: parent.to_path_buf(),
            error,
        })?;
    }
    let encoded =
        serde_json::to_vec_pretty(value).map_err(|error| MaintenanceError::ParseJson {
            path: path.to_path_buf(),
            error,
        })?;
    fs::write(path, encoded).map_err(|error| MaintenanceError::Write {
        path: path.to_path_buf(),
        error,
    })
}

fn run_git(
    repo_path: Option<&Path>,
    args: &[String],
) -> Result<GitProcessOutput, MaintenanceError> {
    let mut command = Command::new("git");
    if let Some(repo_path) = repo_path {
        command.arg(format!("--git-dir={}", repo_path.display()));
    }
    command.args(args);
    let output = command
        .output()
        .map_err(|error| MaintenanceError::SpawnGit {
            args: args.to_vec(),
            error,
        })?;
    Ok(GitProcessOutput {
        success: output.status.success(),
        status: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        args: args.to_vec(),
    })
}

fn run_git_expect_success(
    repo_path: Option<&Path>,
    args: &[String],
) -> Result<GitProcessOutput, MaintenanceError> {
    let output = run_git(repo_path, args)?;
    if output.success {
        Ok(output)
    } else {
        Err(MaintenanceError::Git {
            args: output.args.clone(),
            status: output.status,
            detail: format_git_failure(&output),
        })
    }
}

fn format_git_failure(output: &GitProcessOutput) -> String {
    let mut parts = Vec::new();
    if !output.stderr.is_empty() {
        parts.push(output.stderr.clone());
    }
    if !output.stdout.is_empty() {
        parts.push(output.stdout.clone());
    }
    if parts.is_empty() {
        format!(
            "git failed for args {:?} with status {:?}",
            output.args, output.status
        )
    } else {
        parts.join(" | ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitProcessOutput {
    success: bool,
    status: Option<i32>,
    stdout: String,
    stderr: String,
    args: Vec<String>,
}

fn current_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
