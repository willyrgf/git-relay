use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::proof_support::cases::{CaseDefinition, STANDARD_CASE_ARTIFACTS};
use crate::proof_support::lab::{CaseReport, LabProfile, ProofLab, AUTHORITATIVE_REPO_ID};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P11",
        setup: "Prepare release matrix targets with one admitted candidate and one unadmitted target.",
        action: "Run release-manifest build + release report, assert floor status remains open without complete admitted evidence, then record synthetic dual-platform conformance files and assert they do not close the real release floor.",
        required_assertions: &[
            "p11.seed.push",
            "p11.release_manifest.fail_closed",
            "p11.release_manifest.supported_target_admitted",
            "p11.release_manifest.persisted",
            "p11.release_floor.open_without_full_evidence",
            "p11.release_floor.synthetic_cross_platform_rejected",
            "p11.release_blocking_reason.machine_readable",
            "p11.host_evidence.persisted",
            "p11.provider_inputs.validated",
            "p11.provider_manifest.used",
        ],
        required_artifacts: STANDARD_CASE_ARTIFACTS,
        pass_criteria: &[
            "release manifest evidence is persisted",
            "missing or unadmitted targets keep floor status open",
            "synthetic dual-platform conformance files do not satisfy real host-admitted release closure",
            "host evidence persists per host under its platform",
        ],
        fail_criteria: &[
            "release floor closes from synthetic cross-platform files inside a single host-local test",
        ],
        contract_refs: &[
            "RFC_PROOF_E2E_TEST.md#P11",
            "RFC_PROOF_E2E_TEST.md#9-machine-readable-git-conformance-evidence",
            "git-relay-rfc.md release admission fail-closed contract",
            "verification-plan release floor evidence constraints",
        ],
        runner: run,
    }
}

