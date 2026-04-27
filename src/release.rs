use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{
    AppConfig, RepositoryDescriptor, RepositoryMode, ServiceManager, SupportedPlatform,
};
use crate::migration::validated_targeted_relock_nix_versions;
use crate::upstream::MatrixTargetManifest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FloorStatus {
    Open,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostVersionEvidence {
    pub host_id: String,
    pub platform: SupportedPlatform,
    pub service_manager: ServiceManager,
    pub observed_git_version: String,
    pub observed_nix_version: String,
    pub recorded_at_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoReleaseManifestSummary {
    pub repo_id: String,
    pub manifest_path: Option<PathBuf>,
    pub manifest_present: bool,
    pub all_entries_admitted: bool,
    pub admitted_entries: usize,
    pub total_entries: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseConformanceReport {
    pub generated_at_ms: u128,
    pub current_host: HostVersionEvidence,
    pub platform_evidence: Vec<HostVersionEvidence>,
    pub repo_manifests: Vec<RepoReleaseManifestSummary>,
    pub exact_git_floor: Option<String>,
    pub exact_git_floor_status: FloorStatus,
    pub exact_nix_floor: Option<String>,
    pub exact_nix_floor_status: FloorStatus,
    pub blocking_reasons: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ReleaseError {
    #[error("failed to read {path}: {error}", path = path.display())]
    Read {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to create directory {path}: {error}", path = path.display())]
    CreateDir {
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
    #[error("invalid git conformance evidence at {path}: {detail}", path = path.display())]
    InvalidGitConformanceEvidence { path: PathBuf, detail: String },
    #[error("invalid release manifest evidence at {path}: {detail}", path = path.display())]
    InvalidReleaseManifest { path: PathBuf, detail: String },
    #[error("failed to spawn {program} with args {args:?}: {error}")]
    SpawnCommand {
        program: String,
        args: Vec<String>,
        #[source]
        error: std::io::Error,
    },
    #[error("{program} failed for args {args:?} with status {status:?}: {detail}")]
    Command {
        program: String,
        args: Vec<String>,
        status: Option<i32>,
        detail: String,
    },
}

pub fn build_release_conformance_report(
    config: &AppConfig,
    descriptors: &[RepositoryDescriptor],
    target_repo: Option<&str>,
) -> Result<ReleaseConformanceReport, ReleaseError> {
    let generated_at_ms = current_time_ms();
    let current_host = HostVersionEvidence {
        host_id: detect_host_id(),
        platform: config.deployment.platform,
        service_manager: config.deployment.service_manager,
        observed_git_version: read_command(
            git_binary().to_string_lossy().as_ref(),
            &["--version".to_owned()],
        )?
        .trim()
        .to_owned(),
        observed_nix_version: read_command(
            nix_binary().to_string_lossy().as_ref(),
            &["--version".to_owned()],
        )?
        .trim()
        .to_owned(),
        recorded_at_ms: generated_at_ms,
    };
    persist_host_evidence(&config.paths.state_root, &current_host)?;
    let mut platform_evidence = load_host_evidence(&config.paths.state_root)?;
    platform_evidence.sort_by(|left, right| {
        platform_label(left.platform)
            .cmp(platform_label(right.platform))
            .then_with(|| left.host_id.cmp(&right.host_id))
    });

    let selected = descriptors
        .iter()
        .filter(|descriptor| descriptor.mode == RepositoryMode::Authoritative)
        .filter(|descriptor| target_repo.is_none_or(|repo_id| descriptor.repo_id == repo_id))
        .collect::<Vec<_>>();
    let repo_manifests = selected
        .into_iter()
        .map(|descriptor| load_repo_manifest_summary(&config.paths.state_root, &descriptor.repo_id))
        .collect::<Result<Vec<_>, _>>()?;

    let all_manifests_admitted = !repo_manifests.is_empty()
        && repo_manifests
            .iter()
            .all(|manifest| manifest.manifest_present && manifest.all_entries_admitted);
    let supported_platforms_complete = platform_evidence
        .iter()
        .any(|entry| entry.platform == SupportedPlatform::Macos)
        && platform_evidence
            .iter()
            .any(|entry| entry.platform == SupportedPlatform::Linux);
    let git_conformance = load_git_conformance_evidence(&config.paths.state_root)?;
    let exact_git_floor = if supported_platforms_complete {
        exact_git_floor_from_evidence(&git_conformance)
    } else {
        None
    };
    let nix_versions_validated = platform_evidence.iter().all(|entry| {
        validated_targeted_relock_nix_versions()
            .iter()
            .any(|candidate| *candidate == entry.observed_nix_version)
    });

    let mut blocking_reasons = Vec::new();
    if repo_manifests.is_empty() {
        blocking_reasons.push(
            "no release manifest evidence is recorded for the selected repositories".to_owned(),
        );
    } else if !all_manifests_admitted {
        blocking_reasons.push(
            "at least one repository release manifest is missing or not fully admitted".to_owned(),
        );
    }
    if !supported_platforms_complete {
        blocking_reasons.push(
            "release host evidence does not yet cover both supported platforms (macOS and Linux)"
                .to_owned(),
        );
    }
    if !nix_versions_validated {
        blocking_reasons.push(
            "recorded host Nix versions are outside the validated targeted relock matrix"
                .to_owned(),
        );
    }
    if exact_git_floor.is_none() {
        blocking_reasons.push(
            "exact Git floor evidence remains open until admitted deterministic-core git-conformance records exist for both supported platforms at the same Git version"
                .to_owned(),
        );
    }

    Ok(ReleaseConformanceReport {
        generated_at_ms,
        current_host,
        platform_evidence,
        repo_manifests,
        exact_git_floor: exact_git_floor.clone(),
        exact_git_floor_status: if exact_git_floor.is_some() && all_manifests_admitted {
            FloorStatus::Closed
        } else {
            FloorStatus::Open
        },
        exact_nix_floor: validated_targeted_relock_nix_versions()
            .first()
            .map(|value| (*value).to_owned()),
        exact_nix_floor_status: if all_manifests_admitted
            && supported_platforms_complete
            && nix_versions_validated
        {
            FloorStatus::Closed
        } else {
            FloorStatus::Open
        },
        blocking_reasons,
    })
}

fn persist_host_evidence(
    state_root: &Path,
    evidence: &HostVersionEvidence,
) -> Result<(), ReleaseError> {
    let path = host_evidence_path(state_root, evidence.platform, &evidence.host_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| ReleaseError::CreateDir {
            path: parent.to_path_buf(),
            error,
        })?;
    }
    let encoded = serde_json::to_vec_pretty(evidence).map_err(|error| ReleaseError::ParseJson {
        path: path.clone(),
        error,
    })?;
    fs::write(&path, encoded).map_err(|error| ReleaseError::Write {
        path: path.clone(),
        error,
    })?;
    Ok(())
}

fn load_host_evidence(state_root: &Path) -> Result<Vec<HostVersionEvidence>, ReleaseError> {
    let directory = state_root.join("release").join("hosts");
    if !directory.exists() {
        return Ok(Vec::new());
    }

    let mut evidence = Vec::new();
    for entry in fs::read_dir(&directory).map_err(|error| ReleaseError::Read {
        path: directory.clone(),
        error,
    })? {
        let entry = entry.map_err(|error| ReleaseError::Read {
            path: directory.clone(),
            error,
        })?;
        let path = entry.path();
        if path.is_dir() {
            for nested in fs::read_dir(&path).map_err(|error| ReleaseError::Read {
                path: path.clone(),
                error,
            })? {
                let nested = nested.map_err(|error| ReleaseError::Read {
                    path: path.clone(),
                    error,
                })?;
                let nested_path = nested.path();
                if nested_path.extension().and_then(|value| value.to_str()) == Some("json") {
                    evidence.push(read_host_evidence_file(&nested_path)?);
                }
            }
        } else if path.extension().and_then(|value| value.to_str()) == Some("json") {
            evidence.push(read_host_evidence_file(&path)?);
        }
    }
    Ok(evidence)
}

fn read_host_evidence_file(path: &Path) -> Result<HostVersionEvidence, ReleaseError> {
    let source = fs::read_to_string(path).map_err(|error| ReleaseError::Read {
        path: path.to_path_buf(),
        error,
    })?;
    serde_json::from_str(&source).map_err(|error| ReleaseError::ParseJson {
        path: path.to_path_buf(),
        error,
    })
}

fn load_git_conformance_evidence(
    state_root: &Path,
) -> Result<Vec<StoredGitConformanceEvidenceRecord>, ReleaseError> {
    let root = state_root.join("release").join("git-conformance");
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut evidence = Vec::new();
    for platform_entry in fs::read_dir(&root).map_err(|error| ReleaseError::Read {
        path: root.clone(),
        error,
    })? {
        let platform_entry = platform_entry.map_err(|error| ReleaseError::Read {
            path: root.clone(),
            error,
        })?;
        let platform_path = platform_entry.path();
        if !platform_path.is_dir() {
            continue;
        }
        let platform_name = platform_entry.file_name().to_string_lossy().to_string();
        for entry in fs::read_dir(&platform_path).map_err(|error| ReleaseError::Read {
            path: platform_path.clone(),
            error,
        })? {
            let entry = entry.map_err(|error| ReleaseError::Read {
                path: platform_path.clone(),
                error,
            })?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let source = fs::read_to_string(&path).map_err(|error| ReleaseError::Read {
                path: path.clone(),
                error,
            })?;
            let parsed: StoredGitConformanceEvidence =
                serde_json::from_str(&source).map_err(|error| {
                    ReleaseError::InvalidGitConformanceEvidence {
                        path: path.clone(),
                        detail: format!("schema parse failed: {error}"),
                    }
                })?;
            validate_git_conformance_evidence(&path, &platform_name, &parsed)?;
            evidence.push(StoredGitConformanceEvidenceRecord { evidence: parsed });
        }
    }

    Ok(evidence)
}

fn validate_git_conformance_evidence(
    path: &Path,
    platform_name: &str,
    evidence: &StoredGitConformanceEvidence,
) -> Result<(), ReleaseError> {
    if !matches!(platform_name, "macos" | "linux") {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: format!("unsupported platform directory {}", platform_name),
        });
    }
    if evidence.schema_version != 1 {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: format!("unsupported schema_version {}", evidence.schema_version),
        });
    }
    if platform_label(evidence.platform) != platform_name {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: format!(
                "payload platform {} did not match directory platform {}",
                platform_label(evidence.platform),
                platform_name
            ),
        });
    }
    if evidence.git_version.trim().is_empty() {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: "git_version must not be empty".to_owned(),
        });
    }
    let expected_key = sanitize_path_component(&evidence.git_version);
    if evidence.git_version_key != expected_key {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: format!(
                "git_version_key {} did not match sanitized git_version {}",
                evidence.git_version_key, expected_key
            ),
        });
    }
    let file_key = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if file_key != evidence.git_version_key {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: format!(
                "file key {} did not match payload git_version_key {}",
                file_key, evidence.git_version_key
            ),
        });
    }
    if evidence.nix_system.trim().is_empty() {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: "nix_system must not be empty".to_owned(),
        });
    }
    let expected_service_manager = match evidence.platform {
        SupportedPlatform::Macos => ServiceManager::Launchd,
        SupportedPlatform::Linux => ServiceManager::Systemd,
    };
    if evidence.service_manager != expected_service_manager {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: format!(
                "service_manager {} did not match expected {} for platform {}",
                service_manager_label(evidence.service_manager),
                service_manager_label(expected_service_manager),
                platform_label(evidence.platform),
            ),
        });
    }
    if evidence.openssh_version.trim().is_empty() {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: "openssh_version must not be empty".to_owned(),
        });
    }
    if evidence.filesystem_profile.trim().is_empty() {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: "filesystem_profile must not be empty".to_owned(),
        });
    }
    if evidence.git_relay_commit.trim().is_empty() {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: "git_relay_commit must not be empty".to_owned(),
        });
    }
    if evidence.flake_lock_sha256.trim().is_empty() {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: "flake_lock_sha256 must not be empty".to_owned(),
        });
    }
    for (binary_name, digest) in evidence.binary_digests.entries() {
        if digest.trim().is_empty() {
            return Err(ReleaseError::InvalidGitConformanceEvidence {
                path: path.to_path_buf(),
                detail: format!("binary_digests.{} must not be empty", binary_name),
            });
        }
    }
    if evidence.cases.is_empty() {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: "cases must contain at least one case result".to_owned(),
        });
    }
    let mut seen_case_ids = BTreeSet::new();
    let mut case_status_by_id = BTreeMap::new();
    for case in &evidence.cases {
        if case.case_id.trim().is_empty() {
            return Err(ReleaseError::InvalidGitConformanceEvidence {
                path: path.to_path_buf(),
                detail: "cases[*].case_id must not be empty".to_owned(),
            });
        }
        if !seen_case_ids.insert(case.case_id.clone()) {
            return Err(ReleaseError::InvalidGitConformanceEvidence {
                path: path.to_path_buf(),
                detail: format!("duplicate case_id {} in cases", case.case_id),
            });
        }
        case_status_by_id.insert(case.case_id.clone(), case.status);
    }
    let missing_mandatory = mandatory_case_ids()
        .iter()
        .filter(|case_id| !seen_case_ids.contains(**case_id))
        .copied()
        .collect::<Vec<_>>();
    if !missing_mandatory.is_empty() {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: format!(
                "cases missing mandatory case ids {}",
                missing_mandatory.join(", ")
            ),
        });
    }
    if evidence.all_mandatory_cases_passed
        && evidence
            .cases
            .iter()
            .any(|case| case.status == StoredGitConformanceCaseStatus::Fail)
    {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail:
                "all_mandatory_cases_passed=true conflicted with at least one failing case status"
                    .to_owned(),
        });
    }
    if evidence.all_mandatory_cases_passed {
        let failing_mandatory = mandatory_case_ids()
            .iter()
            .filter(|case_id| {
                case_status_by_id
                    .get(**case_id)
                    .is_some_and(|status| *status == StoredGitConformanceCaseStatus::Fail)
            })
            .copied()
            .collect::<Vec<_>>();
        if !failing_mandatory.is_empty() {
            return Err(ReleaseError::InvalidGitConformanceEvidence {
                path: path.to_path_buf(),
                detail: format!(
                    "all_mandatory_cases_passed=true conflicted with failing mandatory cases {}",
                    failing_mandatory.join(", ")
                ),
            });
        }
    }
    if evidence.normalized_summary_sha256.trim().is_empty() {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: "normalized_summary_sha256 must not be empty".to_owned(),
        });
    }
    if evidence.recorded_at_ms != 0 {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: format!(
                "recorded_at_ms must be deterministic zero for release-admitting evidence, found {}",
                evidence.recorded_at_ms
            ),
        });
    }
    Ok(())
}

