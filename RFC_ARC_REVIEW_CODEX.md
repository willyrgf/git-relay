# Architectural Review: Git Relay RFC

**Status:** Review  
**Date:** 2026-04-02  
**Reviewing:** `git-relay-rfc.md`  
**Reviewer:** Codex

This review incorporates a clarified product constraint from the author:

- Git Relay does **not** need to preserve unmodified `flake.nix` or `flake.lock`.
- A Git Relay onboarding flow may update Git configuration and may also migrate project Nix inputs and re-lock them.

That clarification materially changes the architecture decision. The tarball compatibility plane is no longer justified as a mandatory foundational requirement for the main RFC.

## 1. Executive Verdict

`Reject in current form`

The current RFC is built around a premise that no longer holds: that Git Relay must transparently cover existing `github:` / `gitlab:` / `sourcehut:` shorthand fetches without source migration. If source migration is allowed, the RFC should be rewritten around a Git-only core plus an explicit Nix migration workflow. The current text still contains unresolved issues around migration semantics, transitive shorthand gaps, authoritative repository invariants, and crash-safe `durable-local` push acknowledgement.

## 2. Top Findings

1. `Blocking` `Sections 1, 3.5, 6.3, 10.2, 15, 21, 22.1, 25`
The RFC's central justification for a mandatory tarball plane is weakened by the clarified product requirement. If onboarding may rewrite `flake.nix` and `flake.lock`, then a Git-only architecture becomes the simpler and more coherent default.

2. `Blocking` `Sections 10, 20, 21, 23`
The RFC lacks a first-class migration model. Editing `.gitconfig` is machine bootstrap; editing `flake.nix` and `flake.lock` is repository migration. Treating both as a single "install" step is architecturally sloppy and operationally risky.

3. `Blocking` `Sections 12, 13.1, 19.2`
`durable-local` push acknowledgement is still not defined as an atomic, crash-safe contract. The RFC says success is returned after local acceptance and durable queueing, but it never defines how ref updates and queue persistence are made indivisible.

4. `Major` `Sections 9, 11, 16`
The identity and cache model is underspecified even after removing tarballs. The RFC still needs precise canonicalization rules, ref freshness policy, concurrent miss coalescing, and authoritative repository divergence handling.

5. `Major` `Sections 14, 18, 24.5`
The upstream credential model is too blunt. Relay-owned machine credentials may be correct for async replication, but the RFC does not define attribution, least privilege, or credential isolation boundaries.

6. `Major` `Sections 2, 4, 21, 23`
The product scope is still too broad for an MVP. Cache-only Git relay, transparent Git bootstrap, authoritative push relay, async replication, and Nix migration are already enough. The document should stop pretending it can specify all of that plus tarball compatibility cleanly in one pass.

7. `Major` `Migration validation`
Migration is not a trivial text rewrite. Local validation showed that `narHash` equality between tarball and Git fetches is repository-dependent: a simple repo matched, while a repo using `.gitattributes export-ignore` diverged. The migration flow must explicitly re-lock and must not assume hash preservation.

## 3. Section-by-Section RFC Review

### Summary and Problem Framing

- verdict: `Needs rewrite`
- what works:
  - The RFC identifies a real and coherent pain cluster: repeated Git fetches, weak offline reuse, fragmented remote topology, and brittle push fan-out.
  - The Git/Nix mismatch is a real problem.
- what is missing or risky:
  - The summary still treats tarball compatibility as foundational rather than contingent on product scope.
  - It does not separate "machine bootstrap" from "repository migration."
- what should change:
  - Reframe the product as a Git relay first.
  - State explicitly that Nix shorthand compatibility is achieved by migrating direct inputs to Git URLs, not by preserving existing shorthand fetches.

### Goals and Non-Goals

- verdict: `Weak`
- what works:
  - The goals are directionally right.
  - The non-goals correctly reject generic MITM and a custom Git reimplementation.
- what is missing or risky:
  - The goals still imply full shorthand compatibility.
  - The non-goals do not say whether Git Relay rewrites repository source as part of onboarding.
- what should change:
  - Add a goal for "explicit repository migration tooling for Nix direct inputs."
  - Add a non-goal stating that unmodified shorthand-based transitive dependencies are not guaranteed to route through the relay in MVP.

### Proposed Solution

- verdict: `Needs rewrite`
- what works:
  - Git-first remains the correct framing.
- what is missing or risky:
  - The two-plane solution is now over-scoped relative to the clarified requirement.
- what should change:
  - Replace the proposed solution with a Git-only control/data plane plus a migration tool for Nix inputs.
  - Move any future tarball compatibility work to a later RFC or a clearly optional follow-on phase.

### Recommended Architecture

- verdict: `Weak`
- what works:
  - OpenSSH, system Git, `git-http-backend`, SQLite, and filesystem bare repos are sensible building blocks.
