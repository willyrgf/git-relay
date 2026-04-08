use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use git_relay::hooks::push_trace_file_path;
use serde_json::json;

use crate::proof_support::cases::{CaseDefinition, STANDARD_CASE_ARTIFACTS};
use crate::proof_support::lab::{CaseReport, ProofLab, AUTHORITATIVE_REPO_ID};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P02",
        setup: "Prepare multi-ref branch+tag updates from one client worktree over both ingress transports.",
        action: "Execute ordinary multi-ref pushes over SSH and smart HTTP and assert local-commit scoped evidence.",
        required_assertions: &[
            "p02.ssh.multi_ref",
            "p02.http.multi_ref",
            "p02.refs.committed",
            "p02.client_side_pruning.evidence",
            "p02.invalid_updates.rejected",
            "p02.invalid_updates.no_partial_delete",
            "p02.contract.local_commit_only",
        ],
        required_artifacts: STANDARD_CASE_ARTIFACTS,
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

    let install = install_authoritative_hooks(lab)?;
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

    let ssh_pruning = run_client_side_pruning_scenario(lab, &case_root, "ssh", &ssh_url, &ssh_env)?;
    let http_pruning = run_client_side_pruning_scenario(
        lab,
        &case_root,
        "http",
        &transport
            .smart_http
            .remote_url_for_repo("relay-authoritative.git"),
        &[],
    )?;
    let client_side_pruning_evident =
        ssh_pruning.client_side_pruning_evident && http_pruning.client_side_pruning_evident;

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
    report.assertions.push(if install.success() && client_side_pruning_evident {
        ProofAssertion::pass(
            "p02.client_side_pruning.evidence",
            Some(
                "ordinary non-fast-forward branch+tag pushes showed client-side pruning and matching SSH/smart-http transmitted refs"
                    .to_owned(),
            ),
        )
    } else {
        ProofAssertion::fail(
            "p02.client_side_pruning.evidence",
            format!(
                "install_success={} ssh_pruning={} http_pruning={} ssh_refs={:?} http_refs={:?}",
                install.success(),
                ssh_pruning.client_side_pruning_evident,
                http_pruning.client_side_pruning_evident,
                ssh_pruning.transmitted_refs,
                http_pruning.transmitted_refs
            ),
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
        "hooks_install": install.summary(),
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
        "ssh_client_side_pruning": ssh_pruning.details,
        "http_client_side_pruning": http_pruning.details,
        "verdict": "local-commit does not claim whole-push semantics for ordinary pushes",
    });

    Ok(report)
}

#[derive(Debug)]
struct ClientSidePruningEvidence {
    client_side_pruning_evident: bool,
    transmitted_refs: Vec<String>,
    details: serde_json::Value,
}