fn exact_git_floor_from_evidence(records: &[StoredGitConformanceEvidenceRecord]) -> Option<String> {
    let mut admitted = BTreeMap::<String, BTreeSet<String>>::new();
    for record in records {
        let evidence = &record.evidence;
        if evidence.profile != StoredGitConformanceProfile::DeterministicCore
            || !evidence.all_mandatory_cases_passed
        {
            continue;
        }
        admitted
            .entry(evidence.git_version.clone())
            .or_default()
            .insert(platform_label(evidence.platform).to_owned());
    }

    let mut candidates = admitted
        .into_iter()
        .filter(|(_, platforms)| platforms.contains("macos") && platforms.contains("linux"))
        .map(|(git_version, _)| git_version)
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| git_version_cmp(left, right));
    candidates.into_iter().next()
}

fn git_version_cmp(left: &str, right: &str) -> Ordering {
    parse_git_version_components(left)
        .cmp(&parse_git_version_components(right))
        .then_with(|| left.cmp(right))
}

fn parse_git_version_components(value: &str) -> Vec<u64> {
    let mut numbers = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        if character.is_ascii_digit() {
            current.push(character);
        } else if !current.is_empty() {
            numbers.push(current.parse().unwrap_or(0));
            current.clear();
        }
    }
    if !current.is_empty() {
        numbers.push(current.parse().unwrap_or(0));
    }
    numbers
}

