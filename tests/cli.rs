use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use git_relay::audit::structured_log_file_path;
use git_relay::hooks::push_trace_file_path;
use git_relay::platform::{PlatformProbe, RealPlatformProbe};
use git_relay::reconcile::pending_request_file_path;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

fn init_bare_repo(path: &Path) {
    StdCommand::new("git")
        .args(["-c", "init.defaultBranch=main", "init", "--bare"])
        .arg(path)
        .status()
        .expect("git init")
        .success()
        .then_some(())
        .expect("git init success");
}

fn configure_authoritative_repo(path: &Path) {
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
        StdCommand::new("git")
            .arg(format!("--git-dir={}", path.display()))
            .args(["config", key, value])
            .status()
            .expect("git config")
            .success()
            .then_some(())
            .expect("git config success");
    }
}

fn detect_filesystem(path: &Path) -> String {
    RealPlatformProbe
        .filesystem_type(path)
        .expect("filesystem type")
}

fn write_config_fixture(temp: &TempDir) -> PathBuf {
    let repo_root = temp.path().join("repos");
    let repo_config_root = temp.path().join("repos.d");
    fs::create_dir_all(&repo_root).expect("repo root");
    fs::create_dir_all(&repo_config_root).expect("repo config root");

    let filesystem = detect_filesystem(temp.path());
    let platform = match std::env::consts::OS {
        "macos" => "macos",
        "linux" => "linux",
        other => panic!("unsupported host {other}"),
    };
    let service_manager = match std::env::consts::OS {
        "macos" => "launchd",
        "linux" => "systemd",
        other => panic!("unsupported host {other}"),
    };

    let config_path = temp.path().join("config.toml");
    let config = format!(
        r#"
[listen]
ssh = "127.0.0.1:4222"
https = "127.0.0.1:4318"
enable_http_read = false
enable_http_write = false

[paths]
state_root = "{}"
repo_root = "{}"
repo_config_root = "{}"

[reconcile]
on_push = true
manual_enabled = true
periodic_enabled = false
worker_mode = "short-lived"
lock_timeout_ms = 5000

[policy]
default_repo_mode = "cache-only"
default_refresh = "ttl:60s"
negative_cache_ttl = "5s"
default_push_ack = "local-commit"

[migration]
supported_targets = ["git+https", "git+ssh"]
refuse_dirty_worktree = true
targeted_relock_mode = "validated-only"

[deployment]
platform = "{platform}"
service_manager = "{service_manager}"
service_label = "dev.git-relay"
git_only_command_mode = "openssh-force-command"
forced_command_wrapper = "/usr/local/bin/git-relay-ssh-force-command"
disable_forwarding = true
runtime_secret_env_file = "{env_file}"
required_secret_keys = ["GITHUB_READ_TOKEN", "GITHUB_WRITE_KEY"]
allowed_git_services = ["git-upload-pack", "git-receive-pack"]
supported_filesystems = ["{filesystem}"]

[auth_profiles.github-read]
kind = "https-token"
secret_ref = "env:GITHUB_READ_TOKEN"

[auth_profiles.github-write]
kind = "ssh-key"
secret_ref = "env:GITHUB_WRITE_KEY"
"#,
        temp.path().display(),
        repo_root.display(),
        repo_config_root.display(),
        env_file = temp.path().join("git-relay.env").display(),
    );
    fs::write(&config_path, config).expect("config");
    fs::write(
        temp.path().join("git-relay.env"),
        "GITHUB_READ_TOKEN=alpha\nGITHUB_WRITE_KEY=beta\n",
    )
    .expect("env file");
    config_path
}

fn write_authoritative_descriptor(
    temp: &TempDir,
    repo_path: &Path,
    include_read_upstream: bool,
) -> PathBuf {
    let descriptor = if include_read_upstream {
        format!(
            r#"
repo_id = "github.com/example/repo.git"
canonical_identity = "github.com/example/repo.git"
repo_path = "{}"
mode = "authoritative"
lifecycle = "ready"
authority_model = "relay-authoritative"
tracking_refs = "same-repo-hidden"
refresh = "authoritative-local"
push_ack = "local-commit"
reconcile_policy = "on-push+manual"
exported_refs = ["refs/heads/*", "refs/tags/*"]

[[read_upstreams]]
name = "github-read"
url = "https://github.com/example/repo.git"
auth_profile = "github-read"

[[write_upstreams]]
name = "github-write"
url = "ssh://git@github.com/example/repo.git"
auth_profile = "github-write"
require_atomic = true
"#,
            repo_path.display()
        )
    } else {
        format!(
            r#"
repo_id = "github.com/example/repo.git"
canonical_identity = "github.com/example/repo.git"
repo_path = "{}"
mode = "authoritative"
lifecycle = "ready"
authority_model = "relay-authoritative"
tracking_refs = "same-repo-hidden"
refresh = "authoritative-local"
push_ack = "local-commit"
reconcile_policy = "on-push+manual"
exported_refs = ["refs/heads/*", "refs/tags/*"]

[[write_upstreams]]
name = "github-write"
url = "ssh://git@github.com/example/repo.git"
auth_profile = "github-write"
require_atomic = true
"#,
            repo_path.display()
        )
    };
    let path = temp.path().join("repos.d").join("repo.toml");
    fs::write(&path, descriptor).expect("descriptor");
    path
}

fn write_authoritative_descriptor_with_custom_read_upstream(
    temp: &TempDir,
    repo_path: &Path,
    read_upstream_url: Option<&str>,
) -> PathBuf {
    let mut descriptor = format!(
        r#"
repo_id = "github.com/example/repo.git"
canonical_identity = "github.com/example/repo.git"
repo_path = "{}"
mode = "authoritative"
lifecycle = "ready"
authority_model = "relay-authoritative"
tracking_refs = "same-repo-hidden"
refresh = "authoritative-local"
push_ack = "local-commit"
reconcile_policy = "on-push+manual"
exported_refs = ["refs/heads/*", "refs/tags/*"]
"#,
        repo_path.display()
    );
    if let Some(read_upstream_url) = read_upstream_url {
        descriptor.push_str(&format!(
            r#"

[[read_upstreams]]
name = "github-read"
url = "{read_upstream_url}"
auth_profile = "github-read"
"#
        ));
    }
    descriptor.push_str(
        r#"

[[write_upstreams]]
name = "github-write"
url = "ssh://git@github.com/example/repo.git"
auth_profile = "github-write"
require_atomic = true
"#,
    );

    let path = temp.path().join("repos.d").join("repo.toml");
    fs::write(&path, descriptor).expect("descriptor");
    path
}

fn write_authoritative_descriptor_with_write_upstreams(
    temp: &TempDir,
    repo_path: &Path,
    write_upstreams: &[(&str, &str, bool)],
) -> PathBuf {
    let mut descriptor = format!(
        r#"
repo_id = "github.com/example/repo.git"
canonical_identity = "github.com/example/repo.git"
repo_path = "{}"
mode = "authoritative"
lifecycle = "ready"
authority_model = "relay-authoritative"
tracking_refs = "same-repo-hidden"
refresh = "authoritative-local"
push_ack = "local-commit"
reconcile_policy = "on-push+manual"
exported_refs = ["refs/heads/*", "refs/tags/*"]
"#,
        repo_path.display()
    );
    for (name, url, require_atomic) in write_upstreams {
        descriptor.push_str(&format!(
            r#"

[[write_upstreams]]
name = "{name}"
url = "{url}"
auth_profile = "github-write"
require_atomic = {require_atomic}
"#
        ));
    }

    let path = temp.path().join("repos.d").join("repo.toml");
    fs::write(&path, descriptor).expect("descriptor");
    path
}

fn write_cache_only_descriptor(
    temp: &TempDir,
    repo_path: &Path,
    refresh: &str,
    read_upstream_url: &str,
) -> PathBuf {
    let descriptor = format!(
        r#"
repo_id = "github.com/example/cache.git"
canonical_identity = "github.com/example/cache.git"
repo_path = "{}"
mode = "cache-only"
lifecycle = "ready"
authority_model = "upstream-source"
tracking_refs = "same-repo-hidden"
refresh = "{refresh}"
push_ack = "local-commit"
reconcile_policy = "on-push+manual"
exported_refs = ["refs/heads/*", "refs/tags/*"]

[[read_upstreams]]
name = "github-read"
url = "{read_upstream_url}"
auth_profile = "github-read"
"#,
        repo_path.display()
    );
    let path = temp.path().join("repos.d").join("cache.toml");
    fs::write(&path, descriptor).expect("descriptor");
    path
}

