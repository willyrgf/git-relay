# Git Relay Development Guide for AI Agents

This document is the source-of-truth for AI agents working in this repository.
It is based on the same engineering values as the reference Rust+Nix guide you provided: small changes, strong typing, thin entrypoints, explicit boundaries, and tests for risky behavior.

For this project, those values must be applied to Git Relay's actual design:

- Git-first, not tarball-first.
- Native Git transport boundaries, not custom protocol reimplementation.
- Local bare repositories as the durable truth source.
- Deterministic recovery from Git state, not a replay journal or SQLite metadata store.
- Fail-closed validation, migration, and deployment behavior.

## Read First (Non-Negotiables)

- Keep changes small and local; prefer one logical change per commit or PR.
- If you commit, use a lower-case subject line.
- Treat `git-relay-rfc.md` as the design contract.
- Treat `verification-plan.md` as the evidence-backed narrowing of that draft contract.
  When the RFC is broader than measured behavior, preserve the narrower verified behavior and update both docs together.
- Never log, print, or persist secrets.
  This includes auth material, runtime env secrets, tokens, private keys, and anything derived from them.
- Keep binaries thin:
  - `src/main.rs`
  - `src/bin/git-relayd.rs`
  - `src/bin/git-relay-ssh-force-command.rs`
  - `src/bin/git-relay-install-hooks.rs`
  Entry points should parse args or env, call reusable library code, and render results only.
- Preserve the Git boundary:
  `git-receive-pack`, `git-upload-pack`, hook execution, and smart HTTP are the truth boundary for Git behavior.
  Do not smuggle correctness-critical semantics into wrappers or side files.
- Preserve the authoritative write contract:
  local acknowledgement is `local-commit` for the refs Git actually committed.
  Do not claim ordinary native inbound pushes are whole-push atomic.
- Preserve the reconcile contract:
  one run is one bounded execution unit with one desired snapshot and one captured upstream set.
  Mixed per-upstream terminal outcomes are allowed.
  Cross-upstream atomicity is not.
- Preserve the startup contract:
  upstream state starts as `unknown` until fresh observation.
  Cached observation is advisory only at startup.
- Preserve the hidden-ref contract for authoritative repositories:
  `refs/git-relay/*` is internal-only and must not be pushable or exposed to clients.
- Runtime secrets must remain outside `/nix/store`.
- Supported deployment platforms are macOS and Linux only.
  Supported service managers are launchd on macOS and systemd on Linux.
- Targeted flake relock is only promised inside the validated Nix/version-and-graph matrix captured by the code and `verification-plan.md`.

## Source of Truth

- `git-relay-rfc.md`: authoritative product and architecture contract.
- `verification-plan.md`: measured constraints, release gates, and narrowed claims.
- `flake.nix`: canonical packaging, checks, app entrypoints, and dev shell.
- `packaging/example/git-relay.example.toml`: example deployment profile.
- `packaging/example/git-relay.env.example`: runtime secret shape.
- `tests/cli.rs`: end-to-end fixtures showing the intended authoritative and cache-only contracts.

## Design Contract (Architecture Invariants)

The code should continue to enforce these invariants.
Treat violations here as high risk.

- Git Relay is Git-first.
  Do not introduce a second foundational data plane for archive or tarball compatibility.
- MVP persistent truth is Git plus declarative config.
  Do not add SQLite or correctness-critical metadata stores for write acceptance or reconcile truth.
- Local authoritative refs are the accepted durable state after successful `git-receive-pack`.
- Ordinary native inbound pushes are not whole-push atomic at the relay boundary.
  Client-requested `--atomic` may be honored by Git, but hooks alone must not be treated as proof or enforcement.
- Reconcile derives from current local refs and current observation.
  It must not depend on replaying every accepted push event.
- Apply attempts must not mutate observed upstream truth optimistically.
  Observed refs change only from explicit observation.
- Startup is conservative.
  `unknown` at startup is correct until fresh observation completes.
- Filesystem locks and in-progress markers are transient coordination artifacts, not correctness sources.
- Internal tracking refs stay under `refs/git-relay/*`.
  In the initial implementation, tracking placement is `same-repo-hidden`.
