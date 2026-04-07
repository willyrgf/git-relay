use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum ProofMode {
    Fast,
    Full,
    ProviderAdmission,
}

impl ProofMode {
    pub fn profile_label(&self) -> &'static str {
        match self {
            Self::ProviderAdmission => "provider-admission",
            Self::Fast | Self::Full => "deterministic-core",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseStatus {
    Pass,
    Fail,
}

impl CaseStatus {
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Pass)
    }

    pub fn to_bool(self) -> bool {
        matches!(self, Self::Pass)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofArtifactKind {
    Raw,
    Normalized,
    Failure,
    Manifest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofToolchain {
    pub git_version: String,
    pub nix_version: String,
    pub openssh_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofAssertion {
    pub id: String,
    pub status: CaseStatus,
    pub detail: Option<String>,
}

impl ProofAssertion {
    pub fn pass(id: impl Into<String>, detail: Option<String>) -> Self {
        Self {
            id: id.into(),
            status: CaseStatus::Pass,
            detail,
        }
    }

    pub fn fail(id: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status: CaseStatus::Fail,
            detail: Some(detail.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofCaseArtifact {
    pub label: String,
    pub path: String,
    pub kind: ProofArtifactKind,
}

impl ProofCaseArtifact {
    pub fn new(label: impl Into<String>, path: &Path, kind: ProofArtifactKind) -> Self {
        Self {
            label: label.into(),
            path: path.display().to_string(),
            kind,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofCaseResult {
    pub case_id: String,
    pub status: CaseStatus,
    pub started_at_ms: u128,
    pub completed_at_ms: u128,
    pub assertions: Vec<ProofAssertion>,
    pub artifacts: Vec<ProofCaseArtifact>,
    pub contracts: Vec<String>,
    pub transport_profiles: Vec<String>,
}

impl ProofCaseResult {
    pub fn new(case_id: impl Into<String>) -> Self {
        let now = current_time_ms();
        Self {
            case_id: case_id.into(),
            status: CaseStatus::Pass,
            started_at_ms: now,
            completed_at_ms: now,
            assertions: Vec::new(),
            artifacts: Vec::new(),
            contracts: Vec::new(),
            transport_profiles: Vec::new(),
        }
    }

    pub fn add_assertion(&mut self, assertion: ProofAssertion) {
        if !assertion.status.is_pass() {
            self.status = CaseStatus::Fail;
        }
        self.assertions.push(assertion);
    }

    pub fn add_artifact(&mut self, label: impl Into<String>, path: &Path, kind: ProofArtifactKind) {
        self.artifacts
            .push(ProofCaseArtifact::new(label, path, kind));
    }

    pub fn finish(mut self) -> Self {
        self.completed_at_ms = current_time_ms();
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedProofAssertion {
    pub id: String,
    pub status: CaseStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedProofCase {
    pub case_id: String,
    pub status: CaseStatus,
    pub assertions: Vec<NormalizedProofAssertion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofSuiteSummaryRaw {
    pub schema_version: u32,
    pub suite: String,
    pub mode: ProofMode,
    pub suite_id: Option<String>,
    pub toolchain: ProofToolchain,
    pub cases: Vec<ProofCaseResult>,
    pub overall_status: CaseStatus,
    pub normalized_summary_sha256: Option<String>,
    pub started_at_ms: u128,
    pub completed_at_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedProofSuiteSummary {
    pub schema_version: u32,
    pub suite: String,
    pub mode: ProofMode,
    pub toolchain: ProofToolchain,
    pub cases: Vec<NormalizedProofCase>,
    pub overall_status: CaseStatus,
    pub normalized_summary_sha256: Option<String>,
}

impl ProofSuiteSummaryRaw {
    pub fn new(mode: ProofMode, toolchain: ProofToolchain, suite_id: Option<String>) -> Self {
        Self {
            schema_version: 1,
            suite: "rfc-proof-e2e".to_owned(),
            mode,
            suite_id,
            toolchain,
            cases: Vec::new(),
            overall_status: CaseStatus::Pass,
            normalized_summary_sha256: None,
            started_at_ms: current_time_ms(),
            completed_at_ms: current_time_ms(),
        }
    }

    pub fn add_case_result(&mut self, result: ProofCaseResult) {
        if !result.status.is_pass() {
            self.overall_status = CaseStatus::Fail;
        }
        self.cases.push(result);
    }

    pub fn finish(&mut self) {
        self.completed_at_ms = current_time_ms();
    }

    pub fn set_normalized_summary_hash(&mut self, hash: String) {
        self.normalized_summary_sha256 = Some(hash);
    }

    pub fn to_normalized(&self, summary_sha256: Option<String>) -> NormalizedProofSuiteSummary {
        let mut cases = self
            .cases
            .iter()
            .map(|entry| {
                let assertions = entry
                    .assertions
                    .iter()
                    .map(|item| NormalizedProofAssertion {
                        id: item.id.clone(),
                        status: item.status,
                    })
                    .collect();
                NormalizedProofCase {
                    case_id: entry.case_id.clone(),
                    status: entry.status,
                    assertions,
                }
            })
            .collect::<Vec<_>>();
        cases.sort_by(|left, right| left.case_id.cmp(&right.case_id));

        NormalizedProofSuiteSummary {
            schema_version: self.schema_version,
            suite: self.suite.clone(),
            mode: self.mode,
            toolchain: self.toolchain.clone(),
            cases,
            overall_status: self.overall_status,
            normalized_summary_sha256: summary_sha256,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProofEvidencePaths {
    pub case_dir: std::path::PathBuf,
}

impl ProofEvidencePaths {
    pub fn case_artifact_path(&self, case_id: &str, file_name: &str) -> std::path::PathBuf {
        self.case_dir.join(case_id).join(file_name)
    }
}

pub fn current_time_ms() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