fn write_matrix_targets_fixture(
    temp: &TempDir,
    file_name: &str,
    targets: &[(&str, &str, &str, &str, &str, bool, bool)],
) -> PathBuf {
    let mut encoded_targets = Vec::new();
    for (target_id, product, class, transport, url, require_atomic, same_repo_hidden_refs) in
        targets
    {
        let host_key_policy = match *transport {
            "ssh" => "pinned-known-hosts",
            "smart-http" => "not-applicable",
            other => panic!("unsupported transport {other}"),
        };
        encoded_targets.push(serde_json::json!({
            "target_id": target_id,
            "product": product,
            "class": class,
            "transport": transport,
            "url": url,
            "credential_source": format!("env:{}_CREDENTIAL", target_id.to_uppercase()),
            "host_key_policy": host_key_policy,
            "require_atomic": require_atomic,
            "same_repo_hidden_refs": same_repo_hidden_refs,
        }));
    }

    let path = temp.path().join(file_name);
    fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "targets": encoded_targets,
        }))
        .expect("manifest json"),
    )
    .expect("manifest");
    path
}

fn init_work_repo(path: &Path) {
    fs::create_dir_all(path).expect("work repo");
    StdCommand::new("git")
        .args(["-c", "init.defaultBranch=main", "init"])
        .arg(path)
        .status()
        .expect("git init")
        .success()
        .then_some(())
        .expect("git init success");
    StdCommand::new("git")
        .args([
            "-C",
            path.to_str().expect("path"),
            "config",
            "user.name",
            "Git Relay Test",
        ])
        .status()
        .expect("git config")
        .success()
        .then_some(())
        .expect("git config success");
    StdCommand::new("git")
        .args([
            "-C",
            path.to_str().expect("path"),
            "config",
            "user.email",
            "git-relay@example.com",
        ])
        .status()
        .expect("git config")
        .success()
        .then_some(())
        .expect("git config success");
}

fn commit_file(path: &Path, file_name: &str, contents: &str, message: &str) {
    fs::write(path.join(file_name), contents).expect("write file");
    StdCommand::new("git")
        .args(["-C", path.to_str().expect("path"), "add", file_name])
        .status()
        .expect("git add")
        .success()
        .then_some(())
        .expect("git add success");
    StdCommand::new("git")
        .args(["-C", path.to_str().expect("path"), "commit", "-m", message])
        .status()
        .expect("git commit")
        .success()
        .then_some(())
        .expect("git commit success");
}

fn cargo_bin_path(name: &str) -> PathBuf {
    let command = Command::cargo_bin(name).expect("cargo bin");
    PathBuf::from(command.get_program())
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\"'\"'"))
}

fn write_fake_ssh_command(temp: &TempDir, config_path: &Path) -> PathBuf {
    let script = temp.path().join("fake-ssh");
    let wrapper = cargo_bin_path("git-relay-ssh-force-command");
    let source = format!(
        "#!/bin/sh\nset -eu\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -o|-i|-p|-l|-S|-F|-J|-E|-c|-m)\n      shift 2\n      ;;\n    -T|-n|-N|-4|-6|-a|-A|-q|-v|-vv|-vvv|-x|-X|-Y|-y|-C|-f|-G)\n      shift\n      ;;\n    --)\n      shift\n      break\n      ;;\n    -*)\n      shift\n      ;;\n    *)\n      break\n      ;;\n  esac\ndone\nif [ \"$#\" -lt 2 ]; then\n  echo \"fake ssh expected host and remote command\" >&2\n  exit 1\nfi\nhost=\"$1\"\nshift\nSSH_ORIGINAL_COMMAND=\"$*\" exec {wrapper} --config {config}\n",
        wrapper = shell_quote(&wrapper),
        config = shell_quote(config_path),
    );
    fs::write(&script, source).expect("fake ssh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&script)
            .expect("fake ssh metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("fake ssh chmod");
    }
    script
}

fn write_flake_project_fixture(temp: &TempDir, dir_name: &str, flake: &str, lock: &str) -> PathBuf {
    let project = temp.path().join(dir_name);
    init_work_repo(&project);
    fs::write(project.join("flake.nix"), flake).expect("write flake.nix");
    fs::write(project.join("flake.lock"), lock).expect("write flake.lock");
    StdCommand::new("git")
        .args([
            "-C",
            project.to_str().expect("project"),
            "add",
            "flake.nix",
            "flake.lock",
        ])
        .status()
        .expect("git add")
        .success()
        .then_some(())
        .expect("git add success");
    StdCommand::new("git")
        .args([
            "-C",
            project.to_str().expect("project"),
            "commit",
            "-m",
            "fixture",
        ])
        .status()
        .expect("git commit")
        .success()
        .then_some(())
        .expect("git commit success");
    project
}

fn write_fake_nix_command(
    temp: &TempDir,
    script_name: &str,
    version: &str,
    first_lock: &Path,
    second_lock: Option<&Path>,
) -> (PathBuf, PathBuf) {
    let script = temp.path().join(script_name);
    let log_path = temp.path().join(format!("{script_name}.log"));
    let counter_path = temp.path().join(format!("{script_name}.count"));
    let second_lock = second_lock.unwrap_or(first_lock);
    let source = format!(
        "#!/bin/sh\nset -eu\nlog_path={log_path}\ncounter_path={counter_path}\nfirst_lock={first_lock}\nsecond_lock={second_lock}\nprintf '%s\\n' \"$*\" >> \"$log_path\"\nif [ \"$#\" -eq 1 ] && [ \"$1\" = \"--version\" ]; then\n  printf '%s\\n' {version}\n  exit 0\nfi\nif [ \"$#\" -eq 3 ] && [ \"$1\" = \"flake\" ] && [ \"$2\" = \"update\" ]; then\n  count=0\n  if [ -f \"$counter_path\" ]; then\n    count=$(cat \"$counter_path\")\n  fi\n  count=$((count + 1))\n  printf '%s' \"$count\" > \"$counter_path\"\n  if [ \"$count\" -eq 1 ]; then\n    cp \"$first_lock\" \"$PWD/flake.lock\"\n  else\n    cp \"$second_lock\" \"$PWD/flake.lock\"\n  fi\n  exit 0\nfi\necho \"unexpected fake nix invocation: $*\" >&2\nexit 1\n",
        log_path = shell_quote(&log_path),
        counter_path = shell_quote(&counter_path),
        first_lock = shell_quote(first_lock),
        second_lock = shell_quote(second_lock),
        version = shell_quote(Path::new(version)),
    );
    fs::write(&script, source).expect("write fake nix");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&script)
            .expect("fake nix metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("fake nix chmod");
    }
    (script, log_path)
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

fn package_example_config_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("packaging")
        .join("example")
        .join("git-relay.example.toml")
}

fn read_push_trace(state_root: &Path, repo_id: &str, push_id: &str) -> Vec<Value> {
    let path = push_trace_file_path(state_root, repo_id, push_id);
    let source = fs::read_to_string(path).expect("push trace");
    source
        .lines()
        .map(|line| serde_json::from_str(line).expect("trace json"))
        .collect()
}

fn read_structured_logs(state_root: &Path) -> Vec<Value> {
    let path = structured_log_file_path(state_root);
    let source = fs::read_to_string(path).expect("structured logs");
    source
        .lines()
        .map(|line| serde_json::from_str(line).expect("structured log json"))
        .collect()
}

fn sanitize_repo_state_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "unknown".to_owned()
    } else {
        sanitized
    }
}

fn read_git_ref(repo_path: &Path, ref_name: &str) -> String {
    let output = StdCommand::new("git")
        .arg(format!("--git-dir={}", repo_path.display()))
        .args(["rev-parse", ref_name])
        .output()
        .expect("git rev-parse");
    assert!(output.status.success(), "git rev-parse failed");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn read_worktree_ref(repo_path: &Path, ref_name: &str) -> String {
    let output = StdCommand::new("git")
        .args([
            "-C",
            repo_path.to_str().expect("path"),
            "rev-parse",
            ref_name,
        ])
        .output()
        .expect("git rev-parse");
    assert!(output.status.success(), "git rev-parse failed");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn git_ref_exists(repo_path: &Path, ref_name: &str) -> bool {
    StdCommand::new("git")
        .arg(format!("--git-dir={}", repo_path.display()))
        .args(["rev-parse", "--verify", "--quiet", ref_name])
        .status()
        .expect("git rev-parse")
        .success()
}

fn assert_git_fsck_clean(repo_path: &Path) {
    StdCommand::new("git")
        .arg(format!("--git-dir={}", repo_path.display()))
        .args(["fsck", "--strict"])
        .status()
        .expect("git fsck")
        .success()
        .then_some(())
        .expect("git fsck success");
}

fn assert_no_probe_refs(repo_path: &Path) {
    let output = StdCommand::new("git")
        .arg(format!("--git-dir={}", repo_path.display()))
        .args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads/git-relay-probe",
            "refs/tags/git-relay-probe-",
        ])
        .output()
        .expect("git for-each-ref");
    assert!(output.status.success(), "git for-each-ref failed");
    let refs = String::from_utf8_lossy(&output.stdout);
    assert!(
        refs.trim().is_empty(),
        "remote probe refs should be cleaned up, found: {refs}"
    );
}