- Same-repo hidden tracking refs are valid only if authoritative repos enforce all of:
  - `receive.fsckObjects=true`
  - `transfer.hideRefs=refs/git-relay`
  - `uploadpack.hideRefs=refs/git-relay`
  - `receive.hideRefs=refs/git-relay`
  - `uploadpack.allowReachableSHA1InWant=false`
  - `uploadpack.allowAnySHA1InWant=false`
  - `uploadpack.allowTipSHA1InWant=false`
  - `core.fsync=all`
  - `core.fsyncMethod=fsync`
- Authoritative repos fail closed unless validator checks pass.
- Cache-only repos must not accept writes and must define read upstreams.
- Automatic reconciliation in the initial implementation is `on_push + manual` only.
  Do not silently grow periodic reconcile semantics without updating config, validator, docs, and tests together.
- CLI, audit, migration, and report JSON shapes are operator-facing contracts.
  Do not change them casually.
- Config parsing is intentionally strict.
  Preserve `serde(deny_unknown_fields)` and fail-closed behavior unless there is a strong compatibility reason.

## Core Principles

- Modularity: keep reusable behavior in library modules under `src/`.
- Explicit boundaries: transport wrappers and CLIs should stay thin over library logic.
- Type safety: prefer typed enums and structs over stringly typed branching.
- Correctness first: Git correctness, repo safety, and migration safety beat convenience.
- Recovery by recomputation: prefer deriving state from repositories and current observation over inventing extra mutable metadata.
- Stable operator surface: configs, JSON reports, and rendered service units should evolve deliberately.
- Performance with proof: avoid needless process spawning or string copying in hot paths, but do not trade away clarity without evidence.

## Module Placement Contract

Keep new logic in the module that matches the existing responsibility split.

- `src/config.rs`
  Config schema, descriptor schema, strongly typed policy enums, parsing.
- `src/validator.rs`
  Repository contract validation, deployment profile enforcement, authoritative hardening checks.
- `src/ssh_wrapper.rs`
  Parse `SSH_ORIGINAL_COMMAND`, canonicalize repo selection, authorize allowed Git services.
- `src/hooks.rs`
  Hook installation, hook dispatch, local write acceptance guardrails, enqueue reconcile requests.
- `src/read_path.rs`
  Read preparation, refresh policy behavior, negative cache, cache retention controls.
- `src/reconcile.rs`
  Reconcile coordination, run records, divergence markers, lock handling, repair.
- `src/upstream.rs`
  Upstream probing, atomic capability detection, matrix probes, release manifests.
- `src/migration.rs`
  Flake shorthand inspection, deterministic rewrites, targeted relock, fail-closed migration rules.
- `src/deploy.rs`
  Runtime-profile validation and launchd/systemd rendering.
- `src/classification.rs`
  Startup and safety classification only.
- `src/audit.rs`
  Structured audit event shape and JSONL append behavior.
- `src/release.rs`
  Release-floor and conformance reporting from recorded evidence.
- `src/git.rs`
  Shared Git command abstraction.
- `src/platform.rs`
  Host platform and filesystem probing.
- `src/crash.rs`
  Crash checkpoint injection only.

If you are adding behavior to a binary and it could be reused or tested in-process, it probably belongs in the library instead.

## Development Workflow

Use the repo's real commands, not copied commands from another project.

### Local setup

```bash
nix develop
```

