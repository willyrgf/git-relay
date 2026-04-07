mod proof_support;

use std::path::{Path, PathBuf};

use proof_support::{
    artifact,
    lab::{LabError, LabProfile, ProofLab, ProviderAdmissionInputs},
    schema::{
        CaseStatus, ProofArtifactKind, ProofAssertion, ProofCaseResult, ProofMode,
        ProofSuiteSummaryRaw,
    },
};
use tempfile::TempDir;

fn proof_tests_enabled() -> bool {
    match std::env::var("GIT_RELAY_PROOF_ENABLE") {
        Ok(value) => matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"),
        Err(_) => false,
    }
}

fn should_skip_proof_tests() -> bool {
    if proof_tests_enabled() {
        return false;
    }
    eprintln!(
        "skipping RFC proof tests because GIT_RELAY_PROOF_ENABLE is not enabled in this environment"
    );
    true
}

fn run_suite(
    mode: ProofMode,
    lab_profile: &LabProfile,
    suite_id: &str,
    provider_inputs: Option<ProviderAdmissionInputs>,
) -> Result<(PathBuf, ProofSuiteSummaryRaw), String> {
    let mut lab =
        ProofLab::new(lab_profile, suite_id, provider_inputs).map_err(|error| error.to_string())?;
    let mut summary =
        ProofSuiteSummaryRaw::new(mode, lab.toolchain.clone(), Some(suite_id.to_owned()));

    let mut cases = proof_support::cases::all_cases();
    cases.sort_by(|left, right| left.case_id.cmp(right.case_id));

    for case in cases {
        let result = run_case(&mut lab, &case, summary.mode).map_err(|error| error.to_string())?;
        summary.add_case_result(result);
    }

    let suite_root = lab
        .persist_summary(&mut summary)
        .map_err(|error| error.to_string())?;
    let _ = lab.temp_dir.keep();
    Ok((suite_root, summary))
}

fn run_case(
    lab: &mut ProofLab,
    case: &proof_support::cases::CaseDefinition,
    mode: ProofMode,
) -> Result<ProofCaseResult, LabError> {
    let mut result = ProofCaseResult::new(case.case_id);
    result.contracts = case
        .contract_refs
        .iter()
        .map(|value| (*value).to_owned())
        .collect();

    let mut case_json = case.base_case_json();
    match case.run(lab, mode) {
        Ok(report) => {
            let mut assertions = report.assertions;
            if assertions.is_empty() {
                assertions.push(ProofAssertion::fail(
                    format!("{}.assertions.present", case.case_id.to_ascii_lowercase()),
                    "case runner returned no assertions",
                ));
            }
            for assertion in assertions {
                result.add_assertion(assertion);
            }
            result.transport_profiles = report.transport_profiles;
            for artifact in report.artifacts {
                result.add_artifact(artifact.label, &artifact.path, artifact.kind);
            }
            if let serde_json::Value::Object(map) = &mut case_json {
                map.insert("details".to_owned(), report.details);
            }
        }
        Err(error) => {
            result.add_assertion(ProofAssertion::fail(
                format!("{}.runner.error", case.case_id.to_ascii_lowercase()),
                error,
            ));
            if let serde_json::Value::Object(map) = &mut case_json {
                map.insert(
                    "details".to_owned(),
                    serde_json::json!({
                        "runner_error": "case execution failed before assertions completed",
                    }),
                );
            }
        }
    }

    if let serde_json::Value::Object(map) = &mut case_json {
        map.insert("status".to_owned(), serde_json::json!(result.status));
        let assertions_path = lab
            .case_root(case.case_id)?
            .join(format!("{}.raw.json", case.case_id));
        map.insert(
            "assertions".to_owned(),
            serde_json::to_value(&result.assertions).map_err(|source| LabError::ParseJson {
                path: assertions_path,
                source,
            })?,
        );
        let transports_path = lab
            .case_root(case.case_id)?
            .join(format!("{}.raw.json", case.case_id));
        map.insert(
            "transport_profiles".to_owned(),
            serde_json::to_value(&result.transport_profiles).map_err(|source| {
                LabError::ParseJson {
                    path: transports_path,
                    source,
                }
            })?,
        );
        map.insert("contracts".to_owned(), serde_json::json!(result.contracts));
    }

    let redacted_case_json = artifact::redact_json_value(&case_json, lab.runner.secret_pairs())?;
    lab.record_case_event(case.case_id, result.status, &redacted_case_json);
    let (raw_path, normalized_path) =
        lab.persist_case_artifacts(case.case_id, &redacted_case_json)?;
    result.add_artifact("case.raw", &raw_path, ProofArtifactKind::Raw);
    result.add_artifact(
        "case.normalized",
        &normalized_path,
        ProofArtifactKind::Normalized,
    );

    if result.status == CaseStatus::Fail {
        persist_failure_capture(lab, case.case_id, &redacted_case_json, &result.assertions)?;
    }

    Ok(result.finish())
}

