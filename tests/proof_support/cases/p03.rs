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
            "p03.mixed_terminal_outcomes",
            "p03.deterministic_upstream_order",
            "p03.persisted_terminal_record_shape",
            "p03.stale_run_superseded",
            "p03.transient_markers_cleaned",
        ],
        required_artifacts: STANDARD_CASE_ARTIFACTS,
        pass_criteria: &[
            "one run id captures all upstream outcomes",
            "mixed terminal upstream outcomes are recorded explicitly under one run",
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

    let external = lab
        .case_root("P03")
        .map_err(|error| error.to_string())?
        .join("external-beta");
    if external.exists() {
        fs::remove_dir_all(&external).map_err(|error| error.to_string())?;
    }
    lab.run_git_expect_success(
        &[
            "clone".to_owned(),
            lab.upstream_beta.display().to_string(),
            external.display().to_string(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            external.display().to_string(),
            "config".to_owned(),
            "user.name".to_owned(),
            "Git Relay Proof".to_owned(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            external.display().to_string(),
            "config".to_owned(),
            "user.email".to_owned(),
            "git-relay-proof@example.com".to_owned(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    lab.commit_file(
        &external,
        "README.md",
        "p03 external beta drift\n",
        "p03 external beta drift",
    )
    .map_err(|error| error.to_string())?;
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            external.display().to_string(),
            "push".to_owned(),
            "origin".to_owned(),
            "HEAD:refs/heads/main".to_owned(),
        ],
        None,
        &[],
    )
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
    let mut mixed_terminal_outcomes = false;
    let mut repo_safety = String::new();
    let mut upstream_states = Vec::new();
    if capture.success() {
        let parsed: serde_json::Value =
            serde_json::from_str(&capture.stdout).map_err(|error| error.to_string())?;
        let runs = parsed
            .as_array()
            .ok_or("reconcile output was not an array")?;
        if let Some(run) = runs.first() {
            run_id = run["run_id"].as_str().unwrap_or_default().to_owned();
            repo_safety = run["repo_safety"].as_str().unwrap_or_default().to_owned();
            if let Some(upstream_results) = run["upstream_results"].as_array() {
                upstream_count = upstream_results.len();
                let ids = upstream_results
                    .iter()
                    .filter_map(|entry| entry["upstream_id"].as_str().map(str::to_owned))
                    .collect::<Vec<_>>();
                let mut sorted = ids.clone();
                sorted.sort();
                ordered = ids == sorted;
                upstream_states = upstream_results
                    .iter()
                    .filter_map(|entry| {
                        Some(json!({
                            "upstream_id": entry["upstream_id"].as_str()?,
                            "state": entry["state"].as_str()?,
                            "divergent": entry["divergent"].as_bool().unwrap_or(false),
                            "detail": entry["detail"].as_str(),
                        }))
                    })
                    .collect();
                mixed_terminal_outcomes =
                    upstream_results.iter().any(|entry| {
                        entry["upstream_id"] == "alpha" && entry["state"] == "in_sync"
                    }) && upstream_results.iter().any(|entry| {
                        entry["upstream_id"] == "beta" && entry["state"] == "out_of_sync"
                    }) && upstream_results.iter().any(|entry| {
                        entry["upstream_id"] == "gamma" && entry["state"] == "stalled"
                    });
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
    let persisted_run_path = run_dir.join(format!("{run_id}.json"));
    let persisted_terminal_shape = if !run_id.is_empty() && persisted_run_path.exists() {
        let persisted: serde_json::Value = serde_json::from_slice(
            &fs::read(&persisted_run_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        persisted["run_id"] == run_id
            && persisted["repo_id"] == AUTHORITATIVE_REPO_ID
            && persisted["status"] == "completed"
            && persisted["completed_at_ms"].as_u64().is_some()
            && persisted["superseded_by"].is_null()
            && persisted["desired_snapshot"]
                .as_array()
                .map(|items| !items.is_empty())
                .unwrap_or(false)
            && persisted["captured_upstreams"]
                .as_array()
                .map(|items| items.len() == 3)
                .unwrap_or(false)
            && persisted["upstream_results"]
                .as_array()
                .map(|items| items.len() == 3)
                .unwrap_or(false)
    } else {
        false
    };

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
    report.assertions.push(if mixed_terminal_outcomes {
        ProofAssertion::pass(
            "p03.mixed_terminal_outcomes",
            Some(
                "single reconcile run recorded alpha=in_sync, beta=out_of_sync, and gamma=stalled"
                    .to_owned(),
            ),
        )
    } else {
        ProofAssertion::fail(
            "p03.mixed_terminal_outcomes",
            format!("repo_safety={repo_safety} upstream_states={upstream_states:?}"),
        )
    });
    report.assertions.push(if persisted_terminal_shape {
        ProofAssertion::pass(
            "p03.persisted_terminal_record_shape",
            Some("persisted terminal run record contains completed status, desired snapshot, captured upstreams, and per-upstream outcomes".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p03.persisted_terminal_record_shape",
            format!("persisted run record missing or incomplete at {}", persisted_run_path.display()),
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
        "repo_safety": repo_safety,
        "upstream_states": upstream_states,
        "mixed_terminal_outcomes": mixed_terminal_outcomes,
        "ordered": ordered,
        "persisted_terminal_shape": persisted_terminal_shape,
        "persisted_run_path": persisted_run_path,
        "stale_superseded": stale_superseded,
        "transient_clean": transient_clean,
    });

    Ok(report)
}