### Format and lint

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
```

### Test

```bash
cargo test
```

### Nix build and checks

```bash
nix build .#git-relay
nix build .#git-relayd
nix build .#git-relay-service-templates
nix flake check
```

Prefer `nix flake check` before landing changes that affect packaging, service rendering, or the deployment surface.

## Code Style and API Guidelines

### Rust

- Prefer explicit and readable code over cleverness.
- Avoid panics in library code.
  Return typed errors with `thiserror` where behavior matters.
- Prefer typed errors over dynamic error plumbing except in top-level glue.
- Prefer small helper functions over large nested branches.
- Minimize cloning in hot or repeated Git paths.
- Keep serialization types stable and intentionally named.

### Errors and Output

- Config, migration, reconcile, validator, deploy, and upstream code should keep error messages specific and actionable.
- Do not leak secrets into error strings.
- CLI JSON output is part of the operator contract.
  If you change a serialized struct used by commands, update tests and docs in the same change.
- Structured audit logs should carry IDs and state, not sensitive material.

### Unsafe

Avoid `unsafe`.
If you must use it:

- explain the safety invariant directly in code,
- keep the unsafe surface minimal,
- and add tests that would fail if the invariant is broken.

Current note:
`src/crash.rs` uses `libc::_exit` to model crash checkpoints precisely.
Do not casually expand that pattern elsewhere.

## Sensitive Areas

Changes here require extra care and usually extra tests.

### Authoritative write path

Files:

- `src/hooks.rs`
- `src/ssh_wrapper.rs`
- `src/validator.rs`
- `src/crash.rs`

Rules:

- Do not weaken `local-commit` acknowledgement semantics.
- Do not treat wrapper cleanup or `post-receive` as part of acceptance truth.
- Do not allow clients to push `refs/git-relay/*`.
- Do not weaken authoritative hardening requirements without updating RFC, verification plan, validator, fixtures, and tests together.

### Reconcile and observed state

Files:

- `src/reconcile.rs`
- `src/upstream.rs`
- `src/classification.rs`

Rules:

- Keep lock contents advisory only.
- Keep run records terminal and operator-visible.
- Preserve the difference between local acceptance, per-upstream atomic apply, and multi-upstream execution completeness.
- If `require_atomic = true`, unsupported or ambiguous capability must remain unsupported, not downgraded to best effort.

### Migration

File:

- `src/migration.rs`

Rules:

- Rewrite only supported literal shorthand forms.
- Unsupported or ambiguous inputs must fail closed.
- Keep dirty-worktree refusal behavior intact unless explicitly changed with docs and tests.
- Keep targeted relock scoped and idempotent for the validated matrix only.
- Never silently broaden the supported Nix matrix without evidence and corresponding doc updates.

### Deployment and secrets

Files:

- `src/deploy.rs`
- `packaging/example/git-relay.example.toml`
- `packaging/example/git-relay.env.example`

Rules:

- Keep runtime secret files absolute and outside `/nix/store`.
- Preserve the macOS/Linux plus launchd/systemd support contract.
- If service render output changes, update build checks and example artifacts in the same change.

## Testing Guidance

Prefer boundary-focused tests that lock in behavior at the integration seams this project cares about.

- Use real bare repositories and real `git` commands when testing Git behavior.
- Prefer `tempfile::TempDir` fixtures like the existing tests.
- Add regression tests for any bug that can reappear.
- If you touch authoritative validation, add tests for the exact required Git config keys.
- If you touch SSH command parsing or authorization, add tests for malformed commands, repo-root escapes, lifecycle checks, and divergence blocking.
- If you touch hook behavior, add tests for allowed refs, denied internal refs, denied deletes, divergence rejection, and reconcile enqueueing.
- If you touch reconcile behavior, add tests for run recording, stale lock repair, superseding, divergence markers, and terminal cleanup.
- If you touch migration behavior, add tests for deterministic rewrite output, unsupported expressions, relock scope, and idempotence.
- If you touch service rendering or runtime validation, add tests covering both supported service formats where applicable.

## Documentation Updates Required

Update docs in the same change when you change behavior.

- Update `git-relay-rfc.md` when you change design contracts, invariants, or supported behavior.
- Update `verification-plan.md` when measured behavior, release gates, or validated matrices change.
- Update `packaging/example/git-relay.example.toml` when config shape or deployment expectations change.
- Update `packaging/example/git-relay.env.example` when required secrets change.
- Update `README.md` if the repo entrypoint docs need to point at new operator-facing behavior.

Documentation is not follow-up work for contract changes.

## What To Avoid

- Do not add a database or replay-log dependency for correctness-critical write or reconcile state unless the architecture itself is being intentionally changed.
- Do not introduce new dependencies without clear justification.
- Do not weaken fail-closed parsing or validation just to make fixtures easier.
- Do not store secrets in fixtures, structured logs, config examples, snapshots, or error payloads.
- Do not casually change JSON report shapes, descriptor schema, or service-render output.
- Do not let cache-only behavior leak into authoritative semantics or vice versa.
- Do not treat lockfiles, marker files, or audit logs as the authoritative ref source.

## Commenting Guidance

Write comments for future readers, not for the PR.

Good comments explain constraints or invariants, for example:

```rust
// Observed upstream refs are written only after explicit observation.
// Apply attempts must not mutate the observed namespace optimistically.
```

Avoid comments that restate the code.

## Practical Default

When in doubt, preserve these four properties:

1. Native Git remains the authoritative behavior boundary.
2. Current local refs remain the durable source of truth.
3. Recovery remains deterministic from repository state plus explicit observation.
4. Unsupported or ambiguous cases fail closed instead of guessing.
