use std::fs;
use std::path::Path;

use serde_json::json;

use crate::proof_support::cases::{CaseDefinition, STANDARD_CASE_ARTIFACTS};
use crate::proof_support::lab::{CaseReport, ProofLab, AUTHORITATIVE_REPO_ID};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P05",
        setup: "Start with a reachable non-atomic upstream whose update hook rejects one ref from a multi-ref apply.",
        action: "Run reconcile once to produce a real partial non-atomic apply, then remove the rejection, advance local state, and run reconcile again.",
        required_assertions: &[
            "p05.first_run.executed",
            "p05.no_optimistic_observed_ref",
            "p05.recomputed_from_current_local_refs",
            "p05.second_run.converged",
            "p05.observed_matches_local",
        ],
        required_artifacts: STANDARD_CASE_ARTIFACTS,
        pass_criteria: &[
            "first run records a partial non-atomic apply from a real upstream rejection",
            "observed refs reflect explicit post-apply observation rather than optimistic desired state",
            "later run recomputes from current local refs and converges without replay history",
        ],
        fail_criteria: &[
            "observed refs claim the rejected ref before upstream accepted it",
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
    let observed_main_ref = format!("refs/git-relay/upstreams/{upstream_id}/heads/main");
    let observed_allowed_ref = format!("refs/git-relay/upstreams/{upstream_id}/heads/p05-allowed");
    let observed_blocked_ref = format!("refs/git-relay/upstreams/{upstream_id}/heads/p05-blocked");

    lab.write_authoritative_descriptor_with_write_upstreams(&[(
        upstream_id,
        &lab.upstream_alpha,
        false,
    )])
    .map_err(|error| error.to_string())?;

    let case_root = lab.case_root("P05").map_err(|error| error.to_string())?;
    let local_refs = case_root.join("local-authoritative-refs");
    if local_refs.exists() {
        fs::remove_dir_all(&local_refs).map_err(|error| error.to_string())?;
    }
    clone_and_configure(lab, &lab.authoritative_repo, &local_refs)?;
    lab.commit_file(
        &local_refs,
        "allowed.txt",
        "p05 allowed branch\n",
        "p05 allowed branch",
    )
    .map_err(|error| error.to_string())?;
    push_worktree_ref(lab, &local_refs, "HEAD:refs/heads/p05-allowed")?;
    lab.commit_file(
        &local_refs,
        "blocked.txt",
        "p05 blocked branch\n",
        "p05 blocked branch",
    )
    .map_err(|error| error.to_string())?;
    push_worktree_ref(lab, &local_refs, "HEAD:refs/heads/p05-blocked")?;

    install_partial_reject_update_hook(&lab.upstream_alpha, "refs/heads/p05-blocked")?;

    let first = run_reconcile(lab)?;
    let first_desired_oid = desired_main_oid(&first.stdout)?;
    let first_result = upstream_result(&first.stdout, upstream_id)?;

    let upstream_allowed_exists = lab
        .git_ref_exists(&lab.upstream_alpha, "refs/heads/p05-allowed")
        .map_err(|error| error.to_string())?;
    let upstream_blocked_exists = lab
        .git_ref_exists(&lab.upstream_alpha, "refs/heads/p05-blocked")
        .map_err(|error| error.to_string())?;
    let observed_allowed_exists = lab
        .git_ref_exists(&lab.authoritative_repo, &observed_allowed_ref)
        .map_err(|error| error.to_string())?;
    let observed_blocked_exists = lab
        .git_ref_exists(&lab.authoritative_repo, &observed_blocked_ref)
        .map_err(|error| error.to_string())?;
    let observed_allowed_matches_upstream = ref_values_match(
        lab,
        &lab.authoritative_repo,
        &observed_allowed_ref,
        &lab.upstream_alpha,
        "refs/heads/p05-allowed",
    )?;
    let partial_non_atomic_apply = first.success()
        && first_result["state"] == "out_of_sync"
        && first_result["apply_attempted"] == true
        && upstream_allowed_exists
        && !upstream_blocked_exists
        && observed_allowed_exists
        && observed_allowed_matches_upstream
        && !observed_blocked_exists;

    let local_update = case_root.join("local-authoritative-update");
    if local_update.exists() {
        fs::remove_dir_all(&local_update).map_err(|error| error.to_string())?;
    }
    clone_and_configure(lab, &lab.authoritative_repo, &local_update)?;
    lab.commit_file(
        &local_update,
        "README.md",
        "p05 updated local authoritative main\n",
        "p05 local authoritative advance",
    )
    .map_err(|error| error.to_string())?;
    push_worktree_ref(lab, &local_update, "HEAD:refs/heads/main")?;
    let new_local_main = lab
        .read_git_ref(&lab.authoritative_repo, "refs/heads/main")
        .map_err(|error| error.to_string())?;

    remove_partial_reject_update_hook(&lab.upstream_alpha)?;

    let second = run_reconcile(lab)?;
    let second_desired_oid = desired_main_oid(&second.stdout)?;
    let second_result = upstream_result(&second.stdout, upstream_id)?;
    let second_converged = second.success() && second_result["state"] == "in_sync";

    let observed_after_second = lab
        .git_ref_exists(&lab.authoritative_repo, &observed_main_ref)
        .map_err(|error| error.to_string())?;
    let observed_matches_local = observed_after_second
        && ref_values_match(
            lab,
            &lab.authoritative_repo,
            "refs/heads/main",
            &lab.authoritative_repo,
            &observed_main_ref,
        )?
        && ref_values_match(
            lab,
            &lab.authoritative_repo,
            "refs/heads/p05-allowed",
            &lab.authoritative_repo,
            &observed_allowed_ref,
        )?
        && ref_values_match(
            lab,
            &lab.authoritative_repo,
            "refs/heads/p05-blocked",
            &lab.authoritative_repo,
            &observed_blocked_ref,
        )?;
    let upstream_matches_local = second_converged
        && ref_values_match(
            lab,
            &lab.authoritative_repo,
            "refs/heads/main",
            &lab.upstream_alpha,
            "refs/heads/main",
        )?
        && ref_values_match(
            lab,
            &lab.authoritative_repo,
            "refs/heads/p05-allowed",
            &lab.upstream_alpha,
            "refs/heads/p05-allowed",
        )?
        && ref_values_match(
            lab,
            &lab.authoritative_repo,
            "refs/heads/p05-blocked",
            &lab.upstream_alpha,
            "refs/heads/p05-blocked",
        )?;
    let replay_independent_recompute = first_desired_oid != second_desired_oid
        && second_desired_oid == new_local_main
        && upstream_matches_local;

    report.assertions.push(if first.success() {
        ProofAssertion::pass(
            "p05.first_run.executed",
            Some("first reconcile run completed after partial non-atomic apply".to_owned()),
        )
    } else {
        ProofAssertion::fail("p05.first_run.executed", first.summary())
    });
    report.assertions.push(if partial_non_atomic_apply {
        ProofAssertion::pass(
            "p05.no_optimistic_observed_ref",
            Some("observed refs matched the explicitly observed partial upstream state and did not claim the rejected ref".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p05.no_optimistic_observed_ref",
            format!(
                "partial_apply={partial_non_atomic_apply} upstream_allowed={upstream_allowed_exists} upstream_blocked={upstream_blocked_exists} observed_allowed={observed_allowed_exists} observed_blocked={observed_blocked_exists} observed_allowed_matches_upstream={observed_allowed_matches_upstream}"
            ),
        )
    });
    report.assertions.push(if replay_independent_recompute {
        ProofAssertion::pass(
            "p05.recomputed_from_current_local_refs",
            Some(
                "second run recomputed desired state from current local refs and converged newer local main without replay history"
                    .to_owned(),
            ),
        )
    } else {
        ProofAssertion::fail(
            "p05.recomputed_from_current_local_refs",
            format!(
                "first_desired_oid={first_desired_oid} second_desired_oid={second_desired_oid} new_local_main={new_local_main} upstream_matches_local={upstream_matches_local}"
            ),
        )
    });
    report
        .assertions
        .push(if second.success() && second_converged {
            ProofAssertion::pass(
                "p05.second_run.converged",
                Some("second run converged after partial apply recovery".to_owned()),
            )
        } else {
            ProofAssertion::fail(
                "p05.second_run.converged",
                format!(
                    "second_success={} second_result={}",
                    second.success(),
                    second_result
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
        "observed_main_ref": observed_main_ref,
        "observed_allowed_ref": observed_allowed_ref,
        "observed_blocked_ref": observed_blocked_ref,
        "first_desired_oid": first_desired_oid,
        "second_desired_oid": second_desired_oid,
        "new_local_main": new_local_main,
        "first_result": first_result,
        "second_result": second_result,
        "partial_non_atomic_apply": partial_non_atomic_apply,
        "upstream_allowed_exists": upstream_allowed_exists,
        "upstream_blocked_exists": upstream_blocked_exists,
        "observed_allowed_exists": observed_allowed_exists,
        "observed_blocked_exists": observed_blocked_exists,
        "observed_allowed_matches_upstream": observed_allowed_matches_upstream,
        "observed_after_second": observed_after_second,
        "replay_independent_recompute": replay_independent_recompute,
        "second_converged": second_converged,
        "upstream_matches_local": upstream_matches_local,
        "observed_matches_local": observed_matches_local,
    });

    Ok(report)
}

fn clone_and_configure(lab: &ProofLab, source: &Path, worktree: &Path) -> Result<(), String> {
    lab.run_git_expect_success(
        &[
            "clone".to_owned(),
            source.display().to_string(),
            worktree.display().to_string(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            worktree.display().to_string(),
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
            worktree.display().to_string(),
            "config".to_owned(),
            "user.email".to_owned(),
            "git-relay-proof@example.com".to_owned(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn push_worktree_ref(lab: &ProofLab, worktree: &Path, refspec: &str) -> Result<(), String> {
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            worktree.display().to_string(),
            "push".to_owned(),
            "origin".to_owned(),
            refspec.to_owned(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn run_reconcile(lab: &ProofLab) -> Result<crate::proof_support::cmd::CommandCapture, String> {
    lab.run_git_relay(
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
    .map_err(|error| error.to_string())
}

fn install_partial_reject_update_hook(repo: &Path, rejected_ref: &str) -> Result<(), String> {
    let hook = repo.join("hooks").join("update");
    let source = format!(
        "#!/bin/sh\nset -eu\nif [ \"$1\" = '{}' ]; then\n  echo 'p05 rejecting partial non-atomic ref' >&2\n  exit 1\nfi\nexit 0\n",
        rejected_ref.replace('\'', "'\"'\"'")
    );
    fs::write(&hook, source).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&hook)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn remove_partial_reject_update_hook(repo: &Path) -> Result<(), String> {
    let hook = repo.join("hooks").join("update");
    if hook.exists() {
        fs::remove_file(&hook).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn ref_values_match(
    lab: &ProofLab,
    left_repo: &Path,
    left_ref: &str,
    right_repo: &Path,
    right_ref: &str,
) -> Result<bool, String> {
    let left = lab
        .read_git_ref(left_repo, left_ref)
        .map_err(|error| error.to_string())?;
    let right = lab
        .read_git_ref(right_repo, right_ref)
        .map_err(|error| error.to_string())?;
    Ok(left == right)
}

fn upstream_result(source: &str, upstream_id: &str) -> Result<serde_json::Value, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(source).map_err(|error| error.to_string())?;
    parsed
        .as_array()
        .and_then(|runs| runs.first())
        .and_then(|run| run["upstream_results"].as_array())
        .and_then(|results| {
            results
                .iter()
                .find(|entry| entry["upstream_id"] == upstream_id)
                .cloned()
        })
        .ok_or_else(|| format!("upstream result {upstream_id} missing from reconcile output"))
}

fn desired_main_oid(source: &str) -> Result<String, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(source).map_err(|error| error.to_string())?;
    parsed
        .as_array()
        .and_then(|runs| runs.first())
        .and_then(|run| run["desired_snapshot"].as_array())
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["ref_name"] == "refs/heads/main")
        })
        .and_then(|entry| entry["oid"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| "desired main oid missing from reconcile output".to_owned())
}