fn run(lab: &mut ProofLab, mode: ProofMode) -> Result<CaseReport, String> {
    let mut report = CaseReport::with_details(json!({}));
    let case_root = lab.case_root("P11").map_err(|error| error.to_string())?;

    lab.write_authoritative_descriptor_with_write_upstreams(&[(
        "alpha",
        &lab.upstream_alpha,
        true,
    )])
    .map_err(|error| error.to_string())?;
    configure_same_repo_hidden_target(lab, &lab.upstream_alpha)?;

    let work_repo = case_root.join("release-work");
    if work_repo.exists() {
        fs::remove_dir_all(&work_repo).map_err(|error| error.to_string())?;
    }
    lab.run_git_expect_success(
        &[
            "clone".to_owned(),
            lab.authoritative_repo.display().to_string(),
            work_repo.display().to_string(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            work_repo.display().to_string(),
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
            work_repo.display().to_string(),
            "config".to_owned(),
            "user.email".to_owned(),
            "git-relay-proof@example.com".to_owned(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    lab.commit_file(
        &work_repo,
        "README.md",
        "p11 release evidence\n",
        "p11 release evidence",
    )
    .map_err(|error| error.to_string())?;
    let push = lab
        .run_git(
            &[
                "-C".to_owned(),
                work_repo.display().to_string(),
                "push".to_owned(),
                "origin".to_owned(),
                "HEAD:refs/heads/main".to_owned(),
            ],
            None,
            &[],
        )
        .map_err(|error| error.to_string())?;

    let missing_target = case_root.join("missing-provider-target.git");
    let (targets_manifest, provider_manifest_used) = if mode == ProofMode::ProviderAdmission {
        let Some(inputs) = lab.provider_inputs.as_ref() else {
            return Err("provider-admission mode missing provider input files".to_owned());
        };
        (inputs.target_manifest.clone(), true)
    } else {
        (
            lab.write_matrix_targets_fixture(
                "p11-targets.json",
                &[
                    (
                        "supported-alpha",
                        "local-git",
                        "self-managed",
                        "ssh",
                        &lab.upstream_alpha.display().to_string(),
                        true,
                        true,
                    ),
                    (
                        "missing-beta",
                        "local-git",
                        "managed",
                        "ssh",
                        &missing_target.display().to_string(),
                        false,
                        false,
                    ),
                ],
            )
            .map_err(|error| error.to_string())?,
            false,
        )
    };

    let manifest_build = lab
        .run_git_relay(
            &[
                "replication".to_owned(),
                "build-release-manifest".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--repo".to_owned(),
                AUTHORITATIVE_REPO_ID.to_owned(),
                "--targets".to_owned(),
                targets_manifest.display().to_string(),
                "--json".to_owned(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;
    let (manifest_open, missing_target_unadmitted, supported_target_admitted) =
        parse_manifest_open(&manifest_build.stdout)?;

    let manifest_latest = lab
        .state_root
        .join("upstream-probes")
        .join("release-manifests")
        .join(ProofLab::repo_state_component(AUTHORITATIVE_REPO_ID))
        .join("latest.json");
    let manifest_persisted = manifest_latest.exists();

    let fake_nix = write_fake_nix_version_command(
        &case_root,
        "fake-nix-p11",
        "nix (Determinate Nix 3.0.0) 2.26.3",
    )?;
    let release_report = lab
        .run_git_relay(
            &[
                "release".to_owned(),
                "report".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--repo".to_owned(),
                AUTHORITATIVE_REPO_ID.to_owned(),
                "--json".to_owned(),
            ],
            &[(
                "GIT_RELAY_NIX_BIN".to_owned(),
                fake_nix.display().to_string(),
            )],
        )
        .map_err(|error| error.to_string())?;

    let (git_floor_open, repo_manifest_open, blocking_reason_open, host_evidence_persisted) =
        if release_report.success() {
            let parsed = parse_json(&release_report.stdout)?;
            let git_open = parsed["exact_git_floor_status"] == "open";
            let repo_open = parsed["repo_manifests"]
                .as_array()
                .and_then(|items| {
                    items
                        .iter()
                        .find(|entry| entry["repo_id"] == AUTHORITATIVE_REPO_ID)
                })
                .map(|entry| {
                    entry["manifest_present"] == true && entry["all_entries_admitted"] == false
                })
                .unwrap_or(false);
            let blocking = parsed["blocking_reasons"]
                .as_array()
                .map(|items| {
                    items.iter().any(|entry| {
                        entry
                            .as_str()
                            .map(|value| value.contains("Git floor evidence remains open"))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            let host_evidence_dir =
                lab.state_root
                    .join("release")
                    .join("hosts")
                    .join(match std::env::consts::OS {
                        "macos" => "macos",
                        "linux" => "linux",
                        other => return Err(format!("unsupported host platform {other}")),
                    });
            let host_evidence = host_evidence_dir
                .read_dir()
                .map(|entries| {
                    entries.filter_map(Result::ok).any(|entry| {
                        entry.path().extension().and_then(|value| value.to_str()) == Some("json")
                    })
                })
                .unwrap_or(false);
            (git_open, repo_open, blocking, host_evidence)
        } else {
            (false, false, false, false)
        };

    let synthetic_git_version = lab.toolchain.git_version.clone();
    let macos_conformance = lab
        .persist_release_git_conformance_evidence("macos", &synthetic_git_version, true)
        .map_err(|error| error.to_string())?;
    let linux_conformance = lab
        .persist_release_git_conformance_evidence("linux", &synthetic_git_version, true)
        .map_err(|error| error.to_string())?;
    let release_report_after_synthetic = lab
        .run_git_relay(
            &[
                "release".to_owned(),
                "report".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--repo".to_owned(),
                AUTHORITATIVE_REPO_ID.to_owned(),
                "--json".to_owned(),
            ],
            &[(
                "GIT_RELAY_NIX_BIN".to_owned(),
                fake_nix.display().to_string(),
            )],
        )
        .map_err(|error| error.to_string())?;
    let (synthetic_floor_still_open, synthetic_floor_blocked_on_hosts) =
        if release_report_after_synthetic.success() {
            let parsed = parse_json(&release_report_after_synthetic.stdout)?;
            let host_blocking = parsed["blocking_reasons"]
                .as_array()
                .map(|items| {
                    items.iter().any(|entry| {
                        entry
                            .as_str()
                            .map(|value| value.contains("host evidence does not yet cover"))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            (parsed["exact_git_floor_status"] == "open", host_blocking)
        } else {
            (false, false)
        };

    let provider_inputs_checked = if mode == ProofMode::ProviderAdmission {
        if lab.profile != LabProfile::ProviderAdmission {
            false
        } else if let Some(inputs) = lab.provider_inputs.as_ref() {
            inputs.target_manifest.is_absolute()
                && inputs.credentials_file.is_absolute()
                && inputs.target_manifest.exists()
                && inputs.credentials_file.exists()
        } else {
            false
        }
    } else {
        true
    };

    report.assertions.push(if push.success() {
        ProofAssertion::pass(
            "p11.seed.push",
            Some("release source commit pushed to authoritative repo".to_owned()),
        )
    } else {
        ProofAssertion::fail("p11.seed.push", push.summary())
    });
    report.assertions.push(
        if !manifest_build.success() && manifest_open && missing_target_unadmitted {
            ProofAssertion::pass(
                "p11.release_manifest.fail_closed",
                Some("release manifest remained open for unadmitted targets".to_owned()),
            )
        } else {
            ProofAssertion::fail("p11.release_manifest.fail_closed", manifest_build.summary())
        },
    );
    report.assertions.push(if supported_target_admitted {
        ProofAssertion::pass(
            "p11.release_manifest.supported_target_admitted",
            Some("release manifest contains admitted evidence for supported target".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p11.release_manifest.supported_target_admitted",
            "release manifest did not admit supported-alpha target evidence",
        )
    });
    report.assertions.push(if manifest_persisted {
        ProofAssertion::pass(
            "p11.release_manifest.persisted",
            Some("latest release manifest evidence was persisted".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p11.release_manifest.persisted",
            format!("missing {}", manifest_latest.display()),
        )
    });
    report.assertions.push(
        if release_report.success() && git_floor_open && repo_manifest_open {
            ProofAssertion::pass(
                "p11.release_floor.open_without_full_evidence",
                Some("release floor remained open without complete admitted evidence".to_owned()),
            )
        } else {
            ProofAssertion::fail(
                "p11.release_floor.open_without_full_evidence",
                release_report.summary(),
            )
        },
    );
    report.assertions.push(
        if release_report_after_synthetic.success()
            && synthetic_floor_still_open
            && synthetic_floor_blocked_on_hosts
        {
            ProofAssertion::pass(
                "p11.release_floor.synthetic_cross_platform_rejected",
                Some(
                    "synthetic dual-platform conformance files did not close the release floor without real per-host platform evidence"
                        .to_owned(),
                ),
            )
        } else {
            ProofAssertion::fail(
                "p11.release_floor.synthetic_cross_platform_rejected",
                release_report_after_synthetic.summary(),
            )
        },
    );
    report.assertions.push(if blocking_reason_open {
        ProofAssertion::pass(
            "p11.release_blocking_reason.machine_readable",
            Some("blocking reason explicitly reports open Git floor evidence".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p11.release_blocking_reason.machine_readable",
            "release report did not include open Git floor blocking reason",
        )
    });
    report.assertions.push(if host_evidence_persisted {
        ProofAssertion::pass(
            "p11.host_evidence.persisted",
            Some("current host evidence persisted under release/hosts".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p11.host_evidence.persisted",
            "release host evidence file was missing",
        )
    });
    report.assertions.push(if provider_inputs_checked {
        ProofAssertion::pass(
            "p11.provider_inputs.validated",
            Some("provider-admission explicit inputs remained validated".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p11.provider_inputs.validated",
            "provider-admission mode did not keep explicit input validation fail-closed",
        )
    });
    report.assertions.push(
        if mode != ProofMode::ProviderAdmission || provider_manifest_used {
            ProofAssertion::pass(
                "p11.provider_manifest.used",
                Some("provider-admission mode consumed explicit target manifest".to_owned()),
            )
        } else {
            ProofAssertion::fail(
                "p11.provider_manifest.used",
                "provider-admission mode did not use explicit target manifest inputs",
            )
        },
    );

    report.details = json!({
        "manifest_build": manifest_build.summary(),
        "manifest_open": manifest_open,
        "missing_target_unadmitted": missing_target_unadmitted,
        "supported_target_admitted": supported_target_admitted,
        "provider_manifest_used": provider_manifest_used,
        "manifest_persisted": manifest_persisted,
        "manifest_latest": manifest_latest,
        "release_report": release_report.summary(),
        "release_report_after_synthetic": release_report_after_synthetic.summary(),
        "git_floor_open": git_floor_open,
        "synthetic_floor_still_open": synthetic_floor_still_open,
        "synthetic_floor_blocked_on_hosts": synthetic_floor_blocked_on_hosts,
        "repo_manifest_open": repo_manifest_open,
        "blocking_reason_open": blocking_reason_open,
        "host_evidence_persisted": host_evidence_persisted,
        "provider_inputs_checked": provider_inputs_checked,
        "macos_conformance": macos_conformance,
        "linux_conformance": linux_conformance,
    });

    Ok(report)
}

fn parse_manifest_open(source: &str) -> Result<(bool, bool, bool), String> {
    let parsed = parse_json(source)?;
    let manifest_open = parsed["all_entries_admitted"] == false;
    let missing_target_unadmitted = parsed["entries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .any(|entry| entry["target_id"] == "missing-beta" && entry["admitted"] == false)
        })
        .unwrap_or(false);
    let supported_target_admitted = parsed["entries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .any(|entry| entry["target_id"] == "supported-alpha" && entry["admitted"] == true)
        })
        .unwrap_or(false);
    Ok((
        manifest_open,
        missing_target_unadmitted,
        supported_target_admitted,
    ))
}

fn configure_same_repo_hidden_target(lab: &ProofLab, repo: &Path) -> Result<(), String> {
    let entries = [
        ("receive.fsckObjects", "true"),
        ("transfer.hideRefs", "refs/git-relay"),
        ("uploadpack.hideRefs", "refs/git-relay"),
        ("receive.hideRefs", "refs/git-relay"),
        ("uploadpack.allowReachableSHA1InWant", "false"),
        ("uploadpack.allowAnySHA1InWant", "false"),
        ("uploadpack.allowTipSHA1InWant", "false"),
        ("core.fsync", "all"),
        ("core.fsyncMethod", "fsync"),
    ];
    for (key, value) in entries {
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
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn write_fake_nix_version_command(
    root: &Path,
    script_name: &str,
    version: &str,
) -> Result<PathBuf, String> {
    let script = root.join(script_name);
    let source = format!(
        "#!/bin/sh\nset -eu\nif [ \"$#\" -eq 1 ] && [ \"$1\" = \"--version\" ]; then\n  printf '%s\\n' {version}\n  exit 0\nfi\necho \"unexpected fake nix invocation: $*\" >&2\nexit 1\n",
        version = shell_quote(version)
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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn parse_json(source: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(source).map_err(|error| error.to_string())
}
