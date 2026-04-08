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

## Terminology

This plan distinguishes three different properties that should not be collapsed into one word.

- `atomicity`: one state transition either commits or does not commit.
- `execution-unit completeness`: one reconcile run handles the full configured upstream set from one desired-state snapshot and reaches a terminal recorded outcome.
- `terminal cleanup`: transient in-progress markers and locks are removed or superseded cleanly after normal completion or recovery, while terminal evidence remains visible for operators.

In this system:

- local Git ref acceptance may be atomic,
- per-upstream multi-ref apply may be atomic when the remote supports it,
- and multi-upstream fan-out is not atomic.

The relay can still provide high-level "one run handles everything" behavior through execution-unit completeness without claiming false cross-upstream atomicity.

## Execution Status

<!-- EXECUTION_LEDGER:START -->
| Check | Status | Latest run | Decision |
|---|---|---|---|
| A | pass | 20260405T012510Z-A-local-ack | Local SSH and local smart-HTTP acknowledgement are governed by receive-pack and committed local ref state for both single-ref and transmitted multi-ref pushes. Post-receive is non-critical, and the only observed client ambiguity is the normal Git case where commit happened but the client did not observe success. |
| B | fail | 20260405T005620Z-B-whole-push | Whole-push local acceptance is not implementable for ordinary pushes using only server-side Git hooks, receive-pack, and filesystem locks. The relay can reject the whole push for bad refs that are actually transmitted, but send-pack may prune a locally rejected ref before the relay sees the user-requested push set. |
| C | pass | 20260404T072857Z-C-startup-classify | A deterministic and conservative startup classifier is implementable if startup never trusts cached upstream observation as current truth. Cached observation can inform recovery only after an explicit fresh observation step. |
| D | pass | 20260405T000831Z-D-atomic-capability | Atomic capability can be classified from concrete receive-pack behavior across the validated local transports and the configured hosted managed-forge target: advertisement plus --atomic push outcome, with ambiguity treated as unsupported. |
| E | pass | 20260404T073220Z-E-partial-apply | A non-atomic upstream apply can be recovered safely without a replay log if the relay updates internal observed refs only from explicit observation and always recomputes desired state from current local refs. |
| F | pass | 20260405T003947Z-F-hidden-refs | The same-repository hidden-ref model is viable only when the authoritative server enforces hideRefs and disables SHA-by-id wants. That contract now holds across the validated local transports and the configured self-managed hosted target. |
| G | pass | 20260405T013304Z-G-targeted-relock | On the validated local Nix variants (nix (Determinate Nix 3.0.0) 2.26.3, nix (Nix) 2.28.5, nix (Nix) 2.30.3+2, nix (Nix) 2.31.3), targeted relock stayed scoped and idempotent across the validated local Git-input graph shapes: multi-direct inputs with follows, transitive subgraphs, and root overrides, using both `nix flake lock --update-input alpha` and `nix flake update alpha`. The supported contract can therefore be narrowed to this validated version-and-graph matrix. |
| H | pass | 20260404T080317Z-H-rewrite-fixtures | A parser-backed deterministic rewrite is implementable for literal direct-input shorthand forms if the grammar stays intentionally narrow and unsupported expressions fail closed. |
| I | pass | 20260404T073519Z-I-locks | Short-lived workers can use advisory lock directories safely if lock contents remain advisory, stale locks are broken via liveness checks, and recovery re-derives work from Git state. |
| J | pass | 20260404T074302Z-J-execution-unit | A reconcile run can be specified as one bounded execution unit with one desired snapshot and one captured upstream set, while mixed per-upstream outcomes remain recorded under that same run and stale prior runs are superseded. |
| K | pass | 20260405T031317Z-K-durability-floor | Across the validated macOS and Linux hosts, the exercised Git variants upheld the selected authoritative-write crash checkpoints when authoritative repos used `core.fsync=all` and `core.fsyncMethod=fsync`. The supported durability contract can therefore be limited to those validated platforms and filesystems rather than a broader unproven matrix. |
| L | pass | 20260405T031318Z-L-deployment-repro | A pinned Nix-built deployment scaffold is now reproducible across the validated macOS and Linux hosts: package build, runtime-profile validation, runtime environment-file handling outside `/nix/store`, hook-wrapper installation, SSH forced-command routing, and service-manager bring-up via launchd on macOS and systemd on Linux. |
<!-- EXECUTION_LEDGER:END -->

