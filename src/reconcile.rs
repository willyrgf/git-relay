use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::classification::{RepositorySafetyState, UpstreamConvergenceState};
use crate::config::{
    AppConfig, AuthorityModel, RepositoryDescriptor, RepositoryLifecycle, RepositoryMode,
};

const INTERNAL_UPSTREAM_REF_PREFIX: &str = "refs/git-relay/upstreams";
const INTERNAL_DIVERGENCE_REF_PREFIX: &str = "refs/git-relay/safety/divergent";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefSnapshotEntry {
    pub ref_name: String,
    pub oid: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileRunStatus {
    InProgress,
    Completed,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtomicCapabilityVerdict {
    Supported,
    Unsupported,
    Inconclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileUpstreamResult {
    pub upstream_id: String,
    pub require_atomic: bool,
    pub state: UpstreamConvergenceState,
    pub divergent: bool,
    pub apply_attempted: bool,
    pub detail: Option<String>,
    pub atomic_capability: Option<AtomicCapabilityVerdict>,
    pub observed_before: Vec<RefSnapshotEntry>,
    pub observed_after: Vec<RefSnapshotEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileRunRecord {
    pub run_id: String,
    pub repo_id: String,
    pub repo_path: PathBuf,
    pub started_at_ms: u128,
    pub completed_at_ms: Option<u128>,
    pub desired_snapshot: Vec<RefSnapshotEntry>,
    pub captured_upstreams: Vec<String>,
    pub repo_safety: RepositorySafetyState,
    pub status: ReconcileRunStatus,
    pub superseded_by: Option<String>,
    pub upstream_results: Vec<ReconcileUpstreamResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingReconcileRequest {
    pub repo_id: String,
    pub repo_path: PathBuf,
    pub requested_at_ms: u128,
    pub last_push_id: Option<String>,
    pub last_request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DivergenceMarker {
    pub repo_id: String,
    pub upstream_id: String,
    pub run_id: String,
    pub recorded_at_ms: u128,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LockMetadata {
    repo_id: String,
    run_id: String,
    pid: u32,
    acquired_at_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct InProgressMarker {
    repo_id: String,
    run_id: String,
    pid: u32,
    started_at_ms: u128,
}

struct ReconcileLockGuard {
    path: PathBuf,
}

impl Drop for ReconcileLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("repository {repo_id} is {mode:?}; manual reconcile is supported only for authoritative repositories")]
    UnsupportedRepositoryMode {
        repo_id: String,
        mode: RepositoryMode,
    },
    #[error("repository {repo_id} is {lifecycle:?}; manual reconcile requires lifecycle ready")]
    RepositoryNotReady {
        repo_id: String,
        lifecycle: RepositoryLifecycle,
    },
    #[error("repository {repo_id} already has a live reconcile run in progress")]
    RunInProgress { repo_id: String },
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
    #[error("failed to write git input for args {args:?}: {error}")]
    WriteGitInput {
        args: Vec<String>,
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
}

pub fn enqueue_reconcile_request(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
    push_id: Option<&str>,
    request_id: Option<&str>,
) -> Result<PendingReconcileRequest, ReconcileError> {
    let pending = PendingReconcileRequest {
        repo_id: descriptor.repo_id.clone(),
        repo_path: descriptor.repo_path.clone(),
        requested_at_ms: current_time_ms(),
        last_push_id: push_id.map(str::to_owned),
        last_request_id: request_id.map(str::to_owned),
    };
    let path = pending_request_path(&config.paths.state_root, &descriptor.repo_id);
    write_json(&path, &pending)?;
    Ok(pending)
}

pub fn pending_request_file_path(state_root: &Path, repo_id: &str) -> PathBuf {
    pending_request_path(state_root, repo_id)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicationStatus {
    pub repo_id: String,
    pub pending_request: Option<PendingReconcileRequest>,
    pub latest_run: Option<ReconcileRunRecord>,
}

pub fn load_pending_reconcile_requests(
    state_root: &Path,
) -> Result<Vec<PendingReconcileRequest>, ReconcileError> {
    let directory = pending_request_directory(state_root);
    if !directory.exists() {
        return Ok(Vec::new());
    }

    let mut requests = Vec::new();
    for entry in fs::read_dir(&directory).map_err(|error| ReconcileError::Read {
        path: directory.clone(),
        error,
    })? {
        let entry = entry.map_err(|error| ReconcileError::Read {
            path: directory.clone(),
            error,
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        if let Some(request) = read_json_optional::<PendingReconcileRequest>(&path)? {
            requests.push(request);
        }
    }
    requests.sort_by(|left, right| left.repo_id.cmp(&right.repo_id));
    Ok(requests)
}

pub fn replication_status_for_repo(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
) -> Result<ReplicationStatus, ReconcileError> {
    Ok(ReplicationStatus {
        repo_id: descriptor.repo_id.clone(),
        pending_request: read_pending_request(&config.paths.state_root, &descriptor.repo_id)?,
        latest_run: latest_run_record(&config.paths.state_root, &descriptor.repo_id)?,
    })
}

pub fn load_divergence_markers(repo_path: &Path) -> Result<Vec<DivergenceMarker>, ReconcileError> {
    let refs = list_divergence_marker_refs(repo_path)?;
    let mut markers = refs
        .into_iter()
        .map(|(_, oid)| read_divergence_marker(repo_path, &oid))
        .collect::<Result<Vec<_>, _>>()?;
    markers.sort_by(|left, right| left.upstream_id.cmp(&right.upstream_id));
    Ok(markers)
}

pub fn process_pending_reconcile_requests(
    config: &AppConfig,
    descriptors: &[RepositoryDescriptor],
) -> Result<Vec<ReconcileRunRecord>, ReconcileError> {
    let mut runs = Vec::new();
    let pending = load_pending_reconcile_requests(&config.paths.state_root)?;
    for request in pending {
        let Some(descriptor) = descriptors
            .iter()
            .find(|descriptor| descriptor.repo_id == request.repo_id)
        else {
            clear_pending_request(config, &request.repo_id);
            continue;
        };

        if descriptor.mode != RepositoryMode::Authoritative
            || descriptor.lifecycle != RepositoryLifecycle::Ready
        {
            clear_pending_request(config, &request.repo_id);
            continue;
        }

        match reconcile_repository(config, descriptor) {
            Ok(run) => runs.push(run),
            Err(ReconcileError::RunInProgress { .. }) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(runs)
}

pub fn reconcile_repository(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
) -> Result<ReconcileRunRecord, ReconcileError> {
    if descriptor.mode != RepositoryMode::Authoritative {
        return Err(ReconcileError::UnsupportedRepositoryMode {
            repo_id: descriptor.repo_id.clone(),
            mode: descriptor.mode,
        });
    }
    if descriptor.lifecycle != RepositoryLifecycle::Ready {
        return Err(ReconcileError::RepositoryNotReady {
            repo_id: descriptor.repo_id.clone(),
            lifecycle: descriptor.lifecycle,
        });
    }

    let run_id = generate_run_id();
    let _lock = acquire_reconcile_lock(config, descriptor, &run_id)?;
    let started_at_ms = current_time_ms();
    let desired_snapshot =
        list_local_exported_refs(&descriptor.repo_path, &descriptor.exported_refs)?;
    let mut upstreams = descriptor.write_upstreams.clone();
    upstreams.sort_by(|left, right| left.name.cmp(&right.name));

    supersede_stale_run_if_present(config, descriptor, &run_id)?;
    write_json(
        &in_progress_marker_path(&config.paths.state_root, &descriptor.repo_id),
        &InProgressMarker {
            repo_id: descriptor.repo_id.clone(),
            run_id: run_id.clone(),
            pid: std::process::id(),
            started_at_ms,
        },
    )?;

    let mut run = ReconcileRunRecord {
        run_id: run_id.clone(),
        repo_id: descriptor.repo_id.clone(),
        repo_path: descriptor.repo_path.clone(),
        started_at_ms,
        completed_at_ms: None,
        desired_snapshot: desired_snapshot.clone(),
        captured_upstreams: upstreams
            .iter()
            .map(|upstream| upstream.name.clone())
            .collect(),
        repo_safety: if upstreams.is_empty() {
            RepositorySafetyState::Healthy
        } else {
            RepositorySafetyState::Degraded
        },
        status: ReconcileRunStatus::InProgress,
        superseded_by: None,
        upstream_results: Vec::new(),
    };
    persist_run_record(config, &run)?;

    for upstream in &upstreams {
        let result = reconcile_one_upstream(descriptor, upstream, &desired_snapshot);
        run.upstream_results.push(result?);
        persist_run_record(config, &run)?;
    }

    run.repo_safety = classify_repository_safety(&run.upstream_results);
    persist_divergence_markers(
        &descriptor.repo_path,
        &descriptor.repo_id,
        &run.run_id,
        &run.upstream_results,
    )?;
    run.status = ReconcileRunStatus::Completed;
    run.completed_at_ms = Some(current_time_ms());
    persist_run_record(config, &run)?;
    clear_in_progress_marker(config, &descriptor.repo_id);
    clear_pending_request(config, &descriptor.repo_id);

    Ok(run)
}

fn reconcile_one_upstream(
    descriptor: &RepositoryDescriptor,
    upstream: &crate::config::WriteUpstream,
    desired_snapshot: &[RefSnapshotEntry],
) -> Result<ReconcileUpstreamResult, ReconcileError> {
    let previous_observed = read_observed_refs(&descriptor.repo_path, &upstream.name)?;

    let observed_before =
        match observe_upstream(&descriptor.repo_path, &upstream.name, &upstream.url)? {
            Ok(snapshot) => snapshot,
            Err(detail) => {
                return Ok(ReconcileUpstreamResult {
                    upstream_id: upstream.name.clone(),
                    require_atomic: upstream.require_atomic,
                    state: UpstreamConvergenceState::Stalled,
                    divergent: false,
                    apply_attempted: false,
                    detail: Some(detail),
                    atomic_capability: None,
                    observed_before: Vec::new(),
                    observed_after: Vec::new(),
                });
            }
        };
    let divergent = detect_divergence(
        descriptor.authority_model,
        &previous_observed,
        &observed_before,
        desired_snapshot,
    );
    if divergent {
        return Ok(ReconcileUpstreamResult {
            upstream_id: upstream.name.clone(),
            require_atomic: upstream.require_atomic,
            state: UpstreamConvergenceState::OutOfSync,
            divergent: true,
            apply_attempted: false,
            detail: Some(
                "fresh upstream observation differs from both the prior observed state and the current desired state under relay-authoritative policy"
                    .to_owned(),
            ),
            atomic_capability: None,
            observed_before: observed_before.clone(),
            observed_after: observed_before,
        });
    }

    let mut apply_attempted = false;
    let mut detail = None;
    let mut atomic_capability = None;

    if !same_snapshot(&observed_before, desired_snapshot) {
        if upstream.require_atomic {
            let probe =
                probe_atomic_capability(&descriptor.repo_path, &upstream.url, desired_snapshot)?;
            atomic_capability = Some(probe);
            if probe != AtomicCapabilityVerdict::Supported {
                return Ok(ReconcileUpstreamResult {
                    upstream_id: upstream.name.clone(),
                    require_atomic: true,
                    state: if probe == AtomicCapabilityVerdict::Unsupported {
                        UpstreamConvergenceState::Unsupported
                    } else {
                        UpstreamConvergenceState::Stalled
                    },
                    divergent: false,
                    apply_attempted: false,
                    detail: Some(match probe {
                        AtomicCapabilityVerdict::Supported => "atomic capability supported".to_owned(),
                        AtomicCapabilityVerdict::Unsupported => {
                            "upstream does not support required atomic multi-ref apply".to_owned()
                        }
                        AtomicCapabilityVerdict::Inconclusive => {
                            "atomic capability probe was inconclusive and is treated as unsupported for this run"
                                .to_owned()
                        }
                    }),
                    atomic_capability,
                    observed_before: observed_before.clone(),
                    observed_after: observed_before,
                });
            }
        }

        if !desired_snapshot.is_empty() {
            apply_attempted = true;
            if let Err(apply_error) = push_desired_snapshot(
                &descriptor.repo_path,
                &upstream.url,
                desired_snapshot,
                upstream.require_atomic,
            )? {
                detail = Some(apply_error);
            }
        }
    }

    let observed_after =
        match observe_upstream(&descriptor.repo_path, &upstream.name, &upstream.url)? {
            Ok(snapshot) => snapshot,
            Err(observe_error) => {
                return Ok(ReconcileUpstreamResult {
                    upstream_id: upstream.name.clone(),
                    require_atomic: upstream.require_atomic,
                    state: UpstreamConvergenceState::Stalled,
                    divergent: false,
                    apply_attempted,
                    detail: Some(observe_error),
                    atomic_capability,
                    observed_before,
                    observed_after: Vec::new(),
                });
            }
        };
    Ok(ReconcileUpstreamResult {
        upstream_id: upstream.name.clone(),
        require_atomic: upstream.require_atomic,
        state: if same_snapshot(&observed_after, desired_snapshot) {
            UpstreamConvergenceState::InSync
        } else if detail.is_some() {
            UpstreamConvergenceState::OutOfSync
        } else {
            UpstreamConvergenceState::OutOfSync
        },
        divergent: false,
        apply_attempted,
        detail,
        atomic_capability,
        observed_before,
        observed_after,
    })
}

fn acquire_reconcile_lock(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
    run_id: &str,
) -> Result<ReconcileLockGuard, ReconcileError> {
    let lock_path = reconcile_lock_path(&config.paths.state_root, &descriptor.repo_id);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|error| ReconcileError::CreateDir {
            path: parent.to_path_buf(),
            error,
        })?;
    }

    loop {
        match fs::create_dir(&lock_path) {
            Ok(()) => {
                let metadata = LockMetadata {
                    repo_id: descriptor.repo_id.clone(),
                    run_id: run_id.to_owned(),
                    pid: std::process::id(),
                    acquired_at_ms: current_time_ms(),
                };
                write_json(&lock_metadata_path(&lock_path), &metadata)?;
                return Ok(ReconcileLockGuard { path: lock_path });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if lock_is_stale(config, &lock_path)? {
                    let _ = fs::remove_dir_all(&lock_path);
                    continue;
                }
                return Err(ReconcileError::RunInProgress {
                    repo_id: descriptor.repo_id.clone(),
                });
            }
            Err(error) => {
                return Err(ReconcileError::CreateDir {
                    path: lock_path,
                    error,
                });
            }
        }
    }
}

fn lock_is_stale(config: &AppConfig, lock_path: &Path) -> Result<bool, ReconcileError> {
    let metadata_path = lock_metadata_path(lock_path);
    let metadata = match read_json_optional::<LockMetadata>(&metadata_path)? {
        Some(metadata) => metadata,
        None => return Ok(true),
    };
    let age_ms = current_time_ms().saturating_sub(metadata.acquired_at_ms) as u64;
    if age_ms <= config.reconcile.lock_timeout_ms {
        return Ok(false);
    }
    Ok(!pid_is_alive(metadata.pid))
}

fn supersede_stale_run_if_present(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
    new_run_id: &str,
) -> Result<(), ReconcileError> {
    let marker_path = in_progress_marker_path(&config.paths.state_root, &descriptor.repo_id);
    let marker = read_json_optional::<InProgressMarker>(&marker_path)?;
    if let Some(marker) = marker {
        let run_path = run_record_path(
            &config.paths.state_root,
            &descriptor.repo_id,
            &marker.run_id,
        );
        if let Some(mut run) = read_json_optional::<ReconcileRunRecord>(&run_path)? {
            if run.status == ReconcileRunStatus::InProgress {
                run.status = ReconcileRunStatus::Superseded;
                run.superseded_by = Some(new_run_id.to_owned());
                run.completed_at_ms = Some(current_time_ms());
                write_json(&run_path, &run)?;
            }
        }
        let _ = fs::remove_file(marker_path);
    }
    Ok(())
}

fn persist_run_record(config: &AppConfig, run: &ReconcileRunRecord) -> Result<(), ReconcileError> {
    let path = run_record_path(&config.paths.state_root, &run.repo_id, &run.run_id);
    write_json(&path, run)
}

fn clear_in_progress_marker(config: &AppConfig, repo_id: &str) {
    let _ = fs::remove_file(in_progress_marker_path(&config.paths.state_root, repo_id));
}

fn clear_pending_request(config: &AppConfig, repo_id: &str) {
    let _ = fs::remove_file(pending_request_path(&config.paths.state_root, repo_id));
}

fn read_pending_request(
    state_root: &Path,
    repo_id: &str,
) -> Result<Option<PendingReconcileRequest>, ReconcileError> {
    read_json_optional(&pending_request_path(state_root, repo_id))
}

fn latest_run_record(
    state_root: &Path,
    repo_id: &str,
) -> Result<Option<ReconcileRunRecord>, ReconcileError> {
    let directory = run_record_directory(state_root, repo_id);
    if !directory.exists() {
        return Ok(None);
    }

    let mut candidates = fs::read_dir(&directory)
        .map_err(|error| ReconcileError::Read {
            path: directory.clone(),
            error,
        })?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| ReconcileError::Read {
                    path: directory.clone(),
                    error,
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    candidates.sort();
    candidates.reverse();

    for path in candidates {
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        if let Some(record) = read_json_optional::<ReconcileRunRecord>(&path)? {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

fn list_local_exported_refs(
    repo_path: &Path,
    exported_patterns: &[String],
) -> Result<Vec<RefSnapshotEntry>, ReconcileError> {
    let output = run_git_expect_success(
        Some(repo_path),
        &[
            "for-each-ref".to_owned(),
            "--format=%(objectname) %(refname)".to_owned(),
            "refs/heads".to_owned(),
            "refs/tags".to_owned(),
        ],
    )?;
    let mut refs = parse_ref_snapshot(&output.stdout)?;
    refs.retain(|entry| matches_exported_ref(exported_patterns, &entry.ref_name));
    refs.sort_by(|left, right| left.ref_name.cmp(&right.ref_name));
    Ok(refs)
}

fn read_observed_refs(
    repo_path: &Path,
    upstream_id: &str,
) -> Result<Vec<RefSnapshotEntry>, ReconcileError> {
    let prefix = observed_ref_prefix(upstream_id);
    let output = run_git_expect_success(
        Some(repo_path),
        &[
            "for-each-ref".to_owned(),
            "--format=%(objectname) %(refname)".to_owned(),
            format!("{prefix}/heads"),
            format!("{prefix}/tags"),
        ],
    )?;
    parse_internal_observed_snapshot(upstream_id, &output.stdout)
}

fn observe_upstream(
    repo_path: &Path,
    upstream_id: &str,
    url: &str,
) -> Result<Result<Vec<RefSnapshotEntry>, String>, ReconcileError> {
    let prefix = observed_ref_prefix(upstream_id);
    let output = run_git(
        Some(repo_path),
        &[
            "fetch".to_owned(),
            "--prune".to_owned(),
            "--no-tags".to_owned(),
            url.to_owned(),
            format!("+refs/heads/*:{prefix}/heads/*"),
            format!("+refs/tags/*:{prefix}/tags/*"),
        ],
    )?;
    if !output.success {
        return Ok(Err(format_git_failure(&output)));
    }
    Ok(Ok(read_observed_refs(repo_path, upstream_id)?))
}

fn probe_atomic_capability(
    repo_path: &Path,
    upstream_url: &str,
    desired_snapshot: &[RefSnapshotEntry],
) -> Result<AtomicCapabilityVerdict, ReconcileError> {
    let mut args = vec![
        "push".to_owned(),
        "--porcelain".to_owned(),
        "--dry-run".to_owned(),
        "--atomic".to_owned(),
        upstream_url.to_owned(),
    ];
    args.extend(build_push_refspecs(desired_snapshot));
    let output = run_git(Some(repo_path), &args)?;
    if output.success {
        return Ok(AtomicCapabilityVerdict::Supported);
    }

    let detail = format_git_failure(&output).to_ascii_lowercase();
    if detail.contains("atomic") && (detail.contains("support") || detail.contains("advertis")) {
        Ok(AtomicCapabilityVerdict::Unsupported)
    } else {
        Ok(AtomicCapabilityVerdict::Inconclusive)
    }
}

fn push_desired_snapshot(
    repo_path: &Path,
    upstream_url: &str,
    desired_snapshot: &[RefSnapshotEntry],
    require_atomic: bool,
) -> Result<Result<(), String>, ReconcileError> {
    let mut args = vec!["push".to_owned(), "--porcelain".to_owned()];
    if require_atomic {
        args.push("--atomic".to_owned());
    }
    args.push(upstream_url.to_owned());
    args.extend(build_push_refspecs(desired_snapshot));

    let output = run_git(Some(repo_path), &args)?;
    if output.success {
        Ok(Ok(()))
    } else {
        Ok(Err(format_git_failure(&output)))
    }
}

fn run_git(repo_path: Option<&Path>, args: &[String]) -> Result<GitProcessOutput, ReconcileError> {
    let mut command = Command::new("git");
    if let Some(repo_path) = repo_path {
        command.arg(format!("--git-dir={}", repo_path.display()));
    }
    command.args(args);
    let output = command.output().map_err(|error| ReconcileError::SpawnGit {
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

fn run_git_expect_success_with_input(
    repo_path: Option<&Path>,
    args: &[String],
    input: &[u8],
) -> Result<GitProcessOutput, ReconcileError> {
    let output = run_git_with_input(repo_path, args, input)?;
    if output.success {
        Ok(output)
    } else {
        let detail = format_git_failure(&output);
        Err(ReconcileError::Git {
            args: output.args.clone(),
            status: output.status,
            detail,
        })
    }
}

fn run_git_with_input(
    repo_path: Option<&Path>,
    args: &[String],
    input: &[u8],
) -> Result<GitProcessOutput, ReconcileError> {
    let mut command = Command::new("git");
    if let Some(repo_path) = repo_path {
        command.arg(format!("--git-dir={}", repo_path.display()));
    }
    command.args(args);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| ReconcileError::SpawnGit {
        args: args.to_vec(),
        error,
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input)
            .map_err(|error| ReconcileError::WriteGitInput {
                args: args.to_vec(),
                error,
            })?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| ReconcileError::SpawnGit {
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
) -> Result<GitProcessOutput, ReconcileError> {
    let output = run_git(repo_path, args)?;
    if output.success {
        Ok(output)
    } else {
        let detail = format_git_failure(&output);
        Err(ReconcileError::Git {
            args: output.args.clone(),
            status: output.status,
            detail,
        })
    }
}

#[derive(Debug, Clone)]
struct GitProcessOutput {
    success: bool,
    status: Option<i32>,
    stdout: String,
    stderr: String,
    args: Vec<String>,
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

fn parse_ref_snapshot(source: &str) -> Result<Vec<RefSnapshotEntry>, ReconcileError> {
    let mut refs = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((oid, ref_name)) = trimmed.split_once(char::is_whitespace) else {
            return Err(ReconcileError::Git {
                args: vec!["parse-ref-snapshot".to_owned()],
                status: None,
                detail: format!("malformed ref line {trimmed}"),
            });
        };
        let ref_name = ref_name.trim().to_owned();
        if ref_name.ends_with("^{}") {
            continue;
        }
        refs.push(RefSnapshotEntry {
            ref_name,
            oid: oid.trim().to_owned(),
        });
    }
    refs.sort_by(|left, right| left.ref_name.cmp(&right.ref_name));
    Ok(refs)
}

fn parse_internal_observed_snapshot(
    upstream_id: &str,
    source: &str,
) -> Result<Vec<RefSnapshotEntry>, ReconcileError> {
    let prefix = observed_ref_prefix(upstream_id);
    let mut refs = Vec::new();
    for entry in parse_ref_snapshot(source)? {
        let ref_name = if let Some(rest) = entry.ref_name.strip_prefix(&format!("{prefix}/heads/"))
        {
            format!("refs/heads/{rest}")
        } else if let Some(rest) = entry.ref_name.strip_prefix(&format!("{prefix}/tags/")) {
            format!("refs/tags/{rest}")
        } else {
            return Err(ReconcileError::Git {
                args: vec!["parse-internal-observed-snapshot".to_owned()],
                status: None,
                detail: format!("unexpected internal observed ref {}", entry.ref_name),
            });
        };
        refs.push(RefSnapshotEntry {
            ref_name,
            oid: entry.oid,
        });
    }
    refs.sort_by(|left, right| left.ref_name.cmp(&right.ref_name));
    Ok(refs)
}

fn detect_divergence(
    authority_model: AuthorityModel,
    previous_observed: &[RefSnapshotEntry],
    observed_before: &[RefSnapshotEntry],
    desired_snapshot: &[RefSnapshotEntry],
) -> bool {
    authority_model == AuthorityModel::RelayAuthoritative
        && !previous_observed.is_empty()
        && !same_snapshot(previous_observed, observed_before)
        && !same_snapshot(observed_before, desired_snapshot)
}

fn classify_repository_safety(results: &[ReconcileUpstreamResult]) -> RepositorySafetyState {
    if results.iter().any(|result| result.divergent) {
        RepositorySafetyState::Divergent
    } else if results
        .iter()
        .all(|result| result.state == UpstreamConvergenceState::InSync)
    {
        RepositorySafetyState::Healthy
    } else {
        RepositorySafetyState::Degraded
    }
}

fn persist_divergence_markers(
    repo_path: &Path,
    repo_id: &str,
    run_id: &str,
    results: &[ReconcileUpstreamResult],
) -> Result<(), ReconcileError> {
    let existing = list_divergence_marker_refs(repo_path)?;
    let mut desired = BTreeMap::new();

    for result in results.iter().filter(|result| result.divergent) {
        let ref_name = divergence_ref_name(&result.upstream_id);
        let marker = DivergenceMarker {
            repo_id: repo_id.to_owned(),
            upstream_id: result.upstream_id.clone(),
            run_id: run_id.to_owned(),
            recorded_at_ms: current_time_ms(),
            detail: result.detail.clone(),
        };
        write_divergence_marker(repo_path, &ref_name, &marker)?;
        desired.insert(ref_name, ());
    }

    for (ref_name, _) in existing {
        if !desired.contains_key(&ref_name) {
            delete_internal_ref(repo_path, &ref_name)?;
        }
    }

    Ok(())
}

fn write_divergence_marker(
    repo_path: &Path,
    ref_name: &str,
    marker: &DivergenceMarker,
) -> Result<(), ReconcileError> {
    let encoded = serde_json::to_vec_pretty(marker).map_err(|error| ReconcileError::ParseJson {
        path: repo_path.to_path_buf(),
        error,
    })?;
    let oid = write_git_blob(repo_path, &encoded)?;
    update_internal_ref(repo_path, ref_name, &oid)
}

fn read_divergence_marker(repo_path: &Path, oid: &str) -> Result<DivergenceMarker, ReconcileError> {
    let output = run_git_expect_success(
        Some(repo_path),
        &["cat-file".to_owned(), "blob".to_owned(), oid.to_owned()],
    )?;
    serde_json::from_str(&output.stdout).map_err(|error| ReconcileError::ParseJson {
        path: repo_path.to_path_buf(),
        error,
    })
}

fn list_divergence_marker_refs(repo_path: &Path) -> Result<Vec<(String, String)>, ReconcileError> {
    list_internal_refs(repo_path, INTERNAL_DIVERGENCE_REF_PREFIX)
}

fn list_internal_refs(
    repo_path: &Path,
    prefix: &str,
) -> Result<Vec<(String, String)>, ReconcileError> {
    let output = run_git_expect_success(
        Some(repo_path),
        &[
            "for-each-ref".to_owned(),
            "--format=%(refname) %(objectname)".to_owned(),
            prefix.to_owned(),
        ],
    )?;
    parse_ref_targets(&output.stdout)
}

fn parse_ref_targets(source: &str) -> Result<Vec<(String, String)>, ReconcileError> {
    let mut refs = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((ref_name, oid)) = trimmed.split_once(char::is_whitespace) else {
            return Err(ReconcileError::Git {
                args: vec!["parse-ref-targets".to_owned()],
                status: None,
                detail: format!("malformed ref target line {trimmed}"),
            });
        };
        refs.push((ref_name.to_owned(), oid.trim().to_owned()));
    }
    refs.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(refs)
}

fn write_git_blob(repo_path: &Path, input: &[u8]) -> Result<String, ReconcileError> {
    let output = run_git_expect_success_with_input(
        Some(repo_path),
        &[
            "hash-object".to_owned(),
            "-w".to_owned(),
            "--stdin".to_owned(),
        ],
        input,
    )?;
    Ok(output.stdout.trim().to_owned())
}

fn update_internal_ref(repo_path: &Path, ref_name: &str, oid: &str) -> Result<(), ReconcileError> {
    run_git_expect_success(
        Some(repo_path),
        &["update-ref".to_owned(), ref_name.to_owned(), oid.to_owned()],
    )?;
    Ok(())
}

fn delete_internal_ref(repo_path: &Path, ref_name: &str) -> Result<(), ReconcileError> {
    let output = run_git(
        Some(repo_path),
        &[
            "update-ref".to_owned(),
            "-d".to_owned(),
            ref_name.to_owned(),
        ],
    )?;
    let detail = format_git_failure(&output);
    if output.success || detail.contains("not a valid ref") {
        Ok(())
    } else {
        Err(ReconcileError::Git {
            args: output.args,
            status: output.status,
            detail,
        })
    }
}

fn build_push_refspecs(snapshot: &[RefSnapshotEntry]) -> Vec<String> {
    snapshot
        .iter()
        .map(|entry| format!("{}:{}", entry.ref_name, entry.ref_name))
        .collect()
}

fn same_snapshot(left: &[RefSnapshotEntry], right: &[RefSnapshotEntry]) -> bool {
    snapshot_to_map(left) == snapshot_to_map(right)
}

fn snapshot_to_map(snapshot: &[RefSnapshotEntry]) -> BTreeMap<String, String> {
    snapshot
        .iter()
        .map(|entry| (entry.ref_name.clone(), entry.oid.clone()))
        .collect()
}

fn matches_exported_ref(patterns: &[String], ref_name: &str) -> bool {
    patterns.iter().any(|pattern| {
        if let Some(prefix) = pattern.strip_suffix('*') {
            ref_name.starts_with(prefix)
        } else {
            ref_name == pattern
        }
    })
}

fn observed_ref_prefix(upstream_id: &str) -> String {
    format!(
        "{INTERNAL_UPSTREAM_REF_PREFIX}/{}",
        sanitize_ref_component(upstream_id)
    )
}

fn divergence_ref_name(upstream_id: &str) -> String {
    format!(
        "{INTERNAL_DIVERGENCE_REF_PREFIX}/{}",
        sanitize_ref_component(upstream_id)
    )
}

fn sanitize_ref_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '/' | '-' | '_' | '.') {
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

fn sanitize_path_component(value: &str) -> String {
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

fn reconcile_lock_path(state_root: &Path, repo_id: &str) -> PathBuf {
    state_root
        .join("reconcile")
        .join("locks")
        .join(format!("{}.lock", sanitize_path_component(repo_id)))
}

fn lock_metadata_path(lock_path: &Path) -> PathBuf {
    lock_path.join("metadata.json")
}

fn in_progress_marker_path(state_root: &Path, repo_id: &str) -> PathBuf {
    state_root
        .join("reconcile")
        .join("in-progress")
        .join(format!("{}.json", sanitize_path_component(repo_id)))
}

fn pending_request_path(state_root: &Path, repo_id: &str) -> PathBuf {
    pending_request_directory(state_root).join(format!("{}.json", sanitize_path_component(repo_id)))
}

fn pending_request_directory(state_root: &Path) -> PathBuf {
    state_root.join("reconcile").join("pending")
}

fn run_record_path(state_root: &Path, repo_id: &str, run_id: &str) -> PathBuf {
    run_record_directory(state_root, repo_id).join(format!("{run_id}.json"))
}

fn run_record_directory(state_root: &Path, repo_id: &str) -> PathBuf {
    state_root
        .join("reconcile")
        .join("runs")
        .join(sanitize_path_component(repo_id))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), ReconcileError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| ReconcileError::CreateDir {
            path: parent.to_path_buf(),
            error,
        })?;
    }
    let encoded = serde_json::to_vec_pretty(value).map_err(|error| ReconcileError::ParseJson {
        path: path.to_path_buf(),
        error,
    })?;
    let mut file = fs::File::create(path).map_err(|error| ReconcileError::Write {
        path: path.to_path_buf(),
        error,
    })?;
    file.write_all(&encoded)
        .map_err(|error| ReconcileError::Write {
            path: path.to_path_buf(),
            error,
        })?;
    file.write_all(b"\n")
        .map_err(|error| ReconcileError::Write {
            path: path.to_path_buf(),
            error,
        })?;
    Ok(())
}

fn read_json_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, ReconcileError> {
    if !path.exists() {
        return Ok(None);
    }
    let source = fs::read_to_string(path).map_err(|error| ReconcileError::Read {
        path: path.to_path_buf(),
        error,
    })?;
    let value = serde_json::from_str(&source).map_err(|error| ReconcileError::ParseJson {
        path: path.to_path_buf(),
        error,
    })?;
    Ok(Some(value))
}

fn current_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn generate_run_id() -> String {
    format!("reconcile-{}-{}", std::process::id(), current_time_ms())
}

fn pid_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(pid as i32, 0) };
        if rc == 0 {
            return true;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(code) if code == libc::EPERM => true,
            Some(code) if code == libc::ESRCH => false,
            _ => false,
        }
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::TempDir;

    use crate::classification::RepositorySafetyState;
    use crate::config::{
        AppConfig, AuthProfile, AuthProfileKind, AuthorityModel, DeploymentProfile,
        FreshnessPolicy, GitOnlyCommandMode, GitService, ListenConfig, MigrationConfig,
        MigrationTransport, PathsConfig, PolicyConfig, PushAckPolicy, ReconcileConfig,
        ReconcilePolicy, RepositoryDescriptor, RepositoryLifecycle, RepositoryMode, ServiceManager,
        SupportedPlatform, TargetedRelockMode, TrackingRefPlacement, WorkerMode, WriteUpstream,
    };

    use super::{
        enqueue_reconcile_request, observed_ref_prefix, read_observed_refs, reconcile_lock_path,
        reconcile_repository, run_record_path, ReconcileRunStatus, RefSnapshotEntry,
        UpstreamConvergenceState,
    };

    fn init_bare_repo(path: &Path) {
        Command::new("git")
            .args(["-c", "init.defaultBranch=main", "init", "--bare"])
            .arg(path)
            .status()
            .expect("git init")
            .success()
            .then_some(())
            .expect("git init success");
    }

    fn init_work_repo(path: &Path) {
        fs::create_dir_all(path).expect("work repo");
        Command::new("git")
            .args(["-c", "init.defaultBranch=main", "init"])
            .arg(path)
            .status()
            .expect("git init")
            .success()
            .then_some(())
            .expect("git init success");
        Command::new("git")
            .args([
                "-C",
                path.to_str().expect("path"),
                "config",
                "user.name",
                "Git Relay Test",
            ])
            .status()
            .expect("git config")
            .success()
            .then_some(())
            .expect("git config success");
        Command::new("git")
            .args([
                "-C",
                path.to_str().expect("path"),
                "config",
                "user.email",
                "git-relay@example.com",
            ])
            .status()
            .expect("git config")
            .success()
            .then_some(())
            .expect("git config success");
    }

    fn commit_file(path: &Path, file_name: &str, contents: &str, message: &str) {
        fs::write(path.join(file_name), contents).expect("write file");
        Command::new("git")
            .args(["-C", path.to_str().expect("path"), "add", file_name])
            .status()
            .expect("git add")
            .success()
            .then_some(())
            .expect("git add success");
        Command::new("git")
            .args(["-C", path.to_str().expect("path"), "commit", "-m", message])
            .status()
            .expect("git commit")
            .success()
            .then_some(())
            .expect("git commit success");
    }

    fn push_branch(path: &Path, remote: &Path, branch: &str) {
        Command::new("git")
            .args([
                "-C",
                path.to_str().expect("path"),
                "push",
                remote.to_str().expect("remote"),
                &format!("HEAD:refs/heads/{branch}"),
            ])
            .status()
            .expect("git push")
            .success()
            .then_some(())
            .expect("git push success");
    }

    fn configure_authoritative_repo(path: &Path) {
        let entries = [
            ("receive.fsckObjects", "true"),
            ("transfer.hideRefs", "refs/git-relay"),
            ("uploadpack.hideRefs", "refs/git-relay"),
            ("receive.hideRefs", "refs/git-relay"),
            ("uploadpack.allowReachableSHA1InWant", "false"),
            ("uploadpack.allowAnySHA1InWant", "false"),
            ("uploadpack.allowTipSHA1InWant", "false"),
            ("core.fsync", "all"),
            ("core.fsyncMethod", "fsync"),
        ];
        for (key, value) in entries {
            Command::new("git")
                .arg(format!("--git-dir={}", path.display()))
                .args(["config", key, value])
                .status()
                .expect("git config")
                .success()
                .then_some(())
                .expect("git config success");
        }
    }

    fn app_config(temp: &TempDir) -> AppConfig {
        AppConfig {
            listen: ListenConfig {
                ssh: "127.0.0.1:4222".to_owned(),
                https: None,
                enable_http_read: false,
                enable_http_write: false,
            },
            paths: PathsConfig {
                state_root: temp.path().to_path_buf(),
                repo_root: temp.path().join("repos"),
                repo_config_root: temp.path().join("repos.d"),
            },
            reconcile: ReconcileConfig {
                on_push: true,
                manual_enabled: true,
                periodic_enabled: false,
                worker_mode: WorkerMode::ShortLived,
                lock_timeout_ms: 10,
            },
            policy: PolicyConfig {
                default_repo_mode: RepositoryMode::CacheOnly,
                default_refresh: FreshnessPolicy::Ttl("60s".parse().expect("duration")),
                negative_cache_ttl: "5s".parse().expect("duration"),
                default_push_ack: PushAckPolicy::LocalCommit,
            },
            migration: MigrationConfig {
                supported_targets: vec![MigrationTransport::GitHttps, MigrationTransport::GitSsh],
                refuse_dirty_worktree: true,
                targeted_relock_mode: TargetedRelockMode::ValidatedOnly,
            },
            deployment: DeploymentProfile {
                platform: SupportedPlatform::Macos,
                service_manager: ServiceManager::Launchd,
                service_label: "dev.git-relay".to_owned(),
                git_only_command_mode: GitOnlyCommandMode::OpensshForceCommand,
                forced_command_wrapper: "/usr/local/bin/git-relay-ssh-force-command".into(),
                disable_forwarding: true,
                runtime_secret_env_file: temp.path().join("runtime.env"),
                required_secret_keys: vec!["TOKEN".to_owned()],
                allowed_git_services: vec![GitService::GitUploadPack, GitService::GitReceivePack],
                supported_filesystems: vec!["apfs".to_owned()],
            },
            auth_profiles: BTreeMap::from([(
                "github-write".to_owned(),
                AuthProfile {
                    kind: AuthProfileKind::SshKey,
                    secret_ref: "env:TOKEN".to_owned(),
                },
            )]),
        }
    }

    fn descriptor(repo_path: &Path, upstreams: Vec<WriteUpstream>) -> RepositoryDescriptor {
        RepositoryDescriptor {
            repo_id: "github.com/example/repo.git".to_owned(),
            canonical_identity: "github.com/example/repo.git".to_owned(),
            repo_path: repo_path.to_path_buf(),
            mode: RepositoryMode::Authoritative,
            lifecycle: RepositoryLifecycle::Ready,
            authority_model: AuthorityModel::RelayAuthoritative,
            tracking_refs: TrackingRefPlacement::SameRepoHidden,
            refresh: FreshnessPolicy::AuthoritativeLocal,
            push_ack: PushAckPolicy::LocalCommit,
            reconcile_policy: ReconcilePolicy::OnPushManual,
            exported_refs: vec!["refs/heads/*".to_owned(), "refs/tags/*".to_owned()],
            read_upstreams: Vec::new(),
            write_upstreams: upstreams,
        }
    }

    #[test]
    fn reconcile_records_mixed_outcomes_under_one_run() {
        let temp = TempDir::new().expect("tempdir");
        let config = app_config(&temp);
        fs::create_dir_all(&config.paths.repo_root).expect("repo root");
        let authoritative = config.paths.repo_root.join("repo.git");
        init_bare_repo(&authoritative);
        configure_authoritative_repo(&authoritative);

        let work = temp.path().join("work");
        init_work_repo(&work);
        commit_file(&work, "README.md", "hello\n", "initial");
        Command::new("git")
            .args([
                "-C",
                work.to_str().expect("path"),
                "remote",
                "add",
                "origin",
                authoritative.to_str().expect("repo"),
            ])
            .status()
            .expect("git remote add")
            .success()
            .then_some(())
            .expect("git remote add success");
        push_branch(&work, &authoritative, "main");

        let upstream_ok = temp.path().join("upstream-ok.git");
        init_bare_repo(&upstream_ok);
        let descriptor = descriptor(
            &authoritative,
            vec![
                WriteUpstream {
                    name: "alpha".to_owned(),
                    url: upstream_ok.to_str().expect("path").to_owned(),
                    auth_profile: "github-write".to_owned(),
                    require_atomic: false,
                },
                WriteUpstream {
                    name: "beta".to_owned(),
                    url: temp
                        .path()
                        .join("missing-upstream.git")
                        .to_str()
                        .expect("path")
                        .to_owned(),
                    auth_profile: "github-write".to_owned(),
                    require_atomic: false,
                },
            ],
        );

        let run = reconcile_repository(&config, &descriptor).expect("reconcile");

        assert_eq!(
            run.captured_upstreams,
            vec!["alpha".to_owned(), "beta".to_owned()]
        );
        assert_eq!(run.status, ReconcileRunStatus::Completed);
        assert_eq!(run.repo_safety, RepositorySafetyState::Degraded);
        assert_eq!(run.upstream_results.len(), 2);
        assert_eq!(
            run.upstream_results[0].state,
            UpstreamConvergenceState::InSync
        );
        assert_eq!(
            run.upstream_results[1].state,
            UpstreamConvergenceState::Stalled
        );

        let observed = read_observed_refs(&authoritative, "alpha").expect("observed refs");
        assert_eq!(
            observed,
            vec![RefSnapshotEntry {
                ref_name: "refs/heads/main".to_owned(),
                oid: run.desired_snapshot[0].oid.clone(),
            }]
        );
    }

    #[test]
    fn reconcile_detects_direct_upstream_divergence_from_fresh_observation() {
        let temp = TempDir::new().expect("tempdir");
        let config = app_config(&temp);
        fs::create_dir_all(&config.paths.repo_root).expect("repo root");
        let authoritative = config.paths.repo_root.join("repo.git");
        init_bare_repo(&authoritative);
        configure_authoritative_repo(&authoritative);

        let upstream = temp.path().join("upstream.git");
        init_bare_repo(&upstream);

        let work = temp.path().join("work");
        init_work_repo(&work);
        commit_file(&work, "README.md", "hello\n", "initial");
        push_branch(&work, &authoritative, "main");

        let descriptor = descriptor(
            &authoritative,
            vec![WriteUpstream {
                name: "alpha".to_owned(),
                url: upstream.to_str().expect("path").to_owned(),
                auth_profile: "github-write".to_owned(),
                require_atomic: false,
            }],
        );

        let first = reconcile_repository(&config, &descriptor).expect("first reconcile");
        assert_eq!(first.repo_safety, RepositorySafetyState::Healthy);

        let external = temp.path().join("external");
        Command::new("git")
            .args([
                "clone",
                upstream.to_str().expect("path"),
                external.to_str().expect("path"),
            ])
            .status()
            .expect("git clone")
            .success()
            .then_some(())
            .expect("git clone success");
        Command::new("git")
            .args([
                "-C",
                external.to_str().expect("path"),
                "config",
                "user.name",
                "Git Relay Test",
            ])
            .status()
            .expect("git config")
            .success()
            .then_some(())
            .expect("git config success");
        Command::new("git")
            .args([
                "-C",
                external.to_str().expect("path"),
                "config",
                "user.email",
                "git-relay@example.com",
            ])
            .status()
            .expect("git config")
            .success()
            .then_some(())
            .expect("git config success");
        commit_file(&external, "README.md", "external\n", "external mutation");
        push_branch(&external, &upstream, "main");

        let second = reconcile_repository(&config, &descriptor).expect("second reconcile");
        assert_eq!(second.repo_safety, RepositorySafetyState::Divergent);
        assert!(second.upstream_results[0].divergent);
        assert_eq!(
            second.upstream_results[0].state,
            UpstreamConvergenceState::OutOfSync
        );
    }

    #[test]
    fn reconcile_breaks_stale_lock_and_supersedes_stale_run_marker() {
        let temp = TempDir::new().expect("tempdir");
        let config = app_config(&temp);
        fs::create_dir_all(&config.paths.repo_root).expect("repo root");
        let authoritative = config.paths.repo_root.join("repo.git");
        init_bare_repo(&authoritative);
        configure_authoritative_repo(&authoritative);

        let upstream = temp.path().join("upstream.git");
        init_bare_repo(&upstream);
        let work = temp.path().join("work");
        init_work_repo(&work);
        commit_file(&work, "README.md", "hello\n", "initial");
        push_branch(&work, &authoritative, "main");

        let descriptor = descriptor(
            &authoritative,
            vec![WriteUpstream {
                name: "alpha".to_owned(),
                url: upstream.to_str().expect("path").to_owned(),
                auth_profile: "github-write".to_owned(),
                require_atomic: false,
            }],
        );

        let old_run_id = "reconcile-stale-run";
        let stale_record = super::ReconcileRunRecord {
            run_id: old_run_id.to_owned(),
            repo_id: descriptor.repo_id.clone(),
            repo_path: descriptor.repo_path.clone(),
            started_at_ms: 1,
            completed_at_ms: None,
            desired_snapshot: Vec::new(),
            captured_upstreams: vec!["alpha".to_owned()],
            repo_safety: RepositorySafetyState::Degraded,
            status: ReconcileRunStatus::InProgress,
            superseded_by: None,
            upstream_results: Vec::new(),
        };
        super::write_json(
            &run_record_path(&config.paths.state_root, &descriptor.repo_id, old_run_id),
            &stale_record,
        )
        .expect("write stale run");
        super::write_json(
            &super::in_progress_marker_path(&config.paths.state_root, &descriptor.repo_id),
            &super::InProgressMarker {
                repo_id: descriptor.repo_id.clone(),
                run_id: old_run_id.to_owned(),
                pid: 999_999,
                started_at_ms: 1,
            },
        )
        .expect("write stale marker");

        let lock_path = reconcile_lock_path(&config.paths.state_root, &descriptor.repo_id);
        fs::create_dir_all(&lock_path).expect("lock dir");
        super::write_json(
            &lock_path.join("metadata.json"),
            &super::LockMetadata {
                repo_id: descriptor.repo_id.clone(),
                run_id: old_run_id.to_owned(),
                pid: 999_999,
                acquired_at_ms: 1,
            },
        )
        .expect("write lock metadata");

        let run = reconcile_repository(&config, &descriptor).expect("reconcile");
        assert_eq!(run.status, ReconcileRunStatus::Completed);

        let stale_path = run_record_path(&config.paths.state_root, &descriptor.repo_id, old_run_id);
        let stale_json = fs::read_to_string(stale_path).expect("read stale run");
        assert!(stale_json.contains("\"status\": \"superseded\""));
        assert!(stale_json.contains(&run.run_id));
    }

    #[test]
    fn enqueue_reconcile_request_coalesces_by_repo_path() {
        let temp = TempDir::new().expect("tempdir");
        let config = app_config(&temp);
        let descriptor = descriptor(
            &config.paths.repo_root.join("repo.git"),
            vec![WriteUpstream {
                name: "alpha".to_owned(),
                url: "/tmp/upstream.git".to_owned(),
                auth_profile: "github-write".to_owned(),
                require_atomic: false,
            }],
        );

        let pending =
            enqueue_reconcile_request(&config, &descriptor, Some("push-1"), Some("req-1"))
                .expect("enqueue");
        assert_eq!(pending.last_push_id.as_deref(), Some("push-1"));
        let path = super::pending_request_path(&config.paths.state_root, &descriptor.repo_id);
        let source = fs::read_to_string(path).expect("pending request");
        assert!(source.contains("\"last_push_id\": \"push-1\""));
    }

    #[test]
    fn observed_ref_prefix_stays_under_internal_namespace() {
        assert_eq!(
            observed_ref_prefix("github-write"),
            "refs/git-relay/upstreams/github-write"
        );
    }
}