fn parse_single_report(output: &[u8]) -> Value {
    let reports = serde_json::from_slice::<Vec<Value>>(output).expect("json reports");
    assert_eq!(reports.len(), 1, "expected exactly one report");
    reports.into_iter().next().expect("one report")
}

#[test]
fn repo_validate_returns_json_for_valid_authoritative_repo() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);
    write_authoritative_descriptor(&temp, &repo_path, true);

    let mut command = Command::cargo_bin("git-relay").expect("cargo bin");
    command
        .args([
            "repo",
            "validate",
            "--config",
            config_path.to_str().expect("config path"),
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"passed\""))
        .stdout(predicate::str::contains(
            "\"write_acceptance_allowed\": true",
        ));
}

#[test]
fn repo_inspect_reports_descriptor_validation_and_replication_state() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);
    write_authoritative_descriptor(&temp, &repo_path, false);

    let output = Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "repo",
            "inspect",
            "--config",
            config_path.to_str().expect("config path"),
            "--repo",
            "github.com/example/repo.git",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report = parse_single_report(&output);
    assert_eq!(report["descriptor"]["mode"], "authoritative");
    assert_eq!(report["validation"]["status"], "passed");
    assert_eq!(report["startup"]["safety"], "degraded");
    assert!(report["divergence_markers"]
        .as_array()
        .expect("divergence markers")
        .is_empty());
    assert!(report["replication"]["pending_request"].is_null());
    assert!(report["replication"]["latest_run"].is_null());
}

#[test]
fn doctor_reports_runtime_and_repository_health() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);
    write_authoritative_descriptor(&temp, &repo_path, false);

    let output = Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "doctor",
            "--config",
            config_path.to_str().expect("config path"),
            "--repo",
            "github.com/example/repo.git",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).expect("doctor json");
    assert_eq!(report["runtime_validation"]["status"], "passed");
    let repositories = report["repositories"].as_array().expect("repositories");
    assert_eq!(repositories.len(), 1);
    assert_eq!(repositories[0]["validation"]["status"], "passed");
    assert_eq!(repositories[0]["startup"]["safety"], "degraded");
}

#[test]
fn startup_classify_fails_closed_for_invalid_authoritative_repo() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    init_bare_repo(&repo_path);
    write_authoritative_descriptor(&temp, &repo_path, false);

    let mut command = Command::cargo_bin("git-relay").expect("cargo bin");
    command
        .args([
            "startup",
            "classify",
            "--config",
            config_path.to_str().expect("config path"),
            "--json",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("\"safety\": \"quarantined\""))
        .stdout(predicate::str::contains(
            "\"write_acceptance_allowed\": false",
        ));
}

#[test]
fn deploy_validate_runtime_reports_secret_and_contract_health() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);
    write_authoritative_descriptor(&temp, &repo_path, false);

    let mut command = Command::cargo_bin("git-relay").expect("cargo bin");
    command
        .args([
            "deploy",
            "validate-runtime",
            "--config",
            config_path.to_str().expect("config path"),
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"passed\""))
        .stdout(predicate::str::contains("\"secret_count\": 2"));
}

#[test]
fn git_relayd_serve_once_fails_closed_when_runtime_secrets_are_missing() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    fs::remove_file(temp.path().join("git-relay.env")).expect("remove env file");

    let mut command = Command::cargo_bin("git-relayd").expect("cargo bin");
    command
        .args([
            "serve",
            "--config",
            config_path.to_str().expect("config path"),
            "--once",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("runtime_secret_env_file"));
}

#[test]
fn ssh_force_command_accepts_only_git_pack_services_under_repo_root() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_root = temp.path().join("repos");
    let repo_path = repo_root.join("example.git");
    init_bare_repo(&repo_path);

    write_authoritative_descriptor(&temp, &repo_path, false);
    configure_authoritative_repo(&repo_path);

    let mut command = Command::cargo_bin("git-relay-ssh-force-command").expect("cargo bin");
    command
        .env("SSH_ORIGINAL_COMMAND", "git-receive-pack example.git")
        .args([
            "--config",
            config_path.to_str().expect("config"),
            "--check-only",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"service\": \"git-receive-pack\"",
        ))
        .stdout(predicate::str::contains(
            repo_path.to_str().expect("repo path"),
        ))
        .stdout(predicate::str::contains("\"repo_mode\": \"authoritative\""));
}

#[test]
fn authoritative_upload_pack_serves_local_refs_without_read_upstream_refresh() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);
    let missing_read_upstream = temp.path().join("missing-read-upstream.git");
    write_authoritative_descriptor_with_custom_read_upstream(
        &temp,
        &repo_path,
        Some(missing_read_upstream.to_str().expect("path")),
    );

    let work_repo = temp.path().join("work-authoritative-read");
    init_work_repo(&work_repo);
    commit_file(&work_repo, "README.md", "hello\n", "initial");
    StdCommand::new("git")
        .args([
            "-C",
            work_repo.to_str().expect("work repo"),
            "push",
            repo_path.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git push")
        .success()
        .then_some(())
        .expect("authoritative push success");

    let fake_ssh = write_fake_ssh_command(&temp, &config_path);
    let output = StdCommand::new("git")
        .env("GIT_SSH", &fake_ssh)
        .args(["ls-remote", "relay:repo.git", "refs/heads/main"])
        .output()
        .expect("git ls-remote");
    assert!(
        output.status.success(),
        "authoritative upload-pack should succeed without consulting the broken read upstream"
    );

    let local_main = read_git_ref(&repo_path, "refs/heads/main");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&local_main),
        "ls-remote should advertise the locally accepted authoritative ref"
    );
}

#[test]
fn cache_only_upload_pack_refreshes_before_serving_refs() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);

    let upstream_path = temp.path().join("upstream-read.git");
    init_bare_repo(&upstream_path);
    let upstream_work = temp.path().join("work-upstream-read");
    init_work_repo(&upstream_work);
    commit_file(&upstream_work, "README.md", "upstream\n", "initial");
    StdCommand::new("git")
        .args([
            "-C",
            upstream_work.to_str().expect("work repo"),
            "push",
            upstream_path.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git push")
        .success()
        .then_some(())
        .expect("upstream push success");

    let cache_repo = temp.path().join("repos").join("cache.git");
    init_bare_repo(&cache_repo);
    write_cache_only_descriptor(
        &temp,
        &cache_repo,
        "always-refresh",
        upstream_path.to_str().expect("path"),
    );

    let fake_ssh = write_fake_ssh_command(&temp, &config_path);
    let output = StdCommand::new("git")
        .env("GIT_SSH", &fake_ssh)
        .args(["ls-remote", "relay:cache.git", "refs/heads/main"])
        .output()
        .expect("git ls-remote");
    assert!(output.status.success(), "git ls-remote should succeed");

    let upstream_main = read_git_ref(&upstream_path, "refs/heads/main");
    let cache_main = read_git_ref(&cache_repo, "refs/heads/main");
    assert_eq!(cache_main, upstream_main);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&upstream_main),
        "ls-remote should advertise the refreshed cache ref"
    );
}

