use serde_json::json;

use crate::proof_support::cases::CaseDefinition;
use crate::proof_support::lab::{CaseReport, ProofLab, AUTHORITATIVE_REPO_ID};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P08",
        setup: "Probe one self-managed target with same_repo_hidden_refs admission checks.",
        action: "Run probe-matrix before and after authoritative hardening and verify hidden-ref leakage gate behavior.",
        pass_criteria: &[
            "same-repo hidden refs are rejected when leakage is possible",
            "when transport probing is remote, guessed hidden object ids are blocked by admission checks",
            "same-repo hidden refs are admitted only after hardening checks pass",
        ],
        fail_criteria: &[
            "target admitted with hidden-ref leakage",
            "internal refs advertised to clients",
        ],
        contract_refs: &[
            "RFC_PROOF_E2E_TEST.md#P08",
            "git-relay-rfc.md hidden-ref contract",
            "verification-plan result F",
        ],
        runner: run,
    }
}

fn run(lab: &mut ProofLab, _mode: ProofMode) -> Result<CaseReport, String> {
    let mut report = CaseReport::with_details(json!({}));
    let transport = lab
        .start_transport_harness("P08")
        .map_err(|error| error.to_string())?;
    let ssh_url = transport.ssh.remote_url_for_repo(&lab.upstream_alpha);
    let ssh_env = vec![(
        "GIT_SSH_COMMAND".to_owned(),
        transport.ssh.git_ssh_command(),
    )];
    let ssh_required = transport.ssh.shell_allows_remote_commands;
    let target_url = if ssh_required {
        ssh_url.clone()
    } else {
        lab.upstream_alpha.display().to_string()
    };

    lab.write_authoritative_descriptor_with_write_upstreams(&[(
        "alpha",
        &lab.upstream_alpha,
        true,
    )])
    .map_err(|error| error.to_string())?;

    // Explicitly make leakage possible for first admission probe.
    configure_git(
        lab,
        &lab.upstream_alpha,
        "uploadpack.allowReachableSHA1InWant",
        "true",
    )?;
    configure_git(
        lab,
        &lab.upstream_alpha,
        "uploadpack.allowAnySHA1InWant",
        "true",
    )?;
    configure_git(
        lab,
        &lab.upstream_alpha,
        "uploadpack.allowTipSHA1InWant",
        "true",
    )?;

    let manifest = lab
        .write_matrix_targets_fixture(
            "p08-targets.json",
            &[(
                "self-managed-alpha",
                "local-git",
                "self-managed",
                "ssh",
                &target_url,
                true,
                true,
            )],
        )
        .map_err(|error| error.to_string())?;

    let first = lab
        .run_git_relay(
            &[
                "replication".to_owned(),
                "probe-matrix".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--repo".to_owned(),
                AUTHORITATIVE_REPO_ID.to_owned(),
                "--targets".to_owned(),
                manifest.display().to_string(),
                "--json".to_owned(),
            ],
            if ssh_required {
                &ssh_env
            } else {
                &[] as &[(String, String)]
            },
        )
        .map_err(|error| error.to_string())?;

    let mut rejected_when_leaky = false;
    let mut hidden_object_leakage_rejected = false;
    let hidden_object_probe_expected = ssh_required;
    let mut first_reasons: Vec<String> = Vec::new();
    if first.success() {
        let parsed: serde_json::Value =
            serde_json::from_str(&first.stdout).map_err(|error| error.to_string())?;
        if let Some(results) = parsed["results"].as_array() {
            rejected_when_leaky = results.iter().any(|entry| {
                entry["target"]["target_id"] == "self-managed-alpha"
                    && entry["same_repo_hidden_refs_supported"] == false
                    && entry["admission_reasons"]
                        .as_array()
                        .map(|reasons| {
                            reasons.iter().any(|reason| {
                                reason
                                    .as_str()
                                    .map(|value| value.contains("hidden-ref"))
                                    .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false)
            });
            hidden_object_leakage_rejected = results.iter().any(|entry| {
                entry["target"]["target_id"] == "self-managed-alpha"
                    && entry["same_repo_hidden_refs_supported"] == false
                    && entry["admission_reasons"]
                        .as_array()
                        .map(|reasons| {
                            reasons.iter().any(|reason| {
                                reason
                                    .as_str()
                                    .map(|value| value.contains("hidden-object leakage check"))
                                    .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false)
            });
            first_reasons = results
                .iter()
                .filter(|entry| entry["target"]["target_id"] == "self-managed-alpha")
                .flat_map(|entry| {
                    entry["admission_reasons"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                })
                .filter_map(|reason| reason.as_str().map(str::to_owned))
                .collect();
        }
    }

    // Harden target and rerun admission probe.
    for (key, value) in [
        ("receive.fsckObjects", "true"),
        ("transfer.hideRefs", "refs/git-relay"),
        ("uploadpack.hideRefs", "refs/git-relay"),
        ("receive.hideRefs", "refs/git-relay"),
        ("uploadpack.allowReachableSHA1InWant", "false"),
        ("uploadpack.allowAnySHA1InWant", "false"),
        ("uploadpack.allowTipSHA1InWant", "false"),
        ("core.fsync", "all"),
        ("core.fsyncMethod", "fsync"),
    ] {
        configure_git(lab, &lab.upstream_alpha, key, value)?;
    }

    let second = lab
        .run_git_relay(
            &[
                "replication".to_owned(),
                "probe-matrix".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--repo".to_owned(),
                AUTHORITATIVE_REPO_ID.to_owned(),
                "--targets".to_owned(),
                manifest.display().to_string(),
                "--json".to_owned(),
            ],
            if ssh_required {
                &ssh_env
            } else {
                &[] as &[(String, String)]
            },
        )
        .map_err(|error| error.to_string())?;

    let mut admitted_after_hardening = false;
    let mut second_reasons: Vec<String> = Vec::new();
    if second.success() {
        let parsed: serde_json::Value =
            serde_json::from_str(&second.stdout).map_err(|error| error.to_string())?;
        if let Some(results) = parsed["results"].as_array() {
            admitted_after_hardening = results.iter().any(|entry| {
                entry["target"]["target_id"] == "self-managed-alpha"
                    && entry["same_repo_hidden_refs_supported"] == true
                    && entry["supported_for_policy"] == true
            });
            second_reasons = results
                .iter()
                .filter(|entry| entry["target"]["target_id"] == "self-managed-alpha")
                .flat_map(|entry| {
                    entry["admission_reasons"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                })
                .filter_map(|reason| reason.as_str().map(str::to_owned))
                .collect();
        }
    }

    let hidden_refs_not_advertised = {
        let capture = lab
            .run_git(
                &[
                    "ls-remote".to_owned(),
                    target_url.clone(),
                    "refs/git-relay/*".to_owned(),
                ],
                None,
                if ssh_required {
                    &ssh_env
                } else {
                    &[] as &[(String, String)]
                },
            )
            .map_err(|error| error.to_string())?;
        capture.success() && capture.stdout.trim().is_empty()
    };

    report.assertions.push(if first.success() {
        ProofAssertion::pass(
            "p08.first_probe.executed",
            Some("first probe-matrix run completed".to_owned()),
        )
    } else {
        ProofAssertion::fail("p08.first_probe.executed", first.summary())
    });
    report.assertions.push(if rejected_when_leaky {
        ProofAssertion::pass(
            "p08.rejects_hidden_ref_leakage",
            Some("leaky target was rejected".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p08.rejects_hidden_ref_leakage",
            "target was not rejected when hidden-ref leakage was possible",
        )
    });
    report.assertions.push(if admitted_after_hardening {
        ProofAssertion::pass(
            "p08.admits_hardened_target",
            Some("hardened target admitted for same-repo hidden refs".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p08.admits_hardened_target",
            "hardened target did not reach admitted status",
        )
    });
    report.assertions.push(if hidden_object_probe_expected {
        if hidden_object_leakage_rejected {
            ProofAssertion::pass(
                "p08.rejects_hidden_object_leakage",
                Some(
                    "target was rejected when a hidden object remained fetchable by guessed oid"
                        .to_owned(),
                ),
            )
        } else {
            ProofAssertion::fail(
                "p08.rejects_hidden_object_leakage",
                "target did not report hidden-object leakage rejection while same_repo_hidden_refs admission was leaky",
            )
        }
    } else {
        ProofAssertion::pass(
            "p08.rejects_hidden_object_leakage",
            Some(
                "hidden-object leakage probe is not applicable when target URL falls back to local-path transport"
                    .to_owned(),
            ),
        )
    });
    report.assertions.push(if hidden_refs_not_advertised {
        ProofAssertion::pass(
            "p08.hidden_refs_not_advertised",
            Some("refs/git-relay/* is hidden from ls-remote".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p08.hidden_refs_not_advertised",
            "hidden refs were advertised after hardening",
        )
    });

    report.details = json!({
        "ssh_url": ssh_url,
        "target_url": target_url,
        "ssh_required": ssh_required,
        "hidden_object_probe_expected": hidden_object_probe_expected,
        "rejected_when_leaky": rejected_when_leaky,
        "hidden_object_leakage_rejected": hidden_object_leakage_rejected,
        "admitted_after_hardening": admitted_after_hardening,
        "hidden_refs_not_advertised": hidden_refs_not_advertised,
        "first_reasons": first_reasons,
        "second_reasons": second_reasons,
    });

    Ok(report)
}

fn configure_git(
    lab: &ProofLab,
    repo: &std::path::Path,
    key: &str,
    value: &str,
) -> Result<(), String> {
    lab.run_git_expect_success(
        &[
            format!("--git-dir={}", repo.display()),
            "config".to_owned(),
            key.to_owned(),
            value.to_owned(),
        ],
        None,
        &[],
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}
