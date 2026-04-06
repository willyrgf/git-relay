use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::config::{
    AppConfig, AuthorityModel, FreshnessPolicy, GitOnlyCommandMode, GitService, PushAckPolicy,
    ReconcilePolicy, RepositoryDescriptor, RepositoryLifecycle, RepositoryMode,
    TrackingRefPlacement,
};
use crate::git::{GitCommandError, GitExecutor};
use crate::platform::{PlatformProbe, PlatformProbeError};

#[derive(Debug, Clone, Copy)]
pub struct Validator<'a, G, P> {
    git: &'a G,
    platform: &'a P,
}

impl<'a, G, P> Validator<'a, G, P>
where
    G: GitExecutor,
    P: PlatformProbe,
{
    pub fn new(git: &'a G, platform: &'a P) -> Self {
        Self { git, platform }
    }

    pub fn validate(
        &self,
        config: &AppConfig,
        descriptor: &RepositoryDescriptor,
    ) -> Result<ValidationReport, ValidationInfrastructureError> {
        let mut issues = Vec::new();

        self.validate_descriptor_shape(config, descriptor, &mut issues)?;
        self.validate_deployment_contract(config, descriptor, &mut issues)?;

        match descriptor.mode {
            RepositoryMode::Authoritative => {
                self.validate_authoritative_repository(config, descriptor, &mut issues)?;
            }
            RepositoryMode::CacheOnly => {
                self.validate_cache_only_repository(descriptor, &mut issues)?;
            }
        }

        let passed = issues.is_empty();
        let write_acceptance_allowed = passed
            && descriptor.mode == RepositoryMode::Authoritative
            && descriptor.lifecycle == RepositoryLifecycle::Ready;

        Ok(ValidationReport {
            repo_id: descriptor.repo_id.clone(),
            status: if passed {
                ValidationStatus::Passed
            } else {
                ValidationStatus::Failed
            },
            write_acceptance_allowed,
            issues,
        })
    }

    fn validate_descriptor_shape(
        &self,
        config: &AppConfig,
        descriptor: &RepositoryDescriptor,
        issues: &mut Vec<ValidationIssue>,
    ) -> Result<(), ValidationInfrastructureError> {
        if descriptor.repo_id.trim().is_empty() {
            issues.push(ValidationIssue::new(
                "repo_id",
                "repository id must not be empty",
            ));
        }
        if descriptor.canonical_identity.trim().is_empty() {
            issues.push(ValidationIssue::new(
                "canonical_identity",
                "canonical identity must not be empty",
            ));
        }
        if !descriptor.repo_path.is_absolute() {
            issues.push(ValidationIssue::new(
                "repo_path",
                "repository path must be absolute",
            ));
        }
        if !descriptor.repo_path.starts_with(&config.paths.repo_root) {
            issues.push(ValidationIssue::new(
                "repo_path",
                "repository path must remain under paths.repo_root",
            ));
        }

        if descriptor.mode == RepositoryMode::Authoritative
            && descriptor.authority_model != AuthorityModel::RelayAuthoritative
        {
            issues.push(ValidationIssue::new(
                "authority_model",
                "authoritative repositories must use relay-authoritative authority",
            ));
        }
        if descriptor.mode == RepositoryMode::CacheOnly
            && descriptor.authority_model != AuthorityModel::UpstreamSource
        {
            issues.push(ValidationIssue::new(
                "authority_model",
                "cache-only repositories must use upstream-source authority",
            ));
        }
        if descriptor.push_ack != PushAckPolicy::LocalCommit {
            issues.push(ValidationIssue::new(
                "push_ack",
                "only local-commit acknowledgement is supported",
            ));
        }
        if descriptor.reconcile_policy != ReconcilePolicy::OnPushManual {
            issues.push(ValidationIssue::new(
                "reconcile_policy",
                "only on-push+manual reconciliation is supported",
            ));
        }
        match descriptor.mode {
            RepositoryMode::Authoritative => {
                if descriptor.refresh != FreshnessPolicy::AuthoritativeLocal {
                    issues.push(ValidationIssue::new(
                        "refresh",
                        "authoritative repositories must use authoritative-local freshness",
                    ));
                }
            }
            RepositoryMode::CacheOnly => {
                if descriptor.refresh == FreshnessPolicy::AuthoritativeLocal {
                    issues.push(ValidationIssue::new(
                        "refresh",
                        "cache-only repositories cannot use authoritative-local freshness",
                    ));
                }
                if descriptor.read_upstreams.is_empty() {
                    issues.push(ValidationIssue::new(
                        "read_upstreams",
                        "cache-only repositories require at least one read upstream",
                    ));
                }
                if !descriptor.write_upstreams.is_empty() {
                    issues.push(ValidationIssue::new(
                        "write_upstreams",
                        "cache-only repositories must not define write upstreams",
                    ));
                }
            }
        }

        if !config.reconcile.on_push
            || !config.reconcile.manual_enabled
            || config.reconcile.periodic_enabled
        {
            issues.push(ValidationIssue::new(
                "reconcile",
                "deployment reconcile profile must be on_push + manual only",
            ));
        }

        if descriptor.exported_refs.is_empty() {
            issues.push(ValidationIssue::new(
                "exported_refs",
                "exported_refs must not be empty",
            ));
        }
        for exported_ref in &descriptor.exported_refs {
            if !(exported_ref.starts_with("refs/heads/") || exported_ref.starts_with("refs/tags/"))
            {
                issues.push(ValidationIssue::new(
                    "exported_refs",
                    "exported refs may only include refs/heads/* and refs/tags/* patterns in the initial implementation",
                ));
                break;
            }
        }

        self.validate_auth_profile_bindings(config, descriptor, issues);
        Ok(())
    }

    fn validate_auth_profile_bindings(
        &self,
        config: &AppConfig,
        descriptor: &RepositoryDescriptor,
        issues: &mut Vec<ValidationIssue>,
    ) {
        for upstream in &descriptor.read_upstreams {
            if !config.auth_profiles.contains_key(&upstream.auth_profile) {
                issues.push(ValidationIssue::new(
                    "read_upstreams",
                    format!(
                        "read upstream {} references unknown auth profile {}",
                        upstream.name, upstream.auth_profile
                    ),
                ));
            }
        }
        for upstream in &descriptor.write_upstreams {
            if !config.auth_profiles.contains_key(&upstream.auth_profile) {
                issues.push(ValidationIssue::new(
                    "write_upstreams",
                    format!(
                        "write upstream {} references unknown auth profile {}",
                        upstream.name, upstream.auth_profile
                    ),
                ));
            }
        }
    }

    fn validate_deployment_contract(
        &self,
        config: &AppConfig,
        descriptor: &RepositoryDescriptor,
        issues: &mut Vec<ValidationIssue>,
    ) -> Result<(), ValidationInfrastructureError> {
        let platform = self
            .platform
            .current_platform()
            .map_err(ValidationInfrastructureError::Platform)?;
        if platform != config.deployment.platform {
            issues.push(ValidationIssue::new(
                "deployment.platform",
                "configured deployment platform does not match the running host",
            ));
        }
        if !self.platform.service_manager_supported(
            config.deployment.platform,
            config.deployment.service_manager,
        ) {
            issues.push(ValidationIssue::new(
                "deployment.service_manager",
                "supported deployment pairs are macOS+launchd and Linux+systemd only",
            ));
        }
        if config.deployment.git_only_command_mode != GitOnlyCommandMode::OpensshForceCommand {
            issues.push(ValidationIssue::new(
                "deployment.git_only_command_mode",
                "OpenSSH ForceCommand is the only supported SSH command restriction mode",
            ));
        }
        if !config.deployment.disable_forwarding {
            issues.push(ValidationIssue::new(
                "deployment.disable_forwarding",
                "SSH forwarding must be disabled for Git ingress",
            ));
        }
        if !config.deployment.forced_command_wrapper.is_absolute() {
            issues.push(ValidationIssue::new(
                "deployment.forced_command_wrapper",
                "forced command wrapper path must be absolute",
            ));
        }
        let services = config
            .deployment
            .allowed_git_services
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let required = BTreeSet::from([GitService::GitUploadPack, GitService::GitReceivePack]);
        if services != required {
            issues.push(ValidationIssue::new(
                "deployment.allowed_git_services",
                "allowed Git services must be exactly git-upload-pack and git-receive-pack",
            ));
        }
        if config.deployment.supported_filesystems.is_empty() {
            issues.push(ValidationIssue::new(
                "deployment.supported_filesystems",
                "supported filesystem list must not be empty",
            ));
        }

        let state_root_filesystem = self
            .platform
            .filesystem_type(&config.paths.state_root)
            .map_err(ValidationInfrastructureError::Platform)?;
        if !config
            .deployment
            .supported_filesystems
            .iter()
            .any(|expected| expected == &state_root_filesystem)
        {
            issues.push(ValidationIssue::new(
                "paths.state_root",
                format!(
                    "state root filesystem {state_root_filesystem} is not in the supported deployment profile"
                ),
            ));
        }

        if descriptor.repo_path.exists() {
            let repo_filesystem = self
                .platform
                .filesystem_type(&descriptor.repo_path)
                .map_err(ValidationInfrastructureError::Platform)?;
            if !config
                .deployment
                .supported_filesystems
                .iter()
                .any(|expected| expected == &repo_filesystem)
            {
                issues.push(ValidationIssue::new(
                    "repo_path",
                    format!(
                        "repository filesystem {repo_filesystem} is not in the supported deployment profile"
                    ),
                ));
            }
        }

        Ok(())
    }

    fn validate_authoritative_repository(
        &self,
        _config: &AppConfig,
        descriptor: &RepositoryDescriptor,
        issues: &mut Vec<ValidationIssue>,
    ) -> Result<(), ValidationInfrastructureError> {
        if descriptor.tracking_refs != TrackingRefPlacement::SameRepoHidden {
            issues.push(ValidationIssue::new(
                "tracking_refs",
                "same-repo-hidden tracking refs are mandatory in the initial implementation",
            ));
        }
        if descriptor.write_upstreams.is_empty() {
            issues.push(ValidationIssue::new(
                "write_upstreams",
                "authoritative repositories require at least one write upstream",
            ));
        }
        if !descriptor.repo_path.exists() {
            issues.push(ValidationIssue::new(
                "repo_path",
                "authoritative repositories must already exist on disk",
            ));
            return Ok(());
        }

        if !self.is_bare_repository(&descriptor.repo_path)? {
            issues.push(ValidationIssue::new(
                "repo_path",
                "authoritative repository path must point to a bare Git repository",
            ));
            return Ok(());
        }

        self.require_git_config(&descriptor.repo_path, "receive.fsckObjects", "true", issues)?;
        self.require_git_config(
            &descriptor.repo_path,
            "transfer.hideRefs",
            "refs/git-relay",
            issues,
        )?;
        self.require_git_config(
            &descriptor.repo_path,
            "uploadpack.hideRefs",
            "refs/git-relay",
            issues,
        )?;
        self.require_git_config(
            &descriptor.repo_path,
            "receive.hideRefs",
            "refs/git-relay",
            issues,
        )?;
        self.require_git_config(
            &descriptor.repo_path,
            "uploadpack.allowReachableSHA1InWant",
            "false",
            issues,
        )?;
        self.require_git_config(
            &descriptor.repo_path,
            "uploadpack.allowAnySHA1InWant",
            "false",
            issues,
        )?;
        self.require_git_config(
            &descriptor.repo_path,
            "uploadpack.allowTipSHA1InWant",
            "false",
            issues,
        )?;
        self.require_git_config(&descriptor.repo_path, "core.fsync", "all", issues)?;
        self.require_git_config(&descriptor.repo_path, "core.fsyncMethod", "fsync", issues)?;

        Ok(())
    }

    fn validate_cache_only_repository(
        &self,
        descriptor: &RepositoryDescriptor,
        issues: &mut Vec<ValidationIssue>,
    ) -> Result<(), ValidationInfrastructureError> {
        if !descriptor.repo_path.exists() {
            issues.push(ValidationIssue::new(
                "repo_path",
                "cache-only repositories must already exist on disk before entering ready",
            ));
            return Ok(());
        }

        if !self.is_bare_repository(&descriptor.repo_path)? {
            issues.push(ValidationIssue::new(
                "repo_path",
                "cache-only repository path must point to a bare Git repository",
            ));
        }

        Ok(())
    }

    fn is_bare_repository(&self, repo_path: &Path) -> Result<bool, ValidationInfrastructureError> {
        let result = self
            .git
            .git(repo_path, &["rev-parse", "--is-bare-repository"])
            .map_err(ValidationInfrastructureError::Git)?;
        Ok(result == "true")
    }

    fn require_git_config(
        &self,
        repo_path: &Path,
        key: &str,
        expected: &str,
        issues: &mut Vec<ValidationIssue>,
    ) -> Result<(), ValidationInfrastructureError> {
        let actual =
            self.git
                .git(repo_path, &["config", "--get", key])
                .map_err(|error| match error {
                    GitCommandError::NonZeroExit { .. } => {
                        ValidationInfrastructureError::MissingGitConfig {
                            repo_path: repo_path.to_path_buf(),
                            key: key.to_owned(),
                        }
                    }
                    other => ValidationInfrastructureError::Git(other),
                });

        match actual {
            Ok(actual) if actual == expected => Ok(()),
            Ok(actual) => {
                issues.push(ValidationIssue::new(
                    key,
                    format!("expected {key}={expected}, found {actual}"),
                ));
                Ok(())
            }
            Err(ValidationInfrastructureError::MissingGitConfig { .. }) => {
                issues.push(ValidationIssue::new(
                    key,
                    format!("missing required git config {key}={expected}"),
                ));
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationReport {
    pub repo_id: String,
    pub status: ValidationStatus,
    pub write_acceptance_allowed: bool,
    pub issues: Vec<ValidationIssue>,
}

impl ValidationReport {
    pub fn passed(&self) -> bool {
        self.status == ValidationStatus::Passed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
}

impl ValidationIssue {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Error)]
pub enum ValidationInfrastructureError {
    #[error("platform probe failed: {0}")]
    Platform(#[from] PlatformProbeError),
    #[error("git command failed: {0}")]
    Git(#[from] GitCommandError),
    #[error(
        "required git config {key} is missing for repository {repo_path}",
        repo_path = repo_path.display()
    )]
    MissingGitConfig { repo_path: PathBuf, key: String },
    #[error("failed to read repository state at {path}: {error}", path = path.display())]
    ReadRepository {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
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
        RepositoryMode, ServiceManager, SupportedPlatform, TargetedRelockMode,
        TrackingRefPlacement, WorkerMode, WriteUpstream,
    };
    use crate::git::{GitCommandError, GitExecutor, SystemGitExecutor};
    use crate::platform::{PlatformProbe, PlatformProbeError};

    use super::{ValidationStatus, Validator};

    #[derive(Debug)]
    struct FakePlatformProbe {
        platform: SupportedPlatform,
        filesystem: String,
    }

    impl PlatformProbe for FakePlatformProbe {
        fn current_platform(&self) -> Result<SupportedPlatform, PlatformProbeError> {
            Ok(self.platform)
        }

        fn filesystem_type(&self, _path: &Path) -> Result<String, PlatformProbeError> {
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

    fn base_config(temp: &TempDir, platform: SupportedPlatform, fs: &str) -> AppConfig {
        let repo_root = temp.path().join("repos");
        let repo_config_root = temp.path().join("repos.d");
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
            migration: MigrationConfig {
                supported_targets: vec![MigrationTransport::GitHttps, MigrationTransport::GitSsh],
                refuse_dirty_worktree: true,
                targeted_relock_mode: TargetedRelockMode::ValidatedOnly,
            },
            deployment: DeploymentProfile {
                platform,
                service_manager: match platform {
                    SupportedPlatform::Macos => ServiceManager::Launchd,
                    SupportedPlatform::Linux => ServiceManager::Systemd,
                },
                service_label: "dev.git-relay".to_owned(),
                git_only_command_mode: GitOnlyCommandMode::OpensshForceCommand,
                forced_command_wrapper: PathBuf::from("/usr/local/bin/git-relay-ssh-force-command"),
                disable_forwarding: true,
                runtime_secret_env_file: temp.path().join("git-relay.env"),
                required_secret_keys: vec![
                    "GITHUB_READ_TOKEN".to_owned(),
                    "GITHUB_WRITE_KEY".to_owned(),
                ],
                allowed_git_services: vec![GitService::GitUploadPack, GitService::GitReceivePack],
                supported_filesystems: vec![fs.to_owned()],
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

    #[test]
    fn authoritative_repo_passes_when_section_13_2_contract_is_present() {
        let temp = TempDir::new().expect("tempdir");
        let config = base_config(&temp, SupportedPlatform::Macos, "apfs");
        let descriptor = authoritative_descriptor(&temp);
        init_bare_repo(&descriptor.repo_path);
        configure_authoritative_repo(&descriptor.repo_path);

        let git = SystemGitExecutor;
        let platform = FakePlatformProbe {
            platform: SupportedPlatform::Macos,
            filesystem: "apfs".to_owned(),
        };
        let validator = Validator::new(&git, &platform);
        let report = validator
            .validate(&config, &descriptor)
            .expect("validation");

        assert_eq!(report.status, ValidationStatus::Passed);
        assert!(report.write_acceptance_allowed);
    }

    #[test]
    fn authoritative_repo_fails_closed_when_hidden_ref_contract_is_missing() {
        let temp = TempDir::new().expect("tempdir");
        let config = base_config(&temp, SupportedPlatform::Macos, "apfs");
        let descriptor = authoritative_descriptor(&temp);
        init_bare_repo(&descriptor.repo_path);
        configure_authoritative_repo(&descriptor.repo_path);

        std::process::Command::new("git")
            .arg(format!("--git-dir={}", descriptor.repo_path.display()))
            .args(["config", "--unset", "uploadpack.allowAnySHA1InWant"])
            .status()
            .expect("git config unset")
            .success()
            .then_some(())
            .expect("git config unset success");

        let git = SystemGitExecutor;
        let platform = FakePlatformProbe {
            platform: SupportedPlatform::Macos,
            filesystem: "apfs".to_owned(),
        };
        let validator = Validator::new(&git, &platform);
        let report = validator
            .validate(&config, &descriptor)
            .expect("validation");

        assert_eq!(report.status, ValidationStatus::Failed);
        assert!(!report.write_acceptance_allowed);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "uploadpack.allowAnySHA1InWant"));
    }

    #[test]
    fn authoritative_repo_fails_when_filesystem_is_outside_supported_profile() {
        let temp = TempDir::new().expect("tempdir");
        let config = base_config(&temp, SupportedPlatform::Linux, "ext2/ext3");
        let descriptor = authoritative_descriptor(&temp);
        init_bare_repo(&descriptor.repo_path);
        configure_authoritative_repo(&descriptor.repo_path);

        let git = SystemGitExecutor;
        let platform = FakePlatformProbe {
            platform: SupportedPlatform::Linux,
            filesystem: "nfs".to_owned(),
        };
        let validator = Validator::new(&git, &platform);
        let report = validator
            .validate(&config, &descriptor)
            .expect("validation");

        assert_eq!(report.status, ValidationStatus::Failed);
        assert!(report.issues.iter().any(|issue| issue.code == "repo_path"));
    }

    #[derive(Debug)]
    struct FailingGitExecutor;

    impl GitExecutor for FailingGitExecutor {
        fn git(&self, _git_dir: &Path, args: &[&str]) -> Result<String, GitCommandError> {
            Err(GitCommandError::NonZeroExit {
                args: args.iter().map(|item| (*item).to_owned()).collect(),
                status: Some(1),
                stderr: "boom".to_owned(),
            })
        }
    }

    #[test]
    fn infrastructure_errors_surface_cleanly() {
        let temp = TempDir::new().expect("tempdir");
        let config = base_config(&temp, SupportedPlatform::Macos, "apfs");
        let descriptor = authoritative_descriptor(&temp);
        init_bare_repo(&descriptor.repo_path);

        let git = FailingGitExecutor;
        let platform = FakePlatformProbe {
            platform: SupportedPlatform::Macos,
            filesystem: "apfs".to_owned(),
        };
        let validator = Validator::new(&git, &platform);
        let error = validator
            .validate(&config, &descriptor)
            .expect_err("infrastructure error");
        assert!(matches!(
            error,
            super::ValidationInfrastructureError::Git(_)
        ));
    }
}
