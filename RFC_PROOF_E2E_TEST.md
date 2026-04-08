# RFC Proof E2E Test Plan (Deterministic Rust + Nix)

Status: implementation-driving (partially implemented; remaining-work checklist updated 2026-04-08)  
Audience: maintainers implementing release-gating proof for `git-relay`

## 1. Purpose

Define a deterministic, implementation-ready end-to-end proof plan for the RFC contract using:

- Rust test harnesses and CLIs
- Nix-pinned execution and reproducible checks

This document is the release-gating source of truth for "fully tested" behavior.

The proof contract preserves the non-negotiables:

- Git-first behavior boundaries (`git-receive-pack` / `git-upload-pack` / hooks / smart HTTP)
- fail-closed validation and policy enforcement
- local refs as durable authoritative truth
- deterministic recovery from Git state plus explicit observation
- no correctness-critical side DB or replay journal
- supported platforms only: macOS + Linux
- runtime secrets outside `/nix/store`
- no secret leakage in logs, errors, or artifacts
- stable CLI/report JSON as operator-facing contract

## 2. What "Fully Tested" Means

A proof run is complete only if it produces machine-checkable evidence for all mandatory cases:

1. P01 local acceptance and crash boundary correctness
2. P02 ordinary multi-ref push semantics (no false whole-push claim)
3. P03 reconcile execution-unit completeness
4. P04 atomic capability and policy enforcement
5. P05 partial non-atomic apply and recomputation recovery
6. P06 direct upstream drift and divergence policy
7. P07 cache-only read-path pull/refresh policy behavior
8. P08 hidden-ref and hidden-object leakage admission proof
9. P09 migration rewrite and validated relock matrix behavior
10. P10 deployment/runtime contract and retention behavior
11. P11 release evidence admission and floor-status closure

Proof gates must use real Git processes, real bare repositories, and real relay binaries.
Mocked Git protocol behavior is not accepted for release gates.

Proof is executed in two mandatory profiles:

1. `deterministic-core`
- local-only disposable topology
- no non-local network dependency
- byte-identical normalized evidence target
2. `provider-admission`
- probes provider/remote targets declared as supported for release policy
- may require non-local network connectivity
- still uses strict schema and fail-closed admission rules
- required for release admission whenever supported provider targets are declared or changed

## 3. Determinism And Security Rules

All mandatory proof runs must satisfy:

1. Deterministic-core profile has no non-local network dependency.
2. Provider-admission profile may use non-local network only for declared release-policy targets.
3. Nix-built binaries only for gate runs:
- `git-relay`
- `git-relayd`
- `git-relay-install-hooks`
- `git-relay-ssh-force-command`
- pinned `git`, `openssh`, and shell tools from Nix inputs
- transport test daemons (`sshd`, smart-HTTP wrapper) launched from Nix-pinned binaries
4. Fixed environment normalization for every subprocess:
- `TZ=UTC`
- `LC_ALL=C`
- `LANG=C`
- deterministic `HOME` inside the suite temp root
- deterministic XDG dirs inside the suite temp root
- `GIT_CONFIG_GLOBAL=/dev/null`
- `GIT_CONFIG_SYSTEM=/dev/null`
5. Fixed commit metadata for synthetic commits:
- `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`
- `GIT_COMMITTER_NAME`, `GIT_COMMITTER_EMAIL`
- fixed `GIT_AUTHOR_DATE`, `GIT_COMMITTER_DATE`
6. Stable ordering for multi-target work:
- sort by `repo_id`, `upstream_id`, `target_id`, and `case_id`
7. Disable opportunistic repo mutations that create nondeterministic timing effects:
- disable auto GC in harness-driven Git mutations
8. Dynamic run data normalization for comparison artifacts:
- run IDs
- timestamps
- PIDs
- transport port numbers
- SSH key fingerprints
- temp absolute paths
- process-dependent transient file names
9. Canonical normalized JSON encoding:
- deterministic key order
- deterministic array order where semantic set ordering applies
10. Artifact secret hygiene:
- redact sensitive values before persisting raw failure captures
- fail the case if redaction cannot safely sanitize a capture
11. Transport auth material for tests is ephemeral only:
- per-run generated SSH host keys and client keys
- per-run generated HTTP auth material
- loopback-only bindings (`127.0.0.1`)
- never use personal host credentials for proof tests
- never persist private key/auth secrets in repo files, Nix store paths, or raw artifacts
12. Runner isolation:
- proof harness executes cases serially under one orchestrator target
- no parallel case execution for deterministic-core profile
- one daemon set (`sshd` and smart HTTP) per case lifecycle
- dynamic port assignment is allowed; ports are normalized in comparison artifacts
- proof suite target runs with deterministic test threading (single-threaded execution)