- what is missing or risky:
  - The architecture section locks in components before the hard contracts are defined.
- what should change:
  - Keep the Git primitives.
  - Defer implementation-language preference until after protocol and durability contracts are fixed.

### Repository Model

- verdict: `Sound`
- what works:
  - `cache-only` vs `authoritative` is the right split.
- what is missing or risky:
  - Promotion, demotion, and divergence handling are unspecified.
- what should change:
  - Define authoritative invariants explicitly.
  - State whether authoritative repos require upstream direct pushes to be forbidden or merely detectable.

### Identity Model

- verdict: `Needs rewrite`
- what works:
  - Normalizing ingress forms to a logical repository identity is necessary.
- what is missing or risky:
  - Canonicalization is underdefined: case rules, `.git` normalization, ports, host aliases, provider renames, repo moves, and auth realm boundaries.
  - Source-tree identity and repository identity are not separated.
- what should change:
  - Define at least three identities:
    - repository identity
    - source-tree identity
    - upstream auth identity

### Transparent Interception Model

- verdict: `Needs rewrite`
- what works:
  - `insteadOf` and `pushInsteadOf` are the correct primary mechanism for Git traffic.
- what is missing or risky:
  - The RFC still describes a secondary tarball mechanism that should no longer be part of the core architecture.
  - "one-time bootstrap" still conflates host bootstrap and repo mutation.
- what should change:
  - Split this into:
    - machine bootstrap: install relay and write Git URL rewrite rules
    - repo migration: rewrite flake inputs to `git+ssh://` or `git+https://` and re-lock
  - State clearly that repository edits are explicit and reviewable, not invisible side effects of a machine installer.

### Read Path

- verdict: `Incomplete`
- what works:
  - The basic Git read-through cache flow is correct.
- what is missing or risky:
  - No freshness model.
  - No concurrent miss suppression.
  - No negative-cache behavior.
  - No explicit behavior for ref lookup when cached state is stale.
- what should change:
  - Define per-repo refresh policy.
  - Define how misses are coalesced.
  - Define the observable behavior when the relay is offline and the requested ref has not been seen before.

### Write Path

- verdict: `Weak`
- what works:
  - Local accept then async replication is a valid default.
- what is missing or risky:
  - Push state transitions are not defined.
  - Ordering across multiple refs and retries is not defined.
  - Idempotency and replay behavior are not defined.
- what should change:
  - Add a push journal model and a replication job state machine.

### Push Acknowledgement Policy

- verdict: `Contradictory`
- what works:
  - `durable-local` is a defensible default.
- what is missing or risky:
  - The RFC does not define the durability boundary precisely enough to justify acknowledgement.
- what should change:
  - Specify whether the authoritative ref update and replication enqueue happen in one transaction or via recoverable journal replay.
  - Add crash-recovery invariants.

### Policy Enforcement

- verdict: `Sound`
- what works:
  - Git-native hooks are the right enforcement points.
- what is missing or risky:
  - `post-receive` is too late to be part of a durability guarantee unless replay is defined.
- what should change:
  - Separate validation, authoritative update, and replication journaling responsibilities more explicitly.

### Authentication and Authorization

- verdict: `Incomplete`
- what works:
  - The RFC correctly separates client-to-relay auth from relay-to-upstream auth.
- what is missing or risky:
  - It does not define attribution, least privilege, token scope, or private repo isolation.
- what should change:
  - Define one concrete MVP model for:
    - client identity
    - per-repo ACL evaluation
    - upstream machine credential scope
    - audit identity for replicated writes

### Tarball Compatibility

- verdict: `Remove from core RFC`
- what works:
  - It correctly names a real compatibility gap.
- what is missing or risky:
  - Under the clarified scope, it no longer belongs in the foundational architecture.
- what should change:
  - Remove Sections 10.2, 11.2, and 15 from the main RFC.
  - Replace them with a short note: future work may revisit tarball compatibility if migrated direct inputs and explicit transitive overrides prove insufficient.

### Storage Model

- verdict: `Weak`
- what works:
  - One bare repo per logical repository is reasonable.
  - SQLite is a good metadata store.
- what is missing or risky:
  - Queue schema, reflog retention, maintenance policy, and corruption recovery are absent.
- what should change:
  - Collapse storage to:
    - bare Git repos
    - SQLite metadata and push journal
  - Remove archive storage from the core model.

### Operations and Observability

- verdict: `Weak`
- what works:
  - The RFC names the right observability categories.
- what is missing or risky:
  - There is no explicit repo state model, push state model, or migration diagnostics.
- what should change:
  - Add states for:
    - repo freshness
    - replication lag
    - authoritative divergence
    - migration status

### Security Considerations

- verdict: `Incomplete`
- what works:
  - It correctly identifies the relay as a high-value trust boundary.