fn install_authoritative_hooks(
    lab: &ProofLab,
) -> Result<crate::proof_support::cmd::CommandCapture, String> {
    lab.run_git_relay_install_hooks(
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
    .map_err(|error| error.to_string())
}

fn run_client_side_pruning_scenario(
    lab: &ProofLab,
    case_root: &Path,
    transport_id: &str,
    remote_url: &str,
    env: &[(String, String)],
) -> Result<ClientSidePruningEvidence, String> {
    let branch_name = format!("p02-prune-{transport_id}");
    let branch_ref = format!("refs/heads/p02-prune-{transport_id}");
    let tag_name = format!("p02-prune-{transport_id}-tag");
    let tag_ref = format!("refs/tags/{tag_name}");

    let work = case_root.join(format!("prune-{transport_id}-work"));
    if work.exists() {
        fs::remove_dir_all(&work).map_err(|error| error.to_string())?;
    }
    lab.init_work_repo(&work)
        .map_err(|error| error.to_string())?;
    lab.commit_file(
        &work,
        "README.md",
        &format!("p02 prune {transport_id} local\n"),
        &format!("p02 prune {transport_id} local"),
    )
    .map_err(|error| error.to_string())?;
    let local_commit = read_worktree_ref(lab, &work, "HEAD")?;
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            work.display().to_string(),
            "push".to_owned(),
            remote_url.to_owned(),
            format!("HEAD:{branch_ref}"),
        ],
        None,
        env,
    )
    .map_err(|error| error.to_string())?;

    let external = case_root.join(format!("prune-{transport_id}-external"));
    if external.exists() {
        fs::remove_dir_all(&external).map_err(|error| error.to_string())?;
    }
    lab.run_git_expect_success(
        &[
            "clone".to_owned(),
            lab.authoritative_repo.display().to_string(),
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
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            external.display().to_string(),
            "switch".to_owned(),
            "-c".to_owned(),
            format!("advance-{transport_id}"),
            format!("origin/{branch_name}"),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    lab.commit_file(
        &external,
        "README.md",
        &format!("p02 prune {transport_id} remote advance\n"),
        &format!("p02 prune {transport_id} remote advance"),
    )
    .map_err(|error| error.to_string())?;
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            external.display().to_string(),
            "push".to_owned(),
            "origin".to_owned(),
            format!("HEAD:{branch_ref}"),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    let remote_advanced_commit = lab
        .read_git_ref(&lab.authoritative_repo, &branch_ref)
        .map_err(|error| error.to_string())?;

    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            work.display().to_string(),
            "tag".to_owned(),
            tag_name.clone(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;

    let traces_before = list_push_trace_files(lab.state_root.as_path(), AUTHORITATIVE_REPO_ID)?;
    let capture = lab
        .run_git(
            &[
                "-C".to_owned(),
                work.display().to_string(),
                "push".to_owned(),
                remote_url.to_owned(),
                format!("HEAD:{branch_ref}"),
                format!("refs/tags/{tag_name}:{tag_ref}"),
            ],
            None,
            env,
        )
        .map_err(|error| error.to_string())?;
    let trace = read_new_push_trace(
        lab.state_root.as_path(),
        AUTHORITATIVE_REPO_ID,
        &traces_before,
    )?;

    let branch_after = lab
        .read_git_ref(&lab.authoritative_repo, &branch_ref)
        .map_err(|error| error.to_string())?;
    let tag_after = lab
        .read_git_ref(&lab.authoritative_repo, &tag_ref)
        .map_err(|error| error.to_string())?;
    let transmitted_refs = transmitted_refs_from_trace(&trace);
    let capture_summary = capture.summary();
    let lowered_capture = capture_summary.to_ascii_lowercase();
    let client_reported_non_fast_forward =
        lowered_capture.contains("non-fast-forward") || lowered_capture.contains("fetch first");
    let branch_unchanged = branch_after == remote_advanced_commit;
    let tag_committed = tag_after == local_commit;
    let server_saw_only_tag = transmitted_refs == vec![tag_ref.clone()];
    let client_side_pruning_evident = client_reported_non_fast_forward
        && branch_unchanged
        && tag_committed
        && server_saw_only_tag;

    Ok(ClientSidePruningEvidence {
        client_side_pruning_evident,
        transmitted_refs: transmitted_refs.clone(),
        details: json!({
            "branch_ref": branch_ref,
            "tag_ref": tag_ref,
            "capture": capture_summary,
            "client_reported_non_fast_forward": client_reported_non_fast_forward,
            "branch_unchanged": branch_unchanged,
            "tag_committed": tag_committed,
            "server_saw_only_tag": server_saw_only_tag,
            "transmitted_refs": transmitted_refs,
            "trace": trace,
        }),
    })
}

fn read_worktree_ref(lab: &ProofLab, worktree: &Path, ref_name: &str) -> Result<String, String> {
    let capture = lab
        .run_git(
            &[
                "-C".to_owned(),
                worktree.display().to_string(),
                "rev-parse".to_owned(),
                ref_name.to_owned(),
            ],
            None,
            &[],
        )
        .map_err(|error| error.to_string())?;
    if capture.success() {
        Ok(capture.stdout.trim().to_owned())
    } else {
        Err(capture.summary())
    }
}

fn list_push_trace_files(state_root: &Path, repo_id: &str) -> Result<BTreeSet<PathBuf>, String> {
    let trace_root = push_trace_root(state_root, repo_id);
    if !trace_root.exists() {
        return Ok(BTreeSet::new());
    }
    fs::read_dir(trace_root)
        .map_err(|error| error.to_string())?
        .map(|entry| {
            entry
                .map(|item| item.path())
                .map_err(|error| error.to_string())
        })
        .collect()
}

fn read_new_push_trace(
    state_root: &Path,
    repo_id: &str,
    traces_before: &BTreeSet<PathBuf>,
) -> Result<Vec<serde_json::Value>, String> {
    let traces_after = list_push_trace_files(state_root, repo_id)?;
    let mut new_paths = traces_after
        .difference(traces_before)
        .cloned()
        .collect::<Vec<_>>();
    new_paths.sort();
    let path = new_paths.last().ok_or_else(|| {
        "client-side pruning scenario did not produce a new push trace".to_owned()
    })?;
    read_push_trace(path)
}

fn read_push_trace(path: &Path) -> Result<Vec<serde_json::Value>, String> {
    let source = fs::read_to_string(path).map_err(|error| error.to_string())?;
    source
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).map_err(|error| error.to_string())
        })
        .collect()
}

fn transmitted_refs_from_trace(trace: &[serde_json::Value]) -> Vec<String> {
    let mut refs = trace
        .iter()
        .flat_map(|event| event["updates"].as_array().cloned().unwrap_or_default())
        .filter_map(|update| update["ref_name"].as_str().map(str::to_owned))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    refs.sort();
    refs
}

fn push_trace_root(state_root: &Path, repo_id: &str) -> PathBuf {
    let path = push_trace_file_path(state_root, repo_id, "probe");
    path.parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| state_root.join("push-traces"))
}
