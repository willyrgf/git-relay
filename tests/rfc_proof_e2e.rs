mod proof_support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use proof_support::{
    artifact,
    lab::{LabError, LabProfile, ProofLab, ProviderAdmissionInputs},
    schema::{
        CaseStatus, ProofArtifactKind, ProofAssertion, ProofCaseResult, ProofMode,
        ProofRequiredArtifact, ProofSuiteSummaryRaw,
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
    result.required_assertions = case
        .required_assertions
        .iter()
        .map(|value| (*value).to_owned())
        .collect();
    result.required_artifacts = case
        .required_artifacts
        .iter()
        .map(|artifact| ProofRequiredArtifact::new(artifact.label, artifact.kind))
        .collect();
    result.contracts = case
        .contract_refs
        .iter()
        .map(|value| (*value).to_owned())
        .collect();

    let mut case_json = case.base_case_json();
    match case.run(lab, mode) {
        Ok(report) => {
            for assertion in report.assertions {
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

    let raw_case_path = lab
        .case_root(case.case_id)?
        .join(format!("{}.raw.json", case.case_id));
    let paths = lab.evidence_paths();
    let raw_path = paths.case_artifact_path(case.case_id, &format!("{}.raw.json", case.case_id));
    let normalized_path =
        paths.case_artifact_path(case.case_id, &format!("{}.normalized.json", case.case_id));
    result.add_artifact("case.raw", &raw_path, ProofArtifactKind::Raw);
    result.add_artifact(
        "case.normalized",
        &normalized_path,
        ProofArtifactKind::Normalized,
    );
    result.set_contract_validation_errors(validate_case_contract(case, &result));

    if let serde_json::Value::Object(map) = &mut case_json {
        map.insert("status".to_owned(), serde_json::json!(result.status));
        map.insert(
            "assertions".to_owned(),
            serde_json::to_value(&result.assertions).map_err(|source| LabError::ParseJson {
                path: raw_case_path.clone(),
                source,
            })?,
        );
        map.insert(
            "transport_profiles".to_owned(),
            serde_json::to_value(&result.transport_profiles).map_err(|source| {
                LabError::ParseJson {
                    path: raw_case_path.clone(),
                    source,
                }
            })?,
        );
        map.insert(
            "contracts".to_owned(),
            serde_json::to_value(&result.contracts).map_err(|source| LabError::ParseJson {
                path: raw_case_path.clone(),
                source,
            })?,
        );
        map.insert(
            "contract_validation".to_owned(),
            serde_json::to_value(&result.contract_validation).map_err(|source| {
                LabError::ParseJson {
                    path: raw_case_path.clone(),
                    source,
                }
            })?,
        );
        map.insert(
            "artifacts".to_owned(),
            serde_json::to_value(&result.artifacts).map_err(|source| LabError::ParseJson {
                path: raw_case_path,
                source,
            })?,
        );
    }

    let redacted_case_json = artifact::redact_json_value(&case_json, lab.runner.secret_pairs())?;
    lab.record_case_event(case.case_id, result.status, &redacted_case_json);
    let _ = lab.persist_case_artifacts(case.case_id, &redacted_case_json)?;

    if result.status == CaseStatus::Fail {
        persist_failure_capture(lab, case.case_id, &redacted_case_json, &result.assertions)?;
    }

    Ok(result.finish())
}

fn validate_case_contract(
    case: &proof_support::cases::CaseDefinition,
    result: &ProofCaseResult,
) -> Vec<String> {
    let mut errors = Vec::new();

    let declared_assertions = case
        .required_assertions
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let mut seen_assertions = BTreeSet::new();
    for assertion in &result.assertions {
        if !seen_assertions.insert(assertion.id.clone()) {
            errors.push(format!(
                "duplicate assertion id emitted for {}: {}",
                case.case_id, assertion.id
            ));
        }
        if assertion.id.ends_with(".runner.error") {
            continue;
        }
        if !declared_assertions.contains(assertion.id.as_str()) {
            errors.push(format!(
                "undeclared assertion id emitted for {}: {}",
                case.case_id, assertion.id
            ));
        }
    }
    for required in case.required_assertions {
        if !seen_assertions.contains(*required) {
            errors.push(format!(
                "missing required assertion for {}: {}",
                case.case_id, required
            ));
        }
    }

    let declared_artifacts = case
        .required_artifacts
        .iter()
        .map(|artifact| (artifact.label, artifact.kind))
        .collect::<BTreeMap<_, _>>();
    let mut seen_artifacts = BTreeMap::new();
    for artifact in &result.artifacts {
        if let Some(previous) = seen_artifacts.insert(artifact.label.clone(), artifact.kind) {
            errors.push(format!(
                "duplicate artifact label emitted for {}: {} ({previous:?} and {:?})",
                case.case_id, artifact.label, artifact.kind
            ));
        }
        match declared_artifacts.get(artifact.label.as_str()) {
            Some(kind) if *kind == artifact.kind => {}
            Some(kind) => errors.push(format!(
                "artifact kind mismatch for {}: {} expected {:?} got {:?}",
                case.case_id, artifact.label, kind, artifact.kind
            )),
            None => errors.push(format!(
                "undeclared artifact emitted for {}: {}",
                case.case_id, artifact.label
            )),
        }
    }
    for required in case.required_artifacts {
        match seen_artifacts.get(required.label) {
            Some(kind) if *kind == required.kind => {}
            Some(kind) => errors.push(format!(
                "required artifact kind mismatch for {}: {} expected {:?} got {:?}",
                case.case_id, required.label, required.kind, kind
            )),
            None => errors.push(format!(
                "missing required artifact for {}: {}",
                case.case_id, required.label
            )),
        }
    }

    errors
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

fn init_provider_target_repo(path: &Path) -> Result<(), String> {
    let status = Command::new("git")
        .args([
            "-c",
            "init.defaultBranch=main",
            "init",
            "--bare",
            path.to_str().ok_or("target path")?,
        ])
        .status()
        .map_err(|error| error.to_string())?;
    if !status.success() {
        return Err("failed to init provider target repo".to_owned());
    }

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
        let git_dir_arg = format!("--git-dir={}", path.display());
        let status = Command::new("git")
            .args([git_dir_arg.as_str(), "config", key, value])
            .status()
            .map_err(|error| error.to_string())?;
        if !status.success() {
            return Err(format!("failed to set {} on provider target repo", key));
        }
    }
    Ok(())
}

#[test]
fn proof_case_definitions_declare_required_assertions_and_artifacts() {
    for case in proof_support::cases::all_cases() {
        assert!(
            !case.required_assertions.is_empty(),
            "{} must declare required assertions",
            case.case_id
        );
        assert!(
            !case.required_artifacts.is_empty(),
            "{} must declare required artifacts",
            case.case_id
        );
    }
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
    let provider_target = provider_inputs_root
        .path()
        .join("provider-supported-alpha.git");
    init_provider_target_repo(&provider_target).expect("provider target repo");
    let missing_target = provider_inputs_root
        .path()
        .join("provider-missing-beta.git");
    let targets = provider_inputs_root.path().join("targets.json");
    let credentials = provider_inputs_root.path().join("credentials.env");
    std::fs::write(
        &targets,
        format!(
            "{{\n  \"schema_version\": 1,\n  \"targets\": [\n    {{\n      \"target_id\": \"supported-alpha\",\n      \"product\": \"local-git\",\n      \"class\": \"self-managed\",\n      \"transport\": \"ssh\",\n      \"url\": \"{}\",\n      \"credential_source\": \"env:SUPPORTED_ALPHA_CREDENTIAL\",\n      \"host_key_policy\": \"pinned-known-hosts\",\n      \"require_atomic\": true,\n      \"same_repo_hidden_refs\": true\n    }},\n    {{\n      \"target_id\": \"missing-beta\",\n      \"product\": \"local-git\",\n      \"class\": \"managed\",\n      \"transport\": \"ssh\",\n      \"url\": \"{}\",\n      \"credential_source\": \"env:MISSING_BETA_CREDENTIAL\",\n      \"host_key_policy\": \"pinned-known-hosts\",\n      \"require_atomic\": false,\n      \"same_repo_hidden_refs\": false\n    }}\n  ]\n}}\n",
            provider_target.display(),
            missing_target.display(),
        ),
    )
    .expect("write provider targets");
    std::fs::write(
        &credentials,
        "SUPPORTED_ALPHA_CREDENTIAL=provider-proof-alpha\nMISSING_BETA_CREDENTIAL=provider-proof-beta\n",
    )
        .expect("write provider credentials");

    let (suite_root, summary) = run_suite(
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
    assert_conformance_manifest_exists(&suite_root, summary.mode, &summary)
        .expect("conformance manifest for provider-admission");
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