#[test]
fn read_prepare_serves_stale_under_explicit_policy_and_negative_cache() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);

    let upstream_path = temp.path().join("upstream-stale.git");
    init_bare_repo(&upstream_path);
    let upstream_work = temp.path().join("work-upstream-stale");
    init_work_repo(&upstream_work);
    commit_file(&upstream_work, "README.md", "upstream\n", "initial");
    StdCommand::new("git")
        .args([
            "-C",
            upstream_work.to_str().expect("work repo"),
            "push",
            upstream_path.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git push")
        .success()
        .then_some(())
        .expect("upstream push success");

    let cache_repo = temp.path().join("repos").join("cache.git");
    init_bare_repo(&cache_repo);
    write_cache_only_descriptor(
        &temp,
        &cache_repo,
        "stale-if-error",
        upstream_path.to_str().expect("path"),
    );

    let first = Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "read",
            "prepare",
            "--config",
            config_path.to_str().expect("config"),
            "--repo",
            "github.com/example/cache.git",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let first_report = parse_single_report(&first);
    assert_eq!(first_report["action"], "refreshed");
    assert_eq!(first_report["negative_cache_hit"], false);

    let cached_main = read_git_ref(&cache_repo, "refs/heads/main");
    let moved_upstream = temp.path().join("upstream-stale-moved.git");
    fs::rename(&upstream_path, &moved_upstream).expect("move upstream away");

    let second = Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "read",
            "prepare",
            "--config",
            config_path.to_str().expect("config"),
            "--repo",
            "github.com/example/cache.git",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let second_report = parse_single_report(&second);
    assert_eq!(second_report["action"], "served_stale");
    assert_eq!(second_report["negative_cache_hit"], false);
    assert_eq!(read_git_ref(&cache_repo, "refs/heads/main"), cached_main);

    let third = Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "read",
            "prepare",
            "--config",
            config_path.to_str().expect("config"),
            "--repo",
            "github.com/example/cache.git",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let third_report = parse_single_report(&third);
    assert_eq!(third_report["action"], "served_stale");
    assert_eq!(third_report["negative_cache_hit"], true);
    assert_eq!(read_git_ref(&cache_repo, "refs/heads/main"), cached_main);
}

#[test]
fn ssh_crash_checkpoints_match_the_local_commit_boundary() {
    let checkpoints = [
        ("before_pre_receive", false),
        ("after_pre_receive_success", false),
        ("after_reference_transaction_prepared", false),
        ("after_reference_transaction_committed", true),
        ("after_receive_pack_success_before_wrapper_exit", true),
        ("after_wrapper_flushes_response", true),
    ];

    for (checkpoint, expect_committed) in checkpoints {
        let temp = TempDir::new().expect("tempdir");
        let config_path = write_config_fixture(&temp);
        let repo_path = temp.path().join("repos").join("repo.git");
        let repo_id = "github.com/example/repo.git";
        init_bare_repo(&repo_path);
        configure_authoritative_repo(&repo_path);
        write_authoritative_descriptor(&temp, &repo_path, false);

        let dispatcher = cargo_bin_path("git-relay");
        Command::cargo_bin("git-relay-install-hooks")
            .expect("cargo bin")
            .args([
                "--repo",
                repo_path.to_str().expect("repo"),
                "--dispatcher",
                dispatcher.to_str().expect("dispatcher"),
                "--config",
                config_path.to_str().expect("config"),
            ])
            .assert()
            .success();

        let fake_ssh = write_fake_ssh_command(&temp, &config_path);
        let checkpoint_log = temp.path().join(format!("{checkpoint}.log"));
        let work_repo = temp.path().join(format!("work-{checkpoint}"));
        init_work_repo(&work_repo);
        commit_file(&work_repo, "README.md", "hello\n", "initial");

        let request_id = format!("request-{checkpoint}");
        let push_id = format!("push-{checkpoint}");
        let status = StdCommand::new("git")
            .env("GIT_SSH", &fake_ssh)
            .env("GIT_RELAY_CRASH_AT", checkpoint)
            .env("GIT_RELAY_CHECKPOINT_LOG", &checkpoint_log)
            .env("GIT_RELAY_REQUEST_ID", &request_id)
            .env("GIT_RELAY_PUSH_ID", &push_id)
            .args([
                "-C",
                work_repo.to_str().expect("work repo"),
                "push",
                "relay:repo.git",
                "HEAD:refs/heads/main",
            ])
            .status()
            .expect("git push over fake ssh");
        assert_eq!(
            git_ref_exists(&repo_path, "refs/heads/main"),
            expect_committed,
            "checkpoint {checkpoint} committed state mismatch"
        );
        if !expect_committed {
            assert!(
                !status.success(),
                "checkpoint {checkpoint} should fail before local ref commit"
            );
        }
        if expect_committed {
            assert_eq!(
                read_git_ref(&repo_path, "refs/heads/main"),
                read_worktree_ref(&work_repo, "HEAD"),
                "checkpoint {checkpoint} should preserve the committed ref"
            );
        }

        let checkpoint_hits = fs::read_to_string(&checkpoint_log).expect("checkpoint log");
        assert!(
            checkpoint_hits
                .lines()
                .any(|line| line.trim() == checkpoint),
            "checkpoint {checkpoint} should be recorded"
        );

        if checkpoint == "after_reference_transaction_committed" {
            let trace = read_push_trace(temp.path(), repo_id, &push_id);
            assert!(
                trace.iter().any(|event| {
                    event["hook"] == "reference-transaction"
                        && event["phase"] == "committed"
                        && event["status"] == "accepted"
                }),
                "committed checkpoint should record the committed reference transaction"
            );
        }

        assert_git_fsck_clean(&repo_path);
    }
}

#[test]
fn install_hooks_writes_git_hook_wrappers() {
    let temp = TempDir::new().expect("tempdir");
    let repo_path = temp.path().join("repo.git");
    fs::create_dir_all(&repo_path).expect("repo");
    let dispatcher = temp.path().join("dispatcher");
    fs::write(&dispatcher, "#!/bin/sh\nexit 0\n").expect("dispatcher");
    let config_path = temp.path().join("config.toml");
    fs::write(&config_path, "").expect("config");

    let mut command = Command::cargo_bin("git-relay-install-hooks").expect("cargo bin");
    command
        .args([
            "--repo",
            repo_path.to_str().expect("repo"),
            "--dispatcher",
            dispatcher.to_str().expect("dispatcher"),
            "--config",
            config_path.to_str().expect("config"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("pre-receive"))
        .stdout(predicate::str::contains("reference-transaction"))
        .stdout(predicate::str::contains("post-receive"));
}

#[test]
fn deploy_render_service_uses_packaged_example_config() {
    let example_config = package_example_config_path();

    let mut command = Command::cargo_bin("git-relay").expect("cargo bin");
    command
        .args([
            "deploy",
            "render-service",
            "--config",
            example_config.to_str().expect("example config"),
            "--format",
            "systemd",
            "--binary-path",
            "/nix/store/example/bin/git-relayd",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "EnvironmentFile=/etc/git-relay/runtime.env",
        ))
        .stdout(predicate::str::contains(
            "ExecStart=/nix/store/example/bin/git-relayd serve --config",
        ));
}

#[test]
fn replication_reconcile_records_mixed_results_and_updates_internal_observed_refs() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);

    let upstream_ok = temp.path().join("upstream-ok.git");
    init_bare_repo(&upstream_ok);
    let upstream_missing = temp.path().join("missing-upstream.git");
    write_authoritative_descriptor_with_write_upstreams(
        &temp,
        &repo_path,
        &[
            ("alpha", upstream_ok.to_str().expect("path"), false),
            ("beta", upstream_missing.to_str().expect("path"), false),
        ],
    );

    let work_repo = temp.path().join("work-reconcile");
    init_work_repo(&work_repo);
    commit_file(&work_repo, "README.md", "hello\n", "initial");
    StdCommand::new("git")
        .args([
            "-C",
            work_repo.to_str().expect("work repo"),
            "push",
            repo_path.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git push")
        .success()
        .then_some(())
        .expect("authoritative push success");

    let output = Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "replication",
            "reconcile",
            "--config",
            config_path.to_str().expect("config"),
            "--repo",
            "github.com/example/repo.git",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).expect("reconcile json");
    let runs = report.as_array().expect("runs array");
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run["repo_safety"], "degraded");
    assert!(run["upstream_results"]
        .as_array()
        .expect("upstream results")
        .iter()
        .any(|item| item["upstream_id"] == "alpha" && item["state"] == "in_sync"));
    assert!(run["upstream_results"]
        .as_array()
        .expect("upstream results")
        .iter()
        .any(|item| item["upstream_id"] == "beta" && item["state"] == "stalled"));

    let local_main = read_git_ref(&repo_path, "refs/heads/main");
    let observed_main = read_git_ref(&repo_path, "refs/git-relay/upstreams/alpha/heads/main");
    assert_eq!(observed_main, local_main);
}

#[test]
fn replication_probe_upstreams_classifies_supported_unsupported_and_missing_targets() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);

    let upstream_supported = temp.path().join("upstream-supported.git");
    init_bare_repo(&upstream_supported);

    let upstream_non_atomic = temp.path().join("upstream-non-atomic.git");
    init_bare_repo(&upstream_non_atomic);
    StdCommand::new("git")
        .arg(format!("--git-dir={}", upstream_non_atomic.display()))
        .args(["config", "receive.advertiseAtomic", "false"])
        .status()
        .expect("git config")
        .success()
        .then_some(())
        .expect("git config success");

    let missing_upstream = temp.path().join("missing-upstream.git");
    write_authoritative_descriptor_with_write_upstreams(
        &temp,
        &repo_path,
        &[
            ("alpha", upstream_supported.to_str().expect("path"), true),
            ("beta", upstream_non_atomic.to_str().expect("path"), true),
            ("gamma", missing_upstream.to_str().expect("path"), false),
        ],
    );

    let work_repo = temp.path().join("work-probe-upstreams");
    init_work_repo(&work_repo);
    commit_file(&work_repo, "README.md", "hello\n", "initial");
    StdCommand::new("git")
        .args([
            "-C",
            work_repo.to_str().expect("work repo"),
            "push",
            repo_path.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git push")
        .success()
        .then_some(())
        .expect("authoritative push success");

    let output = Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "replication",
            "probe-upstreams",
            "--config",
            config_path.to_str().expect("config"),
            "--repo",
            "github.com/example/repo.git",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).expect("probe json");
    let runs = report.as_array().expect("runs array");
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(
        run["source_oid"],
        read_git_ref(&repo_path, "refs/heads/main")
    );

    let results = run["results"].as_array().expect("results");
    assert!(results.iter().any(|item| {
        item["upstream_id"] == "alpha"
            && item["access"]["verdict"] == "accessible"
            && item["atomic_capability"]["verdict"] == "supported"
            && item["disposable_namespace"]["verdict"] == "supported"
            && item["supported_for_policy"] == true
    }));
    assert!(results.iter().any(|item| {
        item["upstream_id"] == "beta"
            && item["access"]["verdict"] == "accessible"
            && item["atomic_capability"]["verdict"] == "unsupported"
            && item["atomic_capability"]["error_classification"] == "protocol_unsupported"
            && item["disposable_namespace"]["verdict"] == "supported"
            && item["supported_for_policy"] == false
    }));
    assert!(results.iter().any(|item| {
        item["upstream_id"] == "gamma"
            && item["access"]["verdict"] == "repository_missing"
            && item["atomic_capability"]["verdict"] == "inconclusive"
            && item["atomic_capability"]["error_classification"] == "repository_missing"
            && item["disposable_namespace"]["verdict"] == "not_attempted"
            && item["supported_for_policy"] == false
    }));

    assert_no_probe_refs(&upstream_supported);
    assert_no_probe_refs(&upstream_non_atomic);

    let probe_runs_dir = temp
        .path()
        .join("upstream-probes")
        .join("runs")
        .join("github.com_example_repo.git");
    let entries = fs::read_dir(&probe_runs_dir)
        .expect("probe runs dir")
        .collect::<Result<Vec<_>, _>>()
        .expect("probe runs");
    assert_eq!(entries.len(), 1, "one probe run record should be persisted");
}

