use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::proof_support::cases::{CaseDefinition, STANDARD_CASE_ARTIFACTS};
use crate::proof_support::lab::{CaseReport, ProofLab};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P09",
        setup: "Create flake fixtures with supported literal shorthand and unsupported grammar variants.",
        action: "Run migrate-flake-inputs with fake nix executors to verify deterministic rewrite + validated relock fail-closed behavior.",
        required_assertions: &[
            "p09.first_rewrite.success",
            "p09.second_rewrite.success",
            "p09.deterministic_and_idempotent",
            "p09.unsupported_grammar.fail_closed",
            "p09.out_of_matrix_nix.fail_closed",
            "p09.scope_violation_restores_files",
            "p09.non_idempotent_relock_restores_files",
        ],
        required_artifacts: STANDARD_CASE_ARTIFACTS,
        pass_criteria: &[
            "supported literal rewrite is deterministic",
            "second rewrite is a no-op",
            "unsupported grammar and out-of-matrix versions fail closed",
            "scope-violation and non-idempotent relock failures restore original files",
        ],
        fail_criteria: &[
            "targeted relock proceeds outside validated matrix",
            "failed rewrite leaves partial mutations",
        ],
        contract_refs: &[
            "RFC_PROOF_E2E_TEST.md#P09",
            "git-relay-rfc.md migration fail-closed contract",
            "verification-plan results G + H",
        ],
        runner: run,
    }
}