Determinism target:

1. deterministic-core profile:
- two consecutive suite executions on the same host and commit produce byte-identical `summary.normalized.json`
- the suite records and compares `summary.normalized.sha256`
2. provider-admission profile:
- schema-valid evidence and deterministic policy verdicts are required
- byte-identical cross-run output is not required because remote provider state may evolve

## 4. Proof Lab Topology

The proof lab is local and disposable, created per suite run under one temp root.

Core repositories:

1. `relay-authoritative.git` (authoritative local bare repo under relay control)
2. `relay-cache.git` (cache-only local bare repo)
3. `upstream-alpha.git` (atomic-capable upstream)
4. `upstream-beta.git` (non-atomic upstream, e.g. `receive.advertiseAtomic=false`)
5. `upstream-gamma.git` (unreachable or intentionally failing upstream)
6. `upstream-read.git` (read upstream for cache-only repo)

Worktrees/actors:

1. `client-work/` pushes to relay authoritative ingress
2. `external-alpha-work/` mutates `upstream-alpha.git` directly (out-of-band drift)
3. `external-read-work/` mutates `upstream-read.git` directly (cache refresh input)

Ingress surfaces:

1. SSH forced-command path (mandatory)
2. Local smart HTTP path via `git-http-backend` (mandatory)

Transport test servers (mandatory in proof lab):

1. Ephemeral localhost `sshd` process with generated per-run host key and test client key
2. Ephemeral localhost smart-HTTP process serving `git-http-backend`
3. Both servers wired to disposable local bare repositories under the suite temp root
4. Both servers configured without dependence on developer machine credentials

Note:

- smart HTTP ingress remains an explicit deployment feature toggle in product config
- proof gating still requires it in the test lab to validate Git boundary parity across SSH and smart HTTP
- containerized remotes are not required for mandatory gates; local disposable directories plus localhost daemons are the baseline

Transport scope contract:

1. SSH coverage is mandatory for all ingress-sensitive cases.
2. Smart HTTP coverage is mandatory for:
- read-path parity checks (P07 and any read-sensitive checks in P01-P06/P10)
- local write-boundary parity checks for P01/P02 in the proof lab
3. Mandatory smart HTTP proof coverage does not change deployment defaults.
- production enablement remains explicit policy
- proof coverage exists to validate boundary invariants, not to silently widen product scope

## 5. Harness Architecture

Primary orchestrator target:

- `tests/rfc_proof_e2e.rs`

Support modules (under `tests/proof_support/`):

1. `lab.rs`: deterministic topology creation and fixture ownership
2. `cmd.rs`: subprocess wrapper, env normalization, capture, redaction
3. `transport.rs`: lifecycle management for ephemeral localhost `sshd` and smart-HTTP daemons
4. `cases/`: one module per proof case (`p01.rs` ... `p11.rs`)
5. `artifact.rs`: typed artifact model and persistence helpers
6. `normalize.rs`: normalization + canonical JSON encoding
7. `schema.rs`: artifact schema and compatibility checks

Ownership boundary:

- harness verifies through public CLI/binary behavior
- reusable logic lives in library modules under `src/`
- entrypoint binaries remain thin

High-level data flow:

1. Build lab topology
2. Execute each case in deterministic order
3. Persist raw per-case evidence
4. Normalize and aggregate suite summary
5. Enforce determinism check (rerun + hash compare in full mode)
6. Emit pass/fail verdict and release-gating artifacts

## 6. Case Contract Template

Every case must declare and implement:

1. `setup`: exact topology/config/hook state prerequisites
2. `action`: exact CLI/Git operations and crash/failure injection points
3. `assertions`: deterministic expected outputs and state transitions, declared in case metadata as required assertion IDs and validated by the harness
4. `pass_criteria`: all required assertions that must hold
5. `fail_criteria`: any condition that must fail closed
6. `artifacts`: required raw and normalized files for the case, declared in case metadata as required artifacts and validated by the harness
7. `contract_refs`: RFC + verification-plan clauses covered

## 7. Scenario Matrix (Mandatory Cases)