fn load_repo_manifest_summary(
    state_root: &Path,
    repo_id: &str,
) -> Result<RepoReleaseManifestSummary, ReleaseError> {
    let path = state_root
        .join("upstream-probes")
        .join("release-manifests")
        .join(sanitize_path_component(repo_id))
        .join("latest.json");
    if !path.exists() {
        return Ok(RepoReleaseManifestSummary {
            repo_id: repo_id.to_owned(),
            manifest_path: None,
            manifest_present: false,
            all_entries_admitted: false,
            admitted_entries: 0,
            total_entries: 0,
        });
    }

    let source = fs::read_to_string(&path).map_err(|error| ReleaseError::Read {
        path: path.clone(),
        error,
    })?;
    let manifest: StoredReleaseManifest =
        serde_json::from_str(&source).map_err(|error| ReleaseError::ParseJson {
            path: path.clone(),
            error,
        })?;
    validate_stored_release_manifest(&path, repo_id, &manifest)?;
    let admitted_entries = manifest
        .entries
        .iter()
        .filter(|entry| entry.admitted)
        .count();

    Ok(RepoReleaseManifestSummary {
        repo_id: repo_id.to_owned(),
        manifest_path: Some(path),
        manifest_present: true,
        all_entries_admitted: manifest.all_entries_admitted,
        admitted_entries,
        total_entries: manifest.entries.len(),
    })
}

