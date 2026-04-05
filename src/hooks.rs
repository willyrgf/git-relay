use std::fs::{self, OpenOptions};
use std::io::{BufRead, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::config::{
    AppConfig, ConfigError, RepositoryDescriptor, RepositoryLifecycle, RepositoryMode,
};
use crate::git::{GitCommandError, GitExecutor};
use crate::platform::PlatformProbe;
use crate::validator::{ValidationInfrastructureError, Validator};

const HOOK_NAMES: [&str; 3] = ["pre-receive", "reference-transaction", "post-receive"];
const ZERO_OID: &str = "0000000000000000000000000000000000000000";

pub fn install_hooks(
    repo_path: &Path,
    dispatcher: &Path,
    config_path: &Path,
) -> Result<Vec<PathBuf>, HookInstallError> {
    if !repo_path.exists() {
        return Err(HookInstallError::MissingRepository(repo_path.to_path_buf()));
    }
    if !dispatcher.is_absolute() {
        return Err(HookInstallError::RelativeDispatcher(
            dispatcher.to_path_buf(),
        ));
    }
    if !config_path.is_absolute() {
        return Err(HookInstallError::RelativeConfigPath(
            config_path.to_path_buf(),
        ));
    }

    let hooks_dir = repo_path.join("hooks");
    fs::create_dir_all(&hooks_dir).map_err(|error| HookInstallError::CreateHooksDir {
        path: hooks_dir.clone(),
        error,
    })?;

    let mut installed = Vec::new();
    for hook_name in HOOK_NAMES {
        let path = hooks_dir.join(hook_name);
        fs::write(
            &path,
            render_hook_script(hook_name, repo_path, dispatcher, config_path),
        )
        .map_err(|error| HookInstallError::WriteHook {
            path: path.clone(),
            error,
        })?;
        let mut permissions = fs::metadata(&path)
            .map_err(|error| HookInstallError::StatHook {
                path: path.clone(),
                error,
            })?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).map_err(|error| HookInstallError::ChmodHook {
            path: path.clone(),
            error,
        })?;
        installed.push(path);
    }

    Ok(installed)
}

