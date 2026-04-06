use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{
    AppConfig, FreshnessPolicy, RepositoryDescriptor, RepositoryLifecycle, RepositoryMode,
};
use crate::upstream::ProbeFailureClass;

const LOCK_WAIT_POLL_MS: u64 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadPreparationIntent {
    ClientServe,
    OperatorPrepare,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadAction {
    ServedLocal,
    Refreshed,
    ServedStale,
    FailedNegativeCache,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadPreparationReport {
    pub repo_id: String,
    pub repo_path: PathBuf,
    pub repo_mode: RepositoryMode,
    pub refresh_policy: String,
    pub action: ReadAction,
    pub refreshed: bool,
    pub stale_served: bool,
    pub negative_cache_hit: bool,
    pub source_upstream: Option<String>,
    pub detail: Option<String>,
    pub last_successful_refresh_at_ms: Option<u128>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RefreshState {
    repo_id: String,
    last_successful_refresh_at_ms: u128,
    last_upstream_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct NegativeCacheEntry {
    repo_id: String,
    failure_class: ProbeFailureClass,
    detail: String,
    expires_at_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RefreshLockMetadata {
    repo_id: String,
    pid: u32,
    acquired_at_ms: u128,
}

struct RefreshLockGuard {
    path: PathBuf,
}

impl Drop for RefreshLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug, Error)]
pub enum ReadPathError {
    #[error("repository {repo_id} is {lifecycle:?} and not ready for normal read traffic")]
    RepositoryNotReady {
        repo_id: String,
        lifecycle: RepositoryLifecycle,
    },
    #[error("repository {repo_id} is cache-only and must define at least one read upstream")]
    MissingReadUpstreams { repo_id: String },
    #[error("repository {repo_id} is cache-only and cannot use authoritative-local freshness")]
    InvalidCacheOnlyFreshness { repo_id: String },
    #[error("repository {repo_id} is authoritative and must use authoritative-local freshness")]
    InvalidAuthoritativeFreshness { repo_id: String },
    #[error(
        "repository {repo_id} cannot be served locally because freshness policy requires explicit operator preparation before any local refs exist"
    )]
    NoLocalStateForServe { repo_id: String },
    #[error("repository {repo_id} has no stale local refs to serve after upstream refresh failed")]
    NoLocalStateForStaleServe { repo_id: String },
    #[error("negative cache is active for repository {repo_id}: {detail}")]
    NegativeCacheActive { repo_id: String, detail: String },
    #[error("all read upstreams failed for repository {repo_id}: {detail}")]
    RefreshFailed { repo_id: String, detail: String },
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

pub fn prepare_repository_for_read(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
) -> Result<ReadPreparationReport, ReadPathError> {
    prepare_repository_for_read_with_intent(config, descriptor, ReadPreparationIntent::ClientServe)
}

pub fn operator_prepare_repository_for_read(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
) -> Result<ReadPreparationReport, ReadPathError> {
    prepare_repository_for_read_with_intent(
        config,
        descriptor,
        ReadPreparationIntent::OperatorPrepare,
    )
}

fn prepare_repository_for_read_with_intent(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
    intent: ReadPreparationIntent,
) -> Result<ReadPreparationReport, ReadPathError> {
    if descriptor.lifecycle != RepositoryLifecycle::Ready {
        return Err(ReadPathError::RepositoryNotReady {
            repo_id: descriptor.repo_id.clone(),
            lifecycle: descriptor.lifecycle,
        });
    }

    match descriptor.mode {
        RepositoryMode::Authoritative => prepare_authoritative_read(config, descriptor),
        RepositoryMode::CacheOnly => prepare_cache_only_read(config, descriptor, intent),
    }
}

fn prepare_authoritative_read(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
) -> Result<ReadPreparationReport, ReadPathError> {
    if descriptor.refresh != FreshnessPolicy::AuthoritativeLocal {
        return Err(ReadPathError::InvalidAuthoritativeFreshness {
            repo_id: descriptor.repo_id.clone(),
        });
    }

    let state = read_refresh_state(&config.paths.state_root, &descriptor.repo_id)?;
    Ok(ReadPreparationReport {
        repo_id: descriptor.repo_id.clone(),
        repo_path: descriptor.repo_path.clone(),
        repo_mode: descriptor.mode,
        refresh_policy: descriptor.refresh.to_string(),
        action: ReadAction::ServedLocal,
        refreshed: false,
        stale_served: false,
        negative_cache_hit: false,
        source_upstream: None,
        detail: None,
        last_successful_refresh_at_ms: state.map(|state| state.last_successful_refresh_at_ms),
    })
}

fn prepare_cache_only_read(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
    intent: ReadPreparationIntent,
) -> Result<ReadPreparationReport, ReadPathError> {
    if descriptor.read_upstreams.is_empty() {
        return Err(ReadPathError::MissingReadUpstreams {
            repo_id: descriptor.repo_id.clone(),
        });
    }
    if descriptor.refresh == FreshnessPolicy::AuthoritativeLocal {
        return Err(ReadPathError::InvalidCacheOnlyFreshness {
            repo_id: descriptor.repo_id.clone(),
        });
    }

    let now_ms = current_time_ms();
    let has_local_refs = repository_has_visible_refs(&descriptor.repo_path)?;
    let state = read_refresh_state(&config.paths.state_root, &descriptor.repo_id)?;

    if can_serve_local_without_refresh(
        descriptor.refresh,
        state.as_ref(),
        has_local_refs,
        now_ms,
        intent,
    ) {
        clear_negative_cache(config, &descriptor.repo_id);
        return Ok(ReadPreparationReport {
            repo_id: descriptor.repo_id.clone(),
            repo_path: descriptor.repo_path.clone(),
            repo_mode: descriptor.mode,
            refresh_policy: descriptor.refresh.to_string(),
            action: ReadAction::ServedLocal,
            refreshed: false,
            stale_served: false,
            negative_cache_hit: false,
            source_upstream: state.as_ref().map(|state| state.last_upstream_id.clone()),
            detail: None,
            last_successful_refresh_at_ms: state.map(|state| state.last_successful_refresh_at_ms),
        });
    }

    if !policy_allows_refresh(descriptor.refresh, intent) {
        return Err(ReadPathError::NoLocalStateForServe {
            repo_id: descriptor.repo_id.clone(),
        });
    }

    if honor_initial_negative_cache(descriptor.refresh, intent) {
        if let Some(entry) = active_negative_cache(config, &descriptor.repo_id, now_ms)? {
            return negative_cache_response(descriptor, state.as_ref(), entry, has_local_refs);
        }
    }

    loop {
        if let Some(_guard) = acquire_refresh_lock(config, descriptor)? {
            let refreshed_state =
                read_refresh_state(&config.paths.state_root, &descriptor.repo_id)?;
            let has_local_refs_now = repository_has_visible_refs(&descriptor.repo_path)?;
            let now_ms = current_time_ms();
            if can_serve_local_without_refresh(
                descriptor.refresh,
                refreshed_state.as_ref(),
                has_local_refs_now,
                now_ms,
                intent,
            ) {
                clear_negative_cache(config, &descriptor.repo_id);
                return Ok(ReadPreparationReport {
                    repo_id: descriptor.repo_id.clone(),
                    repo_path: descriptor.repo_path.clone(),
                    repo_mode: descriptor.mode,
                    refresh_policy: descriptor.refresh.to_string(),
                    action: ReadAction::ServedLocal,
                    refreshed: false,
                    stale_served: false,
                    negative_cache_hit: false,
                    source_upstream: refreshed_state
                        .as_ref()
                        .map(|state| state.last_upstream_id.clone()),
                    detail: None,
                    last_successful_refresh_at_ms: refreshed_state
                        .map(|state| state.last_successful_refresh_at_ms),
                });
            }

            match refresh_from_upstreams(config, descriptor) {
                Ok(state) => {
                    clear_negative_cache(config, &descriptor.repo_id);
                    return Ok(ReadPreparationReport {
                        repo_id: descriptor.repo_id.clone(),
                        repo_path: descriptor.repo_path.clone(),
                        repo_mode: descriptor.mode,
                        refresh_policy: descriptor.refresh.to_string(),
                        action: ReadAction::Refreshed,
                        refreshed: true,
                        stale_served: false,
                        negative_cache_hit: false,
                        source_upstream: Some(state.last_upstream_id.clone()),
                        detail: None,
                        last_successful_refresh_at_ms: Some(state.last_successful_refresh_at_ms),
                    });
                }
                Err(error) => {
                    let detail = error.to_string();
                    let failure_class = classify_refresh_failure(&detail);
                    write_negative_cache(
                        config,
                        &descriptor.repo_id,
                        NegativeCacheEntry {
                            repo_id: descriptor.repo_id.clone(),
                            failure_class,
                            detail: detail.clone(),
                            expires_at_ms: now_ms
                                + config.policy.negative_cache_ttl.as_duration().as_millis(),
                        },
                    )?;

                    return match descriptor.refresh {
                        FreshnessPolicy::StaleIfError if has_local_refs_now => {
                            Ok(ReadPreparationReport {
                                repo_id: descriptor.repo_id.clone(),
                                repo_path: descriptor.repo_path.clone(),
                                repo_mode: descriptor.mode,
                                refresh_policy: descriptor.refresh.to_string(),
                                action: ReadAction::ServedStale,
                                refreshed: false,
                                stale_served: true,
                                negative_cache_hit: false,
                                source_upstream: refreshed_state
                                    .as_ref()
                                    .map(|state| state.last_upstream_id.clone()),
                                detail: Some(detail),
                                last_successful_refresh_at_ms: refreshed_state
                                    .map(|state| state.last_successful_refresh_at_ms),
                            })
                        }
                        FreshnessPolicy::StaleIfError => {
                            Err(ReadPathError::NoLocalStateForStaleServe {
                                repo_id: descriptor.repo_id.clone(),
                            })
                        }
                        _ => Err(ReadPathError::RefreshFailed {
                            repo_id: descriptor.repo_id.clone(),
                            detail,
                        }),
                    };
                }
            }
        }

        let refreshed_state = read_refresh_state(&config.paths.state_root, &descriptor.repo_id)?;
        let has_local_refs_now = repository_has_visible_refs(&descriptor.repo_path)?;
        let now_ms = current_time_ms();
        if can_serve_local_without_refresh(
            descriptor.refresh,
            refreshed_state.as_ref(),
            has_local_refs_now,
            now_ms,
            intent,
        ) {
            clear_negative_cache(config, &descriptor.repo_id);
            return Ok(ReadPreparationReport {
                repo_id: descriptor.repo_id.clone(),
                repo_path: descriptor.repo_path.clone(),
                repo_mode: descriptor.mode,
                refresh_policy: descriptor.refresh.to_string(),
                action: ReadAction::ServedLocal,
                refreshed: false,
                stale_served: false,
                negative_cache_hit: false,
                source_upstream: refreshed_state
                    .as_ref()
                    .map(|state| state.last_upstream_id.clone()),
                detail: None,
                last_successful_refresh_at_ms: refreshed_state
                    .map(|state| state.last_successful_refresh_at_ms),
            });
        }

        if let Some(entry) = active_negative_cache(config, &descriptor.repo_id, now_ms)? {
            return negative_cache_response(
                descriptor,
                refreshed_state.as_ref(),
                entry,
                has_local_refs_now,
            );
        }

        continue;
    }
}

fn can_serve_local_without_refresh(
    policy: FreshnessPolicy,
    state: Option<&RefreshState>,
    has_local_refs: bool,
    now_ms: u128,
    intent: ReadPreparationIntent,
) -> bool {
    match policy {
        FreshnessPolicy::ManualOnly => {
            intent == ReadPreparationIntent::ClientServe && has_local_refs
        }
        FreshnessPolicy::Ttl(ttl) => state
            .map(|state| {
                has_local_refs
                    && now_ms.saturating_sub(state.last_successful_refresh_at_ms)
                        <= ttl.as_duration().as_millis()
            })
            .unwrap_or(false),
        FreshnessPolicy::AuthoritativeLocal => true,
        FreshnessPolicy::AlwaysRefresh | FreshnessPolicy::StaleIfError => false,
    }
}

fn policy_allows_refresh(policy: FreshnessPolicy, intent: ReadPreparationIntent) -> bool {
    match policy {
        FreshnessPolicy::AuthoritativeLocal => false,
        FreshnessPolicy::ManualOnly => intent == ReadPreparationIntent::OperatorPrepare,
        FreshnessPolicy::Ttl(_)
        | FreshnessPolicy::AlwaysRefresh
        | FreshnessPolicy::StaleIfError => true,
    }
}

fn honor_initial_negative_cache(policy: FreshnessPolicy, intent: ReadPreparationIntent) -> bool {
    !matches!(
        (policy, intent),
        (
            FreshnessPolicy::ManualOnly,
            ReadPreparationIntent::OperatorPrepare
        )
    )
}

fn negative_cache_response(
    descriptor: &RepositoryDescriptor,
    state: Option<&RefreshState>,
    entry: NegativeCacheEntry,
    has_local_refs: bool,
) -> Result<ReadPreparationReport, ReadPathError> {
    match descriptor.refresh {
        FreshnessPolicy::StaleIfError if has_local_refs => Ok(ReadPreparationReport {
            repo_id: descriptor.repo_id.clone(),
            repo_path: descriptor.repo_path.clone(),
            repo_mode: descriptor.mode,
            refresh_policy: descriptor.refresh.to_string(),
            action: ReadAction::ServedStale,
            refreshed: false,
            stale_served: true,
            negative_cache_hit: true,
            source_upstream: state.map(|state| state.last_upstream_id.clone()),
            detail: Some(entry.detail),
            last_successful_refresh_at_ms: state.map(|state| state.last_successful_refresh_at_ms),
        }),
        _ => Err(ReadPathError::NegativeCacheActive {
            repo_id: descriptor.repo_id.clone(),
            detail: entry.detail,
        }),
    }
}

fn refresh_from_upstreams(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
) -> Result<RefreshState, ReadPathError> {
    let mut failures = Vec::new();
    for upstream in &descriptor.read_upstreams {
        match refresh_from_one_upstream(&descriptor.repo_path, &upstream.url) {
            Ok(()) => {
                let state = RefreshState {
                    repo_id: descriptor.repo_id.clone(),
                    last_successful_refresh_at_ms: current_time_ms(),
                    last_upstream_id: upstream.name.clone(),
                };
                write_refresh_state(config, &descriptor.repo_id, &state)?;
                return Ok(state);
            }
            Err(error) => failures.push(format!("{}: {error}", upstream.name)),
        }
    }

    Err(ReadPathError::RefreshFailed {
        repo_id: descriptor.repo_id.clone(),
        detail: failures.join(" | "),
    })
}

fn refresh_from_one_upstream(repo_path: &Path, url: &str) -> Result<(), ReadPathError> {
    run_git_expect_success(
        Some(repo_path),
        &[
            "fetch".to_owned(),
            "--prune".to_owned(),
            "--prune-tags".to_owned(),
            url.to_owned(),
            "+refs/heads/*:refs/heads/*".to_owned(),
            "+refs/tags/*:refs/tags/*".to_owned(),
        ],
    )?;
    Ok(())
}

fn repository_has_visible_refs(repo_path: &Path) -> Result<bool, ReadPathError> {
    let output = run_git_expect_success(
        Some(repo_path),
        &[
            "for-each-ref".to_owned(),
            "--format=%(refname)".to_owned(),
            "refs/heads".to_owned(),
            "refs/tags".to_owned(),
        ],
    )?;
    Ok(output.stdout.lines().any(|line| !line.trim().is_empty()))
}

fn acquire_refresh_lock(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
) -> Result<Option<RefreshLockGuard>, ReadPathError> {
    let lock_path = refresh_lock_path(&config.paths.state_root, &descriptor.repo_id);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|error| ReadPathError::CreateDir {
            path: parent.to_path_buf(),
            error,
        })?;
    }

    loop {
        match fs::create_dir(&lock_path) {
            Ok(()) => {
                let metadata = RefreshLockMetadata {
                    repo_id: descriptor.repo_id.clone(),
                    pid: std::process::id(),
                    acquired_at_ms: current_time_ms(),
                };
                write_json(&lock_path.join("metadata.json"), &metadata)?;
                return Ok(Some(RefreshLockGuard { path: lock_path }));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if lock_is_stale(config, &lock_path)? {
                    let _ = fs::remove_dir_all(&lock_path);
                    continue;
                }
                thread::sleep(Duration::from_millis(LOCK_WAIT_POLL_MS));
                return Ok(None);
            }
            Err(error) => {
                return Err(ReadPathError::CreateDir {
                    path: lock_path,
                    error,
                });
            }
        }
    }
}