fn validate_stored_release_manifest(
    path: &Path,
    repo_id: &str,
    manifest: &StoredReleaseManifest,
) -> Result<(), ReleaseError> {
    if manifest.repo_id != repo_id {
        return Err(invalid_release_manifest(
            path,
            format!(
                "payload repo_id {} did not match selected repo_id {}",
                manifest.repo_id, repo_id
            ),
        ));
    }
    if manifest.generated_at_ms == 0 {
        return Err(invalid_release_manifest(
            path,
            "generated_at_ms must be non-zero".to_owned(),
        ));
    }
    if manifest.repo_path.as_os_str().is_empty()
        || manifest.manifest_path.as_os_str().is_empty()
        || manifest.probe_run_id.trim().is_empty()
        || manifest.probe_run_path.as_os_str().is_empty()
    {
        return Err(invalid_release_manifest(
            path,
            "manifest provenance fields must not be empty".to_owned(),
        ));
    }
    if !manifest.manifest_path.exists() {
        return Err(invalid_release_manifest(
            path,
            format!(
                "declared target manifest {} is missing",
                manifest.manifest_path.display()
            ),
        ));
    }
    if !manifest.probe_run_path.exists() {
        return Err(invalid_release_manifest(
            path,
            format!(
                "probe run evidence {} is missing",
                manifest.probe_run_path.display()
            ),
        ));
    }
    if manifest.entries.is_empty() {
        return Err(invalid_release_manifest(
            path,
            "entries must not be empty".to_owned(),
        ));
    }
    let computed_all_admitted = manifest.entries.iter().all(|entry| entry.admitted);
    if manifest.all_entries_admitted != computed_all_admitted {
        return Err(invalid_release_manifest(
            path,
            format!(
                "all_entries_admitted={} did not match entry admission state {}",
                manifest.all_entries_admitted, computed_all_admitted
            ),
        ));
    }

    let source =
        fs::read_to_string(&manifest.manifest_path).map_err(|error| ReleaseError::Read {
            path: manifest.manifest_path.clone(),
            error,
        })?;
    let declared: MatrixTargetManifest =
        serde_json::from_str(&source).map_err(|error| ReleaseError::ParseJson {
            path: manifest.manifest_path.clone(),
            error,
        })?;
    if declared.schema_version != 1 {
        return Err(invalid_release_manifest(
            path,
            format!(
                "declared target manifest schema_version {} is unsupported",
                declared.schema_version
            ),
        ));
    }
    if declared.targets.is_empty() {
        return Err(invalid_release_manifest(
            path,
            "declared target manifest must contain at least one target".to_owned(),
        ));
    }

    let mut declared_by_id = BTreeMap::new();
    for target in declared.targets {
        if target.target_id.trim().is_empty() {
            return Err(invalid_release_manifest(
                path,
                "declared target_id must not be empty".to_owned(),
            ));
        }
        if declared_by_id
            .insert(target.target_id.clone(), target)
            .is_some()
        {
            return Err(invalid_release_manifest(
                path,
                "declared target manifest contains duplicate target_id".to_owned(),
            ));
        }
    }

    let mut entry_ids = BTreeSet::new();
    for entry in &manifest.entries {
        if entry.target_id.trim().is_empty()
            || entry.product.trim().is_empty()
            || entry.url.trim().is_empty()
            || entry.evidence_path.as_os_str().is_empty()
        {
            return Err(invalid_release_manifest(
                path,
                "entry target identity and evidence_path fields must not be empty".to_owned(),
            ));
        }
        if !entry.evidence_path.exists() {
            return Err(invalid_release_manifest(
                path,
                format!(
                    "entry evidence_path {} is missing",
                    entry.evidence_path.display()
                ),
            ));
        }
        if entry
            .admission_reasons
            .iter()
            .any(|reason| reason.trim().is_empty())
        {
            return Err(invalid_release_manifest(
                path,
                format!(
                    "entry target_id {} contains an empty admission reason",
                    entry.target_id
                ),
            ));
        }
        if !entry_ids.insert(entry.target_id.clone()) {
            return Err(invalid_release_manifest(
                path,
                format!("duplicate entry target_id {}", entry.target_id),
            ));
        }
        let Some(target) = declared_by_id.get(&entry.target_id) else {
            return Err(invalid_release_manifest(
                path,
                format!(
                    "entry target_id {} is not present in declared target manifest",
                    entry.target_id
                ),
            ));
        };
        if entry.product != target.product
            || entry.class != target.class
            || entry.transport != target.transport
            || entry.url != target.url
            || entry.require_atomic != target.require_atomic
            || entry.same_repo_hidden_refs != target.same_repo_hidden_refs
        {
            return Err(invalid_release_manifest(
                path,
                format!(
                    "entry target_id {} identity no longer matches declared target manifest",
                    entry.target_id
                ),
            ));
        }
    }

    let declared_ids = declared_by_id.keys().cloned().collect::<BTreeSet<_>>();
    if entry_ids != declared_ids {
        return Err(invalid_release_manifest(
            path,
            "release manifest entry coverage does not match declared targets".to_owned(),
        ));
    }

    Ok(())
}

