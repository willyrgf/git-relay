use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde_json::json;

use crate::proof_support::cases::{CaseDefinition, STANDARD_CASE_ARTIFACTS};
use crate::proof_support::lab::{CaseReport, ProofLab, AUTHORITATIVE_REPO_ID};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P10",
        setup: "Use deterministic config fixtures with runtime env file, hook installer, and retention defaults.",
        action: "Validate runtime profile, render launchd/systemd units deterministically, verify force-command routing, and run git-relayd serve --once retention behavior.",
        required_assertions: &[
            "p10.runtime_validation.passed",
            "p10.runtime_validation.fail_closed",
            "p10.runtime_validation.rejects_nix_store",
            "p10.service_render.deterministic",
            "p10.hooks.installed",
            "p10.force_command.routing",
            "p10.retention.defaults",
            "p10.serve_once.pending_detected",
            "p10.serve_once.drains_pending",
            "p10.retention.pruning",
            "p10.retention.proof_artifacts",
            "p10.retention.release_evidence_pinned",
        ],
        required_artifacts: STANDARD_CASE_ARTIFACTS,
        pass_criteria: &[
            "runtime env contract is enforced fail-closed",
            "service render output is deterministic",
            "hook install plus force-command routing are operational",
            "serve --once drains pending reconcile and retention pruning follows policy",
            "proof artifact retention prunes stale suite/failure/conformance evidence while keeping current admitted release evidence pinned",
        ],
        fail_criteria: &[
            "runtime env contract bypassed",
            "retention behavior diverges from configured policy",
        ],
        contract_refs: &[
            "RFC_PROOF_E2E_TEST.md#P10",
            "git-relay-rfc.md deployment/runtime invariants",
            "verification-plan result L",
        ],
        runner: run,
    }
}

