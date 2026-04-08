use serde_json::json;

use crate::proof_support::cases::{CaseDefinition, STANDARD_CASE_ARTIFACTS};
use crate::proof_support::lab::{CaseReport, ProofLab, AUTHORITATIVE_REPO_ID};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P05",
        setup: "Start with a missing upstream endpoint for a non-atomic target and no replay log state.",
        action: "Run reconcile once (stalled), then repoint to a reachable upstream and run reconcile again.",
        required_assertions: &[
            "p05.first_run.executed",
            "p05.no_optimistic_observed_ref",
            "p05.second_run.converged",
            "p05.observed_matches_local",
        ],
        required_artifacts: STANDARD_CASE_ARTIFACTS,
        pass_criteria: &[
            "observed refs do not mutate optimistically on failed apply",
            "later run recomputes from current local refs and converges",
        ],
        fail_criteria: &[
            "observed refs change before explicit observation",
            "recovery depends on replaying historical push events",
        ],
        contract_refs: &[
            "RFC_PROOF_E2E_TEST.md#P05",
            "git-relay-rfc.md observed refs update contract",
            "verification-plan result E",
        ],
        runner: run,
    }
}

fn run(lab: &mut ProofLab, _mode: ProofMode) -> Result<CaseReport, String> {
    let mut report = CaseReport::with_details(json!({}));

    let upstream_id = "p05-alpha";
    let observed_ref = format!("refs/git-relay/upstreams/{upstream_id}/heads/main");

    lab.write_authoritative_descriptor_with_write_upstreams(&[(
        upstream_id,
        &lab.upstream_gamma_missing,
        false,
    )])
    .map_err(|error| error.to_string())?;

    let first = lab
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

    let observed_after_first = lab
        .git_ref_exists(&lab.authoritative_repo, &observed_ref)
        .map_err(|error| error.to_string())?;

    lab.write_authoritative_descriptor_with_write_upstreams(&[(
        upstream_id,
        &lab.upstream_alpha,
        false,
    )])
    .map_err(|error| error.to_string())?;

    let second = lab
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

    let observed_after_second = lab
        .git_ref_exists(&lab.authoritative_repo, &observed_ref)
        .map_err(|error| error.to_string())?;

    let mut second_converged = false;
    if second.success() {
        let parsed: serde_json::Value =
            serde_json::from_str(&second.stdout).map_err(|error| error.to_string())?;
        if let Some(run) = parsed.as_array().and_then(|runs| runs.first()) {
            if let Some(results) = run["upstream_results"].as_array() {
                second_converged = results.iter().any(|entry| {
                    entry["upstream_id"] == upstream_id && entry["state"] == "in_sync"
                });
            }
        }
    }

    let observed_matches_local = if observed_after_second {
        let local_main = lab
            .read_git_ref(&lab.authoritative_repo, "refs/heads/main")
            .map_err(|error| error.to_string())?;
        let observed_main = lab
            .read_git_ref(&lab.authoritative_repo, &observed_ref)
            .map_err(|error| error.to_string())?;
        local_main == observed_main
    } else {
        false
    };

    report.assertions.push(if first.success() {
        ProofAssertion::pass(
            "p05.first_run.executed",
            Some("first reconcile run completed with missing upstream".to_owned()),
        )
    } else {
        ProofAssertion::fail("p05.first_run.executed", first.summary())
    });
    report.assertions.push(if !observed_after_first {
        ProofAssertion::pass(
            "p05.no_optimistic_observed_ref",
            Some("observed refs unchanged after stalled apply".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p05.no_optimistic_observed_ref",
            "observed refs changed before explicit successful observation",
        )
    });
    report
        .assertions
        .push(if second.success() && second_converged {
            ProofAssertion::pass(
                "p05.second_run.converged",
                Some("second run converged after endpoint recovery".to_owned()),
            )
        } else {
            ProofAssertion::fail(
                "p05.second_run.converged",
                format!(
                    "second_success={} second_converged={}",
                    second.success(),
                    second_converged
                ),
            )
        });
    report.assertions.push(if observed_matches_local {
        ProofAssertion::pass(
            "p05.observed_matches_local",
            Some("observed namespace aligns with current local refs".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p05.observed_matches_local",
            "observed refs did not converge to current local refs",
        )
    });

    report.details = json!({
        "observed_ref": observed_ref,
        "observed_after_first": observed_after_first,
        "observed_after_second": observed_after_second,
        "second_converged": second_converged,
        "observed_matches_local": observed_matches_local,
    });

    Ok(report)
}
