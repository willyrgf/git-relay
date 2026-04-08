use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use git_relay::platform::{PlatformProbe, RealPlatformProbe};
use git_relay::upstream::{
    HostKeyPolicy, MatrixTargetClass, MatrixTargetManifest, MatrixTargetTransport,
};
use serde_json::{json, Value};
use tempfile::TempDir;
use thiserror::Error;

use super::artifact::{
    self, git_conformance_evidence_value, ArtifactError, GitConformanceEvidenceInput,
};
use super::cmd::{CommandCapture, CommandRunnerError, ProofCommandRunner};
use super::normalize::{normalize_value, NormalizeContext};
use super::schema::{
    current_time_ms, CaseStatus, ProofArtifactKind, ProofEvidencePaths, ProofSuiteSummaryRaw,
    ProofToolchain,
};
use super::transport::{TransportError, TransportHarness};

pub const AUTHORITATIVE_REPO_ID: &str = "github.com/example/repo.git";
pub const CACHE_REPO_ID: &str = "github.com/example/cache.git";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabProfile {
    DeterministicCore,
    ProviderAdmission,
}

#[derive(Debug, Clone)]
pub struct ProviderAdmissionInputs {
    pub target_manifest: PathBuf,
    pub credentials_file: PathBuf,
}

impl ProviderAdmissionInputs {
    pub fn validate(&self) -> Result<(), LabError> {
        if !self.target_manifest.is_absolute() {
            return Err(LabError::ProviderInputs(
                "provider-admission target manifest must be an absolute path".to_owned(),
            ));
        }
        if !self.credentials_file.is_absolute() {
            return Err(LabError::ProviderInputs(
                "provider-admission credentials file must be an absolute path".to_owned(),
            ));
        }
        if !self.target_manifest.exists() {
            return Err(LabError::ProviderInputs(format!(
                "provider-admission target manifest {} does not exist",
                self.target_manifest.display()
            )));
        }
        if !self.credentials_file.exists() {
            return Err(LabError::ProviderInputs(format!(
                "provider-admission credentials file {} does not exist",
                self.credentials_file.display()
            )));
        }
        validate_provider_inputs_manifest(self)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct BinaryPaths {
    pub git_relay: PathBuf,
    pub git_relayd: PathBuf,
    pub git_relay_install_hooks: PathBuf,
    pub git_relay_ssh_force_command: PathBuf,
}

impl BinaryPaths {
    pub fn resolve() -> Result<Self, LabError> {
        let gate_mode = std::env::var("GIT_RELAY_PROOF_GATE_MODE")
            .map(|value| value == "1")
            .unwrap_or(false);

        Ok(Self {
            git_relay: resolve_binary(
                gate_mode,
                "GIT_RELAY_PROOF_BIN_GIT_RELAY",
                "CARGO_BIN_EXE_git_relay",
                "git-relay",
            )?,
            git_relayd: resolve_binary(
                gate_mode,
                "GIT_RELAY_PROOF_BIN_GIT_RELAYD",
                "CARGO_BIN_EXE_git_relayd",
                "git-relayd",
            )?,
            git_relay_install_hooks: resolve_binary(
                gate_mode,
                "GIT_RELAY_PROOF_BIN_GIT_RELAY_INSTALL_HOOKS",
                "CARGO_BIN_EXE_git_relay_install_hooks",
                "git-relay-install-hooks",
            )?,
            git_relay_ssh_force_command: resolve_binary(
                gate_mode,
                "GIT_RELAY_PROOF_BIN_GIT_RELAY_SSH_FORCE_COMMAND",
                "CARGO_BIN_EXE_git_relay_ssh_force_command",
                "git-relay-ssh-force-command",
            )?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct CaseArtifactRecord {
    pub label: String,
    pub path: PathBuf,
    pub kind: ProofArtifactKind,
}

#[derive(Debug, Clone)]
pub struct CaseReport {
    pub assertions: Vec<super::schema::ProofAssertion>,
    pub details: Value,
    pub transport_profiles: Vec<String>,
    pub artifacts: Vec<CaseArtifactRecord>,
}

impl CaseReport {
    pub fn with_details(details: Value) -> Self {
        Self {
            assertions: Vec::new(),
            details,
            transport_profiles: Vec::new(),
            artifacts: Vec::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum LabError {
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error(transparent)]
    Command(#[from] CommandRunnerError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("failed to create directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write file {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid json in {path}: {source}")]
    ParseJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("proof command failed: {detail}")]
    CommandFailure { detail: String },
    #[error("provider-admission failed closed: {0}")]
    ProviderInputs(String),
}

pub struct ProofLab {
    pub profile: LabProfile,
    pub temp_dir: TempDir,
    pub temp_root: PathBuf,
    pub state_root: PathBuf,
    pub repo_root: PathBuf,
    pub repo_config_root: PathBuf,
    pub suite_root: PathBuf,
    pub config_path: PathBuf,
    pub runtime_env_path: PathBuf,
    pub authoritative_repo: PathBuf,
    pub cache_repo: PathBuf,
    pub upstream_alpha: PathBuf,
    pub upstream_beta: PathBuf,
    pub upstream_gamma_missing: PathBuf,
    pub upstream_read: PathBuf,
    pub binaries: BinaryPaths,
    pub toolchain: ProofToolchain,
    pub runner: ProofCommandRunner,
    pub provider_inputs: Option<ProviderAdmissionInputs>,
    provider_env: Vec<(String, String)>,
    structured_events: Vec<Value>,
}

struct GitConformanceBuild<'a> {
    profile: &'a str,
    platform: &'a str,
    git_version: &'a str,
    service_manager: &'a str,
    filesystem_profile: &'a str,
    case_summaries: Vec<(String, bool)>,
    required_cases_passed: bool,
    normalized_summary_sha256: &'a str,
}

impl ProofLab {
    pub fn new(
        profile: &LabProfile,
        suite_id: &str,
        provider_inputs: Option<ProviderAdmissionInputs>,
    ) -> Result<Self, LabError> {
        if *profile == LabProfile::ProviderAdmission {
            let Some(inputs) = provider_inputs.as_ref() else {
                return Err(LabError::ProviderInputs(
                    "provider-admission requires explicit target manifest and credentials file"
                        .to_owned(),
                ));
            };
            inputs.validate()?;
        }

        let temp_dir = TempDir::new().map_err(|source| LabError::CreateDir {
            path: PathBuf::from("<tempdir>"),
            source,
        })?;
        let temp_root = temp_dir.path().to_path_buf();

        let home = temp_root.join("home");
        let xdg = temp_root.join("xdg");
        fs::create_dir_all(home.join(".ssh")).map_err(|source| LabError::CreateDir {
            path: home.join(".ssh"),
            source,
        })?;
        fs::create_dir_all(xdg.join("config")).map_err(|source| LabError::CreateDir {
            path: xdg.join("config"),
            source,
        })?;
        fs::create_dir_all(xdg.join("cache")).map_err(|source| LabError::CreateDir {
            path: xdg.join("cache"),
            source,
        })?;
        fs::create_dir_all(xdg.join("data")).map_err(|source| LabError::CreateDir {
            path: xdg.join("data"),
            source,
        })?;

        let mut runner = ProofCommandRunner::new(&home, &xdg);

        let provider_env = if *profile == LabProfile::ProviderAdmission {
            let inputs = provider_inputs
                .as_ref()
                .expect("provider inputs are validated above for provider mode");
            parse_credentials_file(&inputs.credentials_file)?
        } else {
            Vec::new()
        };

        let state_root = temp_root.join("state");
        let repo_root = temp_root.join("repos");
        let repo_config_root = temp_root.join("repos.d");
        let suite_root = state_root.join("proof-e2e").join(suite_id);

        for path in [&state_root, &repo_root, &repo_config_root, &suite_root] {
            fs::create_dir_all(path).map_err(|source| LabError::CreateDir {
                path: path.to_path_buf(),
                source,
            })?;
        }

        let runtime_env_path = temp_root.join("git-relay.env");
        let runtime_env = format!(
            "SSH_AUTH_SOCK=/tmp/relay-proof.sock\nAPI_TOKEN=proof-token-{suite_id}\nHTTP_AUTH_PASSWORD=proof-password-{suite_id}\n"
        );
        fs::write(&runtime_env_path, runtime_env).map_err(|source| LabError::Write {
            path: runtime_env_path.clone(),
            source,
        })?;

        runner.register_secret("API_TOKEN", format!("proof-token-{suite_id}"));
        runner.register_secret("HTTP_AUTH_PASSWORD", format!("proof-password-{suite_id}"));
        for (key, value) in &provider_env {
            runner.register_secret(key.clone(), value.clone());
        }

        let authoritative_repo = repo_root.join("relay-authoritative.git");
        let cache_repo = repo_root.join("relay-cache.git");
        let upstream_alpha = repo_root.join("upstream-alpha.git");
        let upstream_beta = repo_root.join("upstream-beta.git");
        let upstream_read = repo_root.join("upstream-read.git");
        let upstream_gamma_missing = repo_root.join("upstream-gamma-missing.git");

        for repo in [
            &authoritative_repo,
            &cache_repo,
            &upstream_alpha,
            &upstream_beta,
            &upstream_read,
        ] {
            init_bare_repo(&runner, repo)?;
        }
        configure_authoritative_repo(&runner, &authoritative_repo)?;

        let binaries = BinaryPaths::resolve()?;

        let toolchain = detect_toolchain(&runner)?;

        let mut lab = Self {
            profile: *profile,
            temp_dir,
            temp_root,
            state_root,
            repo_root,
            repo_config_root,
            suite_root,
            config_path: PathBuf::new(),
            runtime_env_path,
            authoritative_repo,
            cache_repo,
            upstream_alpha,
            upstream_beta,
            upstream_gamma_missing,
            upstream_read,
            binaries,
            toolchain,
            runner,
            provider_inputs,
            provider_env,
            structured_events: Vec::new(),
        };

        lab.config_path = lab.write_config_fixture()?;
        lab.write_authoritative_descriptor_with_write_upstreams(&[
            ("alpha", &lab.upstream_alpha, true),
            ("beta", &lab.upstream_beta, false),
            ("gamma", &lab.upstream_gamma_missing, false),
        ])?;
        lab.write_cache_only_descriptor("always-refresh", &lab.upstream_read)?;

        lab.seed_initial_commits()?;

        Ok(lab)
    }

    pub fn evidence_paths(&self) -> ProofEvidencePaths {
        ProofEvidencePaths {
            case_dir: self.suite_root.join("cases"),
        }
    }

    pub fn case_root(&self, case_id: &str) -> Result<PathBuf, LabError> {
        let path = self.suite_root.join("cases").join(case_id);
        fs::create_dir_all(&path).map_err(|source| LabError::CreateDir {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    pub fn start_transport_harness(&mut self, case_id: &str) -> Result<TransportHarness, LabError> {
        let case_root = self.case_root(case_id)?;
        let harness =
            TransportHarness::start(&case_root, &self.repo_root).map_err(LabError::from)?;
        self.runner
            .register_secret("PROOF_HTTP_USERNAME", harness.smart_http.username.clone());
        self.runner
            .register_secret("PROOF_HTTP_PASSWORD", harness.smart_http.password.clone());
        Ok(harness)
    }

    pub fn run_git(
        &self,
        args: &[String],
        cwd: Option<&Path>,
        extra_env: &[(String, String)],
    ) -> Result<CommandCapture, LabError> {
        self.runner
            .run("git", args, cwd, extra_env)
            .map_err(LabError::from)
    }

    pub fn run_git_expect_success(
        &self,
        args: &[String],
        cwd: Option<&Path>,
        extra_env: &[(String, String)],
    ) -> Result<CommandCapture, LabError> {
        let capture = self.run_git(args, cwd, extra_env)?;
        if capture.success() {
            Ok(capture)
        } else {
            Err(LabError::CommandFailure {
                detail: capture.summary(),
            })
        }
    }

    pub fn run_git_relay(
        &self,
        args: &[String],
        extra_env: &[(String, String)],
    ) -> Result<CommandCapture, LabError> {
        let program = self.binaries.git_relay.display().to_string();
        let merged_env = self.merge_profile_env(extra_env);
        let capture = self.runner.run(program, args, None, &merged_env)?;
        Ok(capture)
    }

    pub fn run_git_relayd(
        &self,
        args: &[String],
        extra_env: &[(String, String)],
    ) -> Result<CommandCapture, LabError> {
        let program = self.binaries.git_relayd.display().to_string();
        let merged_env = self.merge_profile_env(extra_env);
        let capture = self.runner.run(program, args, None, &merged_env)?;
        Ok(capture)
    }

    pub fn run_git_relay_install_hooks(
        &self,
        args: &[String],
        extra_env: &[(String, String)],
    ) -> Result<CommandCapture, LabError> {
        let program = self.binaries.git_relay_install_hooks.display().to_string();
        let merged_env = self.merge_profile_env(extra_env);
        let capture = self.runner.run(program, args, None, &merged_env)?;
        Ok(capture)
    }

    fn merge_profile_env(&self, extra_env: &[(String, String)]) -> Vec<(String, String)> {
        if self.profile != LabProfile::ProviderAdmission || self.provider_env.is_empty() {
            return extra_env.to_vec();
        }

        let mut merged = self.provider_env.clone();
        for (key, value) in extra_env {
            if let Some(existing) = merged.iter_mut().find(|(entry_key, _)| entry_key == key) {
                existing.1 = value.clone();
            } else {
                merged.push((key.clone(), value.clone()));
            }
        }
        merged
    }

    pub fn read_git_ref(&self, repo_path: &Path, ref_name: &str) -> Result<String, LabError> {
        let capture = self.runner.run(
            "git",
            &[
                format!("--git-dir={}", repo_path.display()),
                "rev-parse".to_owned(),
                ref_name.to_owned(),
            ],
            None,
            &[],
        )?;
        if capture.success() {
            Ok(capture.stdout.trim().to_owned())
        } else {
            Err(LabError::CommandFailure {
                detail: capture.summary(),
            })
        }
    }

    pub fn git_ref_exists(&self, repo_path: &Path, ref_name: &str) -> Result<bool, LabError> {
        let capture = self.runner.run(
            "git",
            &[
                format!("--git-dir={}", repo_path.display()),
                "rev-parse".to_owned(),
                "--verify".to_owned(),
                "--quiet".to_owned(),
                ref_name.to_owned(),
            ],
            None,
            &[],
        )?;
        Ok(capture.success())
    }

    pub fn git_fsck_strict(&self, repo_path: &Path) -> Result<(), LabError> {
        let capture = self.runner.run(
            "git",
            &[
                format!("--git-dir={}", repo_path.display()),
                "fsck".to_owned(),
                "--strict".to_owned(),
            ],
            None,
            &[],
        )?;
        if capture.success() {
            Ok(())
        } else {
            Err(LabError::CommandFailure {
                detail: capture.summary(),
            })
        }
    }

    pub fn write_authoritative_descriptor_with_write_upstreams(
        &self,
        write_upstreams: &[(&str, &Path, bool)],
    ) -> Result<PathBuf, LabError> {
        let mut descriptor = format!(
            r#"
repo_id = "{repo_id}"
canonical_identity = "{repo_id}"
repo_path = "{repo_path}"
mode = "authoritative"
lifecycle = "ready"
authority_model = "relay-authoritative"
tracking_refs = "same-repo-hidden"
refresh = "authoritative-local"
push_ack = "local-commit"
reconcile_policy = "on-push+manual"
exported_refs = ["refs/heads/*", "refs/tags/*"]

[[read_upstreams]]
name = "upstream-read"
url = "{read_url}"
"#,
            repo_id = AUTHORITATIVE_REPO_ID,
            repo_path = self.authoritative_repo.display(),
            read_url = self.upstream_read.display(),
        );

        for (name, url, require_atomic) in write_upstreams {
            descriptor.push_str(&format!(
                r#"

[[write_upstreams]]
name = "{name}"
url = "{url}"
require_atomic = {require_atomic}
"#,
                url = url.display()
            ));
        }

        let path = self.repo_config_root.join("authoritative.toml");
        fs::write(&path, descriptor).map_err(|source| LabError::Write {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    pub fn write_cache_only_descriptor(
        &self,
        refresh: &str,
        read_upstream_url: &Path,
    ) -> Result<PathBuf, LabError> {
        let descriptor = format!(
            r#"
repo_id = "{repo_id}"
canonical_identity = "{repo_id}"
repo_path = "{repo_path}"
mode = "cache-only"
lifecycle = "ready"
authority_model = "upstream-source"
tracking_refs = "same-repo-hidden"
refresh = "{refresh}"
push_ack = "local-commit"
reconcile_policy = "on-push+manual"
exported_refs = ["refs/heads/*", "refs/tags/*"]

[[read_upstreams]]
name = "read-origin"
url = "{read_url}"
"#,
            repo_id = CACHE_REPO_ID,
            repo_path = self.cache_repo.display(),
            read_url = read_upstream_url.display(),
        );

        let path = self.repo_config_root.join("cache.toml");
        fs::write(&path, descriptor).map_err(|source| LabError::Write {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    pub fn rewrite_retention_block(&self, replacement_block: &str) -> Result<(), LabError> {
        let source = fs::read_to_string(&self.config_path).map_err(|source| LabError::Read {
            path: self.config_path.clone(),
            source,
        })?;
        let start = source
            .find("[retention]")
            .ok_or_else(|| LabError::CommandFailure {
                detail: "retention block not found".to_owned(),
            })?;
        let end = source
            .find("\n[migration]\n")
            .ok_or_else(|| LabError::CommandFailure {
                detail: "migration block not found".to_owned(),
            })?;

        let mut updated = String::new();
        updated.push_str(&source[..start]);
        updated.push_str(replacement_block);
        updated.push_str(&source[end..]);
        fs::write(&self.config_path, updated).map_err(|source| LabError::Write {
            path: self.config_path.clone(),
            source,
        })
    }

    pub fn init_work_repo(&self, path: &Path) -> Result<(), LabError> {
        fs::create_dir_all(path).map_err(|source| LabError::CreateDir {
            path: path.to_path_buf(),
            source,
        })?;

        self.run_git_expect_success(
            &[
                "-c".to_owned(),
                "init.defaultBranch=main".to_owned(),
                "init".to_owned(),
                path.display().to_string(),
            ],
            None,
            &[],
        )?;
        self.run_git_expect_success(
            &[
                "-C".to_owned(),
                path.display().to_string(),
                "config".to_owned(),
                "user.name".to_owned(),
                "Git Relay Proof".to_owned(),
            ],
            None,
            &[],
        )?;
        self.run_git_expect_success(
            &[
                "-C".to_owned(),
                path.display().to_string(),
                "config".to_owned(),
                "user.email".to_owned(),
                "git-relay-proof@example.com".to_owned(),
            ],
            None,
            &[],
        )?;
        Ok(())
    }

    pub fn commit_file(
        &self,
        repo_path: &Path,
        file_name: &str,
        contents: &str,
        message: &str,
    ) -> Result<(), LabError> {
        fs::write(repo_path.join(file_name), contents).map_err(|source| LabError::Write {
            path: repo_path.join(file_name),
            source,
        })?;

        self.run_git_expect_success(
            &[
                "-C".to_owned(),
                repo_path.display().to_string(),
                "add".to_owned(),
                file_name.to_owned(),
            ],
            None,
            &[],
        )?;
        self.run_git_expect_success(
            &[
                "-C".to_owned(),
                repo_path.display().to_string(),
                "commit".to_owned(),
                "-m".to_owned(),
                message.to_owned(),
            ],
            None,
            &[],
        )?;
        Ok(())
    }

    pub fn write_matrix_targets_fixture(
        &self,
        file_name: &str,
        targets: &[(&str, &str, &str, &str, &str, bool, bool)],
    ) -> Result<PathBuf, LabError> {
        let mut encoded = Vec::new();
        for (target_id, product, class, transport, url, require_atomic, same_repo_hidden_refs) in
            targets
        {
            let host_key_policy = match *transport {
                "ssh" => "pinned-known-hosts",
                "smart-http" => "not-applicable",
                other => {
                    return Err(LabError::CommandFailure {
                        detail: format!("unsupported matrix transport {other}"),
                    })
                }
            };
            encoded.push(json!({
                "target_id": target_id,
                "product": product,
                "class": class,
                "transport": transport,
                "url": url,
                "credential_source": format!("env:{}_CREDENTIAL", target_id.to_uppercase()),
                "host_key_policy": host_key_policy,
                "require_atomic": require_atomic,
                "same_repo_hidden_refs": same_repo_hidden_refs,
            }));
        }

        let path = self.temp_root.join(file_name);
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "targets": encoded,
            }))
            .map_err(|source| LabError::ParseJson {
                path: path.clone(),
                source,
            })?,
        )
        .map_err(|source| LabError::Write {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    pub fn repo_state_component(repo_id: &str) -> String {
        sanitize_component(repo_id)
    }

    pub fn reconcile_run_dir(&self, repo_id: &str) -> PathBuf {
        self.state_root
            .join("reconcile")
            .join("runs")
            .join(Self::repo_state_component(repo_id))
    }

    pub fn upstream_probe_run_dir(&self, repo_id: &str) -> PathBuf {
        self.state_root
            .join("upstream-probes")
            .join("runs")
            .join(Self::repo_state_component(repo_id))
    }

    pub fn matrix_probe_run_dir(&self, repo_id: &str) -> PathBuf {
        self.state_root
            .join("upstream-probes")
            .join("matrix-runs")
            .join(Self::repo_state_component(repo_id))
    }

    pub fn persist_release_git_conformance_evidence(
        &self,
        platform: &str,
        git_version: &str,
        required_cases_passed: bool,
    ) -> Result<PathBuf, LabError> {
        let service_manager = service_manager_for_platform(platform)?;
        let filesystem_profile = if platform == current_platform_label()? {
            RealPlatformProbe
                .filesystem_type(&self.temp_root)
                .unwrap_or_else(|_| "unknown".to_owned())
        } else {
            format!("synthetic-{platform}")
        };
        let case_summaries = (1..=11)
            .map(|index| (format!("P{index:02}"), required_cases_passed))
            .collect::<Vec<_>>();
        let evidence = self.build_git_conformance_evidence(GitConformanceBuild {
            profile: "deterministic-core",
            platform,
            git_version,
            service_manager,
            filesystem_profile: &filesystem_profile,
            case_summaries,
            required_cases_passed,
            normalized_summary_sha256: "synthetic-p11-summary",
        })?;
        let root = self.state_root.join("release").join("git-conformance");
        let path = artifact::git_conformance_manifest_path(&root, platform, git_version);
        artifact::persist_json(&path, &evidence)?;
        Ok(path)
    }

    pub fn record_case_event(&mut self, case_id: &str, status: CaseStatus, details: &Value) {
        self.structured_events.push(json!({
            "case_id": case_id,
            "status": match status {
                CaseStatus::Pass => "pass",
                CaseStatus::Fail => "fail",
            },
            "recorded_at_ms": current_time_ms(),
            "details": details,
        }));
    }

    pub fn persist_case_artifacts(
        &self,
        case_id: &str,
        case_raw: &Value,
    ) -> Result<(PathBuf, PathBuf), LabError> {
        let paths = self.evidence_paths();
        let raw_path = paths.case_artifact_path(case_id, &format!("{case_id}.raw.json"));
        let normalized_path =
            paths.case_artifact_path(case_id, &format!("{case_id}.normalized.json"));

        let redacted_raw = artifact::redact_json_value(case_raw, self.runner.secret_pairs())?;
        artifact::persist_json(&raw_path, &redacted_raw)?;
        let normalized = normalize_value(
            &redacted_raw,
            &NormalizeContext::for_temp_root(&self.temp_root),
        );
        artifact::persist_json(&normalized_path, &normalized)?;

        Ok((raw_path, normalized_path))
    }

    pub fn persist_summary(
        &mut self,
        summary: &mut ProofSuiteSummaryRaw,
    ) -> Result<PathBuf, LabError> {
        summary.finish();
        let normalized_initial = summary.to_normalized(None);
        let normalized_initial_value =
            serde_json::to_value(&normalized_initial).map_err(|source| LabError::ParseJson {
                path: self.suite_root.join("summary.normalized.json"),
                source,
            })?;
        let normalized_bytes = artifact::canonical_json_bytes(&normalized_initial_value)?;
        let normalized_hash = artifact::sha256_hex(&normalized_bytes);
        summary.set_normalized_summary_hash(normalized_hash.clone());

        let raw_value = serde_json::to_value(&*summary).map_err(|source| LabError::ParseJson {
            path: self.suite_root.join("summary.raw.json"),
            source,
        })?;
        let normalized_value = serde_json::to_value(
            summary.to_normalized(Some(normalized_hash.clone())),
        )
        .map_err(|source| LabError::ParseJson {
            path: self.suite_root.join("summary.normalized.json"),
            source,
        })?;

        artifact::persist_json(self.suite_root.join("summary.raw.json"), &raw_value)?;
        let normalized_output = normalize_value(
            &normalized_value,
            &NormalizeContext::for_temp_root(&self.temp_root),
        );
        artifact::persist_json(
            self.suite_root.join("summary.normalized.json"),
            &normalized_output,
        )?;
        artifact::persist_text(
            self.suite_root.join("summary.normalized.sha256"),
            &format!("{normalized_hash}\n"),
        )?;

        self.persist_structured_events()?;
        self.persist_ref_snapshots()?;
        self.persist_release_manifest_snapshot()?;
        self.persist_git_conformance_manifest(summary, &normalized_hash)?;

        Ok(self.suite_root.clone())
    }

    fn persist_structured_events(&self) -> Result<(), LabError> {
        let logs_dir = self.suite_root.join("logs");
        fs::create_dir_all(&logs_dir).map_err(|source| LabError::CreateDir {
            path: logs_dir.clone(),
            source,
        })?;

        let mut raw = String::new();
        let mut redacted = String::new();
        for event in &self.structured_events {
            let line = serde_json::to_string(event).map_err(|source| LabError::ParseJson {
                path: logs_dir.join("structured-events.raw.jsonl"),
                source,
            })?;
            if artifact::contains_secret_material(&line, self.runner.secret_pairs()) {
                return Err(LabError::Artifact(ArtifactError::SecretLeak));
            }
            raw.push_str(&line);
            raw.push('\n');

            let safe_line = artifact::redact_secrets(&line, self.runner.secret_pairs());
            if artifact::contains_secret_material(&safe_line, self.runner.secret_pairs()) {
                return Err(LabError::Artifact(ArtifactError::SecretLeak));
            }
            redacted.push_str(&safe_line);
            redacted.push('\n');
        }

        artifact::persist_text(logs_dir.join("structured-events.raw.jsonl"), &raw)?;
        artifact::persist_text(logs_dir.join("structured-events.redacted.jsonl"), &redacted)?;
        Ok(())
    }

    fn persist_ref_snapshots(&self) -> Result<(), LabError> {
        let refs_dir = self.suite_root.join("refsnapshots");
        fs::create_dir_all(&refs_dir).map_err(|source| LabError::CreateDir {
            path: refs_dir.clone(),
            source,
        })?;

        let snapshots = [
            ("relay-authoritative", &self.authoritative_repo),
            ("relay-cache", &self.cache_repo),
            ("upstream-alpha", &self.upstream_alpha),
            ("upstream-beta", &self.upstream_beta),
            ("upstream-read", &self.upstream_read),
        ];

        for (name, repo) in snapshots {
            let capture = self.runner.run(
                "git",
                &[
                    format!("--git-dir={}", repo.display()),
                    "for-each-ref".to_owned(),
                    "--format=%(objectname) %(refname)".to_owned(),
                ],
                None,
                &[],
            )?;
            if !capture.success() {
                return Err(LabError::CommandFailure {
                    detail: capture.summary(),
                });
            }
            artifact::persist_text(refs_dir.join(format!("{name}.txt")), &capture.stdout)?;
        }

        Ok(())
    }

    fn persist_release_manifest_snapshot(&self) -> Result<(), LabError> {
        let source = self
            .state_root
            .join("upstream-probes")
            .join("release-manifests");
        let target = self.suite_root.join("manifests").join("release");
        fs::create_dir_all(&target).map_err(|source| LabError::CreateDir {
            path: target.clone(),
            source,
        })?;
        if !source.exists() {
            return Ok(());
        }
        copy_dir_recursive(&source, &target)
    }

    fn persist_git_conformance_manifest(
        &self,
        summary: &ProofSuiteSummaryRaw,
        normalized_hash: &str,
    ) -> Result<(), LabError> {
        let platform = current_platform_label()?;
        let service_manager = service_manager_for_platform(platform)?;
        let filesystem_profile = RealPlatformProbe
            .filesystem_type(&self.temp_root)
            .unwrap_or_else(|_| "unknown".to_owned());
        let case_summaries = summary
            .cases
            .iter()
            .map(|case| (case.case_id.clone(), case.status.to_bool()))
            .collect::<Vec<_>>();
        let evidence = self.build_git_conformance_evidence(GitConformanceBuild {
            profile: summary.mode.profile_label(),
            platform,
            git_version: &summary.toolchain.git_version,
            service_manager,
            filesystem_profile: &filesystem_profile,
            case_summaries,
            required_cases_passed: summary.overall_status == CaseStatus::Pass,
            normalized_summary_sha256: normalized_hash,
        })?;

        let suite_root = self.suite_root.join("manifests").join("git-conformance");
        let suite_path = artifact::git_conformance_manifest_path(
            &suite_root,
            platform,
            &summary.toolchain.git_version,
        );
        artifact::persist_json(&suite_path, &evidence)?;
        if summary.mode.profile_label() == "deterministic-core" {
            let release_root = self.state_root.join("release").join("git-conformance");
            let release_path = artifact::git_conformance_manifest_path(
                &release_root,
                platform,
                &summary.toolchain.git_version,
            );
            artifact::persist_json(&release_path, &evidence)?;
        }
        Ok(())
    }

    fn build_git_conformance_evidence(
        &self,
        input: GitConformanceBuild<'_>,
    ) -> Result<Value, LabError> {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let flake_lock = repo_root.join("flake.lock");
        let flake_lock_sha256 = artifact::hash_file_sha256(&flake_lock)?;

        let git_commit = match std::env::var("GIT_RELAY_PROOF_GIT_COMMIT") {
            Ok(value) if !value.trim().is_empty() => value.trim().to_owned(),
            _ => match self.runner.run(
                "git",
                &["rev-parse".to_owned(), "HEAD".to_owned()],
                Some(&repo_root),
                &[],
            ) {
                Ok(capture) if capture.success() => capture.stdout.trim().to_owned(),
                _ => "unknown".to_owned(),
            },
        };

        let mut binary_digests = BTreeMap::new();
        binary_digests.insert(
            "git-relay".to_owned(),
            artifact::hash_file_sha256(&self.binaries.git_relay)?,
        );
        binary_digests.insert(
            "git-relayd".to_owned(),
            artifact::hash_file_sha256(&self.binaries.git_relayd)?,
        );
        binary_digests.insert(
            "git-relay-install-hooks".to_owned(),
            artifact::hash_file_sha256(&self.binaries.git_relay_install_hooks)?,
        );
        binary_digests.insert(
            "git-relay-ssh-force-command".to_owned(),
            artifact::hash_file_sha256(&self.binaries.git_relay_ssh_force_command)?,
        );

        Ok(git_conformance_evidence_value(
            GitConformanceEvidenceInput {
                profile: input.profile,
                platform: input.platform,
                git_version: input.git_version,
                openssh_version: &self.toolchain.openssh_version,
                nix_system: &std::env::var("NIX_SYSTEM").unwrap_or_else(|_| {
                    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
                }),
                service_manager: input.service_manager,
                filesystem_profile: input.filesystem_profile,
                git_relay_commit: &git_commit,
                flake_lock_sha256: &flake_lock_sha256,
                binary_digests,
                case_summaries: input.case_summaries,
                required_cases_passed: input.required_cases_passed,
                normalized_summary_sha256: input.normalized_summary_sha256,
            },
        ))
    }

    fn write_config_fixture(&self) -> Result<PathBuf, LabError> {
        let filesystem = RealPlatformProbe
            .filesystem_type(&self.temp_root)
            .unwrap_or_else(|_| "unknown".to_owned());
        let platform = match std::env::consts::OS {
            "macos" => "macos",
            "linux" => "linux",
            other => {
                return Err(LabError::CommandFailure {
                    detail: format!("unsupported host {other}"),
                })
            }
        };
        let service_manager = match std::env::consts::OS {
            "macos" => "launchd",
            "linux" => "systemd",
            _ => unreachable!(),
        };

        let config_path = self.temp_root.join("config.toml");
        let config = format!(
            r#"
[listen]
ssh = "127.0.0.1:4222"
https = "127.0.0.1:4318"
enable_http_read = false
enable_http_write = false

[paths]
state_root = "{state_root}"
repo_root = "{repo_root}"
repo_config_root = "{repo_config_root}"

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

[retention]
maintenance_interval = "24h"
cache_idle_ttl = "336h"
terminal_run_ttl = "720h"
terminal_run_keep_count = 20
authoritative_reflog_ttl = "720h"
authoritative_prune_ttl = "168h"

[migration]
supported_targets = ["git+https", "git+ssh"]
refuse_dirty_worktree = true
targeted_relock_mode = "validated-only"

[deployment]
platform = "{platform}"
service_manager = "{service_manager}"
service_label = "dev.git-relay"
git_only_command_mode = "openssh-force-command"
forced_command_wrapper = "{force_command_wrapper}"
disable_forwarding = true
runtime_env_file = "{env_file}"
allowed_git_services = ["git-upload-pack", "git-receive-pack"]
supported_filesystems = ["{filesystem}"]
"#,
            state_root = self.state_root.display(),
            repo_root = self.repo_root.display(),
            repo_config_root = self.repo_config_root.display(),
            force_command_wrapper = self.binaries.git_relay_ssh_force_command.display(),
            env_file = self.runtime_env_path.display(),
        );

        fs::write(&config_path, config).map_err(|source| LabError::Write {
            path: config_path.clone(),
            source,
        })?;
        Ok(config_path)
    }

    fn seed_initial_commits(&self) -> Result<(), LabError> {
        let seed_work = self.temp_root.join("seed-work");
        self.init_work_repo(&seed_work)?;
        self.commit_file(&seed_work, "README.md", "seed\n", "seed initial")?;

        for remote in [
            &self.authoritative_repo,
            &self.upstream_alpha,
            &self.upstream_beta,
            &self.upstream_read,
        ] {
            self.run_git_expect_success(
                &[
                    "-C".to_owned(),
                    seed_work.display().to_string(),
                    "push".to_owned(),
                    remote.display().to_string(),
                    "HEAD:refs/heads/main".to_owned(),
                ],
                None,
                &[],
            )?;
        }

        Ok(())
    }
}

fn detect_toolchain(runner: &ProofCommandRunner) -> Result<ProofToolchain, LabError> {
    let git = runner.run("git", &["--version".to_owned()], None, &[])?;
    let nix = runner.run("nix", &["--version".to_owned()], None, &[]).ok();
    let ssh = runner.run("ssh", &["-V".to_owned()], None, &[]).ok();

    let git_version = if git.success() {
        git.stdout.trim().to_owned()
    } else {
        "unknown".to_owned()
    };
    let nix_version = match nix {
        Some(capture) if capture.success() => capture.stdout.trim().to_owned(),
        _ => "unknown".to_owned(),
    };
    let openssh_version = match ssh {
        Some(capture) if capture.success() => {
            if capture.stdout.trim().is_empty() {
                capture.stderr.trim().to_owned()
            } else {
                capture.stdout.trim().to_owned()
            }
        }
        Some(capture) if !capture.stderr.trim().is_empty() => capture.stderr.trim().to_owned(),
        _ => "unknown".to_owned(),
    };

    Ok(ProofToolchain {
        git_version,
        nix_version,
        openssh_version,
    })
}

fn init_bare_repo(runner: &ProofCommandRunner, path: &Path) -> Result<(), LabError> {
    let capture = runner.run(
        "git",
        &[
            "-c".to_owned(),
            "init.defaultBranch=main".to_owned(),
            "init".to_owned(),
            "--bare".to_owned(),
            path.display().to_string(),
        ],
        None,
        &[],
    )?;
    if capture.success() {
        Ok(())
    } else {
        Err(LabError::CommandFailure {
            detail: capture.summary(),
        })
    }
}

fn configure_authoritative_repo(runner: &ProofCommandRunner, path: &Path) -> Result<(), LabError> {
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
        let capture = runner.run(
            "git",
            &[
                format!("--git-dir={}", path.display()),
                "config".to_owned(),
                key.to_owned(),
                value.to_owned(),
            ],
            None,
            &[],
        )?;
        if !capture.success() {
            return Err(LabError::CommandFailure {
                detail: capture.summary(),
            });
        }
    }

    Ok(())
}

fn resolve_binary(
    gate_mode: bool,
    env_var: &str,
    cargo_env_var: &str,
    label: &str,
) -> Result<PathBuf, LabError> {
    if let Ok(path) = std::env::var(env_var) {
        return Ok(PathBuf::from(path));
    }

    if let Ok(path) = std::env::var(cargo_env_var) {
        return Ok(PathBuf::from(path));
    }

    if gate_mode {
        return Err(LabError::CommandFailure {
            detail: format!(
                "gate mode requires {} to be set for binary {}",
                env_var, label
            ),
        });
    }

    if let Ok(output) = Command::new("which").arg(label).output() {
        if output.status.success() {
            let resolved = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !resolved.is_empty() {
                return Ok(PathBuf::from(resolved));
            }
        }
    }

    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(deps_dir) = current_exe.parent() {
            if let Some(debug_dir) = deps_dir.parent() {
                let candidate = debug_dir.join(label);
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
        }
    }

    Ok(PathBuf::from(label))
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

fn current_platform_label() -> Result<&'static str, LabError> {
    match std::env::consts::OS {
        "macos" => Ok("macos"),
        "linux" => Ok("linux"),
        other => Err(LabError::CommandFailure {
            detail: format!("unsupported platform {other}"),
        }),
    }
}

fn service_manager_for_platform(platform: &str) -> Result<&'static str, LabError> {
    match platform {
        "macos" => Ok("launchd"),
        "linux" => Ok("systemd"),
        other => Err(LabError::CommandFailure {
            detail: format!("unsupported platform {other}"),
        }),
    }
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<(), LabError> {
    let mut entries = fs::read_dir(source)
        .map_err(|error| LabError::Read {
            path: source.to_path_buf(),
            source: error,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| LabError::Read {
            path: source.to_path_buf(),
            source: error,
        })?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let entry_path = entry.path();
        let name = entry.file_name();
        let target_path = target.join(name);
        let file_type = entry.file_type().map_err(|error| LabError::Read {
            path: entry_path.clone(),
            source: error,
        })?;
        if file_type.is_dir() {
            fs::create_dir_all(&target_path).map_err(|error| LabError::CreateDir {
                path: target_path.clone(),
                source: error,
            })?;
            copy_dir_recursive(&entry_path, &target_path)?;
            continue;
        }
        if file_type.is_file() {
            let content = fs::read(&entry_path).map_err(|error| LabError::Read {
                path: entry_path.clone(),
                source: error,
            })?;
            fs::write(&target_path, content).map_err(|error| LabError::Write {
                path: target_path,
                source: error,
            })?;
        }
    }
    Ok(())
}

fn parse_credentials_file(path: &Path) -> Result<Vec<(String, String)>, LabError> {
    let source = fs::read_to_string(path).map_err(|error| LabError::Read {
        path: path.to_path_buf(),
        source: error,
    })?;
    let mut map = BTreeMap::new();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(LabError::ProviderInputs(format!(
                "provider-admission credentials line {} is not KEY=VALUE",
                index + 1
            )));
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            return Err(LabError::ProviderInputs(format!(
                "provider-admission credentials line {} has empty key or value",
                index + 1
            )));
        }
        if map.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(LabError::ProviderInputs(format!(
                "provider-admission credentials duplicated key {}",
                key
            )));
        }
    }

    if map.is_empty() {
        return Err(LabError::ProviderInputs(
            "provider-admission credentials file must declare at least one KEY=VALUE".to_owned(),
        ));
    }

    Ok(map.into_iter().collect())
}

fn validate_provider_inputs_manifest(inputs: &ProviderAdmissionInputs) -> Result<(), LabError> {
    let source = fs::read_to_string(&inputs.target_manifest).map_err(|error| LabError::Read {
        path: inputs.target_manifest.clone(),
        source: error,
    })?;
    let manifest = serde_json::from_str::<MatrixTargetManifest>(&source).map_err(|error| {
        LabError::ProviderInputs(format!(
            "provider-admission target manifest {} is invalid json: {}",
            inputs.target_manifest.display(),
            error
        ))
    })?;
    if manifest.schema_version != 1 {
        return Err(LabError::ProviderInputs(format!(
            "provider-admission target manifest schema_version {} is unsupported",
            manifest.schema_version
        )));
    }
    if manifest.targets.is_empty() {
        return Err(LabError::ProviderInputs(
            "provider-admission target manifest must contain at least one target".to_owned(),
        ));
    }

    let credentials = parse_credentials_file(&inputs.credentials_file)?
        .into_iter()
        .collect::<BTreeMap<_, _>>();

    let mut seen = std::collections::BTreeSet::new();
    for target in &manifest.targets {
        if target.target_id.trim().is_empty() {
            return Err(LabError::ProviderInputs(
                "provider-admission target_id must not be empty".to_owned(),
            ));
        }
        if !seen.insert(target.target_id.clone()) {
            return Err(LabError::ProviderInputs(format!(
                "provider-admission target_id {} is duplicated",
                target.target_id
            )));
        }
        if target.product.trim().is_empty() || target.url.trim().is_empty() {
            return Err(LabError::ProviderInputs(format!(
                "provider-admission target {} must define product and url",
                target.target_id
            )));
        }
        if target.same_repo_hidden_refs && target.class != MatrixTargetClass::SelfManaged {
            return Err(LabError::ProviderInputs(format!(
                "provider-admission target {} cannot set same_repo_hidden_refs unless class=self-managed",
                target.target_id
            )));
        }
        match (target.transport, target.host_key_policy) {
            (MatrixTargetTransport::SmartHttp, HostKeyPolicy::NotApplicable)
            | (MatrixTargetTransport::Ssh, HostKeyPolicy::PinnedKnownHosts)
            | (MatrixTargetTransport::Ssh, HostKeyPolicy::AcceptNew) => {}
            (MatrixTargetTransport::SmartHttp, _) => {
                return Err(LabError::ProviderInputs(format!(
                    "provider-admission target {} uses smart-http and must set host_key_policy=not-applicable",
                    target.target_id
                )));
            }
            (MatrixTargetTransport::Ssh, HostKeyPolicy::NotApplicable) => {
                return Err(LabError::ProviderInputs(format!(
                    "provider-admission target {} uses ssh and must set an SSH host-key policy",
                    target.target_id
                )));
            }
        }

        let Some(var_name) = target.credential_source.strip_prefix("env:") else {
            return Err(LabError::ProviderInputs(format!(
                "provider-admission target {} credential_source must use env:VAR form",
                target.target_id
            )));
        };
        if var_name.trim().is_empty() {
            return Err(LabError::ProviderInputs(format!(
                "provider-admission target {} credential_source env variable is empty",
                target.target_id
            )));
        }
        if !credentials.contains_key(var_name) {
            return Err(LabError::ProviderInputs(format!(
                "provider-admission credentials file is missing required key {} for target {}",
                var_name, target.target_id
            )));
        }
    }

    Ok(())
}
