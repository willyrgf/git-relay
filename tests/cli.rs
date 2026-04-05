use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
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

fn read_git_ref(repo_path: &Path, ref_name: &str) -> String {
    let output = StdCommand::new("git")
        .arg(format!("--git-dir={}", repo_path.display()))
        .args(["rev-parse", ref_name])
        .output()
        .expect("git rev-parse");
    assert!(output.status.success(), "git rev-parse failed");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
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
    assert!(
        run["upstream_results"]
            .as_array()
            .expect("upstream results")
            .iter()
            .any(|item| item["upstream_id"] == "alpha" && item["state"] == "in_sync")
    );
    assert!(
        run["upstream_results"]
            .as_array()
            .expect("upstream results")
            .iter()
            .any(|item| item["upstream_id"] == "beta" && item["state"] == "stalled")
    );

    let local_main = read_git_ref(&repo_path, "refs/heads/main");
    let observed_main = read_git_ref(
        &repo_path,
        "refs/git-relay/upstreams/alpha/heads/main",
    );
    assert_eq!(observed_main, local_main);
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
    assert!(pending_path.exists(), "pending reconcile request should exist");

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
    assert!(
        report["processed_reconciles"]
            .as_array()
            .expect("processed reconciles")
            .iter()
            .any(|item| item["repo_safety"] == "healthy")
    );

    assert!(!pending_path.exists(), "pending reconcile request should be cleared");
    let upstream_main = read_git_ref(&upstream_ok, "refs/heads/main");
    let local_main = read_git_ref(&repo_path, "refs/heads/main");
    let observed_main = read_git_ref(
        &repo_path,
        "refs/git-relay/upstreams/alpha/heads/main",
    );
    assert_eq!(upstream_main, local_main);
    assert_eq!(observed_main, local_main);
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
        !delete_trace.iter().any(|event| event["hook"] == "post-receive"),
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