### Result A

<!-- RESULT:A:START -->
- Status: pass
- Latest run: 20260405T012510Z-A-local-ack
- Environment: Git=git version 2.53.0, OpenSSH=OpenSSH_10.2p1, LibreSSL 3.3.6, Python=3.14.3
- Evidence: evidence/A/20260405T012510Z-A-local-ack
- Observed result: Baseline local push succeeded and created the authoritative ref. Baseline local smart-HTTP push also succeeded and created the authoritative ref. A non-zero post-receive hook did not remove the committed ref and did not change push success. A non-zero post-receive hook on the smart-HTTP path also did not remove the committed ref and did not change push success. A baseline multi-ref push over SSH committed the full transmitted ref set. A baseline multi-ref push over local smart HTTP also committed the full transmitted ref set. A non-zero post-receive hook after an SSH multi-ref push did not change the committed ref set or success outcome. A non-zero post-receive hook after a local smart-HTTP multi-ref push also did not change the committed ref set or success outcome. Committed-but-client-ambiguous outcomes were observed at: after_receive_pack_success_before_wrapper_exit, after_reference_transaction_committed, http_after_reference_transaction_committed, http_multi_ref_after_reference_transaction_committed, multi_ref_after_receive_pack_success_before_wrapper_exit, multi_ref_after_reference_transaction_committed.
- Decision: Local SSH and local smart-HTTP acknowledgement are governed by receive-pack and committed local ref state for both single-ref and transmitted multi-ref pushes. Post-receive is non-critical, and the only observed client ambiguity is the normal Git case where commit happened but the client did not observe success.
- RFC/doc follow-up: Treat this as the accepted local acknowledgement contract for native Git ingress. Broader durability and deployment matrix work remains tracked under K and L rather than under A.
<!-- RESULT:A:END -->

### Result B

<!-- RESULT:B:START -->
- Status: fail
- Latest run: 20260405T005620Z-B-whole-push
- Environment: Git=git version 2.53.0, OpenSSH=OpenSSH_10.2p1, LibreSSL 3.3.6, Python=3.14.3
- Evidence: evidence/B/20260405T005620Z-B-whole-push
- Observed result: A valid multi-ref push updated both tracked refs from one receive-pack path. A valid multi-ref push over local smart HTTP also updated both tracked refs from one receive-pack path. A pre-receive rejection on one protected ref left the entire pushed ref set unchanged. With client-requested --atomic, denied delete, non-fast-forward rejection, and ref lock failure all left the tracked ref set unchanged. With client-requested --atomic over local smart HTTP, denied delete, non-fast-forward rejection, and ref lock failure all left the tracked ref set unchanged. Hooks did not receive an atomic-specific signal on either tested transport: GIT_PROTOCOL stayed unset and GIT_PUSH_OPTION_COUNT stayed neutral (0 or unset) for both plain and --atomic pushes, while receive-pack packet traces still distinguished the --atomic request at the protocol layer. When a non-fast-forward branch update was forced so both refs were actually transmitted, the relay-owned guard rejected the whole push before either branch or tag changed on both SSH and local smart HTTP. Rejected multi-ref pushes still changed a subset of refs at: forbidden_delete_rejection, guarded_non_fast_forward_plus_tag_rejection, http_forbidden_delete_rejection, http_guarded_non_fast_forward_plus_tag_rejection, http_non_fast_forward_plus_tag_rejection, http_ref_lock_contention, non_fast_forward_plus_tag_rejection, ref_lock_contention. The guarded non-fast-forward ordinary-push cases remained partial because send-pack omitted the rejected branch update client-side and transmitted only the tag, so the relay never saw the full user-requested ref set.
- Decision: Whole-push local acceptance is not implementable for ordinary pushes using only server-side Git hooks, receive-pack, and filesystem locks. The relay can reject the whole push for bad refs that are actually transmitted, but send-pack may prune a locally rejected ref before the relay sees the user-requested push set.
- RFC/doc follow-up: Rewrite the RFC to treat ordinary inbound pushes as per-ref at the relay boundary. If whole-push semantics are required, the design needs either a protocol-aware front proxy or a narrower relay API that controls the transmitted ref set before send-pack can prune it. Hooks, internal refs, and filesystem locks alone cannot enforce inbound --atomic.
<!-- RESULT:B:END -->

