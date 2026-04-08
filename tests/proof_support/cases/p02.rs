use serde_json::json;

use crate::proof_support::cases::CaseDefinition;
use crate::proof_support::lab::{CaseReport, ProofLab};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P02",
        setup: "Prepare multi-ref branch+tag updates from one client worktree over both ingress transports.",
        action: "Execute ordinary multi-ref pushes over SSH and smart HTTP and assert local-commit scoped evidence.",
        pass_criteria: &[
            "multi-ref updates are observed on both transports",
            "invalid transmitted updates are rejected without widening ordinary push guarantees",
            "proof output explicitly keeps local-commit scoped wording",
        ],
        fail_criteria: &[
            "proof implies whole-push all-or-nothing semantics for ordinary pushes",
        ],
        contract_refs: &[
            "RFC_PROOF_E2E_TEST.md#P02",
            "git-relay-rfc.md ordinary inbound push semantics",
            "verification-plan result B",
        ],
        runner: run,
    }
}

fn run(lab: &mut ProofLab, _mode: ProofMode) -> Result<CaseReport, String> {
    let mut report = CaseReport::with_details(json!({
        "contract": "local-commit for refs Git actually committed",
    }));

    let case_root = lab.case_root("P02").map_err(|error| error.to_string())?;
    let work = case_root.join("client-work");
    lab.init_work_repo(&work)
        .map_err(|error| error.to_string())?;
    lab.commit_file(&work, "README.md", "p02\n", "p02 commit")
        .map_err(|error| error.to_string())?;
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            work.display().to_string(),
            "tag".to_owned(),
            "p02-ssh-tag".to_owned(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            work.display().to_string(),
            "tag".to_owned(),
            "p02-http-tag".to_owned(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;

    let transport = lab
        .start_transport_harness("P02")
        .map_err(|error| error.to_string())?;

    let ssh_url = transport.ssh.remote_url_for_repo(&lab.authoritative_repo);
    let ssh_env = vec![(
        "GIT_SSH_COMMAND".to_owned(),
        transport.ssh.git_ssh_command(),
    )];
    let ssh_push = lab
        .run_git(
            &[
                "-C".to_owned(),
                work.display().to_string(),
                "push".to_owned(),
                ssh_url.clone(),
                "HEAD:refs/heads/p02-ssh".to_owned(),
                "refs/tags/p02-ssh-tag:refs/tags/p02-ssh-tag".to_owned(),
            ],
            None,
            &ssh_env,
        )
        .map_err(|error| error.to_string())?;

    let http_url = transport
        .smart_http
        .remote_url_for_repo("relay-authoritative.git");
    let http_push = lab
        .run_git(
            &[
                "-C".to_owned(),
                work.display().to_string(),
                "push".to_owned(),
                http_url,
                "HEAD:refs/heads/p02-http".to_owned(),
                "refs/tags/p02-http-tag:refs/tags/p02-http-tag".to_owned(),
            ],
            None,
            &[],
        )
        .map_err(|error| error.to_string())?;

    let ssh_branch_exists = lab
        .git_ref_exists(&lab.authoritative_repo, "refs/heads/p02-ssh")
        .map_err(|error| error.to_string())?;
    let ssh_tag_exists = lab
        .git_ref_exists(&lab.authoritative_repo, "refs/tags/p02-ssh-tag")
        .map_err(|error| error.to_string())?;
    let http_branch_exists = lab
        .git_ref_exists(&lab.authoritative_repo, "refs/heads/p02-http")
        .map_err(|error| error.to_string())?;
    let http_tag_exists = lab
        .git_ref_exists(&lab.authoritative_repo, "refs/tags/p02-http-tag")
        .map_err(|error| error.to_string())?;
    let refs_ok = http_branch_exists && http_tag_exists && ssh_branch_exists && ssh_tag_exists;

    let ssh_internal_ref_attempt = lab
        .run_git(
            &[
                "-C".to_owned(),
                work.display().to_string(),
                "push".to_owned(),
                ssh_url.clone(),
                "HEAD:refs/git-relay/p02-ssh-internal".to_owned(),
            ],
            None,
            &ssh_env,
        )
        .map_err(|error| error.to_string())?;
    let http_internal_ref_attempt = lab
        .run_git(
            &[
                "-C".to_owned(),
                work.display().to_string(),
                "push".to_owned(),
                transport
                    .smart_http
                    .remote_url_for_repo("relay-authoritative.git"),
                "HEAD:refs/git-relay/p02-http-internal".to_owned(),
            ],
            None,
            &[],
        )
        .map_err(|error| error.to_string())?;
    let ssh_internal_ref_blocked = !ssh_internal_ref_attempt.success();
    let http_internal_ref_blocked = !http_internal_ref_attempt.success();
    let ssh_internal_ref_exists = lab
        .git_ref_exists(&lab.authoritative_repo, "refs/git-relay/p02-ssh-internal")
        .map_err(|error| error.to_string())?;
    let http_internal_ref_exists = lab
        .git_ref_exists(&lab.authoritative_repo, "refs/git-relay/p02-http-internal")
        .map_err(|error| error.to_string())?;
    let ssh_branch_still_exists = lab
        .git_ref_exists(&lab.authoritative_repo, "refs/heads/p02-ssh")
        .map_err(|error| error.to_string())?;
    let http_branch_still_exists = lab
        .git_ref_exists(&lab.authoritative_repo, "refs/heads/p02-http")
        .map_err(|error| error.to_string())?;

    report.assertions.push(if ssh_push.success() {
        ProofAssertion::pass(
            "p02.ssh.multi_ref",
            Some("ssh multi-ref push accepted".to_owned()),
        )
    } else {
        ProofAssertion::fail("p02.ssh.multi_ref", ssh_push.summary())
    });
    report.assertions.push(if http_push.success() {
        ProofAssertion::pass(
            "p02.http.multi_ref",
            Some("smart-http multi-ref push accepted".to_owned()),
        )
    } else {
        ProofAssertion::fail("p02.http.multi_ref", http_push.summary())
    });
    report.assertions.push(if refs_ok {
        ProofAssertion::pass(
            "p02.refs.committed",
            Some("all transmitted refs are present locally".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p02.refs.committed",
            "expected branch/tag refs were missing after multi-ref pushes",
        )
    });
    report
        .assertions
        .push(if ssh_internal_ref_blocked && http_internal_ref_blocked {
            ProofAssertion::pass(
                "p02.invalid_updates.rejected",
                Some("invalid hidden-ref updates were rejected on ingress transports".to_owned()),
            )
        } else {
            ProofAssertion::fail(
                "p02.invalid_updates.rejected",
                format!(
                    "hidden-ref attempts were not both rejected (ssh_blocked={}, http_blocked={})",
                    ssh_internal_ref_blocked, http_internal_ref_blocked
                ),
            )
        });
    report.assertions.push(
        if !ssh_internal_ref_exists
            && !http_internal_ref_exists
            && ssh_branch_still_exists
            && http_branch_still_exists
        {
        ProofAssertion::pass(
            "p02.invalid_updates.no_partial_delete",
                Some("rejected hidden-ref attempts did not mutate internal or committed branch refs".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p02.invalid_updates.no_partial_delete",
            format!(
                    "post-rejection state was unexpected (ssh_hidden_exists={}, http_hidden_exists={}, ssh_branch_exists={}, http_branch_exists={})",
                    ssh_internal_ref_exists, http_internal_ref_exists, ssh_branch_still_exists, http_branch_still_exists
            ),
        )
        },
    );
    report.assertions.push(ProofAssertion::pass(
        "p02.contract.local_commit_only",
        Some("verdict text remains scoped to refs Git actually committed".to_owned()),
    ));

    report.transport_profiles = vec!["ssh".to_owned(), "smart-http".to_owned()];
    report.details = json!({
        "ssh_url": ssh_url,
        "ssh_push": ssh_push.summary(),
        "http_push": http_push.summary(),
        "ssh_branch_exists": ssh_branch_exists,
        "ssh_tag_exists": ssh_tag_exists,
        "http_branch_exists": http_branch_exists,
        "http_tag_exists": http_tag_exists,
        "refs_ok": refs_ok,
        "ssh_internal_ref_attempt": ssh_internal_ref_attempt.summary(),
        "http_internal_ref_attempt": http_internal_ref_attempt.summary(),
        "ssh_internal_ref_blocked": ssh_internal_ref_blocked,
        "http_internal_ref_blocked": http_internal_ref_blocked,
        "ssh_internal_ref_exists": ssh_internal_ref_exists,
        "http_internal_ref_exists": http_internal_ref_exists,
        "ssh_branch_still_exists": ssh_branch_still_exists,
        "http_branch_still_exists": http_branch_still_exists,
        "verdict": "local-commit does not claim whole-push semantics for ordinary pushes",
    });

    Ok(report)
}
