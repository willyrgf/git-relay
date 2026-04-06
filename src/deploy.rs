use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use serde::Serialize;

use crate::config::{AppConfig, RepositoryDescriptor};
use crate::validator::{ValidationInfrastructureError, ValidationReport, Validator};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeValidationStatus {
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeValidationIssue {
    pub code: String,
    pub message: String,
}

impl RuntimeValidationIssue {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeValidationReport {
    pub status: RuntimeValidationStatus,
    pub secret_count: usize,
    pub issues: Vec<RuntimeValidationIssue>,
    pub repository_contracts: Vec<ValidationReport>,
}

impl RuntimeValidationReport {
    pub fn passed(&self) -> bool {
        self.status == RuntimeValidationStatus::Passed
    }
}

pub fn validate_runtime_profile<G, P>(
    config: &AppConfig,
    descriptors: &[RepositoryDescriptor],
    validator: &Validator<'_, G, P>,
) -> Result<RuntimeValidationReport, ValidationInfrastructureError>
where
    G: crate::git::GitExecutor,
    P: crate::platform::PlatformProbe,
{
    let mut issues = Vec::new();
    let env_file = &config.deployment.runtime_secret_env_file;

    if !env_file.is_absolute() {
        issues.push(RuntimeValidationIssue::new(
            "deployment.runtime_secret_env_file",
            "runtime secret env file must be absolute",
        ));
    }
    if env_file.starts_with(Path::new("/nix/store")) {
        issues.push(RuntimeValidationIssue::new(
            "deployment.runtime_secret_env_file",
            "runtime secret env file must remain outside /nix/store",
        ));
    }
    if config.deployment.service_label.trim().is_empty() {
        issues.push(RuntimeValidationIssue::new(
            "deployment.service_label",
            "service label must not be empty",
        ));
    }

    let secrets = match load_env_file(env_file) {
        Ok(secrets) => secrets,
        Err(message) => {
            issues.push(RuntimeValidationIssue::new(
                "deployment.runtime_secret_env_file",
                message,
            ));
            BTreeMap::new()
        }
    };

    for key in &config.deployment.required_secret_keys {
        match secrets.get(key) {
            Some(value) if !value.is_empty() => {}
            Some(_) => issues.push(RuntimeValidationIssue::new(
                "deployment.required_secret_keys",
                format!("required runtime secret {key} is present but empty"),
            )),
            None => issues.push(RuntimeValidationIssue::new(
                "deployment.required_secret_keys",
                format!("required runtime secret {key} is missing"),
            )),
        }
    }

    let mut repository_contracts = Vec::new();
    for descriptor in descriptors
        .iter()
        .filter(|descriptor| descriptor.mode == crate::config::RepositoryMode::Authoritative)
    {
        let report = validator.validate(config, descriptor)?;
        if !report.passed() {
            issues.push(RuntimeValidationIssue::new(
                "repository_contract",
                format!(
                    "authoritative repository {} failed contract validation",
                    descriptor.repo_id
                ),
            ));
        }
        repository_contracts.push(report);
    }

    let status = if issues.is_empty() && repository_contracts.iter().all(ValidationReport::passed) {
        RuntimeValidationStatus::Passed
    } else {
        RuntimeValidationStatus::Failed
    };

    Ok(RuntimeValidationReport {
        status,
        secret_count: secrets.len(),
        issues,
        repository_contracts,
    })
}

fn load_env_file(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let source = fs::read_to_string(path).map_err(|error| {
        format!(
            "failed to read runtime secret env file {}: {error}",
            path.display()
        )
    })?;
    let mut values = BTreeMap::new();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let (key, value) = trimmed.split_once('=').ok_or_else(|| {
            format!(
                "invalid runtime secret env line {} in {}; expected KEY=value",
                index + 1,
                path.display()
            )
        })?;
        values.insert(key.trim().to_owned(), value.trim().to_owned());
    }
    Ok(values)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ServiceFormat {
    Launchd,
    Systemd,
}

pub struct ServiceRenderRequest {
    pub binary_path: PathBuf,
    pub config_path: PathBuf,
    pub format: ServiceFormat,
}

pub fn render_service(config: &AppConfig, request: &ServiceRenderRequest) -> String {
    match request.format {
        ServiceFormat::Systemd => render_systemd_service(config, request),
        ServiceFormat::Launchd => render_launchd_service(config, request),
    }
}

fn render_systemd_service(config: &AppConfig, request: &ServiceRenderRequest) -> String {
    format!(
        "[Unit]\nDescription=Git Relay\nAfter=network.target\n\n[Service]\nType=simple\nWorkingDirectory={working_directory}\nEnvironmentFile={env_file}\nExecStart={binary} serve --config {config_path}\nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=multi-user.target\n",
        working_directory = config.paths.state_root.display(),
        env_file = config.deployment.runtime_secret_env_file.display(),
        binary = request.binary_path.display(),
        config_path = request.config_path.display(),
    )
}

fn render_launchd_service(config: &AppConfig, request: &ServiceRenderRequest) -> String {
    let shell_command = format!(
        "set -a; . {env_file}; exec {binary} serve --config {config_path}",
        env_file = shell_escape(&config.deployment.runtime_secret_env_file),
        binary = shell_escape(&request.binary_path),
        config_path = shell_escape(&request.config_path),
    );

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
      <string>/bin/sh</string>
      <string>-lc</string>
      <string>{command}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>WorkingDirectory</key>
    <string>{working_directory}</string>
  </dict>
</plist>
"#,
        label = xml_escape(&config.deployment.service_label),
        command = xml_escape(&shell_command),
        working_directory = xml_escape(&config.paths.state_root.display().to_string()),
    )
}

fn shell_escape(path: &Path) -> String {
    let value = path.display().to_string();
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use crate::config::{
        AppConfig, AuthProfile, AuthProfileKind, AuthorityModel, DeploymentProfile,
        FreshnessPolicy, GitOnlyCommandMode, GitService, ListenConfig, MigrationConfig,
        MigrationTransport, PathsConfig, PolicyConfig, PushAckPolicy, ReadUpstream,
        ReconcileConfig, ReconcilePolicy, RepositoryDescriptor, RepositoryLifecycle,
        RepositoryMode, RetentionConfig, ServiceManager, SupportedPlatform, TargetedRelockMode,
        TrackingRefPlacement, WorkerMode, WriteUpstream,
    };
    use crate::git::SystemGitExecutor;
    use crate::platform::PlatformProbe;
    use crate::validator::Validator;

    use super::{render_service, validate_runtime_profile, ServiceFormat, ServiceRenderRequest};

    #[derive(Debug)]
    struct FakePlatformProbe {
        platform: SupportedPlatform,
        filesystem: String,
    }

    impl PlatformProbe for FakePlatformProbe {
        fn current_platform(
            &self,
        ) -> Result<SupportedPlatform, crate::platform::PlatformProbeError> {
            Ok(self.platform)
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
                    | (SupportedPlatform::Linux, ServiceManager::Systemd)
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

    fn app_config(temp: &TempDir) -> AppConfig {
        let repo_root = temp.path().join("repos");
        let repo_config_root = temp.path().join("repos.d");
        let env_file = temp.path().join("git-relay.env");
        std::fs::create_dir_all(&repo_root).expect("repo root");
        std::fs::create_dir_all(&repo_config_root).expect("repo config root");

        AppConfig {
            listen: ListenConfig {
                ssh: "127.0.0.1:4222".to_owned(),
                https: Some("127.0.0.1:4318".to_owned()),
                enable_http_read: false,
                enable_http_write: false,
            },
            paths: PathsConfig {
                state_root: temp.path().to_path_buf(),
                repo_root,
                repo_config_root,
            },
            reconcile: ReconcileConfig {
                on_push: true,
                manual_enabled: true,
                periodic_enabled: false,
                worker_mode: WorkerMode::ShortLived,
                lock_timeout_ms: 5_000,
            },
            policy: PolicyConfig {
                default_repo_mode: RepositoryMode::CacheOnly,
                default_refresh: FreshnessPolicy::Ttl("60s".parse().expect("duration")),
                negative_cache_ttl: "5s".parse().expect("duration"),
                default_push_ack: PushAckPolicy::LocalCommit,
            },
            retention: RetentionConfig::default(),
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
                forced_command_wrapper: PathBuf::from("/usr/local/bin/git-relay-ssh-force-command"),
                disable_forwarding: true,
                runtime_secret_env_file: env_file,
                required_secret_keys: vec![
                    "GITHUB_READ_TOKEN".to_owned(),
                    "GITHUB_WRITE_KEY".to_owned(),
                ],
                allowed_git_services: vec![GitService::GitUploadPack, GitService::GitReceivePack],
                supported_filesystems: vec!["apfs".to_owned()],
            },
            auth_profiles: BTreeMap::from([
                (
                    "github-read".to_owned(),
                    AuthProfile {
                        kind: AuthProfileKind::HttpsToken,
                        secret_ref: "env:GITHUB_READ_TOKEN".to_owned(),
                    },
                ),
                (
                    "github-write".to_owned(),
                    AuthProfile {
                        kind: AuthProfileKind::SshKey,
                        secret_ref: "env:GITHUB_WRITE_KEY".to_owned(),
                    },
                ),
            ]),
        }
    }

    fn authoritative_descriptor(temp: &TempDir) -> RepositoryDescriptor {
        RepositoryDescriptor {
            repo_id: "github.com/example/repo.git".to_owned(),
            canonical_identity: "github.com/example/repo.git".to_owned(),
            repo_path: temp.path().join("repos").join("repo.git"),
            mode: RepositoryMode::Authoritative,
            lifecycle: RepositoryLifecycle::Ready,
            authority_model: AuthorityModel::RelayAuthoritative,
            tracking_refs: TrackingRefPlacement::SameRepoHidden,
            refresh: FreshnessPolicy::AuthoritativeLocal,
            push_ack: PushAckPolicy::LocalCommit,
            reconcile_policy: ReconcilePolicy::OnPushManual,
            exported_refs: vec!["refs/heads/*".to_owned(), "refs/tags/*".to_owned()],
            read_upstreams: vec![ReadUpstream {
                name: "github-read".to_owned(),
                url: "https://github.com/example/repo.git".to_owned(),
                auth_profile: "github-read".to_owned(),
            }],
            write_upstreams: vec![WriteUpstream {
                name: "github-write".to_owned(),
                url: "ssh://git@github.com/example/repo.git".to_owned(),
                auth_profile: "github-write".to_owned(),
                require_atomic: true,
            }],
        }
    }

    #[test]
    fn runtime_validation_requires_env_file_outside_nix_store_and_repo_contracts() {
        let temp = TempDir::new().expect("tempdir");
        let config = app_config(&temp);
        std::fs::write(
            &config.deployment.runtime_secret_env_file,
            "GITHUB_READ_TOKEN=alpha\nGITHUB_WRITE_KEY=beta\n",
        )
        .expect("env file");

        let descriptor = authoritative_descriptor(&temp);
        init_bare_repo(&descriptor.repo_path);
        configure_authoritative_repo(&descriptor.repo_path);

        let git = SystemGitExecutor;
        let platform = FakePlatformProbe {
            platform: SupportedPlatform::Macos,
            filesystem: "apfs".to_owned(),
        };
        let validator = Validator::new(&git, &platform);

        let report = validate_runtime_profile(&config, &[descriptor], &validator)
            .expect("runtime validation");

        assert!(report.passed());
        assert_eq!(report.secret_count, 2);
    }

    #[test]
    fn runtime_validation_fails_when_required_secret_is_missing() {
        let temp = TempDir::new().expect("tempdir");
        let config = app_config(&temp);
        std::fs::write(
            &config.deployment.runtime_secret_env_file,
            "GITHUB_READ_TOKEN=alpha\n",
        )
        .expect("env file");

        let descriptor = authoritative_descriptor(&temp);
        init_bare_repo(&descriptor.repo_path);
        configure_authoritative_repo(&descriptor.repo_path);

        let git = SystemGitExecutor;
        let platform = FakePlatformProbe {
            platform: SupportedPlatform::Macos,
            filesystem: "apfs".to_owned(),
        };
        let validator = Validator::new(&git, &platform);

        let report = validate_runtime_profile(&config, &[descriptor], &validator)
            .expect("runtime validation");

        assert!(!report.passed());
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "deployment.required_secret_keys"));
    }

    #[test]
    fn renders_systemd_and_launchd_units_from_the_same_profile() {
        let temp = TempDir::new().expect("tempdir");
        let config = app_config(&temp);
        let request = ServiceRenderRequest {
            binary_path: PathBuf::from("/nix/store/git-relay/bin/git-relayd"),
            config_path: temp.path().join("config.toml"),
            format: ServiceFormat::Systemd,
        };
        let systemd = render_service(&config, &request);
        assert!(systemd.contains("EnvironmentFile="));
        assert!(systemd.contains("ExecStart=/nix/store/git-relay/bin/git-relayd serve --config"));

        let launchd = render_service(
            &config,
            &ServiceRenderRequest {
                format: ServiceFormat::Launchd,
                ..request
            },
        );
        assert!(launchd.contains("<key>Label</key>"));
        assert!(launchd.contains("serve --config"));
    }
}