### Result C

<!-- RESULT:C:START -->
- Status: pass
- Latest run: 20260404T072857Z-C-startup-classify
- Environment: Git=git version 2.53.0, Python=3.14.3
- Evidence: evidence/C/20260404T072857Z-C-startup-classify
- Observed result: Startup without any observed upstream refs classified the upstream as unknown, then fresh observation produced in_sync deterministically. Cached matching observation was not trusted at startup; it became in_sync only after an explicit fresh observation step. Cached mismatch was not treated as divergent at startup; after fresh observation the same upstream classified out_of_sync deterministically. Fresh mixed upstream observations produced deterministic per-upstream states and a degraded repo safety state derived from them.
- Decision: A deterministic and conservative startup classifier is implementable if startup never trusts cached upstream observation as current truth. Cached observation can inform recovery only after an explicit fresh observation step.
- RFC/doc follow-up: Rewrite the RFC storage and recovery sections to define desired-state derivation, observed-ref storage, startup=unknown behavior, and the exact transition to in_sync or out_of_sync after fresh observation.
<!-- RESULT:C:END -->

### Result D

<!-- RESULT:D:START -->
- Status: pass
- Latest run: 20260405T000831Z-D-atomic-capability
- Environment: Git=git version 2.53.0, OpenSSH=OpenSSH_10.2p1, LibreSSL 3.3.6, Python=3.14.3
- Evidence: evidence/D/20260405T000831Z-D-atomic-capability
- Observed result: The default local SSH receive-pack session advertised atomic capability and accepted a multi-ref --atomic push. With receive.advertiseAtomic=false, the same SSH path did not advertise atomic capability, rejected --atomic push, and still accepted a plain push. The default local smart-HTTP receive-pack session advertised atomic capability and accepted a multi-ref --atomic push. With receive.advertiseAtomic=false, the same smart-HTTP path did not advertise atomic capability, rejected --atomic push, and still accepted a plain push. Hosted target github-current-origin-ssh over ssh classified atomic capability as supported from concrete session behavior, accepted a plain disposable push, and cleaned up its temporary refs.
- Decision: Atomic capability can be classified from concrete receive-pack behavior across the validated local transports and the configured hosted managed-forge target: advertisement plus --atomic push outcome, with ambiguity treated as unsupported.
- RFC/doc follow-up: Rewrite the RFC to define atomic capability detection as a behavior-based probe, and require unsupported or ambiguous upstreams to remain unconverged when require_atomic=true.
<!-- RESULT:D:END -->

### Result E

<!-- RESULT:E:START -->
- Status: pass
- Latest run: 20260404T073220Z-E-partial-apply
- Environment: Git=git version 2.53.0, Python=3.14.3
- Evidence: evidence/E/20260404T073220Z-E-partial-apply
- Observed result: After a delete was rejected, the internal observed namespace remained at the pre-apply snapshot until re-observation. The first run ended out_of_sync plus degraded from actual upstream state. After local main advanced and the upstream policy changed, the second run converged the newer local main without a replay log.
- Decision: A non-atomic upstream apply can be recovered safely without a replay log if the relay updates internal observed refs only from explicit observation and always recomputes desired state from current local refs.
- RFC/doc follow-up: Rewrite the RFC convergence section so apply attempts never mutate observed truth, failed non-atomic reconcile ends out_of_sync plus degraded, and later reconcile derives from current local refs rather than prior push events.
<!-- RESULT:E:END -->