fn invalid_release_manifest(path: &Path, detail: String) -> ReleaseError {
    ReleaseError::InvalidReleaseManifest {
        path: path.to_path_buf(),
        detail,
    }
}

fn git_binary() -> PathBuf {
    std::env::var_os("GIT_RELAY_GIT_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("git"))
}

fn nix_binary() -> PathBuf {
    std::env::var_os("GIT_RELAY_NIX_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("nix"))
}

fn read_command(program: &str, args: &[String]) -> Result<String, ReleaseError> {
    let output =
        Command::new(program)
            .args(args)
            .output()
            .map_err(|error| ReleaseError::SpawnCommand {
                program: program.to_owned(),
                args: args.to_vec(),
                error,
            })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }

    let mut detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if detail.is_empty() {
        detail = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    }
    Err(ReleaseError::Command {
        program: program.to_owned(),
        args: args.to_vec(),
        status: output.status.code(),
        detail,
    })
}

fn host_evidence_path(state_root: &Path, platform: SupportedPlatform, host_id: &str) -> PathBuf {
    state_root
        .join("release")
        .join("hosts")
        .join(platform_label(platform))
        .join(format!("{}.json", sanitize_path_component(host_id)))
}

fn detect_host_id() -> String {
    if let Ok(value) = std::env::var("GIT_RELAY_HOST_ID") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }

    let output = Command::new("hostname").output();
    if let Ok(capture) = output {
        if capture.status.success() {
            let value = String::from_utf8_lossy(&capture.stdout).trim().to_owned();
            if !value.is_empty() {
                return value;
            }
        }
    }

    "unknown-host".to_owned()
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

fn platform_label(platform: SupportedPlatform) -> &'static str {
    match platform {
        SupportedPlatform::Macos => "macos",
        SupportedPlatform::Linux => "linux",
    }
}

