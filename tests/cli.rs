use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::prelude::*;
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
    let output = match std::env::consts::OS {
        "macos" => StdCommand::new("stat")
            .args(["-f", "%T"])
            .arg(path)
            .output()
            .expect("stat"),
        "linux" => StdCommand::new("stat")
            .args(["-f", "-c", "%T"])
            .arg(path)
            .output()
            .expect("stat"),
        other => panic!("unsupported host {other}"),
    };
    assert!(output.status.success(), "stat failed");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
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

#[test]
fn repo_validate_returns_json_for_valid_authoritative_repo() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    init_bare_repo(&repo_path);
    configure_authoritative_repo(&repo_path);

    let descriptor = format!(
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
    );
    fs::write(temp.path().join("repos.d").join("repo.toml"), descriptor).expect("descriptor");

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
fn startup_classify_fails_closed_for_invalid_authoritative_repo() {
    let temp = TempDir::new().expect("tempdir");
    let config_path = write_config_fixture(&temp);
    let repo_path = temp.path().join("repos").join("repo.git");
    init_bare_repo(&repo_path);

    let descriptor = format!(
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
    );
    fs::write(temp.path().join("repos.d").join("repo.toml"), descriptor).expect("descriptor");

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

    let descriptor = format!(
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
    );
    fs::write(temp.path().join("repos.d").join("repo.toml"), descriptor).expect("descriptor");

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
    let repo_root = temp.path().join("repos");
    let repo_path = repo_root.join("example.git");
    fs::create_dir_all(&repo_path).expect("repo");

    let mut command = Command::cargo_bin("git-relay-ssh-force-command").expect("cargo bin");
    command
        .env("SSH_ORIGINAL_COMMAND", "git-receive-pack example.git")
        .args([
            "--repo-root",
            repo_root.to_str().expect("repo root"),
            "--check-only",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"service\": \"git-receive-pack\"",
        ))
        .stdout(predicate::str::contains(
            repo_path.to_str().expect("repo path"),
        ));
}

#[test]
fn install_hooks_writes_git_hook_wrappers() {
    let temp = TempDir::new().expect("tempdir");
    let repo_path = temp.path().join("repo.git");
    fs::create_dir_all(&repo_path).expect("repo");
    let dispatcher = temp.path().join("dispatcher");
    fs::write(&dispatcher, "#!/bin/sh\nexit 0\n").expect("dispatcher");

    let mut command = Command::cargo_bin("git-relay-install-hooks").expect("cargo bin");
    command
        .args([
            "--repo",
            repo_path.to_str().expect("repo"),
            "--dispatcher",
            dispatcher.to_str().expect("dispatcher"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("pre-receive"))
        .stdout(predicate::str::contains("reference-transaction"))
        .stdout(predicate::str::contains("post-receive"));
}