fn lock_is_stale(config: &AppConfig, lock_path: &Path) -> Result<bool, ReadPathError> {
    let metadata =
        match read_json_optional::<RefreshLockMetadata>(&lock_path.join("metadata.json"))? {
            Some(metadata) => metadata,
            None => return Ok(true),
        };
    let age_ms = current_time_ms().saturating_sub(metadata.acquired_at_ms) as u64;
    if age_ms <= config.reconcile.lock_timeout_ms {
        return Ok(false);
    }
    Ok(!pid_is_alive(metadata.pid))
}

fn active_negative_cache(
    config: &AppConfig,
    repo_id: &str,
    now_ms: u128,
) -> Result<Option<NegativeCacheEntry>, ReadPathError> {
    let path = negative_cache_path(&config.paths.state_root, repo_id);
    let entry = read_json_optional::<NegativeCacheEntry>(&path)?;
    match entry {
        Some(entry) if entry.expires_at_ms > now_ms => Ok(Some(entry)),
        Some(_) => {
            let _ = fs::remove_file(path);
            Ok(None)
        }
        None => Ok(None),
    }
}

fn clear_negative_cache(config: &AppConfig, repo_id: &str) {
    let _ = fs::remove_file(negative_cache_path(&config.paths.state_root, repo_id));
}

