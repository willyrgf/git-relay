use serde_json::json;

use crate::proof_support::cases::{CaseDefinition, STANDARD_CASE_ARTIFACTS};
use crate::proof_support::lab::{CaseReport, ProofLab, AUTHORITATIVE_REPO_ID};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P08",
        setup: "Probe one self-managed target with same_repo_hidden_refs admission checks.",
        action: "Run probe-matrix before and after authoritative hardening and verify hidden-ref leakage gate behavior.",
        required_assertions: &[
            "p08.first_probe.executed",
            "p08.rejects_hidden_ref_leakage",
            "p08.admits_hardened_target",
            "p08.rejects_hidden_object_leakage",
            "p08.blocks_hidden_object_fetch_after_hardening",
            "p08.hidden_refs_not_advertised",
        ],
        required_artifacts: STANDARD_CASE_ARTIFACTS,
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
        .start_plain_ssh_transport("P08")
        .map_err(|error| error.to_string())?;
    let target_url = transport.remote_url_for_repo(&lab.upstream_alpha);
    let target_env = vec![("GIT_SSH_COMMAND".to_owned(), transport.git_ssh_command())];

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
            &target_env,
        )
        .map_err(|error| error.to_string())?;

    let mut rejected_when_leaky = false;
    let mut hidden_object_leakage_rejected = false;
    let mut hidden_object_fetch_allowed_before_hardening = false;
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
    if rejected_when_leaky {
        hidden_object_fetch_allowed_before_hardening = !verify_hidden_object_fetch_blocked(
            lab,
            &target_url,
            &lab.upstream_alpha,
            &target_env,
        )?;
        hidden_object_leakage_rejected = hidden_object_fetch_allowed_before_hardening;
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
            &target_env,
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

    let hidden_object_fetch_blocked_after_hardening = admitted_after_hardening
        && !second_reasons
            .iter()
            .any(|reason| reason.contains("hidden-object leakage check"));

    let hidden_refs_not_advertised = {
        let capture = lab
            .run_git(
                &[
                    "ls-remote".to_owned(),
                    target_url.clone(),
                    "refs/git-relay/*".to_owned(),
                ],
                None,
                &target_env,
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
    report.assertions.push(if hidden_object_leakage_rejected {
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
    });
    report
        .assertions
        .push(if hidden_object_fetch_blocked_after_hardening {
            ProofAssertion::pass(
                "p08.blocks_hidden_object_fetch_after_hardening",
                Some(
                    "hardened target denied fetch-by-oid for a temporary hidden object".to_owned(),
                ),
            )
        } else {
            ProofAssertion::fail(
                "p08.blocks_hidden_object_fetch_after_hardening",
                "hardened target still allowed fetch-by-oid for a temporary hidden object",
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
        "target_url": target_url,
        "rejected_when_leaky": rejected_when_leaky,
        "hidden_object_leakage_rejected": hidden_object_leakage_rejected,
        "hidden_object_fetch_allowed_before_hardening": hidden_object_fetch_allowed_before_hardening,
        "admitted_after_hardening": admitted_after_hardening,
        "hidden_object_fetch_blocked_after_hardening": hidden_object_fetch_blocked_after_hardening,
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

fn verify_hidden_object_fetch_blocked(
    lab: &ProofLab,
    target_url: &str,
    target_repo: &std::path::Path,
    extra_env: &[(String, String)],
) -> Result<bool, String> {
    const EMPTY_TREE_OID: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    let hidden_ref = "refs/git-relay/probe-hidden/p08-hardened-check";
    let local_probe_ref = "refs/git-relay/probe-fetch/p08-hardened-check";

    let commit = lab
        .run_git_expect_success(
            &[
                format!("--git-dir={}", target_repo.display()),
                "-c".to_owned(),
                "user.name=Git Relay Proof".to_owned(),
                "-c".to_owned(),
                "user.email=git-relay-proof@example.invalid".to_owned(),
                "commit-tree".to_owned(),
                EMPTY_TREE_OID.to_owned(),
                "-m".to_owned(),
                "git-relay proof hidden object".to_owned(),
            ],
            None,
            &[],
        )
        .map_err(|error| error.to_string())?;
    let hidden_oid = commit.stdout.trim().to_owned();
    if hidden_oid.is_empty() {
        return Err("hidden-object probe commit-tree returned an empty object id".to_owned());
    }

    lab.run_git_expect_success(
        &[
            format!("--git-dir={}", target_repo.display()),
            "update-ref".to_owned(),
            hidden_ref.to_owned(),
            hidden_oid.clone(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;

    let fetch = lab
        .run_git(
            &[
                "fetch".to_owned(),
                "--no-tags".to_owned(),
                target_url.to_owned(),
                format!("{hidden_oid}:{local_probe_ref}"),
            ],
            Some(&lab.authoritative_repo),
            extra_env,
        )
        .map_err(|error| error.to_string());

    let _ = lab.run_git(
        &[
            "update-ref".to_owned(),
            "-d".to_owned(),
            local_probe_ref.to_owned(),
        ],
        Some(&lab.authoritative_repo),
        &[],
    );
    let _ = lab.run_git(
        &[
            format!("--git-dir={}", target_repo.display()),
            "update-ref".to_owned(),
            "-d".to_owned(),
            hidden_ref.to_owned(),
        ],
        None,
        &[],
    );

    Ok(!fetch?.success())
}
