use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::config::{
    AppConfig, ConfigError, RepositoryDescriptor, RepositoryLifecycle, RepositoryMode,
};
use crate::git::GitExecutor;
use crate::platform::PlatformProbe;
use crate::reconcile::{load_divergence_markers, ReconcileError};
use crate::validator::{ValidationInfrastructureError, Validator};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedSshCommand {
    pub service: String,
    pub repo_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthorizedSshCommand {
    pub service: String,
    pub repo_id: String,
    pub repo_mode: RepositoryMode,
    pub repo_path: PathBuf,
}

pub fn resolve_ssh_command(
    repo_root: &Path,
    original_command: &str,
) -> Result<ResolvedSshCommand, SshWrapperError> {
    let tokens = shlex::split(original_command)
        .ok_or_else(|| SshWrapperError::MalformedCommand(original_command.to_owned()))?;
    if tokens.len() != 2 {
        return Err(SshWrapperError::MalformedCommand(
            original_command.to_owned(),
        ));
    }

    let service = tokens[0].as_str();
    if !matches!(service, "git-upload-pack" | "git-receive-pack") {
        return Err(SshWrapperError::UnsupportedService(tokens[0].clone()));
    }

    let repo_root =
        repo_root
            .canonicalize()
            .map_err(|error| SshWrapperError::CanonicalizeRepoRoot {
                path: repo_root.to_path_buf(),
                error,
            })?;
    let requested_repo = PathBuf::from(&tokens[1]);
    let candidate = if requested_repo.is_absolute() {
        requested_repo
    } else {
        repo_root.join(requested_repo)
    };
    let repo_path =
        candidate
            .canonicalize()
            .map_err(|error| SshWrapperError::CanonicalizeRepoPath {
                path: candidate.clone(),
                error,
            })?;
    if !repo_path.starts_with(&repo_root) {
        return Err(SshWrapperError::RepoOutsideRoot {
            repo_root,
            repo_path,
        });
    }

    Ok(ResolvedSshCommand {
        service: service.to_owned(),
        repo_path,
    })
}

pub fn authorize_ssh_command<G, P>(
    config: &AppConfig,
    descriptors: &[RepositoryDescriptor],
    resolved: ResolvedSshCommand,
    git: &G,
    platform: &P,
) -> Result<AuthorizedSshCommand, SshAuthorizationError>
where
    G: GitExecutor,
    P: PlatformProbe,
{
    let descriptor = find_descriptor_for_repo(descriptors, &resolved.repo_path)?;
    if descriptor.lifecycle != RepositoryLifecycle::Ready {
        return Err(SshAuthorizationError::LifecycleNotReady {
            repo_id: descriptor.repo_id.clone(),
            lifecycle: descriptor.lifecycle,
        });
    }

    match resolved.service.as_str() {
        "git-upload-pack" => Ok(AuthorizedSshCommand {
            service: resolved.service,
            repo_id: descriptor.repo_id.clone(),
            repo_mode: descriptor.mode,
            repo_path: descriptor.repo_path.clone(),
        }),
        "git-receive-pack" => {
            if descriptor.mode != RepositoryMode::Authoritative {
                return Err(SshAuthorizationError::WritesRequireAuthoritative {
                    repo_id: descriptor.repo_id.clone(),
                });
            }
            let validator = Validator::new(git, platform);
            let validation = validator
                .validate(config, descriptor)
                .map_err(SshAuthorizationError::ValidationInfra)?;
            if !validation.passed() {
                let details = validation
                    .issues
                    .iter()
                    .map(|issue| issue.message.clone())
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(SshAuthorizationError::ValidationFailed {
                    repo_id: descriptor.repo_id.clone(),
                    details,
                });
            }
            let divergence_markers = load_divergence_markers(&descriptor.repo_path)
                .map_err(SshAuthorizationError::ReconcileState)?;
            if !divergence_markers.is_empty() {
                let upstreams = divergence_markers
                    .iter()
                    .map(|marker| marker.upstream_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(SshAuthorizationError::RepositoryDivergent {
                    repo_id: descriptor.repo_id.clone(),
                    upstreams,
                });
            }

            Ok(AuthorizedSshCommand {
                service: resolved.service,
                repo_id: descriptor.repo_id.clone(),
                repo_mode: descriptor.mode,
                repo_path: descriptor.repo_path.clone(),
            })
        }
        other => Err(SshAuthorizationError::UnsupportedService(other.to_owned())),
    }
}

pub fn resolve_and_authorize_ssh_command<G, P>(
    config_path: &Path,
    original_command: &str,
    git: &G,
    platform: &P,
) -> Result<AuthorizedSshCommand, ResolveAndAuthorizeError>
where
    G: GitExecutor,
    P: PlatformProbe,
{
    let config = AppConfig::load(config_path)?;
    let descriptors = config.load_repository_descriptors()?;
    let resolved = resolve_ssh_command(&config.paths.repo_root, original_command)?;
    authorize_ssh_command(&config, &descriptors, resolved, git, platform)
        .map_err(ResolveAndAuthorizeError::Authorization)
}

fn find_descriptor_for_repo<'a>(
    descriptors: &'a [RepositoryDescriptor],
    repo_path: &Path,
) -> Result<&'a RepositoryDescriptor, SshAuthorizationError> {
    descriptors
        .iter()
        .find(|descriptor| match descriptor.repo_path.canonicalize() {
            Ok(path) => path == repo_path,
            Err(_) => false,
        })
        .ok_or_else(|| SshAuthorizationError::RepositoryNotConfigured(repo_path.to_path_buf()))
}