fn service_manager_label(service_manager: ServiceManager) -> &'static str {
    match service_manager {
        ServiceManager::Launchd => "launchd",
        ServiceManager::Systemd => "systemd",
    }
}

fn current_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn mandatory_case_ids() -> [&'static str; 11] {
    [
        "P01", "P02", "P03", "P04", "P05", "P06", "P07", "P08", "P09", "P10", "P11",
    ]
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredReleaseManifest {
    generated_at_ms: u128,
    repo_id: String,
    repo_path: PathBuf,
    manifest_path: PathBuf,
    probe_run_id: String,
    probe_run_path: PathBuf,
    all_entries_admitted: bool,
    entries: Vec<StoredReleaseManifestEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredReleaseManifestEntry {
    target_id: String,
    product: String,
    class: crate::upstream::MatrixTargetClass,
    transport: crate::upstream::MatrixTargetTransport,
    url: String,
    require_atomic: bool,
    same_repo_hidden_refs: bool,
    admitted: bool,
    evidence_path: PathBuf,
    admission_reasons: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum StoredGitConformanceProfile {
    DeterministicCore,
    ProviderAdmission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredGitConformanceCaseStatus {
    Pass,
    Fail,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredGitConformanceCase {
    case_id: String,
    status: StoredGitConformanceCaseStatus,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredGitConformanceBinaryDigests {
    #[serde(rename = "git-relay")]
    git_relay: String,
    #[serde(rename = "git-relayd")]
    git_relayd: String,
    #[serde(rename = "git-relay-install-hooks")]
    git_relay_install_hooks: String,
    #[serde(rename = "git-relay-ssh-force-command")]
    git_relay_ssh_force_command: String,
}

impl StoredGitConformanceBinaryDigests {
    fn entries(&self) -> [(&'static str, &str); 4] {
        [
            ("git-relay", self.git_relay.as_str()),
            ("git-relayd", self.git_relayd.as_str()),
            (
                "git-relay-install-hooks",
                self.git_relay_install_hooks.as_str(),
            ),
            (
                "git-relay-ssh-force-command",
                self.git_relay_ssh_force_command.as_str(),
            ),
        ]
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredGitConformanceEvidence {
    schema_version: u64,
    profile: StoredGitConformanceProfile,
    git_version_key: String,
    platform: SupportedPlatform,
    nix_system: String,
    service_manager: ServiceManager,
    git_version: String,
    openssh_version: String,
    filesystem_profile: String,
    git_relay_commit: String,
    flake_lock_sha256: String,
    binary_digests: StoredGitConformanceBinaryDigests,
    cases: Vec<StoredGitConformanceCase>,
    all_mandatory_cases_passed: bool,
    normalized_summary_sha256: String,
    recorded_at_ms: u128,
}

#[derive(Debug)]
struct StoredGitConformanceEvidenceRecord {
    evidence: StoredGitConformanceEvidence,
}