- what is missing or risky:
  - No threat model for subprocess execution, key material, workspace mutation, or multi-user deployment.
- what should change:
  - Add a concrete threat model.
  - Define credential storage, filesystem permissions, and audit requirements.

### Failure Modes and Recovery

- verdict: `Incomplete`
- what works:
  - The RFC identifies upstream fetch failure, replication failure, and divergence as first-order cases.
- what is missing or risky:
  - No crash recovery story for acknowledged pushes.
  - No migration failure rollback story.
  - No DB corruption or queue corruption handling.
- what should change:
  - Add a recovery matrix covering:
    - push acknowledged before worker start
    - crash during ref update
    - crash after ref update before queue persistence
    - failed repository migration

### Configuration Model

- verdict: `Needs rewrite`
- what works:
  - Examples are useful.
- what is missing or risky:
  - The config still includes archive configuration that should not be part of the core RFC.
  - It does not define config precedence, validation, or secret handling.
- what should change:
  - Split configuration into:
    - daemon config
    - metadata DB
    - explicit repo migration command flags

### MVP Scope

- verdict: `Contradictory`
- what works:
  - The list shape is clear.
- what is missing or risky:
  - The included items are too broad.
  - Archive compatibility should not be in MVP if source migration is acceptable.
- what should change:
  - MVP should include:
    - SSH Git relay
    - optional smart HTTP
    - cache-only repos
    - basic authoritative repos
    - durable-local push journal
    - `insteadOf` bootstrap
    - Nix migration command
  - MVP should exclude tarball compatibility.

### Alternatives Considered

- verdict: `Weak`
- what works:
  - Rejecting generic MITM as default is correct.
- what is missing or risky:
  - The current Git-only rejection is no longer valid under the clarified scope.
- what should change:
  - Rewrite the alternatives around:
    - Git-only with explicit migration
    - two-plane compatibility architecture
    - generic interception

### Rollout Plan

- verdict: `Weak`
- what works:
  - The phased structure is usable.
- what is missing or risky:
  - The phases do not match the clarified architecture.
- what should change:
  - New rollout:
    - Phase 1: cache-only SSH relay and explicit relay URLs
    - Phase 2: `insteadOf` bootstrap and Nix migration tooling
    - Phase 3: authoritative push acceptance and replication after durability validation
    - Phase 4: optional smart HTTP hardening
    - Phase 5: optional tarball RFC if evidence justifies it

### Open Questions

- verdict: `Weak`
- what works:
  - Several questions are real.
- what is missing or risky:
  - The highest-risk open questions are missing.
- what should change:
  - Add questions for:
    - preferred migration target: `git+ssh://` vs `git+https://` by repo class
    - repo-migration UX and rollback semantics
    - accepted transitive shorthand gap
    - crash-safe acknowledgement implementation

### Final Recommendation

- verdict: `Needs rewrite`
- what works:
  - The RFC still points in the right general direction: Git-first.
- what is missing or risky:
  - The tarball sidecar is no longer justified as a default architectural commitment.
- what should change:
  - Replace the recommendation with a Git-only core recommendation plus explicit migration tooling and an optional future tarball extension.

## 4. Review of the Prior Agent Review

### What It Got Right

- It correctly rejected TLS MITM as the default architecture.
- It correctly identified that a Git-only path becomes much more attractive once direct Nix inputs can be migrated to Git URLs.
- It was right that tarballs cannot safely be derived from Git state in the general case.
- It was right that the RFC's tarball bootstrap story was vague.

### What It Got Wrong

- It overstated the case for `git+ssh://` as the universal migration target.
- It incorrectly implied GitHub Actions has native SSH coverage for this use case. That is not a safe portability assumption.
- It treated tarball-to-Git `narHash` changes as universal rather than repository-dependent.
- It turned a scope argument into a language argument and overstated the architectural significance of Rust over Go.

### What It Missed

- If project source mutation is allowed, the architecture should explicitly include a repo migration command, not merely a machine bootstrap command.
- `git+https://` should be considered alongside `git+ssh://`, especially for CI portability and public repo access.
- The migration flow itself has non-trivial failure modes and must be part of the architecture.

### Which Recommendations I Endorse, Reject, or Modify

- `tarball compatibility removed from MVP`
  - Endorse.
- `tarball compatibility removed entirely`
  - Modify.
  - Remove it from the foundational RFC and current MVP, but keep the option for a later dedicated RFC if real-world transitive shorthand gaps prove unacceptable.
- `Git-only architecture is superior`
  - Endorse, under the clarified scope.
  - It is superior if Git Relay is allowed to migrate direct Nix inputs and the remaining transitive shorthand gap is accepted as a non-goal for MVP.
