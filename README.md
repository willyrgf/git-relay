# git-relay

Git Relay is a Git-first edge relay for native Git traffic and explicit Nix flake-input migration.

This repository implements:

- fail-closed repository validation and startup classification
- SSH forced-command routing into system Git
- authoritative local push acceptance through native Git hooks
- asynchronous current-state reconciliation to configured upstreams
- cache-only read preparation with refresh, negative cache, pin, eviction, and retention sweeps
- upstream conformance probing and release-manifest generation
- deterministic direct-input flake migration for validated shorthand forms
- operator inspection, repair, release reporting, and structured audit logs

This `README.md` is the implementation guide for the repository as it exists today. The design history and verification history remain in:

- [git-relay-rfc.md](./git-relay-rfc.md)
- [verification-plan.md](./verification-plan.md)

## Status

Implemented in this repository:

- repository descriptors and typed daemon config
- validator-enforced authoritative repository contract
- launchd rendering on macOS and systemd rendering on Linux
- runtime environment-file validation outside `/nix/store`
- SSH ingress through `git-relay-ssh-force-command`
- hook installation through `git-relay-install-hooks`
- `local-commit` acknowledgement with crash-window coverage
- reconcile coordinator with `observe -> compute -> apply -> observe`
- internal observed refs and divergence markers in the same repository
- cache-only read preparation and retention maintenance
- migration inspection and deterministic direct-input rewrite/relock
- structured JSONL logs and operator JSON reports

Deliberately unsupported or not implemented:

- tarball compatibility plane
- SQLite or any replay journal
- smart HTTP push
- `git://`
- dumb HTTP
- Git LFS
- automatic `repo add`
- automatic migration of transitive shorthand inputs
- explicit `cache unpin`

Release gates still open:

- exact supported Git floor is not closed by evidence yet
- exact supported Nix floor is not closed by evidence yet across the supported platform set

## Core Contract

Git Relay in this repository is intentionally opinionated:

- Git-first, not Nix-first
- no tarball compatibility plane in the foundational architecture
- system Git and OpenSSH are first-class primitives
- no SQLite in correctness-critical paths
- correctness-critical persistent state lives in declarative repo config, bare repos, internal Git refs, and advisory filesystem locks
- local write acknowledgement policy is `local-commit`
- upstream convergence is asynchronous current-state reconciliation, not per-push replay
- cross-upstream atomicity is false and is never implied
- internal refs are same-repo hidden refs in the initial implementation only
- internal refs must never be pushed upstream
- reconcile policy is `on_push + manual`
- supported deployment platforms are macOS and Linux
- supported service managers are launchd on macOS and systemd on Linux

## Binaries

The repository builds four binaries:

- `git-relay`: control-plane CLI
- `git-relayd`: daemon entrypoint
- `git-relay-install-hooks`: installs the Git hook wrappers into a bare repository
- `git-relay-ssh-force-command`: OpenSSH `ForceCommand` target that resolves and executes allowed Git services

The Nix flake exports:

- `.#git-relay`
- `.#git-relayd`
- `.#git-relay-install-hooks`
- `.#git-relay-ssh-force-command`
- `.#git-relay-service-templates`

## Repository Layout

- [`packaging/example/git-relay.example.toml`](./packaging/example/git-relay.example.toml): example daemon config
- [`packaging/example/git-relay.env.example`](./packaging/example/git-relay.env.example): runtime environment file example
- [`git-relay-rfc.md`](./git-relay-rfc.md): design contract and workstreams
- [`verification-plan.md`](./verification-plan.md): evidence plan and recorded verification results

Main Rust modules:

- `config`: daemon config and repository descriptor schema
- `validator`: authoritative repository contract checks
- `ssh_wrapper`: SSH forced-command parsing and authorization
- `hooks`: pre-receive, reference-transaction, and post-receive dispatch
- `reconcile`: authoritative convergence engine and recovery state
- `read_path`: cache-only refresh and read-preparation logic
- `upstream`: upstream conformance probes and release manifests
- `migration`: flake migration inspection and rewrite/relock flow
- `maintenance`: retention defaults and daemon maintenance sweeps
- `release`: release-floor reporting