### Result F

<!-- RESULT:F:START -->
- Status: pass
- Latest run: 20260405T003947Z-F-hidden-refs
- Environment: Git=git version 2.53.0, OpenSSH=OpenSSH_10.2p1, LibreSSL 3.3.6, Python=3.14.3
- Evidence: evidence/F/20260405T003947Z-F-hidden-refs
- Observed result: Without hideRefs, the internal ref was advertised and the hidden object was fetchable by object id over SSH. With hideRefs only, the internal ref disappeared from advertisement but the hidden object remained fetchable by object id when reachable SHA wants were enabled. With hideRefs and SHA-by-id wants disabled, the internal ref was hidden and the hidden object fetch failed over SSH. Without hideRefs, the internal ref was advertised and the hidden object was fetchable by object id over local smart HTTP. With hideRefs only, the internal ref disappeared from advertisement but the hidden object remained fetchable by object id over local smart HTTP when reachable SHA wants were enabled. With hideRefs and SHA-by-id wants disabled, the internal ref was hidden and the hidden object fetch failed over local smart HTTP. Hosted self-managed target pp-vnlabs-self-managed-ssh hid the internal ref from advertisement but still allowed object-id fetch when only hideRefs was configured. Hosted self-managed target pp-vnlabs-self-managed-ssh hid the internal ref and blocked object-id fetch when hideRefs and SHA-by-id wants were both disabled.
- Decision: The same-repository hidden-ref model is viable only when the authoritative server enforces hideRefs and disables SHA-by-id wants. That contract now holds across the validated local transports and the configured self-managed hosted target.
- RFC/doc follow-up: Rewrite the RFC security and storage sections to require transfer/upload/receive hideRefs and to forbid uploadpack.allowReachableSHA1InWant, uploadpack.allowAnySHA1InWant, and uploadpack.allowTipSHA1InWant for authoritative repos that keep internal refs in the same repository.
<!-- RESULT:F:END -->

### Result G