#[test]
fn replication_probe_matrix_records_target_metadata_and_policy_support() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);

    let upstream_supported = temp.path().join("matrix-supported.git");
    init_bare_repo(&upstream_supported);

    let upstream_non_atomic = temp.path().join("matrix-non-atomic.git");
    init_bare_repo(&upstream_non_atomic);
    StdCommand::new("git")
        .arg(format!("--git-dir={}", upstream_non_atomic.display()))
        .args(["config", "receive.advertiseAtomic", "false"])
        .status()
        .expect("git config")
        .success()
        .then_some(())
        .expect("git config success");

    write_authoritative_descriptor_with_write_upstreams(
        &temp,
        &repo_path,
        &[("alpha", upstream_supported.to_str().expect("path"), true)],
    );

    let work_repo = temp.path().join("work-probe-matrix");
    init_work_repo(&work_repo);
    commit_file(&work_repo, "README.md", "hello\n", "initial");
    StdCommand::new("git")
        .args([
            "-C",
            work_repo.to_str().expect("work repo"),
            "push",
            repo_path.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git push")
        .success()
        .then_some(())
        .expect("authoritative push success");

    let manifest_path = write_matrix_targets_fixture(
        &temp,
        "matrix-targets.json",
        &[
            (
                "self-managed-alpha",
                "local-git",
                "self-managed",
                "ssh",
                upstream_supported.to_str().expect("path"),
                true,
                true,
            ),
            (
                "managed-beta",
                "local-git",
                "managed",
                "ssh",
                upstream_non_atomic.to_str().expect("path"),
                true,
                false,
            ),
        ],
    );

    let output = Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "replication",
            "probe-matrix",
            "--config",
            config_path.to_str().expect("config"),
            "--repo",
            "github.com/example/repo.git",
            "--targets",
            manifest_path.to_str().expect("manifest"),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let report: Value = serde_json::from_slice(&output).expect("probe matrix json");
    let results = report["results"].as_array().expect("results");
    assert!(results.iter().any(|item| {
        item["target"]["target_id"] == "self-managed-alpha"
            && item["target"]["class"] == "self-managed"
            && item["target"]["same_repo_hidden_refs"] == true
            && item["supported_for_policy"] == true
    }));
    assert!(results.iter().any(|item| {
        item["target"]["target_id"] == "managed-beta"
            && item["atomic_capability"]["verdict"] == "unsupported"
            && item["supported_for_policy"] == false
    }));

    let matrix_runs_dir = temp
        .path()
        .join("upstream-probes")
        .join("matrix-runs")
        .join("github.com_example_repo.git");
    let entries = fs::read_dir(&matrix_runs_dir)
        .expect("matrix runs dir")
        .collect::<Result<Vec<_>, _>>()
        .expect("matrix runs");
    assert_eq!(
        entries.len(),
        1,
        "one matrix run record should be persisted"
    );
}

#[test]
fn replication_build_release_manifest_fails_closed_for_unproven_targets() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);

    let upstream_supported = temp.path().join("release-supported.git");
    init_bare_repo(&upstream_supported);
    write_authoritative_descriptor_with_write_upstreams(
        &temp,
        &repo_path,
        &[("alpha", upstream_supported.to_str().expect("path"), true)],
    );

    let work_repo = temp.path().join("work-release-matrix");
    init_work_repo(&work_repo);
    commit_file(&work_repo, "README.md", "hello\n", "initial");
    StdCommand::new("git")
        .args([
            "-C",
            work_repo.to_str().expect("work repo"),
            "push",
            repo_path.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git push")
        .success()
        .then_some(())
        .expect("authoritative push success");

    let missing_upstream = temp.path().join("release-missing.git");
    let manifest_path = write_matrix_targets_fixture(
        &temp,
        "release-targets.json",
        &[
            (
                "supported-alpha",
                "local-git",
                "self-managed",
                "ssh",
                upstream_supported.to_str().expect("path"),
                true,
                true,
            ),
            (
                "missing-beta",
                "local-git",
                "managed",
                "ssh",
                missing_upstream.to_str().expect("path"),
                false,
                false,
            ),
        ],
    );

    let output = Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "replication",
            "build-release-manifest",
            "--config",
            config_path.to_str().expect("config"),
            "--repo",
            "github.com/example/repo.git",
            "--targets",
            manifest_path.to_str().expect("manifest"),
            "--json",
        ])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();

    let manifest: Value = serde_json::from_slice(&output).expect("release manifest json");
    assert_eq!(manifest["all_entries_admitted"], false);
    let entries = manifest["entries"].as_array().expect("entries");
    assert!(entries
        .iter()
        .any(|item| { item["target_id"] == "supported-alpha" && item["admitted"] == true }));
    assert!(entries
        .iter()
        .any(|item| { item["target_id"] == "missing-beta" && item["admitted"] == false }));

    let latest_manifest_path = temp
        .path()
        .join("upstream-probes")
        .join("release-manifests")
        .join("github.com_example_repo.git")
        .join("latest.json");
    assert!(
        latest_manifest_path.exists(),
        "release manifest latest pointer should be persisted"
    );
}

