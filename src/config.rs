use std::ffi::OsStr;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub listen: ListenConfig,
    pub paths: PathsConfig,
    pub reconcile: ReconcileConfig,
    pub policy: PolicyConfig,
    #[serde(default)]
    pub retention: RetentionConfig,
    pub migration: MigrationConfig,
    pub deployment: DeploymentProfile,
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let source = fs::read_to_string(path).map_err(|error| ConfigError::Read {
            path: path.to_path_buf(),
            error,
        })?;
        toml::from_str(&source).map_err(|error| ConfigError::Parse {
            path: path.to_path_buf(),
            error,
        })
    }

    pub fn load_repository_descriptors(&self) -> Result<Vec<RepositoryDescriptor>, ConfigError> {
        let mut descriptors = Vec::new();
        let root = &self.paths.repo_config_root;
        let entries = fs::read_dir(root).map_err(|error| ConfigError::ReadDir {
            path: root.clone(),
            error,
        })?;

        for entry in entries {
            let entry = entry.map_err(|error| ConfigError::ReadDirEntry {
                path: root.clone(),
                error,
            })?;
            let path = entry.path();
            if path.extension() != Some(OsStr::new("toml")) {
                continue;
            }

            let source = fs::read_to_string(&path).map_err(|error| ConfigError::Read {
                path: path.clone(),
                error,
            })?;
            let descriptor: RepositoryDescriptor =
                toml::from_str(&source).map_err(|error| ConfigError::Parse {
                    path: path.clone(),
                    error,
                })?;
            descriptors.push(descriptor);
        }

        descriptors.sort_by(|left, right| left.repo_id.cmp(&right.repo_id));
        Ok(descriptors)
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {error}")]
    Read {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to read directory {path}: {error}")]
    ReadDir {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to read directory entry under {path}: {error}")]
    ReadDirEntry {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to parse TOML {path}: {error}")]
    Parse {
        path: PathBuf,
        #[source]
        error: toml::de::Error,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListenConfig {
    pub ssh: String,
    pub https: Option<String>,
    #[serde(default)]
    pub enable_http_read: bool,
    #[serde(default)]
    pub enable_http_write: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathsConfig {
    pub state_root: PathBuf,
    pub repo_root: PathBuf,
    pub repo_config_root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcileConfig {
    pub on_push: bool,
    pub manual_enabled: bool,
    #[serde(default)]
    pub periodic_enabled: bool,
    pub worker_mode: WorkerMode,
    pub lock_timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerMode {
    ShortLived,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    pub default_repo_mode: RepositoryMode,
    pub default_refresh: FreshnessPolicy,
    pub negative_cache_ttl: HumanDuration,
    pub default_push_ack: PushAckPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionConfig {
    #[serde(default = "default_maintenance_interval")]
    pub maintenance_interval: HumanDuration,
    #[serde(default = "default_cache_idle_ttl")]
    pub cache_idle_ttl: HumanDuration,
    #[serde(default = "default_terminal_run_ttl")]
    pub terminal_run_ttl: HumanDuration,
    #[serde(default = "default_terminal_run_keep_count")]
    pub terminal_run_keep_count: usize,
    #[serde(default = "default_authoritative_reflog_ttl")]
    pub authoritative_reflog_ttl: HumanDuration,
    #[serde(default = "default_authoritative_prune_ttl")]
    pub authoritative_prune_ttl: HumanDuration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationConfig {
    pub supported_targets: Vec<MigrationTransport>,
    pub refuse_dirty_worktree: bool,
    pub targeted_relock_mode: TargetedRelockMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentProfile {
    pub platform: SupportedPlatform,
    pub service_manager: ServiceManager,
    pub service_label: String,
    pub git_only_command_mode: GitOnlyCommandMode,
    pub forced_command_wrapper: PathBuf,
    pub disable_forwarding: bool,
    pub runtime_env_file: PathBuf,
    pub allowed_git_services: Vec<GitService>,
    pub supported_filesystems: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryDescriptor {
    pub repo_id: String,
    pub canonical_identity: String,
    pub repo_path: PathBuf,
    pub mode: RepositoryMode,
    pub lifecycle: RepositoryLifecycle,
    pub authority_model: AuthorityModel,
    pub tracking_refs: TrackingRefPlacement,
    pub refresh: FreshnessPolicy,
    pub push_ack: PushAckPolicy,
    pub reconcile_policy: ReconcilePolicy,
    pub exported_refs: Vec<String>,
    #[serde(default)]
    pub read_upstreams: Vec<ReadUpstream>,
    #[serde(default)]
    pub write_upstreams: Vec<WriteUpstream>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadUpstream {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteUpstream {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub require_atomic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RepositoryMode {
    CacheOnly,
    Authoritative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RepositoryLifecycle {
    Provisioning,
    Ready,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthorityModel {
    UpstreamSource,
    RelayAuthoritative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrackingRefPlacement {
    SameRepoHidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcilePolicy {
    OnPushManual,
}

impl ReconcilePolicy {
    const LABEL: &'static str = "on-push+manual";
}

impl Display for ReconcilePolicy {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OnPushManual => f.write_str(Self::LABEL),
        }
    }
}

impl FromStr for ReconcilePolicy {
    type Err = ParseEnumError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            Self::LABEL => Ok(Self::OnPushManual),
            _ => Err(ParseEnumError::new(
                "reconcile policy",
                value,
                &[Self::LABEL],
            )),
        }
    }
}

impl Serialize for ReconcilePolicy {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ReconcilePolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::from_str(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshnessPolicy {
    AuthoritativeLocal,
    AlwaysRefresh,
    ManualOnly,
    StaleIfError,
    Ttl(HumanDuration),
}

impl Display for FreshnessPolicy {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuthoritativeLocal => f.write_str("authoritative-local"),
            Self::AlwaysRefresh => f.write_str("always-refresh"),
            Self::ManualOnly => f.write_str("manual-only"),
            Self::StaleIfError => f.write_str("stale-if-error"),
            Self::Ttl(duration) => write!(f, "ttl:{duration}"),
        }
    }
}

impl FromStr for FreshnessPolicy {
    type Err = ParseEnumError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "authoritative-local" => Ok(Self::AuthoritativeLocal),
            "always-refresh" => Ok(Self::AlwaysRefresh),
            "manual-only" => Ok(Self::ManualOnly),
            "stale-if-error" => Ok(Self::StaleIfError),
            _ => {
                if let Some(value) = value.strip_prefix("ttl:") {
                    return Ok(Self::Ttl(HumanDuration::from_str(value).map_err(|_| {
                        ParseEnumError::new(
                            "freshness policy",
                            value,
                            &[
                                "authoritative-local",
                                "always-refresh",
                                "manual-only",
                                "stale-if-error",
                                "ttl:<duration>",
                            ],
                        )
                    })?));
                }
                Err(ParseEnumError::new(
                    "freshness policy",
                    value,
                    &[
                        "authoritative-local",
                        "always-refresh",
                        "manual-only",
                        "stale-if-error",
                        "ttl:<duration>",
                    ],
                ))
            }
        }
    }
}

impl Serialize for FreshnessPolicy {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for FreshnessPolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::from_str(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushAckPolicy {
    LocalCommit,
}

impl PushAckPolicy {
    const LABEL: &'static str = "local-commit";
}

impl Display for PushAckPolicy {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LocalCommit => f.write_str(Self::LABEL),
        }
    }
}

impl FromStr for PushAckPolicy {
    type Err = ParseEnumError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            Self::LABEL => Ok(Self::LocalCommit),
            _ => Err(ParseEnumError::new(
                "push acknowledgement policy",
                value,
                &[Self::LABEL],
            )),
        }
    }
}

impl Serialize for PushAckPolicy {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PushAckPolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::from_str(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanDuration(#[serde(with = "human_duration_serde")] pub Duration);

impl HumanDuration {
    pub fn as_duration(self) -> Duration {
        self.0
    }
}

impl Display for HumanDuration {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let seconds = self.0.as_secs();
        match seconds {
            0 => f.write_str("0s"),
            _ if seconds.is_multiple_of(3600) => write!(f, "{}h", seconds / 3600),
            _ if seconds.is_multiple_of(60) => write!(f, "{}m", seconds / 60),
            _ => write!(f, "{seconds}s"),
        }
    }
}

impl FromStr for HumanDuration {
    type Err = ParseHumanDurationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (number, suffix) = value
            .chars()
            .position(|character| !character.is_ascii_digit())
            .map(|position| value.split_at(position))
            .ok_or(ParseHumanDurationError::MissingSuffix)?;
        let quantity = number
            .parse::<u64>()
            .map_err(|_| ParseHumanDurationError::InvalidNumber(value.to_owned()))?;
        let duration = match suffix {
            "s" => Duration::from_secs(quantity),
            "m" => Duration::from_secs(quantity.saturating_mul(60)),
            "h" => Duration::from_secs(quantity.saturating_mul(60 * 60)),
            _ => {
                return Err(ParseHumanDurationError::UnsupportedSuffix(
                    suffix.to_owned(),
                ))
            }
        };
        Ok(Self(duration))
    }
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            maintenance_interval: default_maintenance_interval(),
            cache_idle_ttl: default_cache_idle_ttl(),
            terminal_run_ttl: default_terminal_run_ttl(),
            terminal_run_keep_count: default_terminal_run_keep_count(),
            authoritative_reflog_ttl: default_authoritative_reflog_ttl(),
            authoritative_prune_ttl: default_authoritative_prune_ttl(),
        }
    }
}

fn default_maintenance_interval() -> HumanDuration {
    HumanDuration(Duration::from_secs(24 * 60 * 60))
}

fn default_cache_idle_ttl() -> HumanDuration {
    HumanDuration(Duration::from_secs(14 * 24 * 60 * 60))
}

fn default_terminal_run_ttl() -> HumanDuration {
    HumanDuration(Duration::from_secs(30 * 24 * 60 * 60))
}

fn default_terminal_run_keep_count() -> usize {
    20
}

fn default_authoritative_reflog_ttl() -> HumanDuration {
    HumanDuration(Duration::from_secs(30 * 24 * 60 * 60))
}

fn default_authoritative_prune_ttl() -> HumanDuration {
    HumanDuration(Duration::from_secs(7 * 24 * 60 * 60))
}

mod human_duration_serde {
    use std::str::FromStr;

    use serde::{Deserialize, Deserializer, Serializer};

    use crate::config::HumanDuration;

    pub fn serialize<S: Serializer>(
        value: &std::time::Duration,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&HumanDuration(*value).to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<std::time::Duration, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(HumanDuration::from_str(&value)
            .map_err(serde::de::Error::custom)?
            .0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MigrationTransport {
    #[serde(rename = "git+https")]
    GitHttps,
    #[serde(rename = "git+ssh")]
    GitSsh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetedRelockMode {
    ValidatedOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SupportedPlatform {
    Macos,
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceManager {
    Launchd,
    Systemd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitOnlyCommandMode {
    OpensshForceCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitService {
    GitUploadPack,
    GitReceivePack,
}

#[derive(Debug, Error)]
pub enum ParseHumanDurationError {
    #[error("duration is missing a unit suffix")]
    MissingSuffix,
    #[error("invalid duration number in {0}")]
    InvalidNumber(String),
    #[error("unsupported duration suffix {0}; expected s, m, or h")]
    UnsupportedSuffix(String),
}

#[derive(Debug, Error)]
#[error("invalid {kind} {value}; expected one of: {expected}")]
pub struct ParseEnumError {
    kind: &'static str,
    value: String,
    expected: String,
}

impl ParseEnumError {
    fn new(kind: &'static str, value: &str, expected: &[&str]) -> Self {
        Self {
            kind,
            value: value.to_owned(),
            expected: expected.join(", "),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::str::FromStr;

    use tempfile::TempDir;

    use super::{AppConfig, FreshnessPolicy, HumanDuration, ReconcilePolicy};

    #[test]
    fn parses_duration_round_trip() {
        let duration = HumanDuration::from_str("5m").expect("parse duration");
        assert_eq!(duration.as_duration().as_secs(), 300);
        assert_eq!(duration.to_string(), "5m");
    }

    #[test]
    fn parses_ttl_freshness_policy() {
        let policy = FreshnessPolicy::from_str("ttl:60s").expect("parse ttl policy");
        assert_eq!(policy.to_string(), "ttl:1m");
    }

    #[test]
    fn parses_on_push_manual_reconcile_policy() {
        let policy = ReconcilePolicy::from_str("on-push+manual").expect("parse policy");
        assert_eq!(policy.to_string(), "on-push+manual");
    }

    #[test]
    fn loads_repository_descriptors_in_sorted_order() {
        let temp = TempDir::new().expect("tempdir");
        let repo_root = temp.path().join("repos");
        let descriptor_root = temp.path().join("repos.d");
        std::fs::create_dir_all(&repo_root).expect("repo root");
        std::fs::create_dir_all(&descriptor_root).expect("descriptor root");

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
supported_targets = ["git+https"]
refuse_dirty_worktree = true
targeted_relock_mode = "validated-only"

[deployment]
platform = "macos"
service_manager = "launchd"
service_label = "dev.git-relay"
git_only_command_mode = "openssh-force-command"
forced_command_wrapper = "{}"
disable_forwarding = true
runtime_env_file = "{}"
allowed_git_services = ["git-upload-pack", "git-receive-pack"]
supported_filesystems = ["apfs"]
"#,
            temp.path().display(),
            repo_root.display(),
            descriptor_root.display(),
            temp.path().join("wrapper").display(),
            temp.path().join("git-relay.env").display(),
        );
        std::fs::write(&config_path, config).expect("config");

        std::fs::write(
            descriptor_root.join("b.toml"),
            format!(
                r#"
repo_id = "b"
canonical_identity = "github.com/example/b.git"
repo_path = "{}"
mode = "cache-only"
lifecycle = "ready"
authority_model = "upstream-source"
tracking_refs = "same-repo-hidden"
refresh = "ttl:60s"
push_ack = "local-commit"
reconcile_policy = "on-push+manual"
exported_refs = ["refs/heads/*"]
"#,
                PathBuf::from("/tmp/bare-b.git").display(),
            ),
        )
        .expect("descriptor b");

        std::fs::write(
            descriptor_root.join("a.toml"),
            format!(
                r#"
repo_id = "a"
canonical_identity = "github.com/example/a.git"
repo_path = "{}"
mode = "cache-only"
lifecycle = "ready"
authority_model = "upstream-source"
tracking_refs = "same-repo-hidden"
refresh = "ttl:60s"
push_ack = "local-commit"
reconcile_policy = "on-push+manual"
exported_refs = ["refs/heads/*"]
"#,
                PathBuf::from("/tmp/bare-a.git").display(),
            ),
        )
        .expect("descriptor a");

        let app_config = AppConfig::load(&config_path).expect("load config");
        let descriptors = app_config
            .load_repository_descriptors()
            .expect("load descriptors");

        let repo_ids = descriptors
            .into_iter()
            .map(|descriptor| descriptor.repo_id)
            .collect::<Vec<_>>();
        assert_eq!(repo_ids, vec!["a".to_owned(), "b".to_owned()]);
    }
}