<!-- RESULT:G:START -->
- Status: pass
- Latest run: 20260405T013304Z-G-targeted-relock
- Environment: Nix=nix (Determinate Nix 3.0.0) 2.26.3, Variants=current,nix_2_28,nix_2_30,nix_2_31, Python=3.14.3
- Evidence: evidence/G/20260405T013304Z-G-targeted-relock
- Observed result: With nix (Determinate Nix 3.0.0) 2.26.3, the direct-input/follows graph kept `nix flake lock --update-input alpha` scoped to alpha, left beta and gamma unchanged, and produced a no-op second relock. With nix (Determinate Nix 3.0.0) 2.26.3, the transitive-subgraph graph updated alpha and alpha's leaf subgraph together, left unrelated beta unchanged, and produced a no-op second relock. With nix (Determinate Nix 3.0.0) 2.26.3, the direct-input/follows graph kept `nix flake update alpha` scoped to alpha, left beta and gamma unchanged, and produced a no-op second update. With nix (Determinate Nix 3.0.0) 2.26.3, the transitive-subgraph graph updated alpha and alpha's leaf subgraph under `nix flake update alpha`, left unrelated beta unchanged, and produced a no-op second update. With nix (Determinate Nix 3.0.0) 2.26.3, the override graph kept the root override leaf and unrelated beta unchanged under `nix flake update alpha`, updated alpha, and produced a no-op second update. With nix (Nix) 2.28.5, the direct-input/follows graph kept `nix flake lock --update-input alpha` scoped to alpha, left beta and gamma unchanged, and produced a no-op second relock. With nix (Nix) 2.28.5, the transitive-subgraph graph updated alpha and alpha's leaf subgraph together, left unrelated beta unchanged, and produced a no-op second relock. With nix (Nix) 2.28.5, the direct-input/follows graph kept `nix flake update alpha` scoped to alpha, left beta and gamma unchanged, and produced a no-op second update. With nix (Nix) 2.28.5, the transitive-subgraph graph updated alpha and alpha's leaf subgraph under `nix flake update alpha`, left unrelated beta unchanged, and produced a no-op second update. With nix (Nix) 2.28.5, the override graph kept the root override leaf and unrelated beta unchanged under `nix flake update alpha`, updated alpha, and produced a no-op second update. With nix (Nix) 2.30.3+2, the direct-input/follows graph kept `nix flake lock --update-input alpha` scoped to alpha, left beta and gamma unchanged, and produced a no-op second relock. With nix (Nix) 2.30.3+2, the transitive-subgraph graph updated alpha and alpha's leaf subgraph together, left unrelated beta unchanged, and produced a no-op second relock. With nix (Nix) 2.30.3+2, the direct-input/follows graph kept `nix flake update alpha` scoped to alpha, left beta and gamma unchanged, and produced a no-op second update. With nix (Nix) 2.30.3+2, the transitive-subgraph graph updated alpha and alpha's leaf subgraph under `nix flake update alpha`, left unrelated beta unchanged, and produced a no-op second update. With nix (Nix) 2.30.3+2, the override graph kept the root override leaf and unrelated beta unchanged under `nix flake update alpha`, updated alpha, and produced a no-op second update. With nix (Nix) 2.31.3, the direct-input/follows graph kept `nix flake lock --update-input alpha` scoped to alpha, left beta and gamma unchanged, and produced a no-op second relock. With nix (Nix) 2.31.3, the transitive-subgraph graph updated alpha and alpha's leaf subgraph together, left unrelated beta unchanged, and produced a no-op second relock. With nix (Nix) 2.31.3, the direct-input/follows graph kept `nix flake update alpha` scoped to alpha, left beta and gamma unchanged, and produced a no-op second update. With nix (Nix) 2.31.3, the transitive-subgraph graph updated alpha and alpha's leaf subgraph under `nix flake update alpha`, left unrelated beta unchanged, and produced a no-op second update. With nix (Nix) 2.31.3, the override graph kept the root override leaf and unrelated beta unchanged under `nix flake update alpha`, updated alpha, and produced a no-op second update.
- Decision: On the validated local Nix variants (nix (Determinate Nix 3.0.0) 2.26.3, nix (Nix) 2.28.5, nix (Nix) 2.30.3+2, nix (Nix) 2.31.3), targeted relock stayed scoped and idempotent across the validated local Git-input graph shapes: multi-direct inputs with follows, transitive subgraphs, and root overrides, using both `nix flake lock --update-input alpha` and `nix flake update alpha`. The supported contract can therefore be narrowed to this validated version-and-graph matrix.
- RFC/doc follow-up: Rewrite the RFC to promise targeted relock only for the validated Nix versions and validated graph shapes exercised here, prefer `nix flake update <input>` in normative examples, and keep broader version or graph cases fail-closed until proven.
<!-- RESULT:G:END -->

### Result H

<!-- RESULT:H:START -->
- Status: pass
- Latest run: 20260404T080317Z-H-rewrite-fixtures
- Environment: Python=3.14.3
- Evidence: evidence/H/20260404T080317Z-H-rewrite-fixtures
- Observed result: Supported literal shorthands rewrote deterministically for fixtures: already_rewritten_noop, github_basic_https, github_basic_ssh, github_ref_and_dir_https, gitlab_nested_groups_https, gitlab_subgroup_with_host_https, sourcehut_rev_https. Unsupported or ambiguous forms failed closed for fixtures: dynamic_expression_rejected, github_ambiguous_ref_query_rejected, non_literal_rejected. A second rewrite run was a byte-identical no-op for every successfully rewritten fixture.
- Decision: A parser-backed deterministic rewrite is implementable for literal direct-input shorthand forms if the grammar stays intentionally narrow and unsupported expressions fail closed.
- RFC/doc follow-up: Rewrite the RFC migration section as a normative grammar over literal URL assignments, with explicit transport policy inputs and explicit fail-closed exclusions for dynamic or ambiguous forms.
<!-- RESULT:H:END -->

### Result I

