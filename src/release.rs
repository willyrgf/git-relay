use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{AppConfig, RepositoryDescriptor, ServiceManager, SupportedPlatform};
use crate::migration::validated_targeted_relock_nix_versions;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FloorStatus {
    Open,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostVersionEvidence {
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
    platform_evidence
        .sort_by(|left, right| platform_label(left.platform).cmp(platform_label(right.platform)));
    let git_conformance = load_git_conformance_evidence(&config.paths.state_root)?;
    let exact_git_floor = exact_git_floor_from_evidence(&git_conformance);

    let selected = descriptors
        .iter()
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
        exact_git_floor_status: if exact_git_floor.is_some() {
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
    let path = host_evidence_path(state_root, evidence.platform);
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
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let source = fs::read_to_string(&path).map_err(|error| ReleaseError::Read {
            path: path.clone(),
            error,
        })?;
        let parsed = serde_json::from_str(&source).map_err(|error| ReleaseError::ParseJson {
            path: path.clone(),
            error,
        })?;
        evidence.push(parsed);
    }
    Ok(evidence)
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
                serde_json::from_str(&source).map_err(|error| ReleaseError::ParseJson {
                    path: path.clone(),
                    error,
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
    if evidence.platform != platform_name {
        return Err(ReleaseError::InvalidGitConformanceEvidence {
            path: path.to_path_buf(),
            detail: format!(
                "payload platform {} did not match directory platform {}",
                evidence.platform, platform_name
            ),
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
    Ok(())
}

fn exact_git_floor_from_evidence(records: &[StoredGitConformanceEvidenceRecord]) -> Option<String> {
    let mut admitted = BTreeMap::<String, BTreeSet<String>>::new();
    for record in records {
        let evidence = &record.evidence;
        if evidence.profile != "deterministic-core" || !evidence.all_mandatory_cases_passed {
            continue;
        }
        admitted
            .entry(evidence.git_version.clone())
            .or_default()
            .insert(evidence.platform.clone());
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

fn host_evidence_path(state_root: &Path, platform: SupportedPlatform) -> PathBuf {
    state_root
        .join("release")
        .join("hosts")
        .join(format!("{}.json", platform_label(platform)))
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

fn current_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[derive(Debug, Deserialize)]
struct StoredReleaseManifest {
    all_entries_admitted: bool,
    #[serde(default)]
    entries: Vec<StoredReleaseManifestEntry>,
}

#[derive(Debug, Deserialize)]
struct StoredReleaseManifestEntry {
    admitted: bool,
}

#[derive(Debug, Deserialize)]
struct StoredGitConformanceEvidence {
    schema_version: u64,
    profile: String,
    git_version_key: String,
    platform: String,
    git_version: String,
    all_mandatory_cases_passed: bool,
}

#[derive(Debug)]
struct StoredGitConformanceEvidenceRecord {
    evidence: StoredGitConformanceEvidence,
}
