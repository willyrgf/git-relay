# RFC Verification Plan

This document turns the RFC review blockers into concrete verification work.

It is not evidence. It is the plan needed to produce evidence.

## Baseline

Tool versions detected in this workspace on 2026-04-03:

- Git: `2.53.0`
- OpenSSH: `10.2p1`
- Nix: `2.26.3`

These versions are good enough to start prototype work. They are not yet validated RFC floors.

## Required Outcome

The RFC should not move into implementation-driving status until each blocker below has:

- a prototype or fixture suite,
- a pass/fail acceptance test,
- a proposed final contract,
- and an RFC rewrite based on measured behavior.

## Workstreams

### 1. Local Accept And Crash Safety

Scope:

- client push to relay
- hook behavior
- local ref transaction outcome
- crash windows before and after acknowledgement

Deliverable:

- a minimal SSH-served authoritative bare repository
- a forced-command wrapper around `git-receive-pack`
- crash injection at named checkpoints

### 2. Upstream Observation And Reconcile

Scope:

- desired state computation
- observed upstream state tracking
- atomic and non-atomic upstream apply
- multi-upstream state derivation

Deliverable:

- one local authoritative bare repository
- two or more upstream bare repositories
- a reconcile driver that can inject partial remote failure

### 3. Nix Migration And Relock

Scope:

- shorthand parsing
- deterministic rewrite output
- targeted relock scope
- cross-version lockfile behavior

Deliverable:

- a fixture corpus of `flake.nix` and `flake.lock` inputs
- a rewrite runner
- a version matrix for Nix

### 4. Runtime And Deployment Contract

Scope:

- supported filesystem semantics
- Git and OpenSSH config contract
- hidden ref behavior
- packaging and service composition

Deliverable:

- an environment matrix
- a deployment profile with explicit supported combinations

## Verification Matrix

### A. `local-commit` acknowledgement correctness

Why it is blocking:

- The RFC promises that acknowledged local refs are durable and readable after crash.
- That promise is only real if the acknowledgement boundary matches Git's committed local ref transaction and does not depend on best-effort wrapper logic.

What to build:

- An SSH ingress harness using OpenSSH `ForceCommand` into a wrapper that execs `git-receive-pack`.
- `pre-receive`, `reference-transaction`, and `post-receive` hooks that write structured events.
- Crash injection at these checkpoints:
  - before `pre-receive`
  - after `pre-receive` success
  - after `reference-transaction prepared`
  - after `reference-transaction committed`
  - after `git-receive-pack` exits success but before wrapper flushes final response
  - after wrapper flushes response

How to verify:

- Push one ref and multiple refs.
- Repeat with crash injection at each checkpoint.
- After restart, inspect bare refs and run `git fsck`.
- Record whether the client saw success, failure, or ambiguity.

Pass criteria:

- No acknowledged push is ever missing locally after restart.
- No rejected push leaves committed refs behind.
- The only ambiguous case is "commit happened but client did not observe success".
- `post-receive` failure never changes accept/reject outcome.

Proposed solution:

- Define the local acknowledgement boundary as successful completion of `git-receive-pack` after Git has committed the local ref transaction.
- Treat the wrapper as transport control only. It must not manufacture success after a failed `git-receive-pack`.
- Allow only `pre-receive`, `reference-transaction`, and `post-receive` in MVP.
- Forbid `update`, `proc-receive`, or any hook that can introduce per-ref custom acceptance semantics in MVP.

RFC rewrite required:

- Rewrite the write path so the local contract is "Git committed local refs" and everything else is non-atomic follow-up work.

### B. Whole-push all-or-nothing local acceptance

Why it is blocking:

- The RFC claims whole-push acceptance.
- That claim is unsafe unless it survives one invalid ref, concurrent contention, and hook failure.

What to build:

- Multi-ref push fixtures:
  - valid branch plus invalid protected branch
  - valid branch plus forbidden delete
  - valid tag plus non-fast-forward branch
- A contention helper that creates ref update races during receive.

How to verify:

- For each fixture, push repeatedly under contention.
- Inspect resulting refs after each run.
- Confirm that either all proposed refs changed or none changed.

Pass criteria:

- No run leaves a subset of pushed refs committed.
- Any ref-lock or transaction failure yields whole-push failure.

Proposed solution:

- Keep MVP whole-push only.
- Put all policy rejection in `pre-receive`.
- Treat any Git ref transaction failure as whole-push failure.
- Keep local writes on one `git-receive-pack` transaction path and do not layer custom per-ref acceptance.

RFC rewrite required:

- Add an explicit "allowed Git server contract" section that bans behaviors that can reintroduce partial local success.

### C. Startup classification from local refs plus internal refs plus descriptors

Why it is blocking:

- The RFC says the system can recover convergence state from Git refs and config after restart.
- That is not true until "observed upstream state" is defined precisely.