<!-- RESULT:I:START -->
- Status: pass
- Latest run: 20260404T073519Z-I-locks
- Environment: Git=git version 2.53.0, Python=3.14.3, FS=/
- Evidence: evidence/I/20260404T073519Z-I-locks
- Observed result: A second worker stayed out of the critical section while the first lock holder was alive. After the lock holder was killed, a third worker broke the stale lock, re-derived current work from Git, and updated the runtime ref to the newer local main with no leftover lock directory.
- Decision: Short-lived workers can use advisory lock directories safely if lock contents remain advisory, stale locks are broken via liveness checks, and recovery re-derives work from Git state.
- RFC/doc follow-up: Rewrite the RFC to define one lock per (repo, upstream, operation-class), local-filesystem-only support, stale-lock liveness checks, and the rule that lock contents are never the correctness source.
<!-- RESULT:I:END -->

### Result J

<!-- RESULT:J:START -->
- Status: pass
- Latest run: 20260404T074302Z-J-execution-unit
- Environment: Git=git version 2.53.0, Python=3.14.3
- Evidence: evidence/J/20260404T074302Z-J-execution-unit
- Observed result: One run captured one desired snapshot and the sorted upstream set alpha,beta,gamma, then recorded mixed outcomes under the same run id: alpha in_sync, beta out_of_sync, gamma unreachable. Cleanup removed the lock and in-progress marker while leaving terminal run evidence, and a stale older run was recorded as superseded.
- Decision: A reconcile run can be specified as one bounded execution unit with one desired snapshot and one captured upstream set, while mixed per-upstream outcomes remain recorded under that same run and stale prior runs are superseded.
- RFC/doc follow-up: Rewrite the RFC to separate execution-unit completeness from atomicity: one run id, one desired snapshot id, one captured upstream set, mixed per-upstream terminal results, transient marker cleanup, and preserved terminal evidence.
<!-- RESULT:J:END -->

### Result K

<!-- RESULT:K:START -->
- Status: pass
- Latest run: 20260405T031317Z-K-durability-floor
- Environment: macOS host: Git=git version 2.53.0, Variants=current,git_2_48_1,git_2_52_0, OpenSSH=OpenSSH_10.2p1, LibreSSL 3.3.6, Python=3.14.3, Platform=Darwin, FS=/; Linux host: Git=git version 2.47.3, Variants=current, OpenSSH=OpenSSH_10.0p2 Debian-7+deb13u1, OpenSSL 3.5.5 27 Jan 2026, Python=3.13.5, Platform=Linux, FS=ext2/ext3
- Evidence: evidence/K/20260405T030348Z-K-durability-floor; evidence/K/20260405T030929Z-K-durability-floor
- Observed result: On the validated macOS host, baseline SSH and smart-HTTP pushes under `core.fsync=all` and `core.fsyncMethod=fsync` succeeded and produced strict-fsck-clean repositories across Git `2.48.1`, `2.52.0`, and `2.53.0`. On the validated Linux host, baseline SSH and smart-HTTP pushes under the same fsync floor succeeded and produced strict-fsck-clean repositories on `ext2/ext3` with Git `2.47.3`. On both validated platforms, crashes before `pre-receive` left no committed ref, and post-commit crash checkpoints produced committed local state with client-visible ambiguity rather than silent rollback.
- Decision: Across the validated macOS and Linux hosts, the exercised Git variants upheld the selected authoritative-write crash checkpoints when authoritative repositories used `core.fsync=all` and `core.fsyncMethod=fsync`. The supported durability contract can therefore be limited to those validated platforms and filesystems rather than a broader unproven matrix.
- RFC/doc follow-up: Rewrite the RFC to require the explicit Git fsync floor for authoritative repositories and to scope durability claims to the supported macOS and Linux platform contract validated here.
<!-- RESULT:K:END -->

### Result L

