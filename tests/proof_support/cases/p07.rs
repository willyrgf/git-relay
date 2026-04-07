use std::fs;

use serde_json::json;

use crate::proof_support::cases::CaseDefinition;
use crate::proof_support::lab::{CaseReport, ProofLab, AUTHORITATIVE_REPO_ID, CACHE_REPO_ID};
use crate::proof_support::schema::{ProofAssertion, ProofMode};

pub fn definition() -> CaseDefinition {
    CaseDefinition {
        case_id: "P07",
        setup: "Use a cache-only repo descriptor with explicit read-upstream freshness policy.",
        action: "Mutate read upstream externally, run read prepare, and assert cache-only command boundaries.",
        pass_criteria: &[
            "cache-only read prepare refreshes from read upstream",
            "stale-if-error serves stale refs and reuses negative-cache entries on repeated failure",
            "cache-only commands fail closed on authoritative repos",
            "ssh and smart-http observations match prepared cache refs",
        ],
        fail_criteria: &[
            "cache stale serving without explicit policy",
            "cache command succeeds on authoritative repo",
        ],
        contract_refs: &[
            "RFC_PROOF_E2E_TEST.md#P07",
            "git-relay-rfc.md cache-only mode contract",
            "verification-plan read-path constraints",
        ],
        runner: run,
    }
}