### P01 Local acceptance and crash boundary

Goal:

- prove accepted local refs follow the committed local Git transaction boundary

Required checks:

1. Crash before commit checkpoints does not leave committed refs
2. Crash after commit checkpoints preserves committed refs
3. `post-receive` failure is non-critical for local acceptance
4. `git fsck --strict` remains clean
5. SSH and smart HTTP ingress paths both satisfy the same boundary

Pass:

- no acknowledged push is missing locally after restart/recovery

Fail:

- any pre-commit crash leaves committed refs, or any acknowledged ref is missing

### P02 Ordinary multi-ref push semantics

Goal:

- prove ordinary native inbound pushes are not upgraded to whole-push guarantees

Required checks:

1. Relay guard rejects transmitted invalid updates early
2. Client-side pruning cases are detected and evidence is explicit
3. Verdict never claims whole-push local semantics for ordinary native pushes
4. SSH and smart HTTP transport observations align

Pass:

- final case report explicitly constrains local contract to refs Git actually committed

Fail:

- any proof verdict implies whole-push all-or-nothing semantics for ordinary pushes

### P03 Reconcile execution-unit completeness

Goal:

- prove one run captures one desired snapshot and one upstream set, with mixed outcomes

Required checks:

1. One run ID carries all per-upstream outcomes
2. Upstream attempt order is deterministic
3. Terminal evidence is kept while transient markers are cleaned
4. Superseded stale runs are recorded as superseded, not silently discarded

Pass:

- one bounded run record with mixed terminal per-upstream outcomes and clean transient state

Fail:

- missing upstream result, nondeterministic ordering, or stale transient state as correctness dependency

### P04 Atomic capability and policy enforcement

Goal:

- prove behavior-based atomic capability classification and fail-closed policy handling

Required checks:

1. `upstream-alpha` classified `supported`
2. `upstream-beta` classified `unsupported` when atomic capability absent
3. `require_atomic=true` upstreams never downgraded silently to best effort

Pass:

- unsupported/inconclusive atomic capability remains unconverged and repo safety reflects degradation

Fail:

- any path falls back to non-atomic apply for `require_atomic=true`

### P05 Partial non-atomic apply and recomputation recovery

Goal:

- prove recovery from partial apply without replay log

Required checks:

1. Failed non-atomic apply does not mutate observed namespace optimistically
2. Next run recomputes desired state from current local refs and policy
3. Later reconcile converges without replaying push history

Pass:

- observed refs change only via explicit observation steps

Fail:

- optimistic observed-ref mutation or replay-history dependency

### P06 Direct upstream drift and divergence policy

Goal:

- prove out-of-band upstream mutation detection and divergence enforcement

Required checks:

1. External actor mutates upstream directly
2. Relay marks out-of-sync/divergent according to policy
3. New authoritative writes are blocked while divergent
4. Repair + reconcile restores healthy write state

Pass:

- divergence marker persisted and write block enforced until intentional repair

Fail:

- writes accepted while divergent or divergence inferred from stale cached observation only

### P07 Cache-only pull/refresh behavior

Goal:

- prove cache-only repos track read-upstream updates under explicit read policy

Required checks:

1. External actor mutates read upstream
2. `read prepare` refreshes cache according to freshness policy
3. negative-cache and stale-serving behavior matches policy
4. cache commands fail closed for authoritative repositories

Pass:

- cache-only behavior is policy-conformant and does not leak into authoritative semantics

Fail:

- stale serving occurs without explicit stale policy, or cache commands succeed on authoritative repo

### P08 Hidden-ref and hidden-object leakage proof

Goal:

- prove same-repo hidden-ref admission is allowed only when leakage checks pass

Required checks:

1. `refs/git-relay/*` is not advertised to clients
2. Internal-only objects are not fetchable by guessed object ID when required upload-pack restrictions are set
3. Admission fails closed if leakage is possible

Pass:

- same-repo hidden-ref target admitted only when both ref advertisement and object-ID leakage checks pass

Fail:

- any target is admitted when leakage checks fail or are missing

### P09 Migration rewrite and relock matrix behavior

Goal:

- prove deterministic rewrite and validated-only targeted relock contract

Required checks:

1. Supported literal shorthand rewrites are deterministic
2. Unsupported/ambiguous forms fail closed
3. Second rewrite is no-op
4. Targeted relock only allowed for validated Nix versions and graph shapes
5. Scope-violation and non-idempotent relock restores original files