<!-- RESULT:L:START -->
- Status: pass
- Latest run: 20260405T031318Z-L-deployment-repro
- Environment: macOS host: Git=git version 2.53.0, Nix=nix (Determinate Nix 3.0.0) 2.26.3, OpenSSH=OpenSSH_10.2p1, LibreSSL 3.3.6, Python=3.14.3; Linux host: Git=git version 2.47.3, Nix=nix (Nix) 2.32.2, OpenSSH=OpenSSH_10.0p2 Debian-7+deb13u1, OpenSSL 3.5.5 27 Jan 2026, Python=3.13.5
- Evidence: evidence/L/20260405T004939Z-L-deployment-repro; evidence/L/20260405T024755Z-L-deployment-repro
- Observed result: On the validated macOS and Linux hosts, the repository builds a pinned `git-relay` package, rewrites packaged launchd and systemd templates to the built binary path, validates the configured runtime environment file outside `/nix/store`, records the parsed environment entry count in the runtime profile, installs hook wrappers into a disposable bare repository, records a hook-dispatch event, resolves allowed SSH forced commands against the configured repo root, and successfully exercises service-manager bring-up through launchd on macOS and systemd on Linux.
- Decision: A pinned Nix-built deployment scaffold is now reproducible across the validated macOS and Linux hosts: package build, runtime-profile validation, runtime environment-file handling outside `/nix/store`, hook-wrapper installation, SSH forced-command routing, and service-manager bring-up via launchd on macOS and systemd on Linux.
- RFC/doc follow-up: Rewrite the RFC to make macOS and Linux the explicit supported deployment platforms, require runtime environment files to stay outside `/nix/store`, and tie deployment claims to the validated launchd/systemd service-manager contract rather than to a broader unverified matrix.
<!-- RESULT:L:END -->

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
- reconcile execution-unit completeness
- terminal cleanup of transient worker state

Deliverable:

- one local authoritative bare repository
- two or more upstream bare repositories
- a reconcile driver that can inject partial remote failure
- a run coordinator that records terminal per-run outcomes

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

- Ordinary multi-ref pushes do not satisfy whole-push local acceptance on the tested Git path.
- Relay-owned `pre-receive` guards can reject the whole push only for invalid refs that are actually transmitted to the server.
- That is still insufficient for ordinary pushes because `send-pack` can prune a locally rejected ref before the relay ever sees the full user-requested push set.
- Candidate solution A: require an explicit client contract such as inbound `--atomic` plus protocol-level verification that the relay can actually detect.
- Candidate solution B: narrow the local contract to per-ref acceptance or to a relay-controlled API or single-ref push shape.

RFC rewrite required:

- Remove any blanket claim that ordinary multi-ref pushes are whole-push atomic locally.
- Add an explicit inbound-push contract section defining whether the relay requires `--atomic` with verifiable negotiation, supports only single-ref local acceptance, or exposes best-effort multi-ref acceptance with detectable partial local success.

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
- a hosted target manifest under `fixtures/hosted/targets.json` that records the
  actual disposable repositories, transport, host-key policy, and credential
  source used for the confirmation run

How to verify:

- Probe remote push capability advertisement.
- Attempt multi-ref `git push --atomic`.
- Record success, failure mode, and downgrade behavior.
- Require the hosted target to allow disposable branch create/delete, tag
  creation, and multi-ref writes without touching production namespaces.

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
- A hosted target manifest that distinguishes managed-forge targets from
  self-managed targets, because managed forges generally do not expose the
  server-side controls needed for the same-repository hidden-ref model.

How to verify:

- Set `transfer.hideRefs`, `uploadpack.hideRefs`, and `receive.hideRefs` for the internal namespace.
- Test clone, fetch, and object-by-id access.
- Repeat over SSH and smart HTTP if HTTP support is enabled.
- For hosted confirmation, require a self-managed target when the architecture
  keeps internal refs in the same authoritative repository. Managed-forge
  confirmation is not sufficient for that model.

Pass criteria:

- Clients cannot see internal refs in advertisement.
- Clients cannot fetch internal-only objects by guessing object IDs.
- Hidden refs do not influence normal negotiation.

Proposed solution:

- `hideRefs` alone is not sufficient if SHA-by-id wants are enabled.
- In MVP, require all of:
  - `transfer.hideRefs=refs/git-relay`
  - `uploadpack.hideRefs=refs/git-relay`
  - `receive.hideRefs=refs/git-relay`
  - `uploadpack.allowReachableSHA1InWant=false`
  - `uploadpack.allowAnySHA1InWant=false`
  - `uploadpack.allowTipSHA1InWant=false`
- If those upload-pack constraints cannot be guaranteed on every supported deployment, move internal tracking refs into a separate side repository per logical repository instead of the authoritative repo.

RFC rewrite required:

- Expand the security and storage sections to define hidden-ref and object-visibility requirements explicitly, including the forbidden SHA-by-id upload-pack settings.

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

### J. Reconcile execution-unit completeness and terminal cleanup

Why it is blocking:

- Higher-level "one push fans out to all configured upstreams in one run" behavior is a valid product contract.
- The RFC currently talks about atomicity where it really needs an execution contract.

What to build:

- A reconcile coordinator that creates one `reconcile_run_id` per run.
- A fixture with several configured upstreams where one succeeds, one fails, and one is temporarily unreachable.
- Crash injection before run start, mid-run, and after the last upstream attempt but before terminal outcome is recorded.

How to verify:

- Start a reconcile run from one captured desired-state snapshot.
- Confirm the run enumerates the full configured upstream set at run start.
- Confirm every eligible upstream is attempted within that run unless the run is explicitly superseded by newer desired state.
- Confirm completion leaves no stale in-progress markers or orphan locks.
- Confirm terminal evidence remains visible after cleanup.

Pass criteria:

- One reconcile run always has one deterministic desired-state snapshot and one explicit upstream set.
- Mixed per-upstream outcomes are recorded under the same `reconcile_run_id`.
- Cleanup removes transient worker state but preserves terminal operator-visible outcome.
- Crash recovery either resumes safely or starts a new run that supersedes the old one without leaving correctness dependent on stale run metadata.

Proposed solution:

- Define reconcile as a bounded execution unit distinct from atomicity.
- Give each run:
  - one `reconcile_run_id`
  - one desired-state snapshot identifier
  - one configured-upstream set captured at run start
- Record per-upstream results under that run.
- On completion, clear transient locks and in-progress markers.
- Retain terminal run evidence in logs or diagnostic artifacts for debugging and repair.
- If newer local state supersedes an active run, mark the old run superseded rather than pretending it fully converged current state.

RFC rewrite required:

- Add a reconcile execution-unit section that separates local atomicity, per-upstream atomic apply, and multi-upstream run completeness.

## Additional Verification Needed Beyond The Nine Blockers

### Git durability floor

Needed:

- crash testing with the intended Git config on the supported filesystems

Proposed solution:

- define a supported filesystem set
- define required Git fsync-related settings for authoritative repositories
- close the exact Git floor only from admitted machine-readable git-conformance evidence recorded for both supported platforms
- do not promise crash durability without a validated config and filesystem matrix

### Packaging and deployment reproducibility

Needed:

- a Nix-built service package with pinned Git and OpenSSH inputs
- a deployment test that proves service startup, hook wiring, SSH forced-command routing, and runtime environment-file handling

Proposed solution:

- make the relay itself Nix-built and pinned
- keep runtime environment files outside the store
- validate system Git and OpenSSH as part of a deployment profile, not as unconstrained host dependencies
- package the deployment profile explicitly:
  - `git-relayd`
  - `git-relay-install-hooks`
  - `git-relay-ssh-force-command`
  - systemd and launchd templates
  - example config and environment files outside `/nix/store`

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
- each reconcile run is one bounded execution unit over one desired-state snapshot and one captured upstream set
- execution-unit completeness is not cross-upstream atomicity
- cleanup removes transient worker state but preserves terminal run evidence
- observed upstream state is updated only by actual observation, not by optimistic push assumptions
- startup without fresh observation yields `unknown`, not guessed `in_sync`
- whole-push acceptance requires an allowed Git hook and config subset
- targeted relock is only promised for validated Nix versions and validated graph shapes
- lock paths are advisory coordination artifacts, not correctness anchors