fn write_negative_cache(
    config: &AppConfig,
    repo_id: &str,
    entry: NegativeCacheEntry,
) -> Result<(), ReadPathError> {
    write_json(
        &negative_cache_path(&config.paths.state_root, repo_id),
        &entry,
    )
}

fn read_refresh_state(
    state_root: &Path,
    repo_id: &str,
) -> Result<Option<RefreshState>, ReadPathError> {
    read_json_optional(&refresh_state_path(state_root, repo_id))
}

fn write_refresh_state(
    config: &AppConfig,
    repo_id: &str,
    state: &RefreshState,
) -> Result<(), ReadPathError> {
    write_json(
        &refresh_state_path(&config.paths.state_root, repo_id),
        state,
    )
}

fn refresh_state_path(state_root: &Path, repo_id: &str) -> PathBuf {
    state_root
        .join("read-refresh")
        .join("state")
        .join(format!("{}.json", sanitize_path_component(repo_id)))
}

fn negative_cache_path(state_root: &Path, repo_id: &str) -> PathBuf {
    state_root
        .join("read-refresh")
        .join("negative")
        .join(format!("{}.json", sanitize_path_component(repo_id)))
}

fn refresh_lock_path(state_root: &Path, repo_id: &str) -> PathBuf {
    state_root
        .join("read-refresh")
        .join("locks")
        .join(format!("{}.lock", sanitize_path_component(repo_id)))
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

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), ReadPathError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| ReadPathError::CreateDir {
            path: parent.to_path_buf(),
            error,
        })?;
    }
    let encoded = serde_json::to_vec_pretty(value).map_err(|error| ReadPathError::ParseJson {
        path: path.to_path_buf(),
        error,
    })?;
    fs::write(path, encoded).map_err(|error| ReadPathError::Write {
        path: path.to_path_buf(),
        error,
    })
}