#[derive(Debug, Error)]
pub enum SshWrapperError {
    #[error("malformed SSH original command {0}")]
    MalformedCommand(String),
    #[error("unsupported SSH service {0}; only git-upload-pack and git-receive-pack are allowed")]
    UnsupportedService(String),
    #[error("failed to canonicalize repo root {path}: {error}", path = path.display())]
    CanonicalizeRepoRoot {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to canonicalize repo path {path}: {error}", path = path.display())]
    CanonicalizeRepoPath {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error(
        "requested repository {repo_path} is outside the configured repo root {repo_root}",
        repo_path = repo_path.display(),
        repo_root = repo_root.display()
    )]
    RepoOutsideRoot {
        repo_root: PathBuf,
        repo_path: PathBuf,
    },
}

#[derive(Debug, Error)]
pub enum SshAuthorizationError {
    #[error("unsupported SSH service {0}")]
    UnsupportedService(String),
    #[error("repository {0} is not configured in the descriptor set")]
    RepositoryNotConfigured(PathBuf),
    #[error("repository {repo_id} is {lifecycle:?} and not ready for normal traffic")]
    LifecycleNotReady {
        repo_id: String,
        lifecycle: RepositoryLifecycle,
    },
    #[error("repository {repo_id} is not authoritative and cannot accept writes")]
    WritesRequireAuthoritative { repo_id: String },
    #[error("repository {repo_id} failed validator re-check before write acceptance: {details}")]
    ValidationFailed { repo_id: String, details: String },
    #[error("repository {repo_id} is divergent for upstreams [{upstreams}] and blocks new writes until repaired")]
    RepositoryDivergent { repo_id: String, upstreams: String },
    #[error("failed to inspect repository reconcile state before SSH authorization: {0}")]
    ReconcileState(#[from] ReconcileError),
    #[error("validator infrastructure failed during SSH authorization: {0}")]
    ValidationInfra(#[from] ValidationInfrastructureError),
}

#[derive(Debug, Error)]
pub enum ResolveAndAuthorizeError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Resolve(#[from] SshWrapperError),
    #[error(transparent)]
    Authorization(#[from] SshAuthorizationError),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use crate::config::{
        AppConfig, AuthProfile, AuthProfileKind, AuthorityModel, DeploymentProfile,
        FreshnessPolicy, GitOnlyCommandMode, GitService, ListenConfig, MigrationConfig,
        MigrationTransport, PathsConfig, PolicyConfig, PushAckPolicy, ReconcileConfig,
        ReconcilePolicy, RepositoryDescriptor, RepositoryLifecycle, RepositoryMode, ServiceManager,
        SupportedPlatform, TargetedRelockMode, TrackingRefPlacement, WorkerMode, WriteUpstream,
    };
    use crate::git::SystemGitExecutor;
    use crate::platform::PlatformProbe;

    use super::{authorize_ssh_command, resolve_ssh_command};

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

    fn base_config(temp: &TempDir) -> AppConfig {
        AppConfig {
            listen: ListenConfig {
                ssh: "127.0.0.1:4222".to_owned(),
                https: Some("127.0.0.1:4318".to_owned()),
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
                platform: SupportedPlatform::Macos,
                service_manager: ServiceManager::Launchd,
                service_label: "dev.git-relay".to_owned(),
                git_only_command_mode: GitOnlyCommandMode::OpensshForceCommand,
                forced_command_wrapper: PathBuf::from("/usr/local/bin/git-relay-ssh-force-command"),
                disable_forwarding: true,
                runtime_secret_env_file: temp.path().join("git-relay.env"),
                required_secret_keys: vec!["GITHUB_WRITE_KEY".to_owned()],
                allowed_git_services: vec![GitService::GitUploadPack, GitService::GitReceivePack],
                supported_filesystems: vec!["apfs".to_owned()],
            },
            auth_profiles: BTreeMap::from([(
                "github-write".to_owned(),
                AuthProfile {
                    kind: AuthProfileKind::SshKey,
                    secret_ref: "env:GITHUB_WRITE_KEY".to_owned(),
                },
            )]),
        }
    }

    fn authoritative_descriptor(path: &Path) -> RepositoryDescriptor {
        RepositoryDescriptor {
            repo_id: "github.com/example/repo.git".to_owned(),
            canonical_identity: "github.com/example/repo.git".to_owned(),
            repo_path: path.to_path_buf(),
            mode: RepositoryMode::Authoritative,
            lifecycle: RepositoryLifecycle::Ready,
            authority_model: AuthorityModel::RelayAuthoritative,
            tracking_refs: TrackingRefPlacement::SameRepoHidden,
            refresh: FreshnessPolicy::AuthoritativeLocal,
            push_ack: PushAckPolicy::LocalCommit,
            reconcile_policy: ReconcilePolicy::OnPushManual,
            exported_refs: vec!["refs/heads/*".to_owned(), "refs/tags/*".to_owned()],
            read_upstreams: Vec::new(),
            write_upstreams: vec![WriteUpstream {
                name: "github-write".to_owned(),
                url: "ssh://git@github.com/example/repo.git".to_owned(),
                auth_profile: "github-write".to_owned(),
                require_atomic: true,
            }],
        }
    }

    fn cache_only_descriptor(path: &Path) -> RepositoryDescriptor {
        RepositoryDescriptor {
            repo_id: "github.com/example/cache.git".to_owned(),
            canonical_identity: "github.com/example/cache.git".to_owned(),
            repo_path: path.to_path_buf(),
            mode: RepositoryMode::CacheOnly,
            lifecycle: RepositoryLifecycle::Ready,
            authority_model: AuthorityModel::UpstreamSource,
            tracking_refs: TrackingRefPlacement::SameRepoHidden,
            refresh: FreshnessPolicy::Ttl("60s".parse().expect("duration")),
            push_ack: PushAckPolicy::LocalCommit,
            reconcile_policy: ReconcilePolicy::OnPushManual,
            exported_refs: vec!["refs/heads/*".to_owned()],
            read_upstreams: Vec::new(),
            write_upstreams: Vec::new(),
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
    fn resolves_allowed_git_service_under_repo_root() {
        let temp = TempDir::new().expect("tempdir");
        let repo_root = temp.path().join("repos");
        let repo = repo_root.join("example.git");
        std::fs::create_dir_all(&repo).expect("repo");

        let resolved =
            resolve_ssh_command(&repo_root, "git-receive-pack example.git").expect("resolve");

        assert_eq!(resolved.service, "git-receive-pack");
        assert_eq!(
            resolved.repo_path,
            repo.canonicalize().expect("canonical repo")
        );
    }

    #[test]
    fn rejects_non_git_services() {
        let temp = TempDir::new().expect("tempdir");
        let repo_root = temp.path().join("repos");
        std::fs::create_dir_all(&repo_root).expect("repo root");

        let error = resolve_ssh_command(&repo_root, "sh -c whoami").expect_err("reject service");
        assert!(matches!(
            error,
            super::SshWrapperError::UnsupportedService(_)
                | super::SshWrapperError::MalformedCommand(_)
        ));
    }

    #[test]
    fn authorize_receive_pack_requires_authoritative_repo() {
        let temp = TempDir::new().expect("tempdir");
        let config = base_config(&temp);
        let repo = temp.path().join("repos").join("cache.git");
        std::fs::create_dir_all(temp.path().join("repos")).expect("repo root");
        init_bare_repo(&repo);

        let resolved = resolve_ssh_command(&config.paths.repo_root, "git-receive-pack cache.git")
            .expect("resolve");
        let git = SystemGitExecutor;
        let platform = FakePlatformProbe {
            filesystem: "apfs".to_owned(),
        };
        let error = authorize_ssh_command(
            &config,
            &[cache_only_descriptor(&repo)],
            resolved,
            &git,
            &platform,
        )
        .expect_err("reject receive-pack");
        assert!(matches!(
            error,
            super::SshAuthorizationError::WritesRequireAuthoritative { .. }
        ));
    }

    #[test]
    fn authorize_receive_pack_allows_valid_authoritative_repo() {
        let temp = TempDir::new().expect("tempdir");
        let config = base_config(&temp);
        let repo = temp.path().join("repos").join("example.git");
        std::fs::create_dir_all(temp.path().join("repos")).expect("repo root");
        std::fs::write(temp.path().join("git-relay.env"), "GITHUB_WRITE_KEY=beta\n").expect("env");
        init_bare_repo(&repo);
        configure_authoritative_repo(&repo);

        let resolved = resolve_ssh_command(&config.paths.repo_root, "git-receive-pack example.git")
            .expect("resolve");
        let git = SystemGitExecutor;
        let platform = FakePlatformProbe {
            filesystem: "apfs".to_owned(),
        };
        let authorized = authorize_ssh_command(
            &config,
            &[authoritative_descriptor(&repo)],
            resolved,
            &git,
            &platform,
        )
        .expect("authorize receive-pack");
        assert_eq!(authorized.repo_mode, RepositoryMode::Authoritative);
        assert_eq!(authorized.repo_id, "github.com/example/repo.git");
    }
}
