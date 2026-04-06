use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{
    AppConfig, RepositoryDescriptor, RepositoryLifecycle, RepositoryMode, WriteUpstream,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtomicCapabilityVerdict {
    Supported,
    Unsupported,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamAccessVerdict {
    Accessible,
    AccessDenied,
    RepositoryMissing,
    TransportUnreachable,
    UnknownFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableNamespaceVerdict {
    Supported,
    Rejected,
    CleanupFailed,
    NotAttempted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeFailureClass {
    AccessDenied,
    RepositoryMissing,
    TransportUnreachable,
    ProtocolUnsupported,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamAccessProbe {
    pub verdict: UpstreamAccessVerdict,
    pub error_classification: Option<ProbeFailureClass>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtomicCapabilityProbe {
    pub verdict: AtomicCapabilityVerdict,
    pub error_classification: Option<ProbeFailureClass>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisposableNamespaceProbe {
    pub verdict: DisposableNamespaceVerdict,
    pub branch_ref: String,
    pub tag_ref: String,
    pub error_classification: Option<ProbeFailureClass>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamConformanceResult {
    pub upstream_id: String,
    pub url: String,
    pub require_atomic: bool,
    pub access: UpstreamAccessProbe,
    pub atomic_capability: AtomicCapabilityProbe,
    pub disposable_namespace: DisposableNamespaceProbe,
    pub supported_for_policy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamConformanceRunRecord {
    pub run_id: String,
    pub repo_id: String,
    pub repo_path: PathBuf,
    pub started_at_ms: u128,
    pub completed_at_ms: u128,
    pub source_oid: String,
    pub results: Vec<UpstreamConformanceResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatrixTargetClass {
    Managed,
    SelfManaged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatrixTargetTransport {
    Ssh,
    SmartHttp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostKeyPolicy {
    PinnedKnownHosts,
    AcceptNew,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatrixTargetManifest {
    pub schema_version: u32,
    pub targets: Vec<MatrixTargetEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatrixTargetEntry {
    pub target_id: String,
    pub product: String,
    pub class: MatrixTargetClass,
    pub transport: MatrixTargetTransport,
    pub url: String,
    pub credential_source: String,
    pub host_key_policy: HostKeyPolicy,
    #[serde(default)]
    pub require_atomic: bool,
    #[serde(default)]
    pub same_repo_hidden_refs: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatrixProbeResult {
    pub target: MatrixTargetEntry,
    pub access: UpstreamAccessProbe,
    pub atomic_capability: AtomicCapabilityProbe,
    pub disposable_namespace: DisposableNamespaceProbe,
    pub supported_for_policy: bool,
    pub same_repo_hidden_refs_supported: bool,
    pub admission_reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatrixProbeRunRecord {
    pub run_id: String,
    pub repo_id: String,
    pub repo_path: PathBuf,
    pub manifest_path: PathBuf,
    pub started_at_ms: u128,
    pub completed_at_ms: u128,
    pub source_oid: String,
    pub results: Vec<MatrixProbeResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamReleaseManifestEntry {
    pub target_id: String,
    pub product: String,
    pub class: MatrixTargetClass,
    pub transport: MatrixTargetTransport,
    pub url: String,
    pub require_atomic: bool,
    pub same_repo_hidden_refs: bool,
    pub admitted: bool,
    pub evidence_path: PathBuf,
    pub admission_reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamReleaseManifest {
    pub generated_at_ms: u128,
    pub repo_id: String,
    pub repo_path: PathBuf,
    pub manifest_path: PathBuf,
    pub probe_run_id: String,
    pub probe_run_path: PathBuf,
    pub all_entries_admitted: bool,
    pub entries: Vec<UpstreamReleaseManifestEntry>,
}

#[derive(Debug, Error)]
pub enum UpstreamProbeError {
    #[error("repository {repo_id} is {mode:?}; upstream probing is supported only for authoritative repositories")]
    UnsupportedRepositoryMode {
        repo_id: String,
        mode: RepositoryMode,
    },
    #[error("repository {repo_id} is {lifecycle:?}; upstream probing requires lifecycle ready")]
    RepositoryNotReady {
        repo_id: String,
        lifecycle: RepositoryLifecycle,
    },
    #[error("repository {repo_id} has no exported local refs available as a probe source")]
    NoLocalProbeSource { repo_id: String },
    #[error("target manifest {path} is invalid: {detail}", path = path.display())]
    InvalidManifest { path: PathBuf, detail: String },
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

pub fn probe_repository_upstreams(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
) -> Result<UpstreamConformanceRunRecord, UpstreamProbeError> {
    ensure_probe_ready(descriptor)?;

    let run_id = generate_run_id();
    let started_at_ms = current_time_ms();
    let source_oid = pick_probe_source_oid(&descriptor.repo_path, &descriptor.exported_refs)?
        .ok_or_else(|| UpstreamProbeError::NoLocalProbeSource {
            repo_id: descriptor.repo_id.clone(),
        })?;

    let mut upstreams = descriptor.write_upstreams.clone();
    upstreams.sort_by(|left, right| left.name.cmp(&right.name));

    let mut results = Vec::new();
    for upstream in &upstreams {
        results.push(probe_one_upstream(
            &descriptor.repo_path,
            upstream,
            &run_id,
            &source_oid,
        )?);
    }

    let run = UpstreamConformanceRunRecord {
        run_id: run_id.clone(),
        repo_id: descriptor.repo_id.clone(),
        repo_path: descriptor.repo_path.clone(),
        started_at_ms,
        completed_at_ms: current_time_ms(),
        source_oid,
        results,
    };
    persist_run_record(config, &run)?;
    Ok(run)
}

pub fn probe_matrix_targets(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
    manifest_path: &Path,
) -> Result<MatrixProbeRunRecord, UpstreamProbeError> {
    ensure_probe_ready(descriptor)?;
    let manifest = load_matrix_manifest(manifest_path)?;

    let run_id = generate_run_id();
    let started_at_ms = current_time_ms();
    let source_oid = pick_probe_source_oid(&descriptor.repo_path, &descriptor.exported_refs)?
        .ok_or_else(|| UpstreamProbeError::NoLocalProbeSource {
            repo_id: descriptor.repo_id.clone(),
        })?;

    let mut targets = manifest.targets;
    targets.sort_by(|left, right| left.target_id.cmp(&right.target_id));

    let mut results = Vec::new();
    for target in &targets {
        results.push(probe_matrix_target(
            &descriptor.repo_path,
            &run_id,
            &source_oid,
            target,
        )?);
    }

    let run = MatrixProbeRunRecord {
        run_id: run_id.clone(),
        repo_id: descriptor.repo_id.clone(),
        repo_path: descriptor.repo_path.clone(),
        manifest_path: manifest_path.to_path_buf(),
        started_at_ms,
        completed_at_ms: current_time_ms(),
        source_oid,
        results,
    };
    persist_matrix_run_record(config, &run)?;
    Ok(run)
}

pub fn build_release_manifest(
    config: &AppConfig,
    descriptor: &RepositoryDescriptor,
    manifest_path: &Path,
) -> Result<UpstreamReleaseManifest, UpstreamProbeError> {
    let run = probe_matrix_targets(config, descriptor, manifest_path)?;
    let probe_run_path =
        matrix_run_record_path(&config.paths.state_root, &run.repo_id, &run.run_id);
    let entries = run
        .results
        .iter()
        .map(|result| UpstreamReleaseManifestEntry {
            target_id: result.target.target_id.clone(),
            product: result.target.product.clone(),
            class: result.target.class,
            transport: result.target.transport,
            url: result.target.url.clone(),
            require_atomic: result.target.require_atomic,
            same_repo_hidden_refs: result.target.same_repo_hidden_refs,
            admitted: result.supported_for_policy && result.same_repo_hidden_refs_supported,
            evidence_path: probe_run_path.clone(),
            admission_reasons: result.admission_reasons.clone(),
        })
        .collect::<Vec<_>>();
    let manifest = UpstreamReleaseManifest {
        generated_at_ms: current_time_ms(),
        repo_id: descriptor.repo_id.clone(),
        repo_path: descriptor.repo_path.clone(),
        manifest_path: manifest_path.to_path_buf(),
        probe_run_id: run.run_id.clone(),
        probe_run_path,
        all_entries_admitted: entries.iter().all(|entry| entry.admitted),
        entries,
    };
    persist_release_manifest(config, &manifest)?;
    Ok(manifest)
}

pub fn probe_atomic_capability(
    repo_path: &Path,
    upstream_url: &str,
    refspecs: &[String],
) -> Result<
    (
        AtomicCapabilityVerdict,
        Option<ProbeFailureClass>,
        Option<String>,
    ),
    UpstreamProbeError,
> {
    let mut args = vec![
        "push".to_owned(),
        "--porcelain".to_owned(),
        "--dry-run".to_owned(),
        "--atomic".to_owned(),
        upstream_url.to_owned(),
    ];
    args.extend(refspecs.iter().cloned());
    let output = run_git(Some(repo_path), &args)?;
    if output.success {
        return Ok((AtomicCapabilityVerdict::Supported, None, None));
    }

    let detail = format_git_failure(&output);
    let classification = classify_failure(&detail);
    let verdict = if classification == ProbeFailureClass::ProtocolUnsupported {
        AtomicCapabilityVerdict::Unsupported
    } else {
        AtomicCapabilityVerdict::Inconclusive
    };
    Ok((verdict, Some(classification), Some(detail)))
}

fn ensure_probe_ready(descriptor: &RepositoryDescriptor) -> Result<(), UpstreamProbeError> {
    if descriptor.mode != RepositoryMode::Authoritative {
        return Err(UpstreamProbeError::UnsupportedRepositoryMode {
            repo_id: descriptor.repo_id.clone(),
            mode: descriptor.mode,
        });
    }
    if descriptor.lifecycle != RepositoryLifecycle::Ready {
        return Err(UpstreamProbeError::RepositoryNotReady {
            repo_id: descriptor.repo_id.clone(),
            lifecycle: descriptor.lifecycle,
        });
    }
    Ok(())
}

fn load_matrix_manifest(path: &Path) -> Result<MatrixTargetManifest, UpstreamProbeError> {
    let source = fs::read_to_string(path).map_err(|error| UpstreamProbeError::Read {
        path: path.to_path_buf(),
        error,
    })?;
    let manifest = serde_json::from_str::<MatrixTargetManifest>(&source).map_err(|error| {
        UpstreamProbeError::ParseJson {
            path: path.to_path_buf(),
            error,
        }
    })?;
    validate_matrix_manifest(path, &manifest)?;
    Ok(manifest)
}

fn validate_matrix_manifest(
    path: &Path,
    manifest: &MatrixTargetManifest,
) -> Result<(), UpstreamProbeError> {
    if manifest.schema_version != 1 {
        return Err(UpstreamProbeError::InvalidManifest {
            path: path.to_path_buf(),
            detail: format!(
                "unsupported schema_version {}; expected 1",
                manifest.schema_version
            ),
        });
    }
    if manifest.targets.is_empty() {
        return Err(UpstreamProbeError::InvalidManifest {
            path: path.to_path_buf(),
            detail: "target manifest must contain at least one target".to_owned(),
        });
    }

    let mut seen = std::collections::BTreeSet::new();
    for target in &manifest.targets {
        if target.target_id.trim().is_empty() {
            return Err(UpstreamProbeError::InvalidManifest {
                path: path.to_path_buf(),
                detail: "target_id must not be empty".to_owned(),
            });
        }
        if !seen.insert(target.target_id.clone()) {
            return Err(UpstreamProbeError::InvalidManifest {
                path: path.to_path_buf(),
                detail: format!("duplicate target_id {}", target.target_id),
            });
        }
        if target.product.trim().is_empty() {
            return Err(UpstreamProbeError::InvalidManifest {
                path: path.to_path_buf(),
                detail: format!("target {} product must not be empty", target.target_id),
            });
        }
        if target.url.trim().is_empty() {
            return Err(UpstreamProbeError::InvalidManifest {
                path: path.to_path_buf(),
                detail: format!("target {} url must not be empty", target.target_id),
            });
        }
        if target.credential_source.trim().is_empty() {
            return Err(UpstreamProbeError::InvalidManifest {
                path: path.to_path_buf(),
                detail: format!(
                    "target {} credential_source must not be empty",
                    target.target_id
                ),
            });
        }
        if target.same_repo_hidden_refs && target.class != MatrixTargetClass::SelfManaged {
            return Err(UpstreamProbeError::InvalidManifest {
                path: path.to_path_buf(),
                detail: format!(
                    "target {} declares same_repo_hidden_refs but is not self-managed",
                    target.target_id
                ),
            });
        }
        match (target.transport, target.host_key_policy) {
            (MatrixTargetTransport::SmartHttp, HostKeyPolicy::NotApplicable)
            | (MatrixTargetTransport::Ssh, HostKeyPolicy::PinnedKnownHosts)
            | (MatrixTargetTransport::Ssh, HostKeyPolicy::AcceptNew) => {}
            (MatrixTargetTransport::SmartHttp, _) => {
                return Err(UpstreamProbeError::InvalidManifest {
                    path: path.to_path_buf(),
                    detail: format!(
                        "target {} uses smart-http and must set host_key_policy=not-applicable",
                        target.target_id
                    ),
                });
            }
            (MatrixTargetTransport::Ssh, HostKeyPolicy::NotApplicable) => {
                return Err(UpstreamProbeError::InvalidManifest {
                    path: path.to_path_buf(),
                    detail: format!(
                        "target {} uses ssh and must set an SSH host-key policy",
                        target.target_id
                    ),
                });
            }
        }
    }

    Ok(())
}

fn probe_one_upstream(
    repo_path: &Path,
    upstream: &WriteUpstream,
    run_id: &str,
    source_oid: &str,
) -> Result<UpstreamConformanceResult, UpstreamProbeError> {
    let probe_refs = disposable_probe_refs(run_id, &upstream.name);
    let multi_refspecs = vec![
        format!("{source_oid}:{}", probe_refs.branch_ref),
        format!("{source_oid}:{}", probe_refs.tag_ref),
    ];

    let access = probe_access(upstream, repo_path)?;
    let (atomic_capability, disposable_namespace) =
        if access.verdict == UpstreamAccessVerdict::Accessible {
            let (atomic_verdict, atomic_classification, atomic_detail) =
                probe_atomic_capability(repo_path, &upstream.url, &multi_refspecs)?;
            (
                AtomicCapabilityProbe {
                    verdict: atomic_verdict,
                    error_classification: atomic_classification,
                    detail: atomic_detail,
                },
                probe_disposable_namespace(repo_path, upstream, &probe_refs, &multi_refspecs)?,
            )
        } else {
            (
                AtomicCapabilityProbe {
                    verdict: AtomicCapabilityVerdict::Inconclusive,
                    error_classification: access.error_classification,
                    detail: access.detail.clone(),
                },
                DisposableNamespaceProbe {
                    verdict: DisposableNamespaceVerdict::NotAttempted,
                    branch_ref: probe_refs.branch_ref,
                    tag_ref: probe_refs.tag_ref,
                    error_classification: access.error_classification,
                    detail: access.detail.clone(),
                },
            )
        };

    let supported_for_policy = access.verdict == UpstreamAccessVerdict::Accessible
        && disposable_namespace.verdict == DisposableNamespaceVerdict::Supported
        && (!upstream.require_atomic
            || atomic_capability.verdict == AtomicCapabilityVerdict::Supported);

    Ok(UpstreamConformanceResult {
        upstream_id: upstream.name.clone(),
        url: upstream.url.clone(),
        require_atomic: upstream.require_atomic,
        access,
        atomic_capability,
        disposable_namespace,
        supported_for_policy,
    })
}

fn probe_matrix_target(
    repo_path: &Path,
    run_id: &str,
    source_oid: &str,
    target: &MatrixTargetEntry,
) -> Result<MatrixProbeResult, UpstreamProbeError> {
    let write_upstream = WriteUpstream {
        name: target.target_id.clone(),
        url: target.url.clone(),
        require_atomic: target.require_atomic,
    };
    let probe_refs = disposable_probe_refs(run_id, &target.target_id);
    let multi_refspecs = vec![
        format!("{source_oid}:{}", probe_refs.branch_ref),
        format!("{source_oid}:{}", probe_refs.tag_ref),
    ];

    let access = probe_access(&write_upstream, repo_path)?;
    let (atomic_capability, disposable_namespace) = if access.verdict
        == UpstreamAccessVerdict::Accessible
    {
        let (atomic_verdict, atomic_classification, atomic_detail) =
            probe_atomic_capability(repo_path, &target.url, &multi_refspecs)?;
        (
            AtomicCapabilityProbe {
                verdict: atomic_verdict,
                error_classification: atomic_classification,
                detail: atomic_detail,
            },
            probe_disposable_namespace(repo_path, &write_upstream, &probe_refs, &multi_refspecs)?,
        )
    } else {
        (
            AtomicCapabilityProbe {
                verdict: AtomicCapabilityVerdict::Inconclusive,
                error_classification: access.error_classification,
                detail: access.detail.clone(),
            },
            DisposableNamespaceProbe {
                verdict: DisposableNamespaceVerdict::NotAttempted,
                branch_ref: probe_refs.branch_ref,
                tag_ref: probe_refs.tag_ref,
                error_classification: access.error_classification,
                detail: access.detail.clone(),
            },
        )
    };

    let mut admission_reasons = Vec::new();
    if access.verdict != UpstreamAccessVerdict::Accessible {
        admission_reasons.push(format!(
            "access verdict {}",
            serialize_label(&access.verdict)
        ));
    }
    if target.require_atomic && atomic_capability.verdict != AtomicCapabilityVerdict::Supported {
        admission_reasons.push(format!(
            "atomic capability {}",
            serialize_label(&atomic_capability.verdict)
        ));
    }
    if disposable_namespace.verdict != DisposableNamespaceVerdict::Supported {
        admission_reasons.push(format!(
            "disposable namespace {}",
            serialize_label(&disposable_namespace.verdict)
        ));
    }

    let same_repo_hidden_refs_supported = !target.same_repo_hidden_refs;
    if target.same_repo_hidden_refs {
        admission_reasons.push(
            "same-repo hidden refs are not admitted until matrix probing adds an explicit hidden-ref leakage check"
                .to_owned(),
        );
    }

    let supported_for_policy = admission_reasons.is_empty();
    Ok(MatrixProbeResult {
        target: target.clone(),
        access,
        atomic_capability,
        disposable_namespace,
        supported_for_policy,
        same_repo_hidden_refs_supported,
        admission_reasons,
    })
}

fn probe_access(
    upstream: &WriteUpstream,
    repo_path: &Path,
) -> Result<UpstreamAccessProbe, UpstreamProbeError> {
    let args = vec![
        "ls-remote".to_owned(),
        "--heads".to_owned(),
        "--tags".to_owned(),
        upstream.url.clone(),
    ];
    let output = run_git(Some(repo_path), &args)?;
    if output.success {
        return Ok(UpstreamAccessProbe {
            verdict: UpstreamAccessVerdict::Accessible,
            error_classification: None,
            detail: None,
        });
    }

    let detail = format_git_failure(&output);
    let classification = classify_failure(&detail);
    let verdict = match classification {
        ProbeFailureClass::AccessDenied => UpstreamAccessVerdict::AccessDenied,
        ProbeFailureClass::RepositoryMissing => UpstreamAccessVerdict::RepositoryMissing,
        ProbeFailureClass::TransportUnreachable => UpstreamAccessVerdict::TransportUnreachable,
        ProbeFailureClass::ProtocolUnsupported | ProbeFailureClass::Unknown => {
            UpstreamAccessVerdict::UnknownFailure
        }
    };
    Ok(UpstreamAccessProbe {
        verdict,
        error_classification: Some(classification),
        detail: Some(detail),
    })
}

fn probe_disposable_namespace(
    repo_path: &Path,
    upstream: &WriteUpstream,
    probe_refs: &DisposableProbeRefs,
    multi_refspecs: &[String],
) -> Result<DisposableNamespaceProbe, UpstreamProbeError> {
    let create_args = {
        let mut args = vec![
            "push".to_owned(),
            "--porcelain".to_owned(),
            upstream.url.clone(),
        ];
        args.extend(multi_refspecs.iter().cloned());
        args
    };
    let create = run_git(Some(repo_path), &create_args)?;
    if !create.success {
        let detail = format_git_failure(&create);
        return Ok(DisposableNamespaceProbe {
            verdict: DisposableNamespaceVerdict::Rejected,
            branch_ref: probe_refs.branch_ref.clone(),
            tag_ref: probe_refs.tag_ref.clone(),
            error_classification: Some(classify_failure(&detail)),
            detail: Some(detail),
        });
    }

    let observed_after_create = observe_remote_refs(
        repo_path,
        &upstream.url,
        &probe_refs.branch_ref,
        &probe_refs.tag_ref,
    )?;
    if !observed_after_create.branch_present || !observed_after_create.tag_present {
        let detail = format!(
            "disposable namespace create succeeded but remote refs were not both visible afterward (branch_present={}, tag_present={})",
            observed_after_create.branch_present, observed_after_create.tag_present
        );
        let _ = cleanup_remote_probe(repo_path, &upstream.url, probe_refs);
        return Ok(DisposableNamespaceProbe {
            verdict: DisposableNamespaceVerdict::Rejected,
            branch_ref: probe_refs.branch_ref.clone(),
            tag_ref: probe_refs.tag_ref.clone(),
            error_classification: Some(ProbeFailureClass::Unknown),
            detail: Some(detail),
        });
    }

    match cleanup_remote_probe(repo_path, &upstream.url, probe_refs)? {
        CleanupOutcome::Succeeded => Ok(DisposableNamespaceProbe {
            verdict: DisposableNamespaceVerdict::Supported,
            branch_ref: probe_refs.branch_ref.clone(),
            tag_ref: probe_refs.tag_ref.clone(),
            error_classification: None,
            detail: None,
        }),
        CleanupOutcome::Failed {
            classification,
            detail,
        } => Ok(DisposableNamespaceProbe {
            verdict: DisposableNamespaceVerdict::CleanupFailed,
            branch_ref: probe_refs.branch_ref.clone(),
            tag_ref: probe_refs.tag_ref.clone(),
            error_classification: Some(classification),
            detail: Some(detail),
        }),
    }
}

fn cleanup_remote_probe(
    repo_path: &Path,
    upstream_url: &str,
    probe_refs: &DisposableProbeRefs,
) -> Result<CleanupOutcome, UpstreamProbeError> {
    let delete_args = vec![
        "push".to_owned(),
        "--porcelain".to_owned(),
        upstream_url.to_owned(),
        format!(":{}", probe_refs.branch_ref),
        format!(":{}", probe_refs.tag_ref),
    ];
    let delete = run_git(Some(repo_path), &delete_args)?;
    if !delete.success {
        let detail = format_git_failure(&delete);
        return Ok(CleanupOutcome::Failed {
            classification: classify_failure(&detail),
            detail,
        });
    }

    let observed_after_delete = observe_remote_refs(
        repo_path,
        upstream_url,
        &probe_refs.branch_ref,
        &probe_refs.tag_ref,
    )?;
    if observed_after_delete.branch_present || observed_after_delete.tag_present {
        return Ok(CleanupOutcome::Failed {
            classification: ProbeFailureClass::Unknown,
            detail: format!(
                "disposable namespace delete succeeded but remote refs remained visible (branch_present={}, tag_present={})",
                observed_after_delete.branch_present, observed_after_delete.tag_present
            ),
        });
    }

    Ok(CleanupOutcome::Succeeded)
}

fn observe_remote_refs(
    repo_path: &Path,
    upstream_url: &str,
    branch_ref: &str,
    tag_ref: &str,
) -> Result<ObservedRemoteRefs, UpstreamProbeError> {
    let output = run_git(
        Some(repo_path),
        &[
            "ls-remote".to_owned(),
            upstream_url.to_owned(),
            branch_ref.to_owned(),
            tag_ref.to_owned(),
        ],
    )?;
    if !output.success {
        let detail = format_git_failure(&output);
        return Err(UpstreamProbeError::Git {
            args: output.args,
            status: output.status,
            detail,
        });
    }

    Ok(ObservedRemoteRefs {
        branch_present: output
            .stdout
            .lines()
            .any(|line| line.trim_end().ends_with(branch_ref)),
        tag_present: output.stdout.lines().any(|line| {
            let trimmed = line.trim_end();
            trimmed.ends_with(tag_ref) || trimmed.ends_with(&format!("{tag_ref}^{{}}"))
        }),
    })
}

fn pick_probe_source_oid(
    repo_path: &Path,
    exported_patterns: &[String],
) -> Result<Option<String>, UpstreamProbeError> {
    let refs = list_local_exported_refs(repo_path, exported_patterns)?;
    let preferred = refs
        .iter()
        .find(|entry| entry.ref_name.starts_with("refs/heads/"))
        .or_else(|| refs.first());
    Ok(preferred.map(|entry| entry.oid.clone()))
}

fn list_local_exported_refs(
    repo_path: &Path,
    exported_patterns: &[String],
) -> Result<Vec<LocalRefSnapshotEntry>, UpstreamProbeError> {
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

fn persist_run_record(
    config: &AppConfig,
    run: &UpstreamConformanceRunRecord,
) -> Result<(), UpstreamProbeError> {
    let path = run_record_path(&config.paths.state_root, &run.repo_id, &run.run_id);
    write_json(&path, run)
}

fn persist_matrix_run_record(
    config: &AppConfig,
    run: &MatrixProbeRunRecord,
) -> Result<(), UpstreamProbeError> {
    let path = matrix_run_record_path(&config.paths.state_root, &run.repo_id, &run.run_id);
    write_json(&path, run)
}

fn persist_release_manifest(
    config: &AppConfig,
    manifest: &UpstreamReleaseManifest,
) -> Result<(), UpstreamProbeError> {
    let path = release_manifest_path(
        &config.paths.state_root,
        &manifest.repo_id,
        &manifest.probe_run_id,
    );
    write_json(&path, manifest)?;
    write_json(
        &release_manifest_latest_path(&config.paths.state_root, &manifest.repo_id),
        manifest,
    )
}

fn run_git(
    repo_path: Option<&Path>,
    args: &[String],
) -> Result<GitProcessOutput, UpstreamProbeError> {
    let mut command = Command::new("git");
    if let Some(repo_path) = repo_path {
        command.arg(format!("--git-dir={}", repo_path.display()));
    }
    command.args(args);
    let output = command
        .output()
        .map_err(|error| UpstreamProbeError::SpawnGit {
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
) -> Result<GitProcessOutput, UpstreamProbeError> {
    let output = run_git(repo_path, args)?;
    if output.success {
        Ok(output)
    } else {
        let detail = format_git_failure(&output);
        Err(UpstreamProbeError::Git {
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

fn classify_failure(detail: &str) -> ProbeFailureClass {
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
    } else if lower.contains("atomic") && (lower.contains("support") || lower.contains("advertis"))
    {
        ProbeFailureClass::ProtocolUnsupported
    } else {
        ProbeFailureClass::Unknown
    }
}

fn parse_ref_snapshot(source: &str) -> Result<Vec<LocalRefSnapshotEntry>, UpstreamProbeError> {
    let mut refs = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((oid, ref_name)) = trimmed.split_once(char::is_whitespace) else {
            return Err(UpstreamProbeError::Git {
                args: vec!["parse-ref-snapshot".to_owned()],
                status: None,
                detail: format!("malformed ref line {trimmed}"),
            });
        };
        let ref_name = ref_name.trim().to_owned();
        if ref_name.ends_with("^{}") {
            continue;
        }
        refs.push(LocalRefSnapshotEntry {
            ref_name,
            oid: oid.trim().to_owned(),
        });
    }
    refs.sort_by(|left, right| left.ref_name.cmp(&right.ref_name));
    Ok(refs)
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

fn disposable_probe_refs(run_id: &str, upstream_id: &str) -> DisposableProbeRefs {
    let suffix = sanitize_component(&format!("{run_id}-{upstream_id}"));
    DisposableProbeRefs {
        branch_ref: format!("refs/heads/git-relay-probe/{suffix}"),
        tag_ref: format!("refs/tags/git-relay-probe-{suffix}"),
    }
}

fn current_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn generate_run_id() -> String {
    format!("probe-{}-{}", std::process::id(), current_time_ms())
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

fn run_record_path(state_root: &Path, repo_id: &str, run_id: &str) -> PathBuf {
    state_root
        .join("upstream-probes")
        .join("runs")
        .join(sanitize_component(repo_id))
        .join(format!("{run_id}.json"))
}

fn matrix_run_record_path(state_root: &Path, repo_id: &str, run_id: &str) -> PathBuf {
    state_root
        .join("upstream-probes")
        .join("matrix-runs")
        .join(sanitize_component(repo_id))
        .join(format!("{run_id}.json"))
}

fn release_manifest_path(state_root: &Path, repo_id: &str, run_id: &str) -> PathBuf {
    state_root
        .join("upstream-probes")
        .join("release-manifests")
        .join(sanitize_component(repo_id))
        .join(format!("{run_id}.json"))
}

fn release_manifest_latest_path(state_root: &Path, repo_id: &str) -> PathBuf {
    state_root
        .join("upstream-probes")
        .join("release-manifests")
        .join(sanitize_component(repo_id))
        .join("latest.json")
}

fn serialize_label<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), UpstreamProbeError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| UpstreamProbeError::CreateDir {
            path: parent.to_path_buf(),
            error,
        })?;
    }
    let encoded =
        serde_json::to_vec_pretty(value).map_err(|error| UpstreamProbeError::ParseJson {
            path: path.to_path_buf(),
            error,
        })?;
    let mut file = fs::File::create(path).map_err(|error| UpstreamProbeError::Write {
        path: path.to_path_buf(),
        error,
    })?;
    file.write_all(&encoded)
        .map_err(|error| UpstreamProbeError::Write {
            path: path.to_path_buf(),
            error,
        })?;
    file.write_all(b"\n")
        .map_err(|error| UpstreamProbeError::Write {
            path: path.to_path_buf(),
            error,
        })?;
    Ok(())
}

#[derive(Debug, Clone)]
struct GitProcessOutput {
    success: bool,
    status: Option<i32>,
    stdout: String,
    stderr: String,
    args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalRefSnapshotEntry {
    ref_name: String,
    oid: String,
}

#[derive(Debug, Clone)]
struct DisposableProbeRefs {
    branch_ref: String,
    tag_ref: String,
}

#[derive(Debug, Clone, Copy)]
struct ObservedRemoteRefs {
    branch_present: bool,
    tag_present: bool,
}

enum CleanupOutcome {
    Succeeded,
    Failed {
        classification: ProbeFailureClass,
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{classify_failure, ProbeFailureClass};

    #[test]
    fn classifies_access_denied_failures() {
        assert_eq!(
            classify_failure(
                "fatal: Authentication failed for 'https://example.invalid/repo.git/'"
            ),
            ProbeFailureClass::AccessDenied
        );
    }

    #[test]
    fn classifies_repository_missing_failures() {
        assert_eq!(
            classify_failure("fatal: '/tmp/missing.git' does not appear to be a git repository"),
            ProbeFailureClass::RepositoryMissing
        );
    }

    #[test]
    fn classifies_atomic_protocol_failures() {
        assert_eq!(
            classify_failure("fatal: the receiving end does not support --atomic push"),
            ProbeFailureClass::ProtocolUnsupported
        );
    }
}