- `migrate Nix inputs to git+ssh:// or git+https://`
  - Endorse with modification.
  - This should be a policy-driven migration tool, not a blanket text replacement.
  - `git+https://` should be preferred where CI portability or anonymous public access matters.
  - `git+ssh://` remains valid where SSH auth and developer parity are the priority.
- `workstation-only constraint should dominate architecture`
  - Reject as an architectural axiom unless the product owner states it explicitly.
  - A workstation-first bootstrap is compatible with the design, but the RFC should not silently rule out nearby/shared deployments.
- `storage model should collapse to Git-only`
  - Endorse for MVP.
- `Rust is a better implementation choice than Go`
  - Reject as premature and not decisive for the RFC.

### Whether Its Proposed Architecture Shift Is Justified

Yes, with revision.

The prior review was directionally right to push toward a Git-only core. It was too absolute in how it defended `git+ssh://`, too dismissive of future tarball needs, and too eager to turn an architecture review into a language recommendation. But under the clarified scope, its main architectural conclusion is substantially correct.

## 5. Recommended Architecture Decision

`Adopt a Git-only architecture`

### Decision

The revised RFC should define Git Relay as:

- a Git relay and cache for SSH and optional smart HTTP
- a single-endpoint authoritative push relay with async replication
- a repository migration tool that rewrites direct Nix flake inputs from shorthand tarball fetchers to Git transports and re-locks them

### Explicit Tradeoffs

- upside:
  - one protocol boundary
  - one cache model
  - one identity model
  - simpler security model
  - simpler operations
- downside:
  - onboarding now includes repository mutation and relocking
  - some transitive shorthand-based inputs may still bypass the relay unless explicitly overridden
  - CI portability depends on choosing appropriate migrated URLs and credentials

### Constraints That Must Be Written Into the RFC

- Direct input migration is part of onboarding.
- Repo mutation is explicit and reviewable.
- MVP does not guarantee interception of all transitive shorthand-based fetches.
- Tarball compatibility is future work, not part of the foundation.

## 6. Required RFC Rewrites

1. Rewrite `Sections 1-6` around a Git-only core.
2. Remove `Sections 10.2, 11.2, and 15` from the main RFC.
3. Replace the current transparency section with two separate workflows:
   - machine bootstrap
   - repository migration
4. Add a new section: `Nix Migration Model`
   - migration targets
   - relock behavior
   - dirty-worktree policy
   - rollback semantics
   - CI credential implications
5. Rewrite `Sections 12, 19, and 24` around crash-safe push acknowledgement and divergence recovery.
6. Rewrite `Sections 21-25` so the architecture decision, MVP, rollout, and recommendation are aligned.

## 7. Validation Plan Before Implementation

### Must Be Prototyped

- `git-relay bootstrap`
  - writes Git URL rewrite rules only
- `git-relay migrate-flake-inputs`
  - rewrites direct inputs
  - updates `flake.lock`
  - preserves formatting reasonably
  - handles dirty worktrees safely

### Must Be Verified

- On supported Nix versions, direct `git+ssh://` flake inputs honor Git `insteadOf`.
- On supported Nix versions, direct `git+https://` flake inputs honor Git `insteadOf`.
- Local validation already confirmed both on Nix 2.26.3.
- Migration changes to `narHash` are repository-dependent.
- Local validation showed:
  - a simple repo produced the same `narHash` for Git and tarball fetches
  - a repo with `.gitattributes export-ignore` produced a different `narHash`
- Therefore the migration tool must always re-lock and must never assume hash preservation.

### Must Be Tested

- direct-input migration on representative flakes
- transitive dependency override coverage using `follows`
- CI portability for public and private repos
- cache miss coalescing
- large-repo first fetch performance
- acknowledged-push crash recovery
- divergence detection and repair

### Needs Verification Before Any Future Tarball RFC

- actual user pain from unhandled transitive shorthand fetches
- whether a future tarball layer would preserve the compatibility properties users care about
- whether the operational cost is justified by real adoption demand

## 8. Final Recommendation to the Author

1. Rewrite the RFC around a Git-only architecture.
2. Make Nix input migration a first-class part of the design.
3. Treat repo mutation as an explicit migration workflow, not as a hidden side effect of installation.
4. Narrow the MVP to Git relay, bootstrap, migration, and crash-safe push durability.
5. Defer tarball compatibility to a later RFC unless validation proves it is indispensable.
6. Do not lock in Rust vs Go at the architecture stage.

## Appendix: Local Validation Performed for This Review

- Nix version used: `2.26.3`
- Git version used: `2.53.0`
- Verified locally:
  - `git+ssh://...` flake fetches honored Git `insteadOf`
  - `git+https://...` flake fetches honored Git `insteadOf`
  - Git and tarball `narHash` equality is not universal

Those results are enough to support the architectural direction, but not enough to skip the broader validation plan.