fn read_json_optional<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<Option<T>, ReadPathError> {
    if !path.exists() {
        return Ok(None);
    }
    let source = fs::read_to_string(path).map_err(|error| ReadPathError::Read {
        path: path.to_path_buf(),
        error,
    })?;
    let value = serde_json::from_str(&source).map_err(|error| ReadPathError::ParseJson {
        path: path.to_path_buf(),
        error,
    })?;
    Ok(Some(value))
}

fn run_git(repo_path: Option<&Path>, args: &[String]) -> Result<GitProcessOutput, ReadPathError> {
    let mut command = Command::new("git");
    if let Some(repo_path) = repo_path {
        command.arg(format!("--git-dir={}", repo_path.display()));
    }
    command.args(args);
    let output = command.output().map_err(|error| ReadPathError::SpawnGit {
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
) -> Result<GitProcessOutput, ReadPathError> {
    let output = run_git(repo_path, args)?;
    if output.success {
        Ok(output)
    } else {
        let detail = format_git_failure(&output);
        Err(ReadPathError::Git {
            args: output.args.clone(),
            status: output.status,
            detail,
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

fn classify_refresh_failure(detail: &str) -> ProbeFailureClass {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("repository not found")
        || lower.contains("does not appear to be a git repository")
        || lower.contains("not a git repository")
        || lower.contains("couldn't find remote ref")
    {
        ProbeFailureClass::RepositoryMissing
    } else if lower.contains("permission denied")
        || lower.contains("access denied")
        || lower.contains("authentication failed")
        || lower.contains("could not read from remote repository")
    {
        ProbeFailureClass::AccessDenied
    } else if lower.contains("could not resolve host")
        || lower.contains("name or service not known")
        || lower.contains("connection refused")
        || lower.contains("connection timed out")
        || lower.contains("operation timed out")
        || lower.contains("network is unreachable")
        || lower.contains("no route to host")
        || lower.contains("connection reset")
    {
        ProbeFailureClass::TransportUnreachable
    } else {
        ProbeFailureClass::Unknown
    }
}

fn current_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
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

#[derive(Debug, Clone)]
struct GitProcessOutput {
    success: bool,
    status: Option<i32>,
    stdout: String,
    stderr: String,
    args: Vec<String>,
}
