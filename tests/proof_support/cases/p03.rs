use std::fs;

use serde_json::json;

use crate::proof_support::cases::{CaseDefinition, STANDARD_CASE_ARTIFACTS};
use crate::proof_support::lab::{CaseReport, ProofLab, AUTHORITATIVE_REPO_ID};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P03",
        setup: "Seed one stale run marker plus three upstream targets (alpha, beta, gamma-missing).",
        action: "Run replication reconcile once and verify one bounded run carries deterministic per-upstream outcomes.",
        required_assertions: &[
            "p03.reconcile.completed",
            "p03.single_run_contains_upstreams",
            "p03.deterministic_upstream_order",
            "p03.stale_run_superseded",
            "p03.transient_markers_cleaned",
        ],
        required_artifacts: STANDARD_CASE_ARTIFACTS,
        pass_criteria: &[
            "one run id captures all upstream outcomes",
            "upstream attempt ordering is deterministic",
            "stale run is marked superseded and transient markers are cleaned",
        ],
        fail_criteria: &[
            "missing upstream result",
            "stale transient markers treated as correctness source",
        ],
        contract_refs: &[
            "RFC_PROOF_E2E_TEST.md#P03",
            "git-relay-rfc.md reconcile execution-unit contract",
            "verification-plan result J",
        ],
        runner: run,
    }
}

fn run(lab: &mut ProofLab, _mode: ProofMode) -> Result<CaseReport, String> {
    let mut report = CaseReport::with_details(json!({}));

    lab.write_authoritative_descriptor_with_write_upstreams(&[
        ("alpha", &lab.upstream_alpha, false),
        ("beta", &lab.upstream_beta, false),
        ("gamma", &lab.upstream_gamma_missing, false),
    ])
    .map_err(|error| error.to_string())?;

    let repo_component = ProofLab::repo_state_component(AUTHORITATIVE_REPO_ID);
    let stale_run_id = "reconcile-stale-run";

    let run_dir = lab.reconcile_run_dir(AUTHORITATIVE_REPO_ID);
    fs::create_dir_all(&run_dir).map_err(|error| error.to_string())?;
    let stale_run_path = run_dir.join(format!("{stale_run_id}.json"));
    fs::write(
        &stale_run_path,
        serde_json::to_vec_pretty(&json!({
            "run_id": stale_run_id,
            "repo_id": AUTHORITATIVE_REPO_ID,
            "repo_path": lab.authoritative_repo,
            "started_at_ms": 1,
            "completed_at_ms": null,
            "desired_snapshot": [],
            "captured_upstreams": ["alpha", "beta", "gamma"],
            "repo_safety": "degraded",
            "status": "in_progress",
            "superseded_by": null,
            "upstream_results": [],
        }))
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    let in_progress = lab
        .state_root
        .join("reconcile")
        .join("in-progress")
        .join(format!("{repo_component}.json"));
    if let Some(parent) = in_progress.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(
        &in_progress,
        serde_json::to_vec_pretty(&json!({
            "repo_id": AUTHORITATIVE_REPO_ID,
            "run_id": stale_run_id,
            "pid": 999999,
            "started_at_ms": 1,
        }))
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    let lock_dir = lab
        .state_root
        .join("reconcile")
        .join("locks")
        .join(format!("{repo_component}.lock"));
    fs::create_dir_all(&lock_dir).map_err(|error| error.to_string())?;
    fs::write(
        lock_dir.join("metadata.json"),
        serde_json::to_vec_pretty(&json!({
            "repo_id": AUTHORITATIVE_REPO_ID,
            "run_id": stale_run_id,
            "pid": 999999,
            "acquired_at_ms": 1,
        }))
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    let capture = lab
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

    let mut run_id = String::new();
    let mut ordered = false;
    let mut upstream_count = 0usize;
    if capture.success() {
        let parsed: serde_json::Value =
            serde_json::from_str(&capture.stdout).map_err(|error| error.to_string())?;
        let runs = parsed
            .as_array()
            .ok_or("reconcile output was not an array")?;
        if let Some(run) = runs.first() {
            run_id = run["run_id"].as_str().unwrap_or_default().to_owned();
            if let Some(upstream_results) = run["upstream_results"].as_array() {
                upstream_count = upstream_results.len();
                let ids = upstream_results
                    .iter()
                    .filter_map(|entry| entry["upstream_id"].as_str().map(str::to_owned))
                    .collect::<Vec<_>>();
                let mut sorted = ids.clone();
                sorted.sort();
                ordered = ids == sorted;
            }
        }
    }

    let stale_after: serde_json::Value =
        serde_json::from_slice(&fs::read(&stale_run_path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let stale_superseded = stale_after["status"] == "superseded"
        && stale_after["superseded_by"]
            .as_str()
            .map(|value| !value.is_empty())
            .unwrap_or(false);

    let transient_clean = !in_progress.exists() && !lock_dir.exists();

    report.assertions.push(if capture.success() {
        ProofAssertion::pass(
            "p03.reconcile.completed",
            Some("reconcile command completed".to_owned()),
        )
    } else {
        ProofAssertion::fail("p03.reconcile.completed", capture.summary())
    });
    report
        .assertions
        .push(if !run_id.is_empty() && upstream_count == 3 {
            ProofAssertion::pass(
                "p03.single_run_contains_upstreams",
                Some("single run captured expected upstream set".to_owned()),
            )
        } else {
            ProofAssertion::fail(
                "p03.single_run_contains_upstreams",
                format!("run_id={run_id} upstream_count={upstream_count}"),
            )
        });
    report.assertions.push(if ordered {
        ProofAssertion::pass(
            "p03.deterministic_upstream_order",
            Some("upstream ordering is sorted and deterministic".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p03.deterministic_upstream_order",
            "upstream ordering was not deterministic",
        )
    });
    report.assertions.push(if stale_superseded {
        ProofAssertion::pass(
            "p03.stale_run_superseded",
            Some("stale run record was superseded".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p03.stale_run_superseded",
            "stale run record did not transition to superseded",
        )
    });
    report.assertions.push(if transient_clean {
        ProofAssertion::pass(
            "p03.transient_markers_cleaned",
            Some("lock and in-progress marker were cleaned".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p03.transient_markers_cleaned",
            format!(
                "lock_exists={} in_progress_exists={}",
                lock_dir.exists(),
                in_progress.exists()
            ),
        )
    });

    report.details = json!({
        "run_id": run_id,
        "upstream_count": upstream_count,
        "ordered": ordered,
        "stale_superseded": stale_superseded,
        "transient_clean": transient_clean,
    });

    Ok(report)
}
