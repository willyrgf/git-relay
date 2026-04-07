use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::proof_support::cases::CaseDefinition;
use crate::proof_support::lab::{CaseReport, LabProfile, ProofLab, AUTHORITATIVE_REPO_ID};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P11",
        setup: "Prepare release matrix targets with one admitted candidate and one unadmitted target.",
        action: "Run release-manifest build + release report and assert floor status remains open without complete admitted evidence.",
        pass_criteria: &[
            "release manifest evidence is persisted",
            "missing or unadmitted targets keep floor status open",
            "host evidence persists per platform",
        ],
        fail_criteria: &[
            "release floor closes without complete machine-readable evidence",
        ],
        contract_refs: &[
            "RFC_PROOF_E2E_TEST.md#P11",
            "RFC_PROOF_E2E_TEST.md#9-machine-readable-git-conformance-evidence",
            "git-relay-rfc.md release admission fail-closed contract",
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
    let targets_manifest = lab
        .write_matrix_targets_fixture(
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
        .map_err(|error| error.to_string())?;

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
    let (manifest_open, missing_target_unadmitted) = parse_manifest_open(&manifest_build.stdout)?;

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
            let host_evidence = lab
                .state_root
                .join("release")
                .join("hosts")
                .join(match std::env::consts::OS {
                    "macos" => "macos.json",
                    "linux" => "linux.json",
                    other => return Err(format!("unsupported host platform {other}")),
                })
                .exists();
            (git_open, repo_open, blocking, host_evidence)
        } else {
            (false, false, false, false)
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

    report.details = json!({
        "manifest_build": manifest_build.summary(),
        "manifest_open": manifest_open,
        "missing_target_unadmitted": missing_target_unadmitted,
        "manifest_persisted": manifest_persisted,
        "manifest_latest": manifest_latest,
        "release_report": release_report.summary(),
        "git_floor_open": git_floor_open,
        "repo_manifest_open": repo_manifest_open,
        "blocking_reason_open": blocking_reason_open,
        "host_evidence_persisted": host_evidence_persisted,
        "provider_inputs_checked": provider_inputs_checked,
    });

    Ok(report)
}

fn parse_manifest_open(source: &str) -> Result<(bool, bool), String> {
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
    Ok((manifest_open, missing_target_unadmitted))
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