#[test]
fn divergent_repositories_block_new_writes_at_startup_ssh_and_pre_receive() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    let repo_id = "github.com/example/repo.git";
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);

    let upstream = temp.path().join("upstream.git");
    init_bare_repo(&upstream);
    write_authoritative_descriptor_with_write_upstreams(
        &temp,
        &repo_path,
        &[("alpha", upstream.to_str().expect("path"), false)],
    );

    let dispatcher = cargo_bin_path("git-relay");
    Command::cargo_bin("git-relay-install-hooks")
        .expect("cargo bin")
        .args([
            "--repo",
            repo_path.to_str().expect("repo"),
            "--dispatcher",
            dispatcher.to_str().expect("dispatcher"),
            "--config",
            config_path.to_str().expect("config"),
        ])
        .assert()
        .success();

    let work_repo = temp.path().join("work-divergence");
    init_work_repo(&work_repo);
    commit_file(&work_repo, "README.md", "hello\n", "initial");
    StdCommand::new("git")
        .args([
            "-C",
            work_repo.to_str().expect("work repo"),
            "push",
            repo_path.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git push")
        .success()
        .then_some(())
        .expect("initial push success");

    Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "replication",
            "reconcile",
            "--config",
            config_path.to_str().expect("config"),
            "--repo",
            repo_id,
            "--json",
        ])
        .assert()
        .success();

    let external = temp.path().join("external");
    StdCommand::new("git")
        .args([
            "clone",
            upstream.to_str().expect("path"),
            external.to_str().expect("path"),
        ])
        .status()
        .expect("git clone")
        .success()
        .then_some(())
        .expect("git clone success");
    StdCommand::new("git")
        .args([
            "-C",
            external.to_str().expect("path"),
            "config",
            "user.name",
            "Git Relay Test",
        ])
        .status()
        .expect("git config")
        .success()
        .then_some(())
        .expect("git config success");
    StdCommand::new("git")
        .args([
            "-C",
            external.to_str().expect("path"),
            "config",
            "user.email",
            "git-relay@example.com",
        ])
        .status()
        .expect("git config")
        .success()
        .then_some(())
        .expect("git config success");
    commit_file(&external, "README.md", "external\n", "external mutation");
    StdCommand::new("git")
        .args([
            "-C",
            external.to_str().expect("path"),
            "push",
            upstream.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git push")
        .success()
        .then_some(())
        .expect("external push success");

    Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "replication",
            "reconcile",
            "--config",
            config_path.to_str().expect("config"),
            "--repo",
            repo_id,
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"repo_safety\": \"divergent\""));

    assert!(
        git_ref_exists(&repo_path, "refs/git-relay/safety/divergent/alpha"),
        "divergence marker should be persisted under hidden internal refs"
    );

    Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "startup",
            "classify",
            "--config",
            config_path.to_str().expect("config"),
            "--repo",
            repo_id,
            "--json",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("\"safety\": \"divergent\""))
        .stdout(predicate::str::contains(
            "\"write_acceptance_allowed\": false",
        ));

    Command::cargo_bin("git-relay-ssh-force-command")
        .expect("cargo bin")
        .env("SSH_ORIGINAL_COMMAND", "git-receive-pack repo.git")
        .args([
            "--config",
            config_path.to_str().expect("config"),
            "--check-only",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("divergent"));

    commit_file(&work_repo, "README.md", "hello again\n", "blocked write");
    let blocked_push_id = "push-blocked-divergent";
    let blocked_status = StdCommand::new("git")
        .env("GIT_RELAY_REQUEST_ID", "request-blocked-divergent")
        .env("GIT_RELAY_PUSH_ID", blocked_push_id)
        .args([
            "-C",
            work_repo.to_str().expect("work repo"),
            "push",
            repo_path.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git push");
    assert!(
        !blocked_status.success(),
        "pre-receive should fail closed while the repository is divergent"
    );

    let trace = read_push_trace(temp.path(), repo_id, blocked_push_id);
    assert!(
        trace.iter().any(|event| {
            event["hook"] == "pre-receive"
                && event["status"] == "rejected"
                && event["message"]
                    .as_str()
                    .map(|message| message.contains("divergent"))
                    .unwrap_or(false)
        }),
        "blocked push should record the divergence rejection in pre-receive"
    );
}

#[test]
fn git_relayd_serve_once_drains_pending_reconcile_requests() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);

    let upstream_ok = temp.path().join("upstream-ok.git");
    init_bare_repo(&upstream_ok);
    write_authoritative_descriptor_with_write_upstreams(
        &temp,
        &repo_path,
        &[("alpha", upstream_ok.to_str().expect("path"), false)],
    );

    let dispatcher = cargo_bin_path("git-relay");
    Command::cargo_bin("git-relay-install-hooks")
        .expect("cargo bin")
        .args([
            "--repo",
            repo_path.to_str().expect("repo"),
            "--dispatcher",
            dispatcher.to_str().expect("dispatcher"),
            "--config",
            config_path.to_str().expect("config"),
        ])
        .assert()
        .success();

    let work_repo = temp.path().join("work-daemon");
    init_work_repo(&work_repo);
    commit_file(&work_repo, "README.md", "hello\n", "initial");
    StdCommand::new("git")
        .env("GIT_RELAY_REQUEST_ID", "request-daemon")
        .env("GIT_RELAY_PUSH_ID", "push-daemon")
        .args([
            "-C",
            work_repo.to_str().expect("work repo"),
            "push",
            repo_path.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git push")
        .success()
        .then_some(())
        .expect("push success");

    let pending_path = pending_request_file_path(temp.path(), "github.com/example/repo.git");
    assert!(
        pending_path.exists(),
        "pending reconcile request should exist"
    );

    let output = Command::cargo_bin("git-relayd")
        .expect("cargo bin")
        .args([
            "serve",
            "--config",
            config_path.to_str().expect("config"),
            "--once",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).expect("serve report");
    assert_eq!(report["runtime_validation"]["status"], "passed");
    assert!(report["processed_reconciles"]
        .as_array()
        .expect("processed reconciles")
        .iter()
        .any(|item| item["repo_safety"] == "healthy"));

    assert!(
        !pending_path.exists(),
        "pending reconcile request should be cleared"
    );
    let upstream_main = read_git_ref(&upstream_ok, "refs/heads/main");
    let local_main = read_git_ref(&repo_path, "refs/heads/main");
    let observed_main = read_git_ref(&repo_path, "refs/git-relay/upstreams/alpha/heads/main");
    assert_eq!(upstream_main, local_main);
    assert_eq!(observed_main, local_main);
}

#[test]
fn replication_status_reports_pending_and_latest_run_state() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);

    let upstream_ok = temp.path().join("upstream-ok.git");
    init_bare_repo(&upstream_ok);
    write_authoritative_descriptor_with_write_upstreams(
        &temp,
        &repo_path,
        &[("alpha", upstream_ok.to_str().expect("path"), false)],
    );

    let dispatcher = cargo_bin_path("git-relay");
    Command::cargo_bin("git-relay-install-hooks")
        .expect("cargo bin")
        .args([
            "--repo",
            repo_path.to_str().expect("repo"),
            "--dispatcher",
            dispatcher.to_str().expect("dispatcher"),
            "--config",
            config_path.to_str().expect("config"),
        ])
        .assert()
        .success();

    let work_repo = temp.path().join("work-status");
    init_work_repo(&work_repo);
    commit_file(&work_repo, "README.md", "hello\n", "initial");
    StdCommand::new("git")
        .env("GIT_RELAY_REQUEST_ID", "request-status")
        .env("GIT_RELAY_PUSH_ID", "push-status")
        .args([
            "-C",
            work_repo.to_str().expect("work repo"),
            "push",
            repo_path.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git push")
        .success()
        .then_some(())
        .expect("push success");

    let before = Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "replication",
            "status",
            "--config",
            config_path.to_str().expect("config"),
            "--repo",
            "github.com/example/repo.git",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let before_status: Value = serde_json::from_slice(&before).expect("status json");
    let before_item = &before_status.as_array().expect("status array")[0];
    assert_eq!(
        before_item["pending_request"]["last_push_id"],
        "push-status"
    );
    assert!(before_item["latest_run"].is_null());

    Command::cargo_bin("git-relayd")
        .expect("cargo bin")
        .args([
            "serve",
            "--config",
            config_path.to_str().expect("config"),
            "--once",
        ])
        .assert()
        .success();

    let after = Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "replication",
            "status",
            "--config",
            config_path.to_str().expect("config"),
            "--repo",
            "github.com/example/repo.git",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let after_status: Value = serde_json::from_slice(&after).expect("status json");
    let after_item = &after_status.as_array().expect("status array")[0];
    assert!(after_item["pending_request"].is_null());
    assert_eq!(after_item["latest_run"]["repo_safety"], "healthy");
}

#[test]
fn hooked_bare_repo_accepts_branch_create_and_rejects_delete_hidden_ref_and_force_push() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    let repo_id = "github.com/example/repo.git";
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);
    write_authoritative_descriptor(&temp, &repo_path, false);

    let dispatcher = cargo_bin_path("git-relay");
    let mut install = Command::cargo_bin("git-relay-install-hooks").expect("cargo bin");
    install
        .args([
            "--repo",
            repo_path.to_str().expect("repo"),
            "--dispatcher",
            dispatcher.to_str().expect("dispatcher"),
            "--config",
            config_path.to_str().expect("config"),
        ])
        .assert()
        .success();

    let work_repo = temp.path().join("work");
    init_work_repo(&work_repo);
    commit_file(&work_repo, "README.md", "hello\n", "initial");

    let accept_push_id = "push-accept-main";
    StdCommand::new("git")
        .env("GIT_RELAY_REQUEST_ID", "request-accept-main")
        .env("GIT_RELAY_PUSH_ID", accept_push_id)
        .args([
            "-C",
            work_repo.to_str().expect("work repo"),
            "push",
            repo_path.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git push")
        .success()
        .then_some(())
        .expect("branch create push success");

    let accepted_trace = read_push_trace(temp.path(), repo_id, accept_push_id);
    assert!(
        accepted_trace.iter().any(|event| {
            event["hook"] == "pre-receive"
                && event["status"] == "accepted"
                && event["quarantine_path"].is_string()
        }),
        "accepted push should record pre-receive with a receive quarantine path"
    );
    assert!(
        accepted_trace
            .iter()
            .any(|event| event["hook"] == "reference-transaction" && event["phase"] == "prepared"),
        "accepted push should record reference-transaction prepared"
    );
    assert!(
        accepted_trace
            .iter()
            .any(|event| event["hook"] == "reference-transaction" && event["phase"] == "committed"),
        "accepted push should record reference-transaction committed"
    );
    assert!(
        accepted_trace.iter().any(|event| {
            event["hook"] == "post-receive"
                && event["status"] == "accepted"
                && event["reconcile_requested"] == true
        }),
        "accepted push should record post-receive reconcile wakeup intent"
    );
    let pending_request = fs::read_to_string(pending_request_file_path(temp.path(), repo_id))
        .expect("pending reconcile request");
    assert!(pending_request.contains("\"last_push_id\": \"push-accept-main\""));

    let delete_push_id = "push-reject-delete";
    let delete_status = StdCommand::new("git")
        .env("GIT_RELAY_REQUEST_ID", "request-reject-delete")
        .env("GIT_RELAY_PUSH_ID", delete_push_id)
        .args([
            "-C",
            work_repo.to_str().expect("work repo"),
            "push",
            repo_path.to_str().expect("repo"),
            ":refs/heads/main",
        ])
        .status()
        .expect("git push delete");
    assert!(!delete_status.success(), "delete push should fail closed");

    let delete_trace = read_push_trace(temp.path(), repo_id, delete_push_id);
    assert!(
        delete_trace
            .iter()
            .any(|event| event["hook"] == "pre-receive" && event["status"] == "rejected"),
        "rejected push should record the pre-receive rejection"
    );
    assert!(
        !delete_trace
            .iter()
            .any(|event| event["hook"] == "reference-transaction" && event["phase"] == "committed"),
        "rejected push must not record a committed local ref transaction"
    );
    assert!(
        !delete_trace
            .iter()
            .any(|event| event["hook"] == "post-receive"),
        "rejected push must not run post-receive"
    );

    let hidden_ref_status = StdCommand::new("git")
        .env("GIT_RELAY_REQUEST_ID", "request-hidden-ref")
        .env("GIT_RELAY_PUSH_ID", "push-hidden-ref")
        .args([
            "-C",
            work_repo.to_str().expect("work repo"),
            "push",
            repo_path.to_str().expect("repo"),
            "HEAD:refs/git-relay/internal",
        ])
        .status()
        .expect("git push hidden ref");
    assert!(
        !hidden_ref_status.success(),
        "hidden ref push should fail closed"
    );

    commit_file(&work_repo, "README.md", "hello v2\n", "fast forward");
    StdCommand::new("git")
        .env("GIT_RELAY_REQUEST_ID", "request-fast-forward")
        .env("GIT_RELAY_PUSH_ID", "push-fast-forward")
        .args([
            "-C",
            work_repo.to_str().expect("work repo"),
            "push",
            repo_path.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git push fast forward")
        .success()
        .then_some(())
        .expect("fast-forward push success");

    let rewrite_repo = temp.path().join("rewrite");
    init_work_repo(&rewrite_repo);
    commit_file(&rewrite_repo, "README.md", "rewrite\n", "rewrite history");
    let force_status = StdCommand::new("git")
        .env("GIT_RELAY_REQUEST_ID", "request-force-reject")
        .env("GIT_RELAY_PUSH_ID", "push-force-reject")
        .args([
            "-C",
            rewrite_repo.to_str().expect("rewrite repo"),
            "push",
            "--force",
            repo_path.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git force push");
    assert!(
        !force_status.success(),
        "non-fast-forward force push should fail closed"
    );
}

#[test]
fn migration_inspect_reports_rewrite_plan_and_unresolved_transitive_shorthand() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let project = write_flake_project_fixture(
        &temp,
        "flake-inspect",
        migration_flake_source(),
        migration_lock_source(),
    );

    let output = Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "migration",
            "inspect",
            "--config",
            config_path.to_str().expect("config"),
            "--flake",
            project.to_str().expect("project"),
            "--input-target",
            "nixpkgs=git+https",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).expect("migration inspect json");

    let planned = report["planned_rewrites"]
        .as_array()
        .expect("planned rewrites");
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0]["input_name"], "nixpkgs");
    assert_eq!(
        planned[0]["after_url"],
        "git+https://github.com/NixOS/nixpkgs?ref=nixos-unstable"
    );
    assert!(report["preview_diff"]
        .as_str()
        .expect("preview diff")
        .contains("git+https://github.com/NixOS/nixpkgs?ref=nixos-unstable"));
    assert!(report["unresolved_transitive_shorthand"]
        .as_array()
        .expect("transitive shorthand")
        .iter()
        .any(|item| {
            item["node_id"] == "indirect"
                && item["shorthand_type"] == "github"
                && item["repo"] == "transitive"
        }));
}

#[test]
fn migrate_flake_inputs_rewrites_direct_inputs_and_runs_scoped_relock() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let project = write_flake_project_fixture(
        &temp,
        "flake-apply-success",
        migration_flake_source(),
        migration_lock_source(),
    );
    let after_lock = temp.path().join("flake.lock.after");
    fs::write(&after_lock, migration_lock_after_source()).expect("after lock");
    let (fake_nix, fake_nix_log) = write_fake_nix_command(
        &temp,
        "fake-nix-success",
        "nix (Determinate Nix 3.0.0) 2.26.3",
        &after_lock,
        None,
    );

    let output = Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .env("GIT_RELAY_NIX_BIN", &fake_nix)
        .args([
            "migrate-flake-inputs",
            "--config",
            config_path.to_str().expect("config"),
            "--flake",
            project.to_str().expect("project"),
            "--input-target",
            "nixpkgs=git+https",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).expect("migration apply json");
    assert_eq!(report["nix_version"], "nix (Determinate Nix 3.0.0) 2.26.3");
    assert_eq!(report["relocked_inputs"][0], "nixpkgs");
    assert!(report["diff"]
        .as_str()
        .expect("diff")
        .contains("flake.lock.after"));

    let flake_source = fs::read_to_string(project.join("flake.nix")).expect("flake.nix");
    assert!(flake_source.contains("git+https://github.com/NixOS/nixpkgs?ref=nixos-unstable"));
    assert_eq!(
        fs::read_to_string(project.join("flake.lock")).expect("flake.lock"),
        migration_lock_after_source()
    );

    let log = fs::read_to_string(fake_nix_log).expect("fake nix log");
    let update_calls = log
        .lines()
        .filter(|line| *line == "flake update nixpkgs")
        .count();
    assert_eq!(update_calls, 2, "targeted relock should run twice");
}

#[test]
fn migrate_flake_inputs_refuses_dirty_worktree_by_default() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let project = write_flake_project_fixture(
        &temp,
        "flake-dirty",
        migration_flake_source(),
        migration_lock_source(),
    );
    fs::write(project.join("README.md"), "dirty\n").expect("dirty file");

    Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "migrate-flake-inputs",
            "--config",
            config_path.to_str().expect("config"),
            "--flake",
            project.to_str().expect("project"),
            "--input-target",
            "nixpkgs=git+https",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("dirty"));
}

#[test]
fn migration_inspect_fails_closed_for_direct_inputs_outside_supported_literal_grammar() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let project = write_flake_project_fixture(
        &temp,
        "flake-unsupported-grammar",
        r#"
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }: { };
}
"#,
        migration_lock_source(),
    );

    Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "migration",
            "inspect",
            "--config",
            config_path.to_str().expect("config"),
            "--flake",
            project.to_str().expect("project"),
            "--input-target",
            "nixpkgs=git+https",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("supported literal grammar"));
}