fn persist_failure_capture(
    lab: &ProofLab,
    case_id: &str,
    redacted_case_json: &serde_json::Value,
    assertions: &[ProofAssertion],
) -> Result<(), LabError> {
    let failure_root = lab.suite_root.join("failures").join(case_id);
    std::fs::create_dir_all(&failure_root).map_err(|source| LabError::CreateDir {
        path: failure_root.clone(),
        source,
    })?;

    let stdout_path = failure_root.join("case.stdout.txt");
    let stderr_path = failure_root.join("case.stderr.txt");
    let pretty =
        serde_json::to_string_pretty(redacted_case_json).map_err(|source| LabError::ParseJson {
            path: stdout_path.clone(),
            source,
        })?;
    let failures = assertions
        .iter()
        .filter(|assertion| assertion.status == CaseStatus::Fail)
        .map(|assertion| {
            format!(
                "{}: {}",
                assertion.id,
                assertion.detail.clone().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    artifact::redact_and_persist_failures(&stdout_path, &pretty, lab.runner.secret_pairs())?;
    artifact::redact_and_persist_failures(&stderr_path, &failures, lab.runner.secret_pairs())?;
    Ok(())
}

fn read_summary_hash(suite_root: &Path) -> Result<String, String> {
    std::fs::read_to_string(suite_root.join("summary.normalized.sha256"))
        .map(|value| value.trim().to_owned())
        .map_err(|error| error.to_string())
}

fn sanitize_key(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn assert_conformance_manifest_exists(
    suite_root: &Path,
    mode: ProofMode,
    summary: &ProofSuiteSummaryRaw,
) -> Result<(), String> {
    let platform = match std::env::consts::OS {
        "macos" => "macos",
        "linux" => "linux",
        other => return Err(format!("unsupported platform {other}")),
    };
    let git_key = sanitize_key(&summary.toolchain.git_version);
    let path = suite_root
        .join("manifests")
        .join("git-conformance")
        .join(platform)
        .join(format!("{git_key}.json"));
    if !path.exists() {
        return Err(format!(
            "missing git conformance manifest {}",
            path.display()
        ));
    }

    let source = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
    let parsed: serde_json::Value =
        serde_json::from_str(&source).map_err(|error| error.to_string())?;
    if parsed["schema_version"] != 1 {
        return Err("unexpected conformance schema version".to_owned());
    }
    if parsed["profile"] != mode.profile_label() {
        return Err("unexpected conformance profile label".to_owned());
    }
    if parsed["all_mandatory_cases_passed"] != (summary.overall_status == CaseStatus::Pass) {
        return Err("all_mandatory_cases_passed did not align with suite status".to_owned());
    }
    Ok(())
}

#[test]
fn proof_e2e_fast_profile_runs_required_cases() {
    if should_skip_proof_tests() {
        return;
    }
    let (suite_root, summary) = run_suite(
        ProofMode::Fast,
        &LabProfile::DeterministicCore,
        "proof-fast",
        None,
    )
    .expect("run fast suite");
    assert_eq!(summary.mode, ProofMode::Fast);
    assert_eq!(summary.overall_status, CaseStatus::Pass);
    assert_eq!(
        summary.cases.len(),
        11,
        "all mandatory cases must be present"
    );
    assert_conformance_manifest_exists(&suite_root, summary.mode, &summary)
        .expect("conformance manifest for fast");
}

#[test]
fn proof_e2e_full_profile_reruns_and_hashes() {
    if should_skip_proof_tests() {
        return;
    }
    let (first_root, first) = run_suite(
        ProofMode::Full,
        &LabProfile::DeterministicCore,
        "proof-full-first",
        None,
    )
    .expect("run first full suite");
    let first_hash = read_summary_hash(&first_root).expect("first hash");

    let (second_root, second) = run_suite(
        ProofMode::Full,
        &LabProfile::DeterministicCore,
        "proof-full-second",
        None,
    )
    .expect("run second full suite");
    let second_hash = read_summary_hash(&second_root).expect("second hash");

    assert_eq!(
        first_hash, second_hash,
        "full profile requires deterministic rerun hash equality"
    );
    assert_eq!(first.overall_status, CaseStatus::Pass);
    assert_eq!(second.overall_status, CaseStatus::Pass);
    assert_conformance_manifest_exists(&second_root, second.mode, &second)
        .expect("conformance manifest for full");
}

#[test]
fn proof_e2e_provider_admission_profile_runs_required_evidence_checks() {
    if should_skip_proof_tests() {
        return;
    }
    let provider_inputs_root = TempDir::new().expect("provider inputs tempdir");
    let targets = provider_inputs_root.path().join("targets.json");
    let credentials = provider_inputs_root.path().join("credentials.env");
    std::fs::write(
        &targets,
        "{\n  \"schema_version\": 1,\n  \"targets\": []\n}\n",
    )
    .expect("write provider targets");
    std::fs::write(&credentials, "PROVIDER_TOKEN=provider-proof-token\n")
        .expect("write provider credentials");

    let (_, summary) = run_suite(
        ProofMode::ProviderAdmission,
        &LabProfile::ProviderAdmission,
        "proof-provider",
        Some(ProviderAdmissionInputs {
            target_manifest: targets,
            credentials_file: credentials,
        }),
    )
    .expect("run provider suite");
    assert_eq!(summary.mode, ProofMode::ProviderAdmission);
    assert_eq!(summary.overall_status, CaseStatus::Pass);
}

#[test]
fn proof_e2e_provider_admission_requires_explicit_inputs() {
    if should_skip_proof_tests() {
        return;
    }
    let result = ProofLab::new(
        &LabProfile::ProviderAdmission,
        "proof-provider-missing-inputs",
        None,
    );
    assert!(
        result.is_err(),
        "provider-admission must fail closed without explicit input files"
    );
}