fn run(lab: &mut ProofLab, _mode: ProofMode) -> Result<CaseReport, String> {
    let mut report = CaseReport::with_details(json!({}));
    let case_root = lab.case_root("P09").map_err(|error| error.to_string())?;

    let project_ok = case_root.join("flake-ok");
    init_flake_project(
        lab,
        &project_ok,
        migration_flake_source(),
        migration_lock_source(),
    )?;

    let after_lock = case_root.join("flake.lock.after");
    fs::write(&after_lock, migration_lock_after_source()).map_err(|error| error.to_string())?;

    let fake_nix = write_fake_nix_command(
        &case_root,
        "fake-nix-validated",
        "nix (Determinate Nix 3.0.0) 2.26.3",
        &after_lock,
        Some(&after_lock),
    )?;

    let first = lab
        .run_git_relay(
            &[
                "migrate-flake-inputs".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--flake".to_owned(),
                project_ok.display().to_string(),
                "--input-target".to_owned(),
                "nixpkgs=git+https".to_owned(),
                "--json".to_owned(),
            ],
            &[(
                "GIT_RELAY_NIX_BIN".to_owned(),
                fake_nix.display().to_string(),
            )],
        )
        .map_err(|error| error.to_string())?;

    let flake_after_first =
        fs::read_to_string(project_ok.join("flake.nix")).map_err(|error| error.to_string())?;
    let lock_after_first =
        fs::read_to_string(project_ok.join("flake.lock")).map_err(|error| error.to_string())?;

    let second = lab
        .run_git_relay(
            &[
                "migrate-flake-inputs".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--flake".to_owned(),
                project_ok.display().to_string(),
                "--input-target".to_owned(),
                "nixpkgs=git+https".to_owned(),
                "--allow-dirty".to_owned(),
                "--json".to_owned(),
            ],
            &[(
                "GIT_RELAY_NIX_BIN".to_owned(),
                fake_nix.display().to_string(),
            )],
        )
        .map_err(|error| error.to_string())?;

    let flake_after_second =
        fs::read_to_string(project_ok.join("flake.nix")).map_err(|error| error.to_string())?;
    let lock_after_second =
        fs::read_to_string(project_ok.join("flake.lock")).map_err(|error| error.to_string())?;

    let deterministic_rewrite = flake_after_first.contains("git+https://github.com/NixOS/nixpkgs")
        && flake_after_first == flake_after_second
        && lock_after_first == lock_after_second;

    let project_bad_grammar = case_root.join("flake-bad-grammar");
    init_flake_project(
        lab,
        &project_bad_grammar,
        unsupported_migration_flake_source(),
        migration_lock_source(),
    )?;
    let inspect_bad = lab
        .run_git_relay(
            &[
                "migration".to_owned(),
                "inspect".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--flake".to_owned(),
                project_bad_grammar.display().to_string(),
                "--input-target".to_owned(),
                "nixpkgs=git+https".to_owned(),
                "--json".to_owned(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;
    let unsupported_grammar_rejected = if !inspect_bad.success() {
        true
    } else {
        let parsed: serde_json::Value =
            serde_json::from_str(&inspect_bad.stdout).map_err(|error| error.to_string())?;
        let no_rewrites = parsed["planned_rewrites"]
            .as_array()
            .map(|items| items.is_empty())
            .unwrap_or(false);
        let unsupported_marked = parsed["direct_inputs"]
            .as_array()
            .map(|items| {
                items.iter().any(|entry| {
                    entry["state"] == "other_literal"
                        || entry["state"] == "blocked_no_policy"
                        || entry["blocked_reason"].is_string()
                })
            })
            .unwrap_or(false);
        no_rewrites && unsupported_marked
    };

    let project_bad_nix = case_root.join("flake-bad-nix");
    init_flake_project(
        lab,
        &project_bad_nix,
        migration_flake_source(),
        migration_lock_source(),
    )?;
    let flake_before_bad_nix =
        fs::read_to_string(project_bad_nix.join("flake.nix")).map_err(|error| error.to_string())?;
    let lock_before_bad_nix = fs::read_to_string(project_bad_nix.join("flake.lock"))
        .map_err(|error| error.to_string())?;
    let fake_nix_unsupported = write_fake_nix_command(
        &case_root,
        "fake-nix-unsupported",
        "nix (Nix) 2.32.0",
        &after_lock,
        Some(&after_lock),
    )?;
    let migrate_bad_nix = lab
        .run_git_relay(
            &[
                "migrate-flake-inputs".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--flake".to_owned(),
                project_bad_nix.display().to_string(),
                "--input-target".to_owned(),
                "nixpkgs=git+https".to_owned(),
                "--json".to_owned(),
            ],
            &[(
                "GIT_RELAY_NIX_BIN".to_owned(),
                fake_nix_unsupported.display().to_string(),
            )],
        )
        .map_err(|error| error.to_string())?;
    let flake_after_bad_nix =
        fs::read_to_string(project_bad_nix.join("flake.nix")).map_err(|error| error.to_string())?;
    let lock_after_bad_nix = fs::read_to_string(project_bad_nix.join("flake.lock"))
        .map_err(|error| error.to_string())?;
    let unsupported_nix_restored =
        flake_before_bad_nix == flake_after_bad_nix && lock_before_bad_nix == lock_after_bad_nix;

    let project_scope_violation = case_root.join("flake-scope-violation");
    init_flake_project(
        lab,
        &project_scope_violation,
        migration_flake_source(),
        migration_lock_source(),
    )?;
    let flake_before_scope = fs::read_to_string(project_scope_violation.join("flake.nix"))
        .map_err(|error| error.to_string())?;
    let lock_before_scope = fs::read_to_string(project_scope_violation.join("flake.lock"))
        .map_err(|error| error.to_string())?;
    let scope_violation_lock = case_root.join("flake.lock.scope-violation");
    fs::write(
        &scope_violation_lock,
        migration_lock_scope_violation_source(),
    )
    .map_err(|error| error.to_string())?;
    let fake_nix_scope_violation = write_fake_nix_command(
        &case_root,
        "fake-nix-scope-violation",
        "nix (Determinate Nix 3.0.0) 2.26.3",
        &scope_violation_lock,
        None,
    )?;
    let migrate_scope_violation = lab
        .run_git_relay(
            &[
                "migrate-flake-inputs".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--flake".to_owned(),
                project_scope_violation.display().to_string(),
                "--input-target".to_owned(),
                "nixpkgs=git+https".to_owned(),
                "--json".to_owned(),
            ],
            &[(
                "GIT_RELAY_NIX_BIN".to_owned(),
                fake_nix_scope_violation.display().to_string(),
            )],
        )
        .map_err(|error| error.to_string())?;
    let flake_after_scope = fs::read_to_string(project_scope_violation.join("flake.nix"))
        .map_err(|error| error.to_string())?;
    let lock_after_scope = fs::read_to_string(project_scope_violation.join("flake.lock"))
        .map_err(|error| error.to_string())?;
    let scope_violation_restored = !migrate_scope_violation.success()
        && flake_before_scope == flake_after_scope
        && lock_before_scope == lock_after_scope;

    let project_non_idempotent = case_root.join("flake-non-idempotent");
    init_flake_project(
        lab,
        &project_non_idempotent,
        migration_flake_source(),
        migration_lock_source(),
    )?;
    let flake_before_non_idempotent = fs::read_to_string(project_non_idempotent.join("flake.nix"))
        .map_err(|error| error.to_string())?;
    let lock_before_non_idempotent = fs::read_to_string(project_non_idempotent.join("flake.lock"))
        .map_err(|error| error.to_string())?;
    let non_idempotent_first_lock = case_root.join("flake.lock.non-idempotent.first");
    let non_idempotent_second_lock = case_root.join("flake.lock.non-idempotent.second");
    fs::write(&non_idempotent_first_lock, migration_lock_after_source())
        .map_err(|error| error.to_string())?;
    fs::write(
        &non_idempotent_second_lock,
        migration_lock_non_idempotent_source(),
    )
    .map_err(|error| error.to_string())?;
    let fake_nix_non_idempotent = write_fake_nix_command(
        &case_root,
        "fake-nix-non-idempotent",
        "nix (Determinate Nix 3.0.0) 2.26.3",
        &non_idempotent_first_lock,
        Some(&non_idempotent_second_lock),
    )?;
    let migrate_non_idempotent = lab
        .run_git_relay(
            &[
                "migrate-flake-inputs".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--flake".to_owned(),
                project_non_idempotent.display().to_string(),
                "--input-target".to_owned(),
                "nixpkgs=git+https".to_owned(),
                "--json".to_owned(),
            ],
            &[(
                "GIT_RELAY_NIX_BIN".to_owned(),
                fake_nix_non_idempotent.display().to_string(),
            )],
        )
        .map_err(|error| error.to_string())?;
    let flake_after_non_idempotent = fs::read_to_string(project_non_idempotent.join("flake.nix"))
        .map_err(|error| error.to_string())?;
    let lock_after_non_idempotent = fs::read_to_string(project_non_idempotent.join("flake.lock"))
        .map_err(|error| error.to_string())?;
    let non_idempotent_restored = !migrate_non_idempotent.success()
        && flake_before_non_idempotent == flake_after_non_idempotent
        && lock_before_non_idempotent == lock_after_non_idempotent;

    report.assertions.push(if first.success() {
        ProofAssertion::pass(
            "p09.first_rewrite.success",
            Some("initial migrate-flake-inputs succeeded".to_owned()),
        )
    } else {
        ProofAssertion::fail("p09.first_rewrite.success", first.summary())
    });
    report.assertions.push(if second.success() {
        ProofAssertion::pass(
            "p09.second_rewrite.success",
            Some("second migrate-flake-inputs run completed".to_owned()),
        )
    } else {
        ProofAssertion::fail("p09.second_rewrite.success", second.summary())
    });
    report.assertions.push(if deterministic_rewrite {
        ProofAssertion::pass(
            "p09.deterministic_and_idempotent",
            Some("rewrite and relock were deterministic and idempotent".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p09.deterministic_and_idempotent",
            "rewrite or relock output differed across repeated run",
        )
    });
    report.assertions.push(if unsupported_grammar_rejected {
        ProofAssertion::pass(
            "p09.unsupported_grammar.fail_closed",
            Some(
                "unsupported grammar remained non-rewritable and did not proceed silently"
                    .to_owned(),
            ),
        )
    } else {
        ProofAssertion::fail(
            "p09.unsupported_grammar.fail_closed",
            "unsupported grammar was treated as rewritable migration input",
        )
    });
    report.assertions.push(if !migrate_bad_nix.success() && unsupported_nix_restored {
        ProofAssertion::pass(
            "p09.out_of_matrix_nix.fail_closed",
            Some("targeted relock refused unsupported nix version and restored original flake files".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p09.out_of_matrix_nix.fail_closed",
            format!(
                "targeted relock did not fail closed cleanly: success={} restored={unsupported_nix_restored}",
                migrate_bad_nix.success()
            ),
        )
    });
    report.assertions.push(if scope_violation_restored {
        ProofAssertion::pass(
            "p09.scope_violation_restores_files",
            Some("scope-violation relock failure restored the original flake files".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p09.scope_violation_restores_files",
            "scope-violation relock failure left mutated flake sources behind",
        )
    });
    report.assertions.push(if non_idempotent_restored {
        ProofAssertion::pass(
            "p09.non_idempotent_relock_restores_files",
            Some("non-idempotent relock failure restored the original flake files".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p09.non_idempotent_relock_restores_files",
            "non-idempotent relock failure left mutated flake sources behind",
        )
    });

    report.details = json!({
        "first": first.summary(),
        "second": second.summary(),
        "deterministic_rewrite": deterministic_rewrite,
        "unsupported_grammar_rejected": unsupported_grammar_rejected,
        "unsupported_nix_rejected": !migrate_bad_nix.success(),
        "unsupported_nix_restored": unsupported_nix_restored,
        "scope_violation": migrate_scope_violation.summary(),
        "scope_violation_restored": scope_violation_restored,
        "non_idempotent": migrate_non_idempotent.summary(),
        "non_idempotent_restored": non_idempotent_restored,
    });

    Ok(report)
}

fn init_flake_project(
    lab: &ProofLab,
    project: &Path,
    flake_source: &str,
    lock_source: &str,
) -> Result<(), String> {
    if project.exists() {
        fs::remove_dir_all(project).map_err(|error| error.to_string())?;
    }
    lab.init_work_repo(project)
        .map_err(|error| error.to_string())?;
    fs::write(project.join("flake.nix"), flake_source).map_err(|error| error.to_string())?;
    fs::write(project.join("flake.lock"), lock_source).map_err(|error| error.to_string())?;
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            project.display().to_string(),
            "add".to_owned(),
            "flake.nix".to_owned(),
            "flake.lock".to_owned(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            project.display().to_string(),
            "commit".to_owned(),
            "-m".to_owned(),
            "seed flake fixture".to_owned(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn write_fake_nix_command(
    root: &Path,
    script_name: &str,
    version: &str,
    first_lock: &Path,
    second_lock: Option<&Path>,
) -> Result<PathBuf, String> {
    let script = root.join(script_name);
    let log_path = root.join(format!("{script_name}.log"));
    let counter_path = root.join(format!("{script_name}.count"));
    let second_lock = second_lock.unwrap_or(first_lock);
    let source = format!(
        "#!/bin/sh\nset -eu\nlog_path={log_path}\ncounter_path={counter_path}\nfirst_lock={first_lock}\nsecond_lock={second_lock}\nprintf '%s\\n' \"$*\" >> \"$log_path\"\nif [ \"$#\" -eq 1 ] && [ \"$1\" = \"--version\" ]; then\n  printf '%s\\n' {version}\n  exit 0\nfi\nif [ \"$#\" -eq 3 ] && [ \"$1\" = \"flake\" ] && [ \"$2\" = \"update\" ]; then\n  count=0\n  if [ -f \"$counter_path\" ]; then\n    count=$(cat \"$counter_path\")\n  fi\n  count=$((count + 1))\n  printf '%s' \"$count\" > \"$counter_path\"\n  if [ \"$count\" -eq 1 ]; then\n    cp \"$first_lock\" \"$PWD/flake.lock\"\n  else\n    cp \"$second_lock\" \"$PWD/flake.lock\"\n  fi\n  exit 0\nfi\necho \"unexpected fake nix invocation: $*\" >&2\nexit 1\n",
        log_path = shell_quote(&log_path),
        counter_path = shell_quote(&counter_path),
        first_lock = shell_quote(first_lock),
        second_lock = shell_quote(second_lock),
        version = shell_quote(Path::new(version)),
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

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\"'\"'"))
}

fn migration_flake_source() -> &'static str {
    r#"
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.overlay.url = "git+https://example.com/overlay";

  outputs = { self, nixpkgs, overlay }: { };
}
"#
}

fn unsupported_migration_flake_source() -> &'static str {
    r#"
{
  inputs.nixpkgs.url = "github:NixOS/${nixpkgs}";
  outputs = { self, nixpkgs }: { };
}
"#
}

fn migration_lock_source() -> &'static str {
    r#"
{
  "nodes": {
    "root": {
      "inputs": {
        "nixpkgs": "nixpkgs",
        "overlay": "overlay"
      }
    },
    "nixpkgs": {
      "inputs": {
        "indirect": "indirect"
      },
      "locked": {
        "type": "github",
        "owner": "NixOS",
        "repo": "nixpkgs",
        "rev": "oldrev"
      },
      "original": {
        "type": "github",
        "owner": "NixOS",
        "repo": "nixpkgs",
        "ref": "nixos-unstable"
      }
    },
    "overlay": {
      "inputs": {
        "nixpkgs": ["nixpkgs"]
      },
      "locked": {
        "type": "git",
        "url": "git+https://example.com/overlay",
        "rev": "overlayrev"
      },
      "original": {
        "type": "git",
        "url": "git+https://example.com/overlay"
      }
    },
    "indirect": {
      "inputs": {},
      "locked": {
        "type": "github",
        "owner": "example",
        "repo": "transitive",
        "rev": "indirectrev"
      },
      "original": {
        "type": "github",
        "owner": "example",
        "repo": "transitive"
      }
    }
  },
  "root": "root",
  "version": 7
}
"#
}

fn migration_lock_after_source() -> &'static str {
    r#"
{
  "nodes": {
    "root": {
      "inputs": {
        "nixpkgs": "nixpkgs",
        "overlay": "overlay"
      }
    },
    "nixpkgs": {
      "inputs": {
        "indirect": "indirect"
      },
      "locked": {
        "type": "git",
        "url": "git+https://github.com/NixOS/nixpkgs?ref=nixos-unstable",
        "rev": "newrev"
      },
      "original": {
        "type": "git",
        "url": "git+https://github.com/NixOS/nixpkgs?ref=nixos-unstable"
      }
    },
    "overlay": {
      "inputs": {
        "nixpkgs": ["nixpkgs"]
      },
      "locked": {
        "type": "git",
        "url": "git+https://example.com/overlay",
        "rev": "overlayrev"
      },
      "original": {
        "type": "git",
        "url": "git+https://example.com/overlay"
      }
    },
    "indirect": {
      "inputs": {},
      "locked": {
        "type": "github",
        "owner": "example",
        "repo": "transitive",
        "rev": "indirectrev"
      },
      "original": {
        "type": "github",
        "owner": "example",
        "repo": "transitive"
      }
    }
  },
  "root": "root",
  "version": 7
}
"#
}

fn migration_lock_scope_violation_source() -> &'static str {
    r#"
{
  "nodes": {
    "root": {
      "inputs": {
        "nixpkgs": "nixpkgs",
        "overlay": "overlay"
      }
    },
    "nixpkgs": {
      "inputs": {
        "indirect": "indirect"
      },
      "locked": {
        "type": "git",
        "url": "git+https://github.com/NixOS/nixpkgs?ref=nixos-unstable",
        "rev": "newrev"
      },
      "original": {
        "type": "git",
        "url": "git+https://github.com/NixOS/nixpkgs?ref=nixos-unstable"
      }
    },
    "overlay": {
      "inputs": {
        "nixpkgs": ["nixpkgs"]
      },
      "locked": {
        "type": "git",
        "url": "git+https://example.com/overlay",
        "rev": "overlayrev-violated"
      },
      "original": {
        "type": "git",
        "url": "git+https://example.com/overlay"
      }
    },
    "indirect": {
      "inputs": {},
      "locked": {
        "type": "github",
        "owner": "example",
        "repo": "transitive",
        "rev": "indirectrev"
      },
      "original": {
        "type": "github",
        "owner": "example",
        "repo": "transitive"
      }
    }
  },
  "root": "root",
  "version": 7
}
"#
}

fn migration_lock_non_idempotent_source() -> &'static str {
    r#"
{
  "nodes": {
    "root": {
      "inputs": {
        "nixpkgs": "nixpkgs",
        "overlay": "overlay"
      }
    },
    "nixpkgs": {
      "inputs": {
        "indirect": "indirect"
      },
      "locked": {
        "type": "git",
        "url": "git+https://github.com/NixOS/nixpkgs?ref=nixos-unstable",
        "rev": "newrev-second"
      },
      "original": {
        "type": "git",
        "url": "git+https://github.com/NixOS/nixpkgs?ref=nixos-unstable"
      }
    },
    "overlay": {
      "inputs": {
        "nixpkgs": ["nixpkgs"]
      },
      "locked": {
        "type": "git",
        "url": "git+https://example.com/overlay",
        "rev": "overlayrev"
      },
      "original": {
        "type": "git",
        "url": "git+https://example.com/overlay"
      }
    },
    "indirect": {
      "inputs": {},
      "locked": {
        "type": "github",
        "owner": "example",
        "repo": "transitive",
        "rev": "indirectrev"
      },
      "original": {
        "type": "github",
        "owner": "example",
        "repo": "transitive"
      }
    }
  },
  "root": "root",
  "version": 7
}
"#
}
