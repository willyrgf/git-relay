use std::fs;
use std::path::{Path, PathBuf};

use git_relay::hooks::push_trace_file_path;
use serde_json::json;

use crate::proof_support::cases::CaseDefinition;
use crate::proof_support::lab::{CaseReport, ProofLab, AUTHORITATIVE_REPO_ID};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P01",
        setup: "Create an authoritative bare repository with hardened config and a disposable client worktree.",
        action: "Push over SSH and smart HTTP using ephemeral per-run credentials, then run deterministic-core crash checkpoints against forced-command ingress and verify local-commit boundary.",
        pass_criteria: &[
            "SSH push succeeds and commits a deterministic ref",
            "smart HTTP push succeeds and commits a deterministic ref",
            "authoritative repository remains fsck --strict clean",
            "deterministic-core crash checkpoints preserve local-commit boundary",
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

fn run(lab: &mut ProofLab, mode: ProofMode) -> Result<CaseReport, String> {
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

    let (crash_boundary_ok, post_receive_non_critical, crash_details) =
        if matches!(mode, ProofMode::Fast | ProofMode::Full) {
            run_deterministic_core_crash_boundary_checks(lab, &case_root)?
        } else {
            (true, true, json!({"skipped": true, "mode": mode}))
        };

    report.assertions.push(if ssh_ok {
        ProofAssertion::pass(
            "p01.ssh.push",
            Some("ssh push committed local ref".to_owned()),
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
    report.assertions.push(if http_ref && ssh_ref {
        ProofAssertion::pass(
            "p01.refs.present",
            Some(
                "ssh and smart-http ingress refs are present in the authoritative repository"
                    .to_owned(),
            ),
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
    report.assertions.push(if crash_boundary_ok {
        ProofAssertion::pass(
            "p01.crash_boundary",
            Some("deterministic-core checkpoints preserved local-commit boundary".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p01.crash_boundary",
            "deterministic-core crash checkpoints did not match local-commit boundary",
        )
    });
    report.assertions.push(if post_receive_non_critical {
        ProofAssertion::pass(
            "p01.post_receive.non_critical",
            Some(
                "post-receive side effects remained non-critical for committed local refs"
                    .to_owned(),
            ),
        )
    } else {
        ProofAssertion::fail(
            "p01.post_receive.non_critical",
            "post-receive side effects became correctness-critical for committed refs",
        )
    });

    report.transport_profiles = vec!["ssh".to_owned(), "smart-http".to_owned()];
    report.details = json!({
        "ssh_url": ssh_url,
        "ssh_push": ssh_push.summary(),
        "http_push": http_push.summary(),
        "ssh_ref_present": ssh_ref,
        "http_ref_present": http_ref,
        "fsck_clean": fsck_ok,
        "crash_boundary": crash_details,
    });

    Ok(report)
}

fn run_deterministic_core_crash_boundary_checks(
    lab: &ProofLab,
    case_root: &Path,
) -> Result<(bool, bool, serde_json::Value), String> {
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
    if !install.success() {
        return Ok((
            false,
            false,
            json!({
                "install_hooks": install.summary(),
            }),
        ));
    }

    let fake_ssh = write_fake_ssh_command(
        case_root,
        &lab.config_path,
        &lab.binaries.git_relay_ssh_force_command,
    )?;
    let checkpoints = [
        ("before_pre_receive", false),
        ("after_pre_receive_success", false),
        ("after_reference_transaction_prepared", false),
        ("after_reference_transaction_committed", true),
        ("after_receive_pack_success_before_wrapper_exit", true),
        ("after_wrapper_flushes_response", true),
    ];

    let mut crash_boundary_ok = true;
    let mut post_receive_non_critical = true;
    let mut details = Vec::new();
    for (checkpoint, expect_committed) in checkpoints {
        let ref_name = format!("refs/heads/p01-crash-{checkpoint}");
        let work_repo = case_root.join(format!("work-{checkpoint}"));
        if work_repo.exists() {
            fs::remove_dir_all(&work_repo).map_err(|error| error.to_string())?;
        }
        lab.init_work_repo(&work_repo)
            .map_err(|error| error.to_string())?;
        lab.commit_file(
            &work_repo,
            "README.md",
            &format!("p01 checkpoint {checkpoint}\n"),
            &format!("p01 checkpoint {checkpoint}"),
        )
        .map_err(|error| error.to_string())?;

        let checkpoint_log = case_root.join(format!("{checkpoint}.log"));
        let request_id = format!("request-{checkpoint}");
        let push_id = format!("push-{checkpoint}");
        let capture = lab
            .run_git(
                &[
                    "-C".to_owned(),
                    work_repo.display().to_string(),
                    "push".to_owned(),
                    "relay:relay-authoritative.git".to_owned(),
                    format!("HEAD:{ref_name}"),
                ],
                None,
                &[
                    ("GIT_SSH".to_owned(), fake_ssh.display().to_string()),
                    ("GIT_RELAY_CRASH_AT".to_owned(), checkpoint.to_owned()),
                    (
                        "GIT_RELAY_CHECKPOINT_LOG".to_owned(),
                        checkpoint_log.display().to_string(),
                    ),
                    ("GIT_RELAY_REQUEST_ID".to_owned(), request_id.clone()),
                    ("GIT_RELAY_PUSH_ID".to_owned(), push_id.clone()),
                ],
            )
            .map_err(|error| error.to_string())?;

        let committed = lab
            .git_ref_exists(&lab.authoritative_repo, &ref_name)
            .map_err(|error| error.to_string())?;
        let mut checkpoint_ok = committed == expect_committed;
        if !expect_committed && capture.success() {
            checkpoint_ok = false;
        }
        if expect_committed && committed {
            let local_ref = lab
                .read_git_ref(&lab.authoritative_repo, &ref_name)
                .map_err(|error| error.to_string())?;
            let work_head = read_worktree_ref(lab, &work_repo, "HEAD")?;
            checkpoint_ok = checkpoint_ok && local_ref == work_head;
            if checkpoint == "after_receive_pack_success_before_wrapper_exit" {
                post_receive_non_critical = post_receive_non_critical && !capture.success();
            }
        }
        let checkpoint_log_hit = fs::read_to_string(&checkpoint_log)
            .ok()
            .map(|source| source.lines().any(|line| line.trim() == checkpoint))
            .unwrap_or(false);
        checkpoint_ok = checkpoint_ok && checkpoint_log_hit;

        if checkpoint == "after_wrapper_flushes_response" {
            let post_receive_seen =
                match read_push_trace(&lab.state_root, AUTHORITATIVE_REPO_ID, &push_id) {
                    Ok(trace) => trace.iter().any(|event| {
                        event["hook"] == "post-receive" && event["status"] == "accepted"
                    }),
                    Err(error) => {
                        details.push(json!({
                            "checkpoint": checkpoint,
                            "push_trace_error": error,
                        }));
                        false
                    }
                };
            post_receive_non_critical = post_receive_non_critical && post_receive_seen;
        }

        crash_boundary_ok = crash_boundary_ok && checkpoint_ok;
        details.push(json!({
            "checkpoint": checkpoint,
            "ref_name": ref_name,
            "expect_committed": expect_committed,
            "committed": committed,
            "capture": capture.summary(),
            "checkpoint_log_hit": checkpoint_log_hit,
        }));
    }

    Ok((
        crash_boundary_ok,
        post_receive_non_critical,
        json!({
            "checkpoints": details,
            "post_receive_non_critical": post_receive_non_critical,
        }),
    ))
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

fn read_push_trace(
    state_root: &Path,
    repo_id: &str,
    push_id: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let path = push_trace_file_path(state_root, repo_id, push_id);
    let source = fs::read_to_string(&path).map_err(|error| error.to_string())?;
    source
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).map_err(|error| error.to_string())
        })
        .collect()
}

fn shell_quote_path(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\"'\"'"))
}

fn write_fake_ssh_command(
    root: &Path,
    config_path: &Path,
    wrapper: &Path,
) -> Result<PathBuf, String> {
    let script = root.join("fake-ssh");
    let source = format!(
        "#!/bin/sh\nset -eu\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -o|-i|-p|-l|-S|-F|-J|-E|-c|-m)\n      shift 2\n      ;;\n    -T|-n|-N|-4|-6|-a|-A|-q|-v|-vv|-vvv|-x|-X|-Y|-y|-C|-f|-G)\n      shift\n      ;;\n    --)\n      shift\n      break\n      ;;\n    -*)\n      shift\n      ;;\n    *)\n      break\n      ;;\n  esac\ndone\nif [ \"$#\" -lt 2 ]; then\n  echo \"fake ssh expected host and remote command\" >&2\n  exit 1\nfi\nhost=\"$1\"\nshift\nSSH_ORIGINAL_COMMAND=\"$*\" exec {wrapper} --config {config}\n",
        wrapper = shell_quote_path(wrapper),
        config = shell_quote_path(config_path),
    );
    fs::write(&script, source).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).map_err(|error| error.to_string())?;
    }
    Ok(script)
}