fn run(lab: &mut ProofLab, _mode: ProofMode) -> Result<CaseReport, String> {
    let mut report = CaseReport::with_details(json!({}));
    let case_root = lab.case_root("P10").map_err(|error| error.to_string())?;

    let runtime_ok = lab
        .run_git_relay(
            &[
                "deploy".to_owned(),
                "validate-runtime".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--repo".to_owned(),
                AUTHORITATIVE_REPO_ID.to_owned(),
                "--json".to_owned(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;
    let runtime_ok_passed = runtime_ok.success()
        && parse_json(&runtime_ok.stdout)
            .ok()
            .and_then(|json| json["status"].as_str().map(str::to_owned))
            == Some("passed".to_owned());

    let bad_config = case_root.join("bad-runtime-config.toml");
    let config_source = fs::read_to_string(&lab.config_path).map_err(|error| error.to_string())?;
    let runtime_line = format!("runtime_env_file = \"{}\"", lab.runtime_env_path.display());
    let bad_source = config_source.replace(&runtime_line, "runtime_env_file = \"relative.env\"");
    fs::write(&bad_config, bad_source).map_err(|error| error.to_string())?;
    let runtime_bad = lab
        .run_git_relay(
            &[
                "deploy".to_owned(),
                "validate-runtime".to_owned(),
                "--config".to_owned(),
                bad_config.display().to_string(),
                "--repo".to_owned(),
                AUTHORITATIVE_REPO_ID.to_owned(),
                "--json".to_owned(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;
    let runtime_fail_closed = !runtime_bad.success();

    let nix_store_config = case_root.join("nix-store-runtime-config.toml");
    let nix_store_source = config_source.replace(
        &runtime_line,
        "runtime_env_file = \"/nix/store/fake-runtime.env\"",
    );
    fs::write(&nix_store_config, nix_store_source).map_err(|error| error.to_string())?;
    let runtime_nix_store = lab
        .run_git_relay(
            &[
                "deploy".to_owned(),
                "validate-runtime".to_owned(),
                "--config".to_owned(),
                nix_store_config.display().to_string(),
                "--repo".to_owned(),
                AUTHORITATIVE_REPO_ID.to_owned(),
                "--json".to_owned(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;
    let runtime_nix_store_rejected = !runtime_nix_store.success()
        && parse_json(&runtime_nix_store.stdout)
            .ok()
            .map(|json| {
                json["status"] == "failed"
                    && json["issues"]
                        .as_array()
                        .map(|items| {
                            items.iter().any(|issue| {
                                issue["code"] == "deployment.runtime_env_file"
                                    && issue["message"]
                                        .as_str()
                                        .map(|message| message.contains("outside /nix/store"))
                                        .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false)
            })
            .unwrap_or(false);

    let systemd_first = render_service(lab, "systemd")?;
    let systemd_second = render_service(lab, "systemd")?;
    let launchd_first = render_service(lab, "launchd")?;
    let launchd_second = render_service(lab, "launchd")?;
    let render_deterministic = systemd_first == systemd_second
        && launchd_first == launchd_second
        && systemd_first.contains("EnvironmentFile=")
        && systemd_first.contains("ExecStart=")
        && launchd_first.contains("<key>Label</key>")
        && launchd_first.contains("serve --config");

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
    let hooks_installed = install.success()
        && ["pre-receive", "reference-transaction", "post-receive"]
            .iter()
            .all(|name| lab.authoritative_repo.join("hooks").join(name).exists());

    let force_check = lab
        .runner
        .run(
            lab.binaries
                .git_relay_ssh_force_command
                .display()
                .to_string(),
            &[
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--check-only".to_owned(),
            ],
            None,
            &[(
                "SSH_ORIGINAL_COMMAND".to_owned(),
                format!("git-upload-pack {}", lab.authoritative_repo.display()),
            )],
        )
        .map_err(|error| error.to_string())?;
    let force_routing_ok = force_check.success()
        && parse_json(&force_check.stdout)
            .ok()
            .map(|json| {
                json["service"] == "git-upload-pack"
                    && json["repo_id"] == AUTHORITATIVE_REPO_ID
                    && json["repo_path"] == json!(lab.authoritative_repo)
            })
            .unwrap_or(false);

    let inspect = lab
        .run_git_relay(
            &[
                "repo".to_owned(),
                "inspect".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--repo".to_owned(),
                AUTHORITATIVE_REPO_ID.to_owned(),
                "--json".to_owned(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;
    let default_retention_ok = if inspect.success() {
        parse_json(&inspect.stdout)
            .ok()
            .and_then(|value| value.as_array().and_then(|items| items.first().cloned()))
            .map(|entry| {
                entry["retention"]["policy"]["terminal_run_ttl"] == "720h"
                    && entry["retention"]["policy"]["terminal_run_keep_count"] == 20
            })
            .unwrap_or(false)
    } else {
        false
    };

    let push_work = case_root.join("push-work");
    if push_work.exists() {
        fs::remove_dir_all(&push_work).map_err(|error| error.to_string())?;
    }
    lab.run_git_expect_success(
        &[
            "clone".to_owned(),
            lab.authoritative_repo.display().to_string(),
            push_work.display().to_string(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            push_work.display().to_string(),
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
            push_work.display().to_string(),
            "config".to_owned(),
            "user.email".to_owned(),
            "git-relay-proof@example.com".to_owned(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    lab.commit_file(
        &push_work,
        "README.md",
        "p10 pending reconcile\n",
        "p10 pending reconcile",
    )
    .map_err(|error| error.to_string())?;
    let push = lab
        .run_git(
            &[
                "-C".to_owned(),
                push_work.display().to_string(),
                "push".to_owned(),
                "origin".to_owned(),
                "HEAD:refs/heads/main".to_owned(),
            ],
            None,
            &[
                ("GIT_RELAY_REQUEST_ID".to_owned(), "request-p10".to_owned()),
                ("GIT_RELAY_PUSH_ID".to_owned(), "push-p10".to_owned()),
            ],
        )
        .map_err(|error| error.to_string())?;

    let pending_path = lab
        .state_root
        .join("reconcile")
        .join("pending")
        .join(format!(
            "{}.json",
            ProofLab::repo_state_component(AUTHORITATIVE_REPO_ID)
        ));
    let pending_before_serve = pending_path.exists();

    let serve_once = lab
        .run_git_relayd(
            &[
                "serve".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--once".to_owned(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;
    let (serve_runtime_ok, serve_drained_pending, pending_after_serve) = if serve_once.success() {
        let parsed = parse_json(&serve_once.stdout)?;
        let runtime_ok = parsed["runtime_validation"]["status"] == "passed";
        let drained = parsed["processed_reconciles"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .any(|entry| entry["repo_id"] == AUTHORITATIVE_REPO_ID)
            })
            .unwrap_or(false);
        (runtime_ok, drained, pending_path.exists())
    } else {
        (false, false, pending_path.exists())
    };

    lab.rewrite_retention_block(
        r#"[retention]
maintenance_interval = "0s"
cache_idle_ttl = "336h"
terminal_run_ttl = "0s"
terminal_run_keep_count = 2
authoritative_reflog_ttl = "720h"
authoritative_prune_ttl = "168h"
"#,
    )
    .map_err(|error| error.to_string())?;

    let reconcile_dir = lab.reconcile_run_dir(AUTHORITATIVE_REPO_ID);
    let upstream_dir = lab.upstream_probe_run_dir(AUTHORITATIVE_REPO_ID);
    let matrix_dir = lab.matrix_probe_run_dir(AUTHORITATIVE_REPO_ID);
    fs::create_dir_all(&reconcile_dir).map_err(|error| error.to_string())?;
    fs::create_dir_all(&upstream_dir).map_err(|error| error.to_string())?;
    fs::create_dir_all(&matrix_dir).map_err(|error| error.to_string())?;
    for (name, started, completed) in [("old-a", 1, 1), ("old-b", 2, 2), ("keep-c", 3, 3)] {
        seed_run_record(&reconcile_dir, name, started, completed)?;
        seed_run_record(&upstream_dir, name, started, completed)?;
        seed_run_record(&matrix_dir, name, started, completed)?;
    }

    let proof_suite_old_a = seed_proof_suite_run(&lab.state_root, "p10-suite-old-a", 1, true)?;
    let proof_suite_old_b = seed_proof_suite_run(&lab.state_root, "p10-suite-old-b", 2, true)?;
    let proof_suite_keep_c = seed_proof_suite_run(&lab.state_root, "p10-suite-keep-c", 3, true)?;
    let stale_failure_capture = proof_suite_old_a.join("failures");

    let admitted_old_macos = seed_git_conformance_manifest(
        &lab.state_root,
        "macos",
        "git version 2.52.0",
        "deterministic-core",
        true,
        0,
    )?;
    sleep_for_fs_tick();
    let admitted_old_linux = seed_git_conformance_manifest(
        &lab.state_root,
        "linux",
        "git version 2.52.0",
        "deterministic-core",
        true,
        0,
    )?;
    sleep_for_fs_tick();
    let stale_non_admitted = seed_git_conformance_manifest(
        &lab.state_root,
        "macos",
        "git version 2.51.0",
        "deterministic-core",
        false,
        0,
    )?;
    sleep_for_fs_tick();
    let retained_non_admitted_linux = seed_git_conformance_manifest(
        &lab.state_root,
        "linux",
        "git version 2.50.0",
        "provider-admission",
        true,
        0,
    )?;
    sleep_for_fs_tick();
    let retained_non_admitted_macos = seed_git_conformance_manifest(
        &lab.state_root,
        "macos",
        "git version 2.49.0",
        "deterministic-core",
        false,
        0,
    )?;
    sleep_for_fs_tick();
    let admitted_current_macos = seed_git_conformance_manifest(
        &lab.state_root,
        "macos",
        "git version 2.53.0",
        "deterministic-core",
        true,
        0,
    )?;
    sleep_for_fs_tick();
    let admitted_current_linux = seed_git_conformance_manifest(
        &lab.state_root,
        "linux",
        "git version 2.53.0",
        "deterministic-core",
        true,
        0,
    )?;

    let prune_once = lab
        .run_git_relayd(
            &[
                "serve".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--once".to_owned(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;
    let (retention_pruned, remaining_reconcile, remaining_upstream, remaining_matrix) =
        if prune_once.success() {
            let parsed = parse_json(&prune_once.stdout)?;
            let pruned = parsed["maintenance_reports"]
                .as_array()
                .and_then(|items| {
                    items
                        .iter()
                        .find(|entry| entry["repo_id"] == AUTHORITATIVE_REPO_ID)
                })
                .map(|entry| {
                    entry["evidence_pruned"]["reconcile_runs_removed"]
                        .as_u64()
                        .unwrap_or(0)
                        >= 1
                        && entry["evidence_pruned"]["upstream_probe_runs_removed"]
                            .as_u64()
                            .unwrap_or(0)
                            >= 1
                        && entry["evidence_pruned"]["matrix_probe_runs_removed"]
                            .as_u64()
                            .unwrap_or(0)
                            >= 1
                })
                .unwrap_or(false);
            (
                pruned,
                count_entries(&reconcile_dir)?,
                count_entries(&upstream_dir)?,
                count_entries(&matrix_dir)?,
            )
        } else {
            (
                false,
                count_entries(&reconcile_dir)?,
                count_entries(&upstream_dir)?,
                count_entries(&matrix_dir)?,
            )
        };
    let retention_keep_count_ok =
        remaining_reconcile == 2 && remaining_upstream == 2 && remaining_matrix == 2;
    let remaining_proof_suites = count_entries(&lab.state_root.join("proof-e2e"))?;
    let proof_suites_pruned = !proof_suite_old_a.exists()
        && !proof_suite_old_b.exists()
        && proof_suite_keep_c.exists()
        && lab.suite_root.exists()
        && remaining_proof_suites == 2;
    let stale_failure_capture_pruned = !stale_failure_capture.exists();
    let proof_artifacts_pruned =
        proof_suites_pruned && stale_failure_capture_pruned && !stale_non_admitted.exists();
    let release_evidence_pinned = admitted_current_macos.exists()
        && admitted_current_linux.exists()
        && !admitted_old_macos.exists()
        && !admitted_old_linux.exists()
        && retained_non_admitted_linux.exists()
        && retained_non_admitted_macos.exists();
    cleanup_case_retention_fixtures(&[
        proof_suite_old_a.as_path(),
        proof_suite_old_b.as_path(),
        proof_suite_keep_c.as_path(),
    ])?;
    cleanup_case_retention_fixtures(&[
        admitted_old_macos.as_path(),
        admitted_old_linux.as_path(),
        admitted_current_macos.as_path(),
        admitted_current_linux.as_path(),
        stale_non_admitted.as_path(),
        retained_non_admitted_linux.as_path(),
        retained_non_admitted_macos.as_path(),
    ])?;

    report.assertions.push(if runtime_ok_passed {
        ProofAssertion::pass(
            "p10.runtime_validation.passed",
            Some("runtime profile passed with absolute env file".to_owned()),
        )
    } else {
        ProofAssertion::fail("p10.runtime_validation.passed", runtime_ok.summary())
    });
    report.assertions.push(if runtime_fail_closed {
        ProofAssertion::pass(
            "p10.runtime_validation.fail_closed",
            Some("relative runtime env path was rejected".to_owned()),
        )
    } else {
        ProofAssertion::fail("p10.runtime_validation.fail_closed", runtime_bad.summary())
    });
    report.assertions.push(if runtime_nix_store_rejected {
        ProofAssertion::pass(
            "p10.runtime_validation.rejects_nix_store",
            Some("runtime env path under /nix/store was rejected".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p10.runtime_validation.rejects_nix_store",
            runtime_nix_store.summary(),
        )
    });
    report.assertions.push(if render_deterministic {
        ProofAssertion::pass(
            "p10.service_render.deterministic",
            Some("launchd/systemd outputs are stable".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p10.service_render.deterministic",
            "service render output was not deterministic or missing required fields",
        )
    });
    report.assertions.push(if hooks_installed {
        ProofAssertion::pass(
            "p10.hooks.installed",
            Some("hook wrappers installed into authoritative repo".to_owned()),
        )
    } else {
        ProofAssertion::fail("p10.hooks.installed", install.summary())
    });
    report.assertions.push(if force_routing_ok {
        ProofAssertion::pass(
            "p10.force_command.routing",
            Some("forced-command check-only resolved expected repo/service".to_owned()),
        )
    } else {
        ProofAssertion::fail("p10.force_command.routing", force_check.summary())
    });
    report.assertions.push(if default_retention_ok {
        ProofAssertion::pass(
            "p10.retention.defaults",
            Some("default retention policy matches RFC defaults".to_owned()),
        )
    } else {
        ProofAssertion::fail("p10.retention.defaults", inspect.summary())
    });
    report
        .assertions
        .push(if push.success() && pending_before_serve {
            ProofAssertion::pass(
                "p10.serve_once.pending_detected",
                Some("pending reconcile request queued before serve --once".to_owned()),
            )
        } else {
            ProofAssertion::fail(
                "p10.serve_once.pending_detected",
                format!(
                    "push_success={} pending_before_serve={pending_before_serve}",
                    push.success()
                ),
            )
        });
    report.assertions.push(if serve_runtime_ok && serve_drained_pending && !pending_after_serve {
        ProofAssertion::pass(
            "p10.serve_once.drains_pending",
            Some("serve --once processed pending reconcile and cleared queue".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p10.serve_once.drains_pending",
            format!(
                "runtime_ok={serve_runtime_ok} drained={serve_drained_pending} pending_after={pending_after_serve}"
            ),
        )
    });
    report.assertions.push(if retention_pruned && retention_keep_count_ok {
        ProofAssertion::pass(
            "p10.retention.pruning",
            Some("terminal run evidence pruning respected ttl/keep-count policy".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p10.retention.pruning",
            format!(
                "retention_pruned={retention_pruned} remaining=({remaining_reconcile},{remaining_upstream},{remaining_matrix})"
            ),
        )
    });
    report.assertions.push(if proof_artifacts_pruned {
        ProofAssertion::pass(
            "p10.retention.proof_artifacts",
            Some("stale suite runs, failure captures, and non-admitted conformance artifacts were pruned".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p10.retention.proof_artifacts",
            format!(
                "proof_suites_pruned={proof_suites_pruned} stale_failure_capture_pruned={stale_failure_capture_pruned} stale_non_admitted_exists={}",
                stale_non_admitted.exists()
            ),
        )
    });
    report.assertions.push(if release_evidence_pinned {
        ProofAssertion::pass(
            "p10.retention.release_evidence_pinned",
            Some("latest admitted release evidence remained pinned while superseded admitted evidence was pruned".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p10.retention.release_evidence_pinned",
            format!(
                "current=({}, {}) old=({}, {}) retained_non_admitted=({}, {})",
                admitted_current_macos.exists(),
                admitted_current_linux.exists(),
                admitted_old_macos.exists(),
                admitted_old_linux.exists(),
                retained_non_admitted_linux.exists(),
                retained_non_admitted_macos.exists(),
            ),
        )
    });

    report.details = json!({
        "runtime_ok": runtime_ok.summary(),
        "runtime_bad": runtime_bad.summary(),
        "runtime_nix_store": runtime_nix_store.summary(),
        "render_deterministic": render_deterministic,
        "hooks_installed": hooks_installed,
        "force_routing_ok": force_routing_ok,
        "default_retention_ok": default_retention_ok,
        "serve_once": serve_once.summary(),
        "pending_before_serve": pending_before_serve,
        "pending_after_serve": pending_after_serve,
        "retention_pruned": retention_pruned,
        "proof_suites_pruned": proof_suites_pruned,
        "stale_failure_capture_pruned": stale_failure_capture_pruned,
        "proof_artifacts_pruned": proof_artifacts_pruned,
        "release_evidence_pinned": release_evidence_pinned,
        "remaining_proof_suites": remaining_proof_suites,
        "admitted_old_macos": admitted_old_macos,
        "admitted_old_linux": admitted_old_linux,
        "admitted_current_macos": admitted_current_macos,
        "admitted_current_linux": admitted_current_linux,
        "stale_non_admitted": stale_non_admitted,
        "retained_non_admitted_linux": retained_non_admitted_linux,
        "retained_non_admitted_macos": retained_non_admitted_macos,
        "remaining_reconcile_runs": remaining_reconcile,
        "remaining_upstream_runs": remaining_upstream,
        "remaining_matrix_runs": remaining_matrix,
        "prune_once": prune_once.summary(),
    });

    Ok(report)
}

fn render_service(lab: &ProofLab, format: &str) -> Result<String, String> {
    let capture = lab
        .run_git_relay(
            &[
                "deploy".to_owned(),
                "render-service".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--format".to_owned(),
                format.to_owned(),
                "--binary-path".to_owned(),
                lab.binaries.git_relayd.display().to_string(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;
    if capture.success() {
        Ok(capture.stdout)
    } else {
        Err(capture.summary())
    }
}

fn seed_run_record(
    directory: &Path,
    run_id: &str,
    started_at_ms: u64,
    completed_at_ms: u64,
) -> Result<(), String> {
    let path = directory.join(format!("{run_id}.json"));
    fs::write(
        path,
        serde_json::to_vec_pretty(&json!({
            "run_id": run_id,
            "repo_id": AUTHORITATIVE_REPO_ID,
            "repo_path": "<repo>",
            "started_at_ms": started_at_ms,
            "completed_at_ms": completed_at_ms,
        }))
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn count_entries(path: &Path) -> Result<usize, String> {
    fs::read_dir(path)
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map(|entries| entries.len())
        .map_err(|error| error.to_string())
}

fn seed_proof_suite_run(
    state_root: &Path,
    suite_id: &str,
    completed_at_ms: u64,
    include_failure_capture: bool,
) -> Result<PathBuf, String> {
    let suite_dir = state_root.join("proof-e2e").join(suite_id);
    fs::create_dir_all(&suite_dir).map_err(|error| error.to_string())?;
    fs::write(
        suite_dir.join("summary.raw.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "suite": "rfc-proof-e2e",
            "mode": "full",
            "started_at_ms": completed_at_ms.saturating_sub(1),
            "completed_at_ms": completed_at_ms,
        }))
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if include_failure_capture {
        let failure_dir = suite_dir.join("failures").join("P10");
        fs::create_dir_all(&failure_dir).map_err(|error| error.to_string())?;
        fs::write(
            failure_dir.join("p10.retention.stderr.txt"),
            "redacted failure evidence\n",
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(suite_dir)
}

fn seed_git_conformance_manifest(
    state_root: &Path,
    platform: &str,
    git_version: &str,
    profile: &str,
    all_mandatory_cases_passed: bool,
    recorded_at_ms: u64,
) -> Result<PathBuf, String> {
    let path = state_root
        .join("release")
        .join("git-conformance")
        .join(platform)
        .join(format!("{}.json", sanitize_key(git_version)));
    fs::create_dir_all(path.parent().expect("conformance parent"))
        .map_err(|error| error.to_string())?;
    fs::write(
        &path,
        serde_json::to_vec_pretty(&git_conformance_evidence_value(
            platform,
            git_version,
            profile,
            all_mandatory_cases_passed,
            recorded_at_ms,
        ))
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(path)
}

fn git_conformance_evidence_value(
    platform: &str,
    git_version: &str,
    profile: &str,
    all_mandatory_cases_passed: bool,
    recorded_at_ms: u64,
) -> serde_json::Value {
    json!({
        "schema_version": 1,
        "profile": profile,
        "git_version_key": sanitize_key(git_version),
        "platform": platform,
        "nix_system": "x86_64-linux",
        "service_manager": if platform == "macos" { "launchd" } else { "systemd" },
        "git_version": git_version,
        "openssh_version": "OpenSSH_10.0p1",
        "filesystem_profile": format!("synthetic-{platform}"),
        "git_relay_commit": "test-commit",
        "flake_lock_sha256": "test-lock",
        "binary_digests": {
            "git-relay": "digest-a",
            "git-relayd": "digest-b",
            "git-relay-install-hooks": "digest-c",
            "git-relay-ssh-force-command": "digest-d"
        },
        "cases": [
            {
                "case_id": "P01",
                "status": if all_mandatory_cases_passed { "pass" } else { "fail" }
            }
        ],
        "all_mandatory_cases_passed": all_mandatory_cases_passed,
        "normalized_summary_sha256": "synthetic-summary",
        "recorded_at_ms": recorded_at_ms
    })
}

fn sanitize_key(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn sleep_for_fs_tick() {
    thread::sleep(Duration::from_millis(5));
}

fn cleanup_case_retention_fixtures(paths: &[&Path]) -> Result<(), String> {
    for path in paths {
        if !path.exists() {
            continue;
        }
        let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
        if metadata.is_dir() {
            fs::remove_dir_all(path).map_err(|error| error.to_string())?;
        } else {
            fs::remove_file(path).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn parse_json(source: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(source).map_err(|error| error.to_string())
}