## Configuration Model

Daemon config is one TOML file with these top-level sections:

- `[listen]`
- `[paths]`
- `[reconcile]`
- `[policy]`
- `[retention]`
- `[migration]`
- `[deployment]`

Example:

```toml
[listen]
ssh = "127.0.0.1:4222"
https = "127.0.0.1:4318"
enable_http_read = false
enable_http_write = false

[paths]
state_root = "/var/lib/git-relay"
repo_root = "/var/lib/git-relay/repos"
repo_config_root = "/etc/git-relay/repos.d"

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

[retention]
maintenance_interval = "24h"
cache_idle_ttl = "336h"
terminal_run_ttl = "720h"
terminal_run_keep_count = 20
authoritative_reflog_ttl = "720h"
authoritative_prune_ttl = "168h"

[migration]
supported_targets = ["git+https", "git+ssh"]
refuse_dirty_worktree = true
targeted_relock_mode = "validated-only"

[deployment]
platform = "linux"
service_manager = "systemd"
service_label = "dev.git-relay"
git_only_command_mode = "openssh-force-command"
forced_command_wrapper = "/usr/local/libexec/git-relay-ssh-force-command"
disable_forwarding = true
runtime_env_file = "/etc/git-relay/runtime.env"
allowed_git_services = ["git-upload-pack", "git-receive-pack"]
supported_filesystems = ["apfs", "ext2/ext3", "ext4"]
```

Runtime environment files stay outside `/nix/store`. Outbound Git authentication uses the relay process's ambient Git and SSH environment, such as `ssh-agent`, `~/.ssh/config`, `GIT_SSH_COMMAND`, and Git credential helpers. The relay does not select per-upstream credentials in config. The example env file is:

```sh
SSH_AUTH_SOCK=/run/user/1000/ssh-agent.sock
GIT_SSH_COMMAND=ssh -F /etc/git-relay/ssh_config
```

## Repository Descriptors

Repository descriptors are separate TOML files under `repo_config_root`. There is no `repo add` command in the current repository; operators create and manage these files directly.

Authoritative example:

```toml
repo_id = "github.com/example/repo.git"
canonical_identity = "github.com/example/repo.git"
repo_path = "/var/lib/git-relay/repos/repo.git"
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
require_atomic = true
```

Cache-only example:

```toml
repo_id = "github.com/example/cache.git"
canonical_identity = "github.com/example/cache.git"
repo_path = "/var/lib/git-relay/repos/cache.git"
mode = "cache-only"
lifecycle = "ready"
authority_model = "upstream-source"
tracking_refs = "same-repo-hidden"
refresh = "ttl:60s"
push_ack = "local-commit"
reconcile_policy = "on-push+manual"
exported_refs = ["refs/heads/*", "refs/tags/*"]

[[read_upstreams]]
name = "github-read"
url = "https://github.com/example/cache.git"
```

Important validator rules for write-accepting authoritative repositories:

- the repo must already exist and be bare
- `core.fsync=all` and `core.fsyncMethod=fsync` are required
- hidden refs under `refs/git-relay/*` must be enforced
- SHA-by-id wants must be disabled for same-repo hidden refs
- the runtime deployment profile and filesystem must match the configured supported platform
- internal tracking placement defaults to `same-repo-hidden`: keep `refs/git-relay/*` in the local authoritative repository, hide them from clients, and do not push them upstream

## Persistent State And Recovery Model

Correctness-critical state is split across declarative config, local bare repos, internal refs, and advisory lock directories.

Inside the bare repository:

- visible authoritative refs live under normal `refs/heads/*` and `refs/tags/*`
- observed upstream refs live under `refs/git-relay/upstreams/<upstream>/heads/*` and `refs/git-relay/upstreams/<upstream>/tags/*`
- divergence markers live under `refs/git-relay/safety/divergent/*`
- `refs/git-relay/*` is local relay control-plane state and is not replicated to upstream servers

Under `state_root`:

- `logs/structured.jsonl`: structured audit events
- `push-traces/<repo>/<push>.jsonl`: hook-path push traces
- `reconcile/pending`: queued reconcile requests
- `reconcile/in-progress`: transient run markers
- `reconcile/locks`: advisory reconcile locks
- `reconcile/runs/<repo>`: terminal reconcile run records
- `read-refresh/state`: last successful cache refresh state
- `read-refresh/negative`: negative-cache entries
- `read-refresh/access`: last observed cache read activity
- `read-refresh/locks`: advisory refresh locks
- `cache-retention/pins`: operator cache pins
- `upstream-probes/runs/<repo>`: per-repo upstream probe records
- `upstream-probes/matrix-runs/<repo>`: matrix probe records
- `upstream-probes/release-manifests/<repo>`: admitted release manifests
- `proof-e2e/<suite>`: RFC proof suite artifacts, including case evidence and redacted failure captures
- `release/git-conformance/<platform>`: machine-readable Git conformance evidence used by `release report` and retention pinning/pruning
- `release/hosts/<platform>/<host_id>`: per-host version evidence used by `release report`
- `retention/maintenance/<repo>.json`: latest maintenance result per repo

Locks and in-progress markers are advisory only. Recovery does not trust them as truth; it re-derives from repository config, local refs, internal observed refs, and fresh upstream observation when needed.

## Retention Defaults

The repository currently ships these defaults:

- `maintenance_interval = 24h`
- `cache_idle_ttl = 14d`
- `terminal_run_ttl = 30d`
- `terminal_run_keep_count = 20`
- `authoritative_reflog_ttl = 30d`
- `authoritative_prune_ttl = 7d`

Current behavior:

- daemon maintenance sweeps run during `git-relayd serve`
- unpinned cache-only repositories with visible refs and stale read activity past `cache_idle_ttl` are evicted
- pinned cache-only repositories are retained by daemon maintenance
- manual `git-relay cache evict` still evicts a pinned cache repository
- caches with no recorded activity evidence are retained rather than evicted speculatively
- reconcile runs, upstream probe runs, and matrix probe runs are pruned by age while keeping at least `terminal_run_keep_count` records per repo
- proof suite runs under `proof-e2e/`, redacted failure capture sets, and non-admitted git-conformance artifacts use the same `terminal_run_ttl` + `terminal_run_keep_count` policy
- admitted release evidence in `release/git-conformance` remains pinned until superseded by a newer admitted release
- authoritative repos run reflog expiration and `git gc` during maintenance sweeps

## Operator Workflows

Validate repository contracts and inspect startup safety:

```sh
git-relay repo validate --config /etc/git-relay/git-relay.toml --json
git-relay startup classify --config /etc/git-relay/git-relay.toml --json
git-relay repo inspect --config /etc/git-relay/git-relay.toml --repo github.com/example/repo.git --json
git-relay doctor --config /etc/git-relay/git-relay.toml --json
```

Install hook wrappers into a bare repo:

```sh
git-relay-install-hooks \
  --repo /var/lib/git-relay/repos/repo.git \
  --dispatcher /usr/local/bin/git-relay \
  --config /etc/git-relay/git-relay.toml
```

Render launchd or systemd units:

```sh
git-relay deploy render-service \
  --config /etc/git-relay/git-relay.toml \
  --format systemd \
  --binary-path /usr/local/bin/git-relayd
```

Run one daemon cycle for validation, queued reconcile, and maintenance:

```sh
git-relayd serve --config /etc/git-relay/git-relay.toml --once
```

Manual replication workflows:

