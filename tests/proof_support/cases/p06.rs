use std::fs;

use serde_json::json;

use crate::proof_support::cases::{CaseDefinition, STANDARD_CASE_ARTIFACTS};
use crate::proof_support::lab::{CaseReport, ProofLab, AUTHORITATIVE_REPO_ID};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P06",
        setup: "Configure one reachable upstream and install hook dispatch for authoritative push guardrails.",
        action: "Mutate upstream out-of-band, detect divergence, verify push block, repair, and reconverge.",
        required_assertions: &[
            "p06.baseline.reconcile",
            "p06.divergence.detected",
            "p06.divergence_marker.persisted",
            "p06.hooks.installed",
            "p06.push.blocked",
            "p06.repair.reconciled",
        ],
        required_artifacts: STANDARD_CASE_ARTIFACTS,
        pass_criteria: &[
            "divergence is detected from fresh upstream observation",
            "divergence marker persists while repository remains divergent",
            "authoritative writes are blocked while divergent",
            "repair plus reconcile restores healthy state",
        ],
        fail_criteria: &[
            "writes accepted while divergent",
            "divergence inferred only from stale cached state",
        ],
        contract_refs: &[
            "RFC_PROOF_E2E_TEST.md#P06",
            "git-relay-rfc.md divergence safety contract",
            "verification-plan result C",
        ],
        runner: run,
    }
}

fn run(lab: &mut ProofLab, _mode: ProofMode) -> Result<CaseReport, String> {
    let mut report = CaseReport::with_details(json!({}));

    lab.write_authoritative_descriptor_with_write_upstreams(&[(
        "alpha",
        &lab.upstream_alpha,
        false,
    )])
    .map_err(|error| error.to_string())?;

    let baseline = lab
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

    let external = lab
        .case_root("P06")
        .map_err(|error| error.to_string())?
        .join("external-alpha");
    if external.exists() {
        fs::remove_dir_all(&external).map_err(|error| error.to_string())?;
    }
    lab.run_git_expect_success(
        &[
            "clone".to_owned(),
            lab.upstream_alpha.display().to_string(),
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
        "external drift\n",
        "external drift mutation",
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

    let divergent = lab
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

    let mut divergence_detected = false;
    if divergent.success() {
        let parsed: serde_json::Value =
            serde_json::from_str(&divergent.stdout).map_err(|error| error.to_string())?;
        if let Some(run) = parsed.as_array().and_then(|runs| runs.first()) {
            divergence_detected = run["repo_safety"] == "divergent"
                && run["upstream_results"]
                    .as_array()
                    .map(|results| {
                        results.iter().any(|entry| {
                            entry["upstream_id"] == "alpha"
                                && entry["divergent"] == true
                                && entry["state"] == "out_of_sync"
                        })
                    })
                    .unwrap_or(false);
        }
    }

    let install = lab
        .run_git_relay_install_hooks(
            &[
                "--repo".to_owned(),
                lab.authoritative_repo.display().to_string(),
                "--dispatcher".to_owned(),
                lab.binaries.git_relay.display().to_string(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;
    let divergence_marker_before_repair = lab
        .git_ref_exists(
            &lab.authoritative_repo,
            "refs/git-relay/safety/divergent/alpha",
        )
        .map_err(|error| error.to_string())?;

    let blocked_work = lab
        .case_root("P06")
        .map_err(|error| error.to_string())?
        .join("blocked-work");
    lab.init_work_repo(&blocked_work)
        .map_err(|error| error.to_string())?;
    lab.commit_file(
        &blocked_work,
        "README.md",
        "blocked while divergent\n",
        "blocked push",
    )
    .map_err(|error| error.to_string())?;

    let blocked_push = lab
        .run_git(
            &[
                "-C".to_owned(),
                blocked_work.display().to_string(),
                "push".to_owned(),
                lab.authoritative_repo.display().to_string(),
                "HEAD:refs/heads/main".to_owned(),
            ],
            None,
            &[
                ("GIT_RELAY_REQUEST_ID".to_owned(), "request-p06".to_owned()),
                ("GIT_RELAY_PUSH_ID".to_owned(), "push-p06".to_owned()),
            ],
        )
        .map_err(|error| error.to_string())?;
    let blocked_while_divergent = !blocked_push.success();

    let local_main = lab
        .read_git_ref(&lab.authoritative_repo, "refs/heads/main")
        .map_err(|error| error.to_string())?;
    lab.run_git_expect_success(
        &[
            format!("--git-dir={}", lab.upstream_alpha.display()),
            "update-ref".to_owned(),
            "refs/heads/main".to_owned(),
            local_main,
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;

    let repaired = lab
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

    let divergence_marker = lab
        .git_ref_exists(
            &lab.authoritative_repo,
            "refs/git-relay/safety/divergent/alpha",
        )
        .map_err(|error| error.to_string())?;
    let repaired_healthy = if repaired.success() {
        let parsed: serde_json::Value =
            serde_json::from_str(&repaired.stdout).map_err(|error| error.to_string())?;
        parsed
            .as_array()
            .and_then(|runs| runs.first())
            .map(|run| run["repo_safety"] == "healthy")
            .unwrap_or(false)
    } else {
        false
    };

    report.assertions.push(if baseline.success() {
        ProofAssertion::pass(
            "p06.baseline.reconcile",
            Some("baseline reconcile completed".to_owned()),
        )
    } else {
        ProofAssertion::fail("p06.baseline.reconcile", baseline.summary())
    });
    report.assertions.push(if divergence_detected {
        ProofAssertion::pass(
            "p06.divergence.detected",
            Some("fresh observation marked upstream as divergent".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p06.divergence.detected",
            "divergence state was not detected after upstream drift",
        )
    });
    report.assertions.push(if install.success() {
        ProofAssertion::pass(
            "p06.hooks.installed",
            Some("hook wrappers installed for authoritative repo".to_owned()),
        )
    } else {
        ProofAssertion::fail("p06.hooks.installed", install.summary())
    });
    report.assertions.push(if divergence_marker_before_repair {
        ProofAssertion::pass(
            "p06.divergence_marker.persisted",
            Some("divergence marker existed before repair cleared it".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p06.divergence_marker.persisted",
            "divergence marker was missing before repair",
        )
    });
    report.assertions.push(if blocked_while_divergent {
        ProofAssertion::pass(
            "p06.push.blocked",
            Some("push was rejected while repository was divergent".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p06.push.blocked",
            "push unexpectedly succeeded while divergent",
        )
    });
    report
        .assertions
        .push(if repaired_healthy && !divergence_marker {
            ProofAssertion::pass(
                "p06.repair.reconciled",
                Some("repair path restored healthy state and cleared divergence marker".to_owned()),
            )
        } else {
            ProofAssertion::fail(
                "p06.repair.reconciled",
                format!(
                    "repaired_healthy={} divergence_marker={}",
                    repaired_healthy, divergence_marker
                ),
            )
        });

    report.details = json!({
        "divergence_detected": divergence_detected,
        "divergence_marker_before_repair": divergence_marker_before_repair,
        "blocked_while_divergent": blocked_while_divergent,
        "repaired_healthy": repaired_healthy,
        "divergence_marker_remaining": divergence_marker,
    });

    Ok(report)
}