What to build:

- A startup recovery routine over synthetic repository states.
- Cases:
  - no upstream observation exists
  - observation exists and matches local desired state
  - observation exists and differs
  - observation may be stale because the relay crashed mid-reconcile

How to verify:

- Run startup classification without network.
- Run it again after fresh upstream observation.
- Compare repo safety state and per-upstream state with expected results.

Pass criteria:

- Startup without fresh observation never guesses `in_sync`.
- Stale or missing observation becomes `unknown` or `observing`, not false `divergent`.
- After fresh observation, classification is deterministic.

Proposed solution:

- Split internal state into:
  - desired state derived from current local exported refs and current policy
  - observed upstream refs under `refs/git-relay/upstreams/<upstream>/observed/...`
- On startup, mark every upstream `unknown` until a fresh observation completes unless the implementation can prove its last observation is still valid.
- Never infer divergence from stale cached observation alone.

RFC rewrite required:

- Rewrite the storage and recovery sections to define observation freshness and the exact state-derivation algorithm.

### D. Upstream atomic capability detection

Why it is blocking:

- `require_atomic = true` is only meaningful if the relay can determine support reliably.

What to build:

- A capability probe against:
  - local bare Git over SSH
  - local Git over smart HTTP if enabled
  - at least one forge implementation used in target deployments

How to verify:

- Probe remote push capability advertisement.
- Attempt multi-ref `git push --atomic`.
- Record success, failure mode, and downgrade behavior.

Pass criteria:

- Capability detection is based on observed server behavior, not provider brand.
- When `require_atomic = true`, unsupported upstreams are never treated as converged.

Proposed solution:

- Detect atomic support from the concrete remote session, not from host name.
- Treat "capability unclear" as "atomic unsupported".
- If an upstream marked `require_atomic = true` does not support atomic apply, classify that upstream `unsupported` and the repository `degraded`.

RFC rewrite required:

- Define capability detection as a protocol-behavior check and define the failure state precisely.

### E. Repair after non-atomic upstream partial apply

Why it is blocking:

- If `require_atomic = false`, partial upstream apply is expected.
- The RFC does not yet define how to recover without lying about observed state.

What to build:

- A non-atomic upstream test where a push of several refs intentionally fails partway through.
- A reconcile loop with an observe step before and after apply.

How to verify:

- Trigger partial remote application.
- Re-observe the upstream immediately after the failed apply.
- Confirm that the next reconcile attempt computes desired state from current local refs, not from an old push event.

Pass criteria:

- Partial remote apply never updates the internal "observed" namespace speculatively.
- A failed non-atomic reconcile always ends in `out_of_sync` plus `degraded`.
- A later reconcile can converge from current local desired state without a per-push replay log.

Proposed solution:

- Make reconcile a strict cycle:
  - observe upstream
  - compute desired diff
  - apply
  - observe upstream again
- Update internal upstream-tracking refs only from observation, never from optimistic push assumptions.
- Treat non-atomic convergence as "detectable inconsistency plus repair", not atomic replication.

RFC rewrite required:

- Rewrite convergence semantics to separate apply attempts from observed truth.

### F. Hidden ref and hidden object leakage

Why it is blocking:

- The RFC stores internal refs in the same object database as client-visible refs.
- If hidden refs or their objects leak through upload-pack, the security model is broken.

What to build:

- A repository with internal refs under `refs/git-relay/...` pointing to unique commits unreachable from exported refs.
- Client fetch attempts by explicit object ID and normal negotiation.

How to verify:

- Set `transfer.hideRefs`, `uploadpack.hideRefs`, and `receive.hideRefs` for the internal namespace.
- Test clone, fetch, and object-by-id access.
- Repeat over SSH and smart HTTP if HTTP support is enabled.

Pass criteria:

- Clients cannot see internal refs in advertisement.
- Clients cannot fetch internal-only objects by guessing object IDs.
- Hidden refs do not influence normal negotiation.

Proposed solution:

- In MVP, require all of:
  - `transfer.hideRefs=refs/git-relay`
  - `uploadpack.hideRefs=refs/git-relay`
  - `receive.hideRefs=refs/git-relay`
- If hidden-object tests still fail under supported Git versions, move internal tracking refs into a separate side repository per logical repository instead of the authoritative repo.

RFC rewrite required:

- Expand the security and storage sections to define hidden-ref and object-visibility requirements explicitly.

### G. Targeted relock stability across Nix versions

Why it is blocking:

- The migration model depends on targeted relock for safe narrow updates.
- The RFC already admits exact behavior is unverified.

What to build:

- A fixture matrix with:
  - one direct shorthand input
  - several direct shorthand inputs
  - transitive dependencies using shorthand
  - `follows`
  - overrides
  - private and public Git targets
- Run the same migration across the supported Nix version matrix.

How to verify:

- Rewrite direct inputs.
- Run `nix flake lock --update-input <name>` where intended.
- Diff `flake.lock` before and after.
- Record whether unrelated lock entries changed.

Pass criteria:

- For a supported Nix version, targeted relock changes only the intended direct input and the unavoidable transitive closure required by Nix semantics.
- Unsupported versions or graphs fail closed instead of widening updates silently.

Proposed solution:

- Pin a narrow supported Nix version set for MVP.
- Treat targeted relock as a supported optimization only for validated versions and graph shapes.
- If scope isolation cannot be proven, fail with an explicit instruction to use a broader relock command.

RFC rewrite required:

- Replace broad claims about targeted relock with a version-matrix contract.

### H. Shorthand rewrite coverage and edge cases

Why it is blocking:

- The migration tool is only safe if rewrite output is deterministic and fail-closed.

What to build:

- A golden fixture corpus for direct input literals covering:
  - `github:owner/repo`
  - `github:owner/repo/ref`
  - `gitlab:group/project`
  - `gitlab:group/subgroup/project`
  - `sourcehut:~user/project`
  - explicit `dir`
  - explicit `rev`
  - host-specific query parameters
  - unsupported dynamic expressions

How to verify:

- Parse and rewrite each fixture.
- Compare output to expected rewritten URL.
- Run migration twice and confirm byte-identical second output.

Pass criteria:

- Literal supported forms always rewrite to the same output.
- Unsupported or ambiguous forms fail closed with a diagnostic.
- Second rewrite run is a no-op.

Proposed solution:

- Implement migration as a parser-backed rewrite over literal URL assignments only.
- Preserve explicit `ref`, `rev`, `dir`, and representable query intent.
- Do not evaluate Nix.
- Do not rewrite dynamic expressions.

RFC rewrite required:

- Replace example-driven rewrite description with a normative supported-forms grammar.

### I. Short-lived workers plus filesystem locks

Why it is blocking:

- The RFC treats lock paths as correctness-relevant but does not define what kind of lock or what recovery rules apply after crash.

What to build:

- A worker harness that runs refresh and reconcile in parallel for the same repository and upstream.
- Crash workers with `SIGKILL` while holding locks.
- Repeat on the supported local filesystem types.

How to verify:

- Confirm only one reconcile worker performs side effects for a given `(repo, upstream)` at a time.
- Confirm stale locks can be detected and broken safely after process death.
- Confirm lock loss does not corrupt repository state.

Pass criteria:

- Duplicate workers may waste work, but they do not create conflicting correctness-critical state.
- A dead worker never leaves the repository permanently stuck.
- Recovery always re-derives desired state from Git and config, not from lock contents.

Proposed solution:

- Use one lock per `(repo, upstream, operation-class)`.
- Use local-filesystem advisory locks or `mkdir`-style lock directories with owner metadata and liveness checks.
- Restrict MVP support to local POSIX filesystems. Do not support network filesystems such as NFS for correctness-critical state.
- Make lock contents advisory only. The correctness source remains Git refs plus policy.

RFC rewrite required:

- Add a lock model section with supported filesystems, stale-lock handling, and lock granularity.

## Additional Verification Needed Beyond The Nine Blockers

### Git durability floor

Needed:

- crash testing with the intended Git config on the supported filesystems

Proposed solution:

- define a supported filesystem set
- define required Git fsync-related settings for authoritative repositories
- do not promise crash durability without a validated config and filesystem matrix

### Packaging and deployment reproducibility

Needed:

- a Nix-built service package with pinned Git and OpenSSH inputs
- a deployment test that proves service startup, hook wiring, SSH forced-command routing, and runtime secret injection

Proposed solution:

- make the relay itself Nix-built and pinned
- keep secrets outside the store
- validate system Git and OpenSSH as part of a deployment profile, not as unconstrained host dependencies

## Recommended Execution Order

1. Build workstream 1 first.
2. Build workstream 2 second.
3. Build workstream 4 before claiming deployable MVP.
4. Build workstream 3 in parallel only after the Nix version matrix is chosen.

Reason:

- local accept and recovery are foundational
- reconcile semantics depend on them
- deployment constraints can invalidate both
- migration can proceed in parallel once runtime constraints are pinned

## Proposed RFC Contract Changes

The final RFC should make these rules explicit:

- local acceptance is one committed Git ref transaction, not wrapper success plus side effects
- upstream convergence is current-state reconciliation, not per-push replay
- non-atomic upstream convergence is detectability plus repair, not atomic replication
- observed upstream state is updated only by actual observation, not by optimistic push assumptions
- startup without fresh observation yields `unknown`, not guessed `in_sync`
- whole-push acceptance requires an allowed Git hook and config subset
- targeted relock is only promised for validated Nix versions and validated graph shapes
- lock paths are advisory coordination artifacts, not correctness anchors

