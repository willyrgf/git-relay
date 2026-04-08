use serde_json::json;

use crate::proof_support::cases::{CaseDefinition, STANDARD_CASE_ARTIFACTS};
use crate::proof_support::lab::{CaseReport, ProofLab, AUTHORITATIVE_REPO_ID};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P04",
        setup: "Configure one atomic-capable upstream and one upstream with receive.advertiseAtomic=false.",
        action: "Probe upstream capabilities and assert require_atomic policy stays fail-closed.",
        required_assertions: &[
            "p04.probe.completed",
            "p04.alpha.supported",
            "p04.beta.unsupported",
            "p04.require_atomic.fail_closed",
            "p04.require_atomic.degraded_safety",
            "p04.probe.cleanup",
        ],
        required_artifacts: STANDARD_CASE_ARTIFACTS,
        pass_criteria: &[
            "alpha classified supported",
            "beta classified unsupported when atomic missing",
            "require_atomic targets are never silently downgraded",
            "repository safety degrades explicitly when require_atomic upstreams remain unsupported",
        ],
        fail_criteria: &[
            "unsupported atomic capability treated as supported",
            "require_atomic target admitted on non-atomic fallback",
        ],
        contract_refs: &[
            "RFC_PROOF_E2E_TEST.md#P04",
            "git-relay-rfc.md atomic capability contract",
            "verification-plan result D",
        ],
        runner: run,
    }
}

fn run(lab: &mut ProofLab, _mode: ProofMode) -> Result<CaseReport, String> {
    let mut report = CaseReport::with_details(json!({}));

    lab.run_git_expect_success(
        &[
            format!("--git-dir={}", lab.upstream_beta.display()),
            "config".to_owned(),
            "receive.advertiseAtomic".to_owned(),
            "false".to_owned(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;

    lab.write_authoritative_descriptor_with_write_upstreams(&[
        ("alpha", &lab.upstream_alpha, true),
        ("beta", &lab.upstream_beta, true),
    ])
    .map_err(|error| error.to_string())?;

    let capture = lab
        .run_git_relay(
            &[
                "replication".to_owned(),
                "probe-upstreams".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--repo".to_owned(),
                AUTHORITATIVE_REPO_ID.to_owned(),
                "--json".to_owned(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;

    let mut alpha_supported = false;
    let mut beta_unsupported = false;
    let mut require_atomic_fail_closed = false;
    let mut degraded_safety = false;
    let mut reconcile_details = json!(null);

    if capture.success() {
        let parsed: serde_json::Value =
            serde_json::from_str(&capture.stdout).map_err(|error| error.to_string())?;
        let runs = parsed.as_array().ok_or("probe output was not an array")?;
        if let Some(run) = runs.first() {
            if let Some(results) = run["results"].as_array() {
                alpha_supported = results.iter().any(|entry| {
                    entry["upstream_id"] == "alpha"
                        && entry["atomic_capability"]["verdict"] == "supported"
                        && entry["supported_for_policy"] == true
                });
                beta_unsupported = results.iter().any(|entry| {
                    entry["upstream_id"] == "beta"
                        && entry["atomic_capability"]["verdict"] == "unsupported"
                        && entry["supported_for_policy"] == false
                });
                require_atomic_fail_closed = results
                    .iter()
                    .filter(|entry| {
                        entry["require_atomic"] == true
                            && entry["atomic_capability"]["verdict"] != "supported"
                    })
                    .all(|entry| entry["supported_for_policy"] == false);
            }
        }
    }

    let reconcile = lab
        .run_git_relay(
            &[
                "replication".to_owned(),
                "reconcile".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--repo".to_owned(),
                AUTHORITATIVE_REPO_ID.to_owned(),
                "--json".to_owned(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;
    if reconcile.success() {
        let parsed: serde_json::Value =
            serde_json::from_str(&reconcile.stdout).map_err(|error| error.to_string())?;
        if let Some(run) = parsed.as_array().and_then(|runs| runs.first()) {
            degraded_safety = run["repo_safety"] == "degraded"
                && run["upstream_results"]
                    .as_array()
                    .map(|results| {
                        results.iter().any(|entry| {
                            entry["upstream_id"] == "beta"
                                && entry["state"] == "unsupported"
                                && entry["apply_attempted"] == false
                                && entry["atomic_capability"] == "unsupported"
                        })
                    })
                    .unwrap_or(false);
            reconcile_details = json!({
                "repo_safety": run["repo_safety"],
                "upstream_results": run["upstream_results"],
            });
        }
    }

    let probe_refs_clean = probe_refs_clean(lab, &lab.upstream_alpha)
        && probe_refs_clean(lab, &lab.upstream_beta)
        && probe_refs_clean(lab, &lab.authoritative_repo);

    report.assertions.push(if capture.success() {
        ProofAssertion::pass(
            "p04.probe.completed",
            Some("probe-upstreams command completed".to_owned()),
        )
    } else {
        ProofAssertion::fail("p04.probe.completed", capture.summary())
    });
    report.assertions.push(if alpha_supported {
        ProofAssertion::pass(
            "p04.alpha.supported",
            Some("alpha is atomic-capable".to_owned()),
        )
    } else {
        ProofAssertion::fail("p04.alpha.supported", "alpha was not classified supported")
    });
    report.assertions.push(if beta_unsupported {
        ProofAssertion::pass(
            "p04.beta.unsupported",
            Some("beta without atomic capability stayed unsupported".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p04.beta.unsupported",
            "beta did not report unsupported atomic capability",
        )
    });
    report.assertions.push(if require_atomic_fail_closed {
        ProofAssertion::pass(
            "p04.require_atomic.fail_closed",
            Some("require_atomic targets remained unconverged".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p04.require_atomic.fail_closed",
            "require_atomic target was silently downgraded",
        )
    });
    report.assertions.push(if degraded_safety {
        ProofAssertion::pass(
            "p04.require_atomic.degraded_safety",
            Some(
                "reconcile kept beta unsupported and surfaced degraded repository safety"
                    .to_owned(),
            ),
        )
    } else {
        ProofAssertion::fail("p04.require_atomic.degraded_safety", reconcile.summary())
    });
    report.assertions.push(if probe_refs_clean {
        ProofAssertion::pass(
            "p04.probe.cleanup",
            Some("probe refs cleaned after capability probe".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p04.probe.cleanup",
            "probe refs remained after capability probe",
        )
    });

    report.details = json!({
        "alpha_supported": alpha_supported,
        "beta_unsupported": beta_unsupported,
        "require_atomic_fail_closed": require_atomic_fail_closed,
        "degraded_safety": degraded_safety,
        "reconcile": reconcile.summary(),
        "reconcile_details": reconcile_details,
        "probe_refs_clean": probe_refs_clean,
    });

    Ok(report)
}

fn probe_refs_clean(lab: &ProofLab, repo: &std::path::Path) -> bool {
    let capture = lab.run_git(
        &[
            format!("--git-dir={}", repo.display()),
            "for-each-ref".to_owned(),
            "--format=%(refname)".to_owned(),
            "refs/heads/git-relay-probe".to_owned(),
            "refs/tags/git-relay-probe-".to_owned(),
        ],
        None,
        &[],
    );
    match capture {
        Ok(capture) => capture.success() && capture.stdout.trim().is_empty(),
        Err(_) => false,
    }
}
