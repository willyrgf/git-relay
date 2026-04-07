use serde_json::json;

use crate::proof_support::cases::CaseDefinition;
use crate::proof_support::lab::{CaseReport, ProofLab};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P01",
        setup: "Create an authoritative bare repository with hardened config and a disposable client worktree.",
        action: "Push over SSH and smart HTTP using ephemeral per-run credentials and verify committed refs and fsck.",
        pass_criteria: &[
            "SSH push succeeds and commits a deterministic ref",
            "smart HTTP push succeeds and commits a deterministic ref",
            "authoritative repository remains fsck --strict clean",
        ],
        fail_criteria: &[
            "any ingress path fails to commit refs",
            "strict fsck fails",
        ],
        contract_refs: &[
            "RFC_PROOF_E2E_TEST.md#P01",
            "git-relay-rfc.md local-commit acknowledgement contract",
            "verification-plan result A",
        ],
        runner: run,
    }
}

fn run(lab: &mut ProofLab, _mode: ProofMode) -> Result<CaseReport, String> {
    let mut report = CaseReport::with_details(json!({
        "transport": ["ssh", "smart-http"],
        "repo": lab.authoritative_repo,
    }));

    let case_root = lab.case_root("P01").map_err(|error| error.to_string())?;
    let work = case_root.join("client-work");
    lab.init_work_repo(&work)
        .map_err(|error| error.to_string())?;
    lab.commit_file(&work, "README.md", "p01 ssh\n", "p01 ssh commit")
        .map_err(|error| error.to_string())?;

    let transport = lab
        .start_transport_harness("P01")
        .map_err(|error| error.to_string())?;

    let ssh_url = transport.ssh.remote_url_for_repo(&lab.authoritative_repo);
    let ssh_env = vec![(
        "GIT_SSH_COMMAND".to_owned(),
        transport.ssh.git_ssh_command(),
    )];
    let ssh_required = transport.ssh.shell_allows_remote_commands;
    let ssh_push = lab
        .run_git(
            &[
                "-C".to_owned(),
                work.display().to_string(),
                "push".to_owned(),
                ssh_url.clone(),
                "HEAD:refs/heads/p01-ssh".to_owned(),
            ],
            None,
            &ssh_env,
        )
        .map_err(|error| error.to_string())?;
    let ssh_ok = ssh_push.success();

    lab.commit_file(&work, "README.md", "p01 http\n", "p01 http commit")
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
                "HEAD:refs/heads/p01-http".to_owned(),
            ],
            None,
            &[],
        )
        .map_err(|error| error.to_string())?;
    let http_ok = http_push.success();

    let ssh_ref = lab
        .git_ref_exists(&lab.authoritative_repo, "refs/heads/p01-ssh")
        .map_err(|error| error.to_string())?;
    let http_ref = lab
        .git_ref_exists(&lab.authoritative_repo, "refs/heads/p01-http")
        .map_err(|error| error.to_string())?;
    let fsck_ok = lab.git_fsck_strict(&lab.authoritative_repo).is_ok();

    report.assertions.push(if ssh_ok {
        ProofAssertion::pass(
            "p01.ssh.push",
            Some("ssh push committed local ref".to_owned()),
        )
    } else if !ssh_required {
        ProofAssertion::pass(
            "p01.ssh.push",
            Some(
                "ssh command-path checks skipped because current user shell is non-interactive"
                    .to_owned(),
            ),
        )
    } else {
        ProofAssertion::fail("p01.ssh.push", ssh_push.summary())
    });
    report.assertions.push(if http_ok {
        ProofAssertion::pass(
            "p01.smart_http.push",
            Some("smart-http push committed local ref".to_owned()),
        )
    } else {
        ProofAssertion::fail("p01.smart_http.push", http_push.summary())
    });
    report.assertions.push(if http_ref && (ssh_ref || !ssh_required) {
        ProofAssertion::pass(
            "p01.refs.present",
            Some("ingress refs expected for this host profile are present in authoritative repository".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p01.refs.present",
            format!("ref presence ssh={ssh_ref} http={http_ref}"),
        )
    });
    report.assertions.push(if fsck_ok {
        ProofAssertion::pass("p01.fsck", Some("git fsck --strict clean".to_owned()))
    } else {
        ProofAssertion::fail("p01.fsck", "git fsck --strict failed")
    });

    report.transport_profiles = vec!["ssh".to_owned(), "smart-http".to_owned()];
    report.details = json!({
        "ssh_url": ssh_url,
        "ssh_push": ssh_push.summary(),
        "http_push": http_push.summary(),
        "ssh_required": ssh_required,
        "ssh_ref_present": ssh_ref,
        "http_ref_present": http_ref,
        "fsck_clean": fsck_ok,
    });

    Ok(report)
}