```sh
git-relay replication status --config /etc/git-relay/git-relay.toml --json
git-relay replication reconcile --config /etc/git-relay/git-relay.toml --repo github.com/example/repo.git --json
git-relay replication probe-upstreams --config /etc/git-relay/git-relay.toml --repo github.com/example/repo.git --json
git-relay replication probe-matrix --config /etc/git-relay/git-relay.toml --repo github.com/example/repo.git --targets ./fixtures/hosted/targets.json --json
git-relay replication build-release-manifest --config /etc/git-relay/git-relay.toml --repo github.com/example/repo.git --targets ./fixtures/hosted/targets.json --json
```

Cache-only workflows:

```sh
git-relay read prepare --config /etc/git-relay/git-relay.toml --repo github.com/example/cache.git --json
git-relay cache pin --config /etc/git-relay/git-relay.toml --repo github.com/example/cache.git --json
git-relay cache evict --config /etc/git-relay/git-relay.toml --repo github.com/example/cache.git --json
```

Migration workflows:

```sh
git-relay migration inspect --config /etc/git-relay/git-relay.toml --flake . --json
git-relay migrate-flake-inputs --config /etc/git-relay/git-relay.toml --flake . --input-target nixpkgs --json
```

Repair and release reporting:

```sh
git-relay repo repair --config /etc/git-relay/git-relay.toml --repo github.com/example/repo.git --json
git-relay release report --config /etc/git-relay/git-relay.toml --json
```

`release report` closes `exact_git_floor` only from admitted machine-readable Git conformance records under `release/git-conformance/<platform>/` plus per-host supported-platform evidence under `release/hosts/<platform>/<host_id>/`. Conformance evidence must include the complete mandatory P01-P11 case set.

## Observability

Structured logs are JSONL and include these stable identifiers when available:

- `request_id`
- `repo_id`
- `push_id`
- `reconcile_run_id`
- `upstream_id`
- `attempt_id`
- authenticated client identity

Operator-facing JSON reports currently include:

- validator status and startup safety
- divergence markers
- replication status and latest run record
- cache retention state
- effective retention policy and last maintenance result
- migration inspection output
- release-floor report output

## Build And Verify

Cargo:

```sh
cargo fmt
cargo test
```

Nix:

```sh
nix flake check
nix build .#git-relay
nix build .#git-relayd
nix build .#git-relay-install-hooks
nix build .#git-relay-ssh-force-command
nix build .#git-relay-service-templates
```

GitHub Actions enforces the release-gate matrix in [`.github/workflows/proof-gates.yml`](./.github/workflows/proof-gates.yml): Linux `full`, macOS `full`, and an explicit provider-admission policy job.
Pure `nix flake check` keeps `rfc-proof-e2e-fast` and `rfc-proof-e2e-full` as static proof-contract checks. The live host-side gate runs through `nix run .#test`, which is the canonical full validation command and also enforces the provider-admission policy baseline. It uses flake-locked relay binaries plus pinned `git`, `openssh`, `python3`, `cargo`, and `rustc` paths so mandatory localhost SSH forced-command evidence and smart-HTTP proof-lab parity evidence are collected without falling back to developer tooling. Smart-HTTP push remains unsupported as a product ingress surface; the proof bridge is local parity infrastructure only.

Fixture-only provider-admission is a policy baseline, not real declared-target admission evidence. When real hosted targets are declared, CI or release automation must provide explicit target and credential inputs through `GIT_RELAY_PROOF_PROVIDER_TARGETS` and `GIT_RELAY_PROOF_PROVIDER_CREDENTIALS`, or provider admission fails closed. `nix run .#test -- provider-admission fixtures/hosted/targets.json` remains available for targeted provider-admission execution.

## Current Source-Truth Notes

If the README, RFC, and code diverge, treat the code and this README as the current repository truth, and treat the RFC plus verification plan as design and evidence history that may still contain open release gates. The repository intentionally fails closed rather than widening support by implication.