#[test]
fn migrate_flake_inputs_restores_original_files_on_scope_violation() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let project = write_flake_project_fixture(
        &temp,
        "flake-scope-violation",
        migration_flake_source(),
        migration_lock_source(),
    );
    let bad_lock = temp.path().join("flake.lock.scope-violation");
    fs::write(&bad_lock, migration_lock_scope_violation_source()).expect("bad lock");
    let (fake_nix, _log_path) = write_fake_nix_command(
        &temp,
        "fake-nix-scope-violation",
        "nix (Determinate Nix 3.0.0) 2.26.3",
        &bad_lock,
        None,
    );

    Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .env("GIT_RELAY_NIX_BIN", &fake_nix)
        .args([
            "migrate-flake-inputs",
            "--config",
            config_path.to_str().expect("config"),
            "--flake",
            project.to_str().expect("project"),
            "--input-target",
            "nixpkgs=git+https",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "changed nodes outside the selected input closure",
        ));

    assert_eq!(
        fs::read_to_string(project.join("flake.nix")).expect("flake.nix"),
        migration_flake_source()
    );
    assert_eq!(
        fs::read_to_string(project.join("flake.lock")).expect("flake.lock"),
        migration_lock_source()
    );
}

#[test]
fn migrate_flake_inputs_restores_original_files_on_non_idempotent_relock() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let project = write_flake_project_fixture(
        &temp,
        "flake-non-idempotent",
        migration_flake_source(),
        migration_lock_source(),
    );
    let first_lock = temp.path().join("flake.lock.first");
    let second_lock = temp.path().join("flake.lock.second");
    fs::write(&first_lock, migration_lock_after_source()).expect("first lock");
    fs::write(&second_lock, migration_lock_non_idempotent_source()).expect("second lock");
    let (fake_nix, _log_path) = write_fake_nix_command(
        &temp,
        "fake-nix-non-idempotent",
        "nix (Determinate Nix 3.0.0) 2.26.3",
        &first_lock,
        Some(&second_lock),
    );

    Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .env("GIT_RELAY_NIX_BIN", &fake_nix)
        .args([
            "migrate-flake-inputs",
            "--config",
            config_path.to_str().expect("config"),
            "--flake",
            project.to_str().expect("project"),
            "--input-target",
            "nixpkgs=git+https",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not idempotent"));

    assert_eq!(
        fs::read_to_string(project.join("flake.nix")).expect("flake.nix"),
        migration_flake_source()
    );
    assert_eq!(
        fs::read_to_string(project.join("flake.lock")).expect("flake.lock"),
        migration_lock_source()
    );
}

#[test]
fn migrate_flake_inputs_fails_outside_the_validated_nix_version_matrix() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let project = write_flake_project_fixture(
        &temp,
        "flake-unsupported-nix",
        migration_flake_source(),
        migration_lock_source(),
    );
    let after_lock = temp.path().join("flake.lock.after");
    fs::write(&after_lock, migration_lock_after_source()).expect("after lock");
    let (fake_nix, _log_path) = write_fake_nix_command(
        &temp,
        "fake-nix-unsupported-version",
        "nix (Nix) 2.32.0",
        &after_lock,
        None,
    );

    Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .env("GIT_RELAY_NIX_BIN", &fake_nix)
        .args([
            "migrate-flake-inputs",
            "--config",
            config_path.to_str().expect("config"),
            "--flake",
            project.to_str().expect("project"),
            "--input-target",
            "nixpkgs=git+https",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "outside the validated targeted relock matrix",
        ));

    assert_eq!(
        fs::read_to_string(project.join("flake.nix")).expect("flake.nix"),
        migration_flake_source()
    );
    assert_eq!(
        fs::read_to_string(project.join("flake.lock")).expect("flake.lock"),
        migration_lock_source()
    );
}