fn render_hook_script(
    hook_name: &str,
    repo_path: &Path,
    dispatcher: &Path,
    config_path: &Path,
) -> String {
    format!(
        "#!/bin/sh\nset -eu\nrepo=\"${{GIT_DIR:-{repo}}}\"\nexec {dispatcher} hook-dispatch --config {config} --hook {hook} --repo \"$repo\" \"$@\"\n",
        repo = shell_quote(repo_path),
        dispatcher = shell_quote(dispatcher),
        config = shell_quote(config_path),
        hook = shell_quote(Path::new(hook_name)),
    )
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\"'\"'"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookStatus {
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefUpdate {
    pub old_oid: String,
    pub new_oid: String,
    pub ref_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookDispatchEvent {
    pub hook: String,
    pub repo: PathBuf,
    pub args: Vec<String>,
    pub updates: Vec<RefUpdate>,
    pub status: HookStatus,
    pub message: Option<String>,
}

impl HookDispatchEvent {
    pub fn accepted(&self) -> bool {
        self.status == HookStatus::Accepted
    }
}

pub fn dispatch_hook_action<G, P, R>(
    config_path: &Path,
    hook: String,
    repo: PathBuf,
    args: Vec<String>,
    stdin: R,
    git: &G,
    platform: &P,
) -> Result<HookDispatchEvent, HookDispatchError>
where
    G: GitExecutor,
    P: PlatformProbe,
    R: BufRead,
{
    let updates = read_ref_updates(stdin)?;
    let repo = repo
        .canonicalize()
        .map_err(|error| HookDispatchError::CanonicalizeRepo { path: repo, error })?;

    let (status, message) = match hook.as_str() {
        "pre-receive" => evaluate_pre_receive(config_path, &repo, &updates, git, platform)?,
        "reference-transaction" | "post-receive" => (HookStatus::Accepted, None),
        other => {
            return Err(HookDispatchError::UnsupportedHook(other.to_owned()));
        }
    };

    let event = HookDispatchEvent {
        hook,
        repo,
        args,
        updates,
        status,
        message,
    };
    record_hook_event(&event);
    Ok(event)
}

fn evaluate_pre_receive<G, P>(
    config_path: &Path,
    repo: &Path,
    updates: &[RefUpdate],
    git: &G,
    platform: &P,
) -> Result<(HookStatus, Option<String>), HookDispatchError>
where
    G: GitExecutor,
    P: PlatformProbe,
{
    let config = AppConfig::load(config_path).map_err(HookDispatchError::Config)?;
    let descriptors = config
        .load_repository_descriptors()
        .map_err(HookDispatchError::Config)?;
    let descriptor = find_descriptor_for_repo(&descriptors, repo)?.clone();

    if descriptor.mode != RepositoryMode::Authoritative {
        return Ok(rejected(
            "writes are allowed only for authoritative repositories",
        ));
    }
    if descriptor.lifecycle != RepositoryLifecycle::Ready {
        return Ok(rejected(
            "repository is not ready for authoritative write acceptance",
        ));
    }

    let validator = Validator::new(git, platform);
    let validation = validator
        .validate(&config, &descriptor)
        .map_err(HookDispatchError::ValidationInfra)?;
    if !validation.passed() {
        let details = validation
            .issues
            .iter()
            .map(|issue| issue.message.clone())
            .collect::<Vec<_>>()
            .join("; ");
        return Ok(rejected(format!(
            "repository contract validation failed before write acceptance: {details}"
        )));
    }

    for update in updates {
        if update.ref_name.starts_with("refs/git-relay/") {
            return Ok(rejected(format!(
                "ref {} is internal and must never be pushed",
                update.ref_name
            )));
        }
        if !matches_exported_ref(&descriptor.exported_refs, &update.ref_name) {
            return Ok(rejected(format!(
                "ref {} is outside the exported-ref policy",
                update.ref_name
            )));
        }
        if is_zero_oid(&update.new_oid) {
            return Ok(rejected(format!(
                "deleting {} is denied by default",
                update.ref_name
            )));
        }
        if update.ref_name.starts_with("refs/tags/") && !is_zero_oid(&update.old_oid) {
            return Ok(rejected(format!(
                "updating existing tag {} is denied by default",
                update.ref_name
            )));
        }
        if update.ref_name.starts_with("refs/heads/")
            && !is_zero_oid(&update.old_oid)
            && !is_fast_forward(repo, &update.old_oid, &update.new_oid, git)?
        {
            return Ok(rejected(format!(
                "non-fast-forward update to {} is denied by default",
                update.ref_name
            )));
        }
    }

    Ok((HookStatus::Accepted, None))
}

fn find_descriptor_for_repo<'a>(
    descriptors: &'a [RepositoryDescriptor],
    repo: &Path,
) -> Result<&'a RepositoryDescriptor, HookDispatchError> {
    descriptors
        .iter()
        .find(|descriptor| match descriptor.repo_path.canonicalize() {
            Ok(path) => path == repo,
            Err(_) => false,
        })
        .ok_or_else(|| HookDispatchError::RepositoryNotFound(repo.to_path_buf()))
}

fn read_ref_updates<R: BufRead>(reader: R) -> Result<Vec<RefUpdate>, HookDispatchError> {
    let mut updates = Vec::new();
    for line in reader.lines() {
        let line = line.map_err(HookDispatchError::ReadStdin)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parts = trimmed.split_whitespace().collect::<Vec<_>>();
        if parts.len() != 3 {
            return Err(HookDispatchError::MalformedUpdateLine(line));
        }
        updates.push(RefUpdate {
            old_oid: parts[0].to_owned(),
            new_oid: parts[1].to_owned(),
            ref_name: parts[2].to_owned(),
        });
    }
    Ok(updates)
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

fn is_zero_oid(value: &str) -> bool {
    value == ZERO_OID
}

fn is_fast_forward<G: GitExecutor>(
    repo: &Path,
    old_oid: &str,
    new_oid: &str,
    git: &G,
) -> Result<bool, HookDispatchError> {
    match git.git(repo, &["merge-base", "--is-ancestor", old_oid, new_oid]) {
        Ok(_) => Ok(true),
        Err(GitCommandError::NonZeroExit {
            status: Some(1), ..
        }) => Ok(false),
        Err(error) => Err(HookDispatchError::Git(error)),
    }
}

fn rejected(message: impl Into<String>) -> (HookStatus, Option<String>) {
    (HookStatus::Rejected, Some(message.into()))
}

fn record_hook_event(event: &HookDispatchEvent) {
    let Some(path) = std::env::var_os("GIT_RELAY_HOOK_EVENT_LOG") else {
        return;
    };
    let path = PathBuf::from(path);
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = writeln!(
        file,
        "{}",
        serde_json::to_string(event).expect("serialize hook event")
    );
}

#[derive(Debug, Error)]
pub enum HookInstallError {
    #[error("repository {0} does not exist")]
    MissingRepository(PathBuf),
    #[error("dispatcher path must be absolute: {0}")]
    RelativeDispatcher(PathBuf),
    #[error("config path must be absolute: {0}")]
    RelativeConfigPath(PathBuf),
    #[error("failed to create hooks directory {path}: {error}", path = path.display())]
    CreateHooksDir {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to write hook {path}: {error}", path = path.display())]
    WriteHook {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to stat hook {path}: {error}", path = path.display())]
    StatHook {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to chmod hook {path}: {error}", path = path.display())]
    ChmodHook {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
}

#[derive(Debug, Error)]
pub enum HookDispatchError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("unsupported hook {0}")]
    UnsupportedHook(String),
    #[error("failed to canonicalize repo {path}: {error}", path = path.display())]
    CanonicalizeRepo {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("repository {0} is not configured in the descriptor set")]
    RepositoryNotFound(PathBuf),
    #[error("failed to read hook stdin: {0}")]
    ReadStdin(#[source] std::io::Error),
    #[error("malformed hook update line: {0}")]
    MalformedUpdateLine(String),
    #[error("git command failed during hook validation: {0}")]
    Git(#[from] GitCommandError),
    #[error("repository contract validation failed: {0}")]
    ValidationInfra(#[from] ValidationInfrastructureError),
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use crate::config::{ServiceManager, SupportedPlatform};
    use crate::git::SystemGitExecutor;
    use crate::platform::PlatformProbe;

    use super::{
        dispatch_hook_action, install_hooks, matches_exported_ref, read_ref_updates, HookStatus,
        ZERO_OID,
    };

    #[derive(Debug)]
    struct FakePlatformProbe {
        filesystem: String,
    }

    impl PlatformProbe for FakePlatformProbe {
        fn current_platform(
            &self,
        ) -> Result<SupportedPlatform, crate::platform::PlatformProbeError> {
            Ok(SupportedPlatform::Macos)
        }

        fn filesystem_type(
            &self,
            _path: &Path,
        ) -> Result<String, crate::platform::PlatformProbeError> {
            Ok(self.filesystem.clone())
        }

        fn service_manager_supported(
            &self,
            platform: SupportedPlatform,
            service_manager: ServiceManager,
        ) -> bool {
            matches!(
                (platform, service_manager),
                (SupportedPlatform::Macos, ServiceManager::Launchd)
            )
        }
    }

    fn init_bare_repo(path: &Path) {
        std::process::Command::new("git")
            .args(["-c", "init.defaultBranch=main", "init", "--bare"])
            .arg(path)
            .status()
            .expect("git init")
            .success()
            .then_some(())
            .expect("git init success");
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
            std::process::Command::new("git")
                .arg(format!("--git-dir={}", path.display()))
                .args(["config", key, value])
                .status()
                .expect("git config")
                .success()
                .then_some(())
                .expect("git config success");
        }
    }

    fn write_config(temp: &TempDir, repo_path: &Path) -> PathBuf {
        let config_path = temp.path().join("config.toml");
        let config = format!(
            r#"
[listen]
ssh = "127.0.0.1:4222"
https = "127.0.0.1:4318"
enable_http_read = false
enable_http_write = false

[paths]
state_root = "{}"
repo_root = "{}"
repo_config_root = "{}"

[reconcile]
on_push = true
manual_enabled = true
periodic_enabled = false
worker_mode = "short-lived"
lock_timeout_ms = 5000

[policy]
default_repo_mode = "cache-only"
default_refresh = "ttl:60s"
negative_cache_ttl = "5s"
default_push_ack = "local-commit"

[migration]
supported_targets = ["git+https", "git+ssh"]
refuse_dirty_worktree = true
targeted_relock_mode = "validated-only"

[deployment]
platform = "macos"
service_manager = "launchd"
service_label = "dev.git-relay"
git_only_command_mode = "openssh-force-command"
forced_command_wrapper = "/usr/local/bin/git-relay-ssh-force-command"
disable_forwarding = true
runtime_secret_env_file = "{}"
required_secret_keys = ["GITHUB_WRITE_KEY"]
allowed_git_services = ["git-upload-pack", "git-receive-pack"]
supported_filesystems = ["apfs"]

[auth_profiles.github-write]
kind = "ssh-key"
secret_ref = "env:GITHUB_WRITE_KEY"
"#,
            temp.path().display(),
            temp.path().join("repos").display(),
            temp.path().join("repos.d").display(),
            temp.path().join("git-relay.env").display(),
        );
        fs::create_dir_all(temp.path().join("repos")).expect("repo root");
        fs::create_dir_all(temp.path().join("repos.d")).expect("descriptor root");
        fs::write(temp.path().join("git-relay.env"), "GITHUB_WRITE_KEY=beta\n").expect("env");
        fs::write(&config_path, config).expect("config");
        fs::write(
            temp.path().join("repos.d").join("repo.toml"),
            format!(
                r#"
repo_id = "github.com/example/repo.git"
canonical_identity = "github.com/example/repo.git"
repo_path = "{}"
mode = "authoritative"
lifecycle = "ready"
authority_model = "relay-authoritative"
tracking_refs = "same-repo-hidden"
refresh = "authoritative-local"
push_ack = "local-commit"
reconcile_policy = "on-push+manual"
exported_refs = ["refs/heads/*", "refs/tags/*"]

[[write_upstreams]]
name = "github-write"
url = "ssh://git@github.com/example/repo.git"
auth_profile = "github-write"
require_atomic = true
"#,
                repo_path.display()
            ),
        )
        .expect("descriptor");
        config_path
    }

    #[test]
    fn installs_executable_git_hooks() {
        let temp = TempDir::new().expect("tempdir");
        let repo = temp.path().join("repo.git");
        std::fs::create_dir_all(&repo).expect("repo");

        let dispatcher = temp.path().join("dispatcher");
        let config_path = temp.path().join("config.toml");
        std::fs::write(&dispatcher, "#!/bin/sh\nexit 0\n").expect("dispatcher");
        std::fs::write(&config_path, "").expect("config");

        let hooks = install_hooks(&repo, &dispatcher, &config_path).expect("install hooks");
        assert_eq!(hooks.len(), 3);
        for hook in hooks {
            let mode = std::fs::metadata(&hook).expect("stat").permissions().mode();
            assert_eq!(mode & 0o111, 0o111);
        }
    }

    #[test]
    fn parses_ref_updates_from_hook_stdin() {
        let updates = read_ref_updates(Cursor::new(
            "0000000000000000000000000000000000000000 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa refs/heads/main\n",
        ))
        .expect("read updates");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].ref_name, "refs/heads/main");
    }

    #[test]
    fn matches_exported_patterns_by_prefix() {
        assert!(matches_exported_ref(
            &["refs/heads/*".to_owned()],
            "refs/heads/main"
        ));
        assert!(!matches_exported_ref(
            &["refs/heads/*".to_owned()],
            "refs/git-relay/internal"
        ));
    }

    #[test]
    fn pre_receive_rejects_internal_refs() {
        let temp = TempDir::new().expect("tempdir");
        let repo = temp.path().join("repos").join("repo.git");
        init_bare_repo(&repo);
        configure_authoritative_repo(&repo);
        let config_path = write_config(&temp, &repo);

        let git = SystemGitExecutor;
        let platform = FakePlatformProbe {
            filesystem: "apfs".to_owned(),
        };
        let outcome = dispatch_hook_action(
            &config_path,
            "pre-receive".to_owned(),
            repo,
            Vec::new(),
            Cursor::new(format!(
                "{ZERO_OID} aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa refs/git-relay/internal\n"
            )),
            &git,
            &platform,
        )
        .expect("dispatch hook");

        assert_eq!(outcome.status, HookStatus::Rejected);
        assert!(outcome
            .message
            .expect("message")
            .contains("must never be pushed"));
    }
}