fn run(lab: &mut ProofLab, _mode: ProofMode) -> Result<CaseReport, String> {
    let mut report = CaseReport::with_details(json!({}));
    let transport = lab
        .start_transport_harness("P07")
        .map_err(|error| error.to_string())?;

    lab.write_cache_only_descriptor("always-refresh", &lab.upstream_read)
        .map_err(|error| error.to_string())?;

    let external = lab
        .case_root("P07")
        .map_err(|error| error.to_string())?
        .join("external-read");
    if external.exists() {
        fs::remove_dir_all(&external).map_err(|error| error.to_string())?;
    }
    lab.run_git_expect_success(
        &[
            "clone".to_owned(),
            lab.upstream_read.display().to_string(),
            external.display().to_string(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            external.display().to_string(),
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
            external.display().to_string(),
            "config".to_owned(),
            "user.email".to_owned(),
            "git-relay-proof@example.com".to_owned(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;
    lab.commit_file(
        &external,
        "README.md",
        "cache refresh input\n",
        "cache upstream mutation",
    )
    .map_err(|error| error.to_string())?;
    lab.run_git_expect_success(
        &[
            "-C".to_owned(),
            external.display().to_string(),
            "push".to_owned(),
            "origin".to_owned(),
            "HEAD:refs/heads/main".to_owned(),
        ],
        None,
        &[],
    )
    .map_err(|error| error.to_string())?;

    let read_prepare = lab
        .run_git_relay(
            &[
                "read".to_owned(),
                "prepare".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--repo".to_owned(),
                CACHE_REPO_ID.to_owned(),
                "--json".to_owned(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;

    let cache_ref_matches = if read_prepare.success() {
        let upstream_main = lab
            .read_git_ref(&lab.upstream_read, "refs/heads/main")
            .map_err(|error| error.to_string())?;
        let cache_main = lab
            .read_git_ref(&lab.cache_repo, "refs/heads/main")
            .map_err(|error| error.to_string())?;
        upstream_main == cache_main
    } else {
        false
    };

    let pin_authoritative = lab
        .run_git_relay(
            &[
                "cache".to_owned(),
                "pin".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--repo".to_owned(),
                AUTHORITATIVE_REPO_ID.to_owned(),
                "--json".to_owned(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;

    let cache_command_fail_closed = !pin_authoritative.success();

    let cache_main = lab
        .read_git_ref(&lab.cache_repo, "refs/heads/main")
        .map_err(|error| error.to_string())?;
    let ssh_url = transport.ssh.remote_url_for_repo(&lab.cache_repo);
    let ssh_env = vec![(
        "GIT_SSH_COMMAND".to_owned(),
        transport.ssh.git_ssh_command(),
    )];
    let ssh_required = transport.ssh.shell_allows_remote_commands;
    let ssh_parity = if ssh_required {
        let capture = lab
            .run_git(
                &[
                    "ls-remote".to_owned(),
                    ssh_url.clone(),
                    "refs/heads/main".to_owned(),
                ],
                None,
                &ssh_env,
            )
            .map_err(|error| error.to_string())?;
        capture.success() && capture.stdout.contains(&cache_main)
    } else {
        true
    };
    let http_url = transport.smart_http.remote_url_for_repo("relay-cache.git");
    let http_capture = lab
        .run_git(
            &[
                "ls-remote".to_owned(),
                http_url.clone(),
                "refs/heads/main".to_owned(),
            ],
            None,
            &[],
        )
        .map_err(|error| error.to_string())?;
    let http_parity = http_capture.success() && http_capture.stdout.contains(&cache_main);

    let missing_upstream = lab
        .case_root("P07")
        .map_err(|error| error.to_string())?
        .join("upstream-read-missing.git");
    if missing_upstream.exists() {
        fs::remove_dir_all(&missing_upstream).map_err(|error| error.to_string())?;
    }
    lab.write_cache_only_descriptor("stale-if-error", &missing_upstream)
        .map_err(|error| error.to_string())?;

    let stale_first = lab
        .run_git_relay(
            &[
                "read".to_owned(),
                "prepare".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--repo".to_owned(),
                CACHE_REPO_ID.to_owned(),
                "--json".to_owned(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;
    let stale_second = lab
        .run_git_relay(
            &[
                "read".to_owned(),
                "prepare".to_owned(),
                "--config".to_owned(),
                lab.config_path.display().to_string(),
                "--repo".to_owned(),
                CACHE_REPO_ID.to_owned(),
                "--json".to_owned(),
            ],
            &[],
        )
        .map_err(|error| error.to_string())?;
    let (stale_served_once, negative_cache_hit, cache_ref_stable_on_failure) =
        if stale_first.success() && stale_second.success() {
            let first = parse_read_prepare_report(&stale_first.stdout)?;
            let second = parse_read_prepare_report(&stale_second.stdout)?;
            let cache_after_failures = lab
                .read_git_ref(&lab.cache_repo, "refs/heads/main")
                .map_err(|error| error.to_string())?;
            (
                first["action"] == "served_stale" && first["negative_cache_hit"] == false,
                second["action"] == "served_stale" && second["negative_cache_hit"] == true,
                cache_after_failures == cache_main,
            )
        } else {
            (false, false, false)
        };

    report.assertions.push(if read_prepare.success() {
        ProofAssertion::pass(
            "p07.read_prepare.success",
            Some("cache read prepare completed".to_owned()),
        )
    } else {
        ProofAssertion::fail("p07.read_prepare.success", read_prepare.summary())
    });
    report.assertions.push(if cache_ref_matches {
        ProofAssertion::pass(
            "p07.cache_ref_matches_upstream",
            Some("cache refs refreshed from read upstream".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p07.cache_ref_matches_upstream",
            "cache repository did not refresh to upstream main",
        )
    });
    report.assertions.push(if cache_command_fail_closed {
        ProofAssertion::pass(
            "p07.cache_fail_closed_on_authoritative",
            Some("cache pin failed closed on authoritative repo".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p07.cache_fail_closed_on_authoritative",
            "cache pin unexpectedly succeeded on authoritative repo",
        )
    });
    report.assertions.push(if ssh_parity {
        ProofAssertion::pass(
            "p07.ssh.read_path.parity",
            Some("ssh ls-remote on cache repo matched prepared cache ref".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p07.ssh.read_path.parity",
            "ssh ls-remote did not match prepared cache main ref",
        )
    });
    report.assertions.push(if http_parity {
        ProofAssertion::pass(
            "p07.http.read_path.parity",
            Some("smart-http ls-remote on cache repo matched prepared cache ref".to_owned()),
        )
    } else {
        ProofAssertion::fail("p07.http.read_path.parity", http_capture.summary())
    });
    report.assertions.push(
        if stale_served_once && cache_ref_stable_on_failure {
            ProofAssertion::pass(
                "p07.stale_if_error.serves_stale",
                Some("stale-if-error served local stale refs without mutating cached heads".to_owned()),
            )
        } else {
            ProofAssertion::fail(
                "p07.stale_if_error.serves_stale",
                "stale-if-error did not serve stale refs with stable cached state on upstream failure",
            )
        },
    );
    report.assertions.push(if negative_cache_hit {
        ProofAssertion::pass(
            "p07.negative_cache.hit",
            Some("repeated upstream failure used active negative-cache entry".to_owned()),
        )
    } else {
        ProofAssertion::fail(
            "p07.negative_cache.hit",
            "second stale-if-error read prepare did not report negative-cache hit",
        )
    });

    report.transport_profiles = vec!["ssh".to_owned(), "smart-http".to_owned()];
    report.details = json!({
        "read_prepare": read_prepare.summary(),
        "cache_ref_matches": cache_ref_matches,
        "cache_command_fail_closed": cache_command_fail_closed,
        "pin_authoritative": pin_authoritative.summary(),
        "ssh_url": ssh_url,
        "ssh_required": ssh_required,
        "ssh_parity": ssh_parity,
        "http_url": http_url,
        "http_parity": http_parity,
        "http_capture": http_capture.summary(),
        "stale_first": stale_first.summary(),
        "stale_second": stale_second.summary(),
        "stale_served_once": stale_served_once,
        "negative_cache_hit": negative_cache_hit,
        "cache_ref_stable_on_failure": cache_ref_stable_on_failure,
    });

    Ok(report)
}

fn parse_read_prepare_report(source: &str) -> Result<serde_json::Value, String> {
    let mut reports: Vec<serde_json::Value> =
        serde_json::from_str(source).map_err(|error| error.to_string())?;
    if reports.len() != 1 {
        return Err(format!(
            "expected one read-prepare report entry, found {}",
            reports.len()
        ));
    }
    Ok(reports.remove(0))
}