#[test]
fn structured_logs_capture_hook_reconcile_and_operator_fields() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    let repo_id = "github.com/example/repo.git";
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);

    let upstream = temp.path().join("structured-upstream.git");
    init_bare_repo(&upstream);
    write_authoritative_descriptor_with_write_upstreams(
        &temp,
        &repo_path,
        &[("alpha", upstream.to_str().expect("path"), false)],
    );

    let dispatcher = cargo_bin_path("git-relay");
    Command::cargo_bin("git-relay-install-hooks")
        .expect("cargo bin")
        .args([
            "--repo",
            repo_path.to_str().expect("repo"),
            "--dispatcher",
            dispatcher.to_str().expect("dispatcher"),
            "--config",
            config_path.to_str().expect("config"),
        ])
        .assert()
        .success();

    let work_repo = temp.path().join("work-structured-logs");
    init_work_repo(&work_repo);
    commit_file(&work_repo, "README.md", "hello\n", "initial");
    StdCommand::new("git")
        .env("GIT_RELAY_REQUEST_ID", "request-structured")
        .env("GIT_RELAY_PUSH_ID", "push-structured")
        .env("GIT_RELAY_CLIENT_IDENTITY", "push-user@example.test")
        .args([
            "-C",
            work_repo.to_str().expect("work repo"),
            "push",
            repo_path.to_str().expect("repo"),
            "HEAD:refs/heads/main",
        ])
        .status()
        .expect("git push")
        .success()
        .then_some(())
        .expect("push success");

    Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .env("GIT_RELAY_CLIENT_IDENTITY", "operator@example.test")
        .args([
            "replication",
            "reconcile",
            "--config",
            config_path.to_str().expect("config"),
            "--repo",
            repo_id,
            "--json",
        ])
        .assert()
        .success();

    let logs = read_structured_logs(temp.path());
    assert!(logs.iter().any(|event| {
        event["event_type"] == "hook.dispatch"
            && event["request_id"] == "request-structured"
            && event["push_id"] == "push-structured"
            && event["repo_id"] == repo_id
            && event["client_identity"] == "push-user@example.test"
    }));
    assert!(logs.iter().any(|event| {
        event["event_type"] == "reconcile.upstream"
            && event["repo_id"] == repo_id
            && event["push_id"] == "push-structured"
            && event["request_id"] == "request-structured"
            && event["upstream_id"] == "alpha"
            && event["reconcile_run_id"].as_str().is_some()
            && event["attempt_id"].as_str().is_some()
    }));
    assert!(logs.iter().any(|event| {
        event["event_type"] == "cli.command"
            && event["client_identity"] == "operator@example.test"
            && event["payload"]["command"] == "replication.reconcile"
    }));
}

#[test]
fn repo_repair_breaks_stale_execution_artifacts_and_rederives_state() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    let repo_id = "github.com/example/repo.git";
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);

    let upstream = temp.path().join("repair-upstream.git");
    init_bare_repo(&upstream);
    write_authoritative_descriptor_with_write_upstreams(
        &temp,
        &repo_path,
        &[("alpha", upstream.to_str().expect("path"), false)],
    );

    let repo_component = sanitize_repo_state_component(repo_id);
    let lock_dir = temp
        .path()
        .join("reconcile")
        .join("locks")
        .join(format!("{repo_component}.lock"));
    fs::create_dir_all(&lock_dir).expect("lock dir");
    fs::write(
        lock_dir.join("metadata.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "repo_id": repo_id,
            "run_id": "stale-run",
            "pid": 999_999u32,
            "acquired_at_ms": 0u128,
        }))
        .expect("lock metadata"),
    )
    .expect("write lock metadata");

    let in_progress_path = temp
        .path()
        .join("reconcile")
        .join("in-progress")
        .join(format!("{repo_component}.json"));
    fs::create_dir_all(
        in_progress_path
            .parent()
            .expect("in-progress parent directory"),
    )
    .expect("in-progress dir");
    fs::write(
        &in_progress_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "repo_id": repo_id,
            "run_id": "stale-run",
            "pid": 999_999u32,
            "started_at_ms": 0u128,
        }))
        .expect("in-progress marker"),
    )
    .expect("write in-progress marker");

    let output = Command::cargo_bin("git-relay")
        .expect("cargo bin")
        .args([
            "repo",
            "repair",
            "--config",
            config_path.to_str().expect("config"),
            "--repo",
            repo_id,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report = parse_single_report(&output);
    assert_eq!(report["repair"]["repo_id"], repo_id);
    assert_eq!(report["repair"]["stale_lock_broken"], true);
    assert_eq!(report["repair"]["stale_in_progress_marker_cleared"], true);
    assert_eq!(report["repair"]["reconcile_run"]["repo_safety"], "healthy");
    assert!(
        !lock_dir.exists(),
        "repair should remove the stale reconcile lock"
    );
    assert!(
        !in_progress_path.exists(),
        "repair should remove the stale in-progress marker"
    );
}