Pass:

- deterministic rewrite + scoped idempotent relock inside validated matrix only

Fail:

- out-of-matrix relock proceeds or failed relock leaves partial file mutation

### P10 Deployment/runtime contract and retention behavior

Goal:

- prove runtime environment constraints, service rendering, and retention behavior

Required checks:

1. runtime env file is required, absolute, and outside `/nix/store`
2. launchd/systemd render output is valid and deterministic
3. hook installation and forced-command routing are operational
4. `git-relayd serve --once` drains queued reconcile and applies default retention policy
5. terminal run evidence pruning follows configured TTL/keep-count policy

Pass:

- runtime validation passes and retention outcomes match configured policy

Fail:

- runtime contract bypassed or retention behavior differs from declared policy

### P11 Release evidence admission and floor status

Goal:

- prove release report is evidence-driven, machine-readable, and fail-closed

Required checks:

1. matrix runs persist release-manifest evidence
2. missing/unadmitted targets keep manifest/report open
3. platform evidence is persisted per supported host
4. exact floor status closes only when required machine-readable evidence exists

Pass:

- no floor closes without complete admitted evidence

Fail:

- floor closes by assumption or incomplete evidence

## 8. Artifact Layout, Schema, And Normalization

Artifact root per suite run:

- `<state_root>/proof-e2e/<suite_run_id>/`

Required files:

1. `summary.raw.json`
2. `summary.normalized.json`
3. `summary.normalized.sha256`
4. `cases/<case_id>.raw.json`
5. `cases/<case_id>.normalized.json`
6. `logs/structured-events.raw.jsonl`
7. `logs/structured-events.redacted.jsonl`
8. `refsnapshots/<repo>.txt`
9. `manifests/release/` (release evidence snapshots)
10. `manifests/git-conformance/` (machine-readable Git conformance evidence)
11. `failures/<case_id>/<step>.{stdout,stderr}.txt` (redacted)

Top-level normalized summary schema:

```json
{
  "schema_version": 1,
  "suite": "rfc-proof-e2e",
  "mode": "fast|full|provider-admission",
  "toolchain": {
    "git_version": "...",
    "nix_version": "...",
    "openssh_version": "..."
  },
  "cases": [
    {
      "case_id": "P01",
      "status": "pass|fail",
      "assertions": [
        {
          "id": "string",
          "status": "pass|fail"
        }
      ]
    }
  ],
  "overall_status": "pass|fail"
}
```

Normalization policy:

1. Replace dynamic fields with placeholders:
- run IDs
- timestamps
- PIDs
- temp paths
2. Preserve semantic verdicts, error classes, and policy outcomes
3. Encode normalized JSON canonically for byte-identical comparison

Secret-safety policy:

1. Never persist runtime env values verbatim
2. Required redaction classes before persisting captures:
- values from runtime env file entries
- environment variables with names containing: `TOKEN`, `SECRET`, `PASSWORD`, `PASS`, `KEY`, `AUTH`
- HTTP `Authorization` header values
- URL credentials in authority segments (`scheme://user:pass@host/...`)
- PEM private key blocks (`-----BEGIN ... PRIVATE KEY-----`)
3. Redaction output uses deterministic placeholder tokens (`<redacted:class>`)
4. If redaction fails or sensitive material remains detectable, fail the case

Proof artifact retention defaults (mandatory):

1. raw suite runs: `ttl=720h`, `keep_count=20`
2. redacted failure captures: `ttl=720h`, `keep_count=20`
3. non-admitted conformance artifacts: `ttl=720h`, `keep_count=20`
4. admitted release evidence remains pinned until superseded by a newer admitted release
5. P10 must verify retention behavior against these defaults unless explicitly overridden by a deterministic test fixture

## 9. Machine-Readable Git Conformance Evidence

Required file:

- `manifests/git-conformance/<platform>/<git_version_key>.json`

Path key contract:

1. `git_version_key` is a sanitized deterministic key derived from raw `git_version`
2. raw `git_version` remains preserved in JSON payload

Required schema:

```json
{
  "schema_version": 1,
  "profile": "deterministic-core|provider-admission",
  "git_version_key": "...",
  "platform": "macos|linux",
  "nix_system": "...",
  "service_manager": "launchd|systemd",
  "git_version": "...",
  "openssh_version": "...",
  "filesystem_profile": "...",
  "git_relay_commit": "...",
  "flake_lock_sha256": "...",
  "binary_digests": {
    "git-relay": "...",
    "git-relayd": "...",
    "git-relay-install-hooks": "...",
    "git-relay-ssh-force-command": "..."
  },
  "cases": [
    {
      "case_id": "P01",
      "status": "pass|fail"
    }
  ],
  "all_mandatory_cases_passed": true,
  "normalized_summary_sha256": "...",
  "recorded_at_ms": 0
}
```

Usage contract:

1. release reporting ingests only this schema for Git floor closure
2. exact Git floor remains open if required evidence is missing on any supported platform
3. evidence without mandatory-case pass status is non-admitting
4. provider-admission evidence is required for every target declared as supported in release policy

## 10. Nix Integration And Check Topology

Proof execution must be wired into flake checks.

Required checks:

1. `checks.<system>.rfc-proof-e2e-fast`
2. `checks.<system>.rfc-proof-e2e-full`
3. `checks.<system>.rfc-proof-provider-admission`

Execution contract:

1. checks run in Nix derivations, not ad-hoc host scripts
2. proof harness invokes only Nix-built relay binaries (no cargo-bin fallback in gate mode)
3. SSH and smart HTTP ingress validation are mandatory in deterministic-core modes (`fast` and `full`)
4. mandatory transport checks use ephemeral localhost daemons with generated test credentials, not developer host credentials
5. `provider-admission` requires explicit target manifest and credentials; if invoked without required inputs it must fail closed with actionable diagnostics

Mode contract:

1. `fast`:
- single-pass deterministic mandatory scenarios
- local-only upstream topology
- machine-readable artifacts produced
2. `full`:
- includes `fast`
- repeated determinism check (rerun + hash compare)
- extended crash and retention variants
- full release-evidence aggregation flow
3. `provider-admission`:
- runs conformance probes for all targets declared supported in release policy
- may use non-local network
- emits machine-readable provider-admission evidence
- fails closed for any target without admitting evidence

Cross-platform release gate:

1. `full` must pass on at least one supported Linux host
2. `full` must pass on at least one supported macOS host
3. `provider-admission` must pass for all declared supported provider targets
4. release floor closure requires both platform evidence sets admitted plus admitted provider-target evidence

## 11. Failure Injection And Multi-Upstream Simulation

Failure injection primitives:

1. named crash checkpoints via crash env controls
2. upstream capability toggles (e.g. `receive.advertiseAtomic=false`)
3. direct external upstream mutation actors
4. missing/unreachable upstream simulation via absent local endpoints
5. stale lock/in-progress marker seeding with dead PID metadata

Multi-upstream simulation contract:

1. one reconcile run captures one desired snapshot and one upstream set
2. upstream attempts execute in sorted deterministic order
3. mixed outcomes in one run are mandatory and operator-visible
4. no cross-upstream atomicity is implied

## 12. Implementation Milestones

### M0 Contract freeze

Deliver:

1. this proof RFC updated with fixed decisions and explicit case contract template

Acceptance:

1. no blocking open questions remain in this document

### M1 Harness and determinism foundation

Deliver:

1. `tests/rfc_proof_e2e.rs` orchestrator
2. `tests/proof_support/*` helpers
3. raw + normalized artifact emission
4. redaction + canonical normalization

Acceptance:

1. one case can run end-to-end and emit schema-valid artifacts
2. full-mode rerun hash comparison is implemented

### M2 Contract-critical scenarios

Deliver:

1. P01-P07 implemented with deterministic assertions

Acceptance:

1. each case produces explicit pass/fail assertions and required artifacts

### M3 Security and release closure

Deliver:

1. P08-P11 implementation
2. machine-readable Git conformance evidence generation
3. release report ingestion for floor closure gates

Acceptance:

1. release floor remains open on missing evidence
2. floor closure path is machine-checkable and deterministic

### M4 Nix gate integration

Deliver:

1. flake checks wired for `rfc-proof-e2e-fast` and `rfc-proof-e2e-full`
2. flake check wired for `rfc-proof-provider-admission`
3. CI policy enforcing platform gate matrix and provider-admission policy

Acceptance:

1. Linux + macOS `full` pass required for release admission
2. declared supported provider targets cannot be admitted without passing `provider-admission`

### Current repository status checklist (updated 2026-04-08)

Completed in the current repository state:

- [x] M0 contract freeze content is present in this document.
- [x] M1 harness foundation exists: `tests/rfc_proof_e2e.rs`, `tests/proof_support/*`, raw + normalized artifacts, redaction, and canonical normalization are implemented.
- [x] P01-P11 case modules exist and run in the proof harness on the current host.
- [x] `flake.nix` wires `rfc-proof-e2e-fast`, `rfc-proof-e2e-full`, and `rfc-proof-provider-admission`.

Still required before this RFC proof contract is fully satisfied:

- [x] Remove SSH skip-to-pass behavior and require SSH evidence in deterministic-core proof runs.
- [x] Align `fast` mode with the mandatory-scenarios contract for P01 crash-boundary coverage, or narrow the mode contract if crash-boundary coverage remains `full`-only.
- [x] Add explicit P02 client-side pruning evidence.
- [x] Tighten P03 to assert mixed terminal per-upstream outcomes explicitly, not only upstream count and ordering.
- [x] Tighten P04 to assert repository safety degradation explicitly when `require_atomic = true` cannot be admitted.
- [x] Tighten P05 to prove recovery and later convergence do not depend on replayed push history.
- [x] Tighten P06 to assert divergence-marker persistence before repair clears it.
- [ ] Tighten P08 so hidden-object leakage proof is mandatory whenever same-repo hidden-ref admission relies on SSH transport probing.
- [ ] Extend P09 E2E proof to cover scope-violation and non-idempotent relock restore behavior, not only unsupported grammar and unsupported Nix version rejection.
- [ ] Extend P10 E2E proof to cover `/nix/store` runtime env rejection in the release-gating proof suite.
- [ ] Implement release-report ingestion of machine-readable Git conformance evidence and add the positive P11 floor-closure path once that ingestion exists.
- [ ] Enforce Linux + macOS `full` plus provider-admission policy in CI or release automation, not only in local `flake.nix` wiring.
- [x] Align case metadata with the section 6 case contract template by declaring required assertions and artifacts explicitly.
- [x] Align normalization and failure-capture outputs with the documented contract, including `repo_id` semantic ordering, per-step failure capture naming, and deterministic git-conformance timestamps or an explicitly narrowed determinism claim.

## 13. Validation Matrix (Release Gate vs Extended)

Mandatory release-gate scenarios:

1. P01-P11 on supported platform hosts
2. multi-upstream fan-out semantics are mandatory:
- one local accepted push must drive one reconcile execution unit over a captured upstream set with mixed per-upstream outcomes allowed
3. ingress-sensitive cases must cover SSH and smart HTTP
4. machine-readable conformance artifacts must be present and admitted
5. for every upstream/provider target declared as supported in release policy, conformance evidence is mandatory before admission

Optional extended scenarios:

1. exploratory probes for providers/targets not yet declared supported
2. stress/soak runs beyond deterministic correctness gate criteria

Optional scenarios must not weaken mandatory gate outcomes.

## 14. Red-Team Risk Register

1. False confidence from over-normalization hiding true regressions  
Mitigation: normalize only explicitly approved dynamic fields; preserve raw artifacts
2. Hidden secret leakage in failure captures  
Mitigation: mandatory redaction + fail-on-detection gate
3. Drift between CLI contract and harness expectations  
Mitigation: schema versioning and explicit contract-reference mapping per case
4. Platform-specific filesystem behavior masked by single-host testing  
Mitigation: require admitted evidence from both macOS and Linux
5. Assuming provider behavior by name instead of measured session behavior  
Mitigation: capability/admission only from concrete probe evidence

## 15. Fixed Decisions (Previously Open Questions)

Resolved on 2026-04-07:

1. Smart HTTP ingress in proof gate:
- mandatory
2. Gate binary provenance:
- Nix-built binaries only for proof-gating execution
3. Git floor closure evidence:
- required machine-readable conformance schema and ingestion
4. Proof artifact retention:
- bounded by default and validated as part of mandatory proof behavior

No blocking open questions remain in this document.

## 16. Acceptance Criteria For This Document

This plan is implementation-ready when:

1. every mandatory case has explicit setup/action/assertions/pass/fail/artifacts
2. each case maps to concrete RFC and verification-plan contract clauses
3. Nix check wiring and platform gate policy are fully specified
4. deterministic artifact schema and normalization rules are explicit
5. secret-redaction and retention behavior are explicitly test-gated
