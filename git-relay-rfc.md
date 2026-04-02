# RFC: Git Relay — Git-First Edge Relay with Explicit Nix Input Migration

**Status:** Draft  
**Date:** 2026-04-02  
**Audience:** Product, platform, infrastructure, and implementation engineers

## 1. Summary

This RFC proposes **Git Relay**, a Git-first edge service that becomes the standard local or nearby endpoint for Git source retrieval and source publication.

Git Relay has three core responsibilities:

1. **Git relay and cache** for native Git transports (`ssh`, `http`, `https` via smart HTTP).
2. **Single-endpoint push acceptance** for repositories that should accept writes through the relay and replicate them upstream asynchronously.
3. **Explicit Nix input migration tooling** for direct flake inputs that currently use shorthand archive fetchers such as `github:`, `gitlab:`, and `sourcehut:`.

The key architectural decision in this RFC is:

> **Git Relay is a Git-only foundation. It does not include a tarball compatibility plane in the foundational architecture or MVP.**

Instead of trying to transparently intercept existing shorthand tarball fetches, Git Relay treats Nix compatibility as a migration problem:

- machine bootstrap installs Git URL rewrite rules,
- repository migration rewrites direct flake inputs to Git URLs,
- and lock files are re-generated under the new transport semantics.

This keeps the product Git-first, preserves a single protocol boundary, and avoids making archive compatibility a foundational requirement before it is proven necessary.

## 2. Problem Statement

Modern development workflows repeatedly consume the same source repositories from many remote locations:

- developers clone and fetch directly,
- CI fetches repositories repeatedly,
- tools resolve dependencies from Git hosts,
- and Nix may recursively fetch transitive source inputs from several hosts.

This causes several recurring problems.

### 2.1 Repeated network transfers

The same repositories and revisions are often fetched many times by many tools and processes, even when identical content was already downloaded earlier.

### 2.2 Weak offline behavior

A developer may have already fetched every source revision needed by a project, but later commands can still fail when upstream network access is absent or unreliable.

### 2.3 Fragmented remote topology

One logical project may depend on:

- canonical upstreams,
- internal mirrors,
- backup mirrors,
- or self-hosted forges.

The client experience is usually fragmented, while the desired experience is a single logical endpoint.

### 2.4 Push fan-out is brittle

When pushes must reach multiple remotes, the logic typically lives in:

- client-side multiple push URLs,
- per-repo scripts,
- custom hooks,
- or out-of-band mirror jobs.

That spreads infrastructure policy into every client and every repository clone.

### 2.5 The Git/Nix mismatch

Some source consumers use real Git transports, while others use shorthand fetchers for Git-hosted repositories that resolve to archive downloads rather than Git protocol traffic.

For Git Relay, this mismatch splits into two different problems:

1. **Direct inputs owned by the adopting repository**, which can be migrated deliberately to Git URLs.
2. **Transitive third-party inputs**, which may still use shorthand fetchers outside the adopting repository’s direct control.

This RFC addresses the first problem directly and treats the second as a bounded MVP gap rather than as a reason to build a second data plane immediately.

## 3. Background and Constraints

### 3.1 Git over SSH is remote execution of Git server-side commands

For SSH transport, the Git client invokes `git-upload-pack` or `git-receive-pack` remotely and then speaks the pack protocol over that channel. This means the clean interception boundary is a Git-aware endpoint, not a generic TCP proxy.

### 3.2 Git already has native server-side building blocks

Git already ships restricted SSH command support via `git-shell`, smart HTTP server support via `git-http-backend`, and standard hooks for policy enforcement around pushes.

### 3.3 Git URL rewriting is the right transparency mechanism for real Git traffic

Git supports `url.<base>.insteadOf` and `url.<base>.pushInsteadOf`, which makes it possible to route normal-looking remote URLs through a relay without retraining users.

### 3.4 Nix supports direct Git-based flake inputs

Nix flake inputs can use Git transports directly, including `git+ssh://` and `git+https://`. When direct inputs are expressed this way, they become visible to Git Relay through normal Git transport interception.

### 3.5 Migration can change lock semantics and `narHash`

Migrating a direct flake input from a shorthand archive fetcher to a Git fetcher changes the transport semantics used to materialize the source tree. The resulting locked metadata, including `narHash`, may change and must be regenerated rather than preserved by assumption.

### 3.6 Direct-input migration and transitive shorthand coverage are different requirements

If Git Relay is allowed to modify `flake.nix` and `flake.lock`, then direct-input coverage does not require a tarball plane.

Transitive third-party shorthand inputs remain a separate problem. They can sometimes be reduced with `follows` or explicit overrides, but they are not guaranteed to disappear entirely in MVP.

### 3.7 The foundational architecture should optimize for one protocol boundary

The highest-risk part of this product is correctness at the Git boundary: clone, fetch, push, authorization, and crash-safe replication. The RFC should optimize for getting that boundary correct first.

## 4. Goals

1. Provide a single standard local or nearby endpoint for Git source access.
2. Preserve normal Git semantics for clone, fetch, and push.
3. Make repeated Git source access local after the first successful fetch.
4. Improve offline and degraded-network behavior for already-seen Git content.
5. Centralize push replication behind a single client-facing endpoint.
6. Provide explicit migration tooling for direct Nix flake inputs that should route through the relay.
7. Keep bootstrap and migration understandable, explicit, and reviewable.

## 5. Non-Goals

1. Reimplement Git protocol semantics in a custom server.
2. Preserve unmodified `github:` / `gitlab:` / `sourcehut:` direct inputs in place.
3. Guarantee interception of all transitive shorthand-based Nix fetches in MVP.
4. Perform broad, generic TLS interception for all outbound traffic by default.
5. Provide full multi-tenant enterprise authorization in the first version.
6. Guarantee offline success for source content that has never been seen before.
7. Cover Git LFS in the MVP.
8. Build a distributed cluster in the MVP.

## 6. Proposed Solution

The recommended solution is:

> **A Git-first edge relay with explicit Nix input migration, not a two-plane Git-plus-tarball system.**

The product has:

- one control plane,
- one Git data plane,
- and one repository migration workflow.

### 6.1 Control plane

A daemon responsible for:

- routing and identity resolution,
- repository policy,
- metadata storage,
- cache state,
- push journaling,
- replication jobs,
- observability,
- and administration.

### 6.2 Git data plane

A native Git server layer for real Git transports.

- SSH ingress via OpenSSH and a restricted user.
- Command routing via `ForceCommand` and `SSH_ORIGINAL_COMMAND`.
- Optional HTTP/HTTPS ingress via `git-http-backend`.
- Local bare repositories as the authoritative on-disk representation.
- Native `git-upload-pack` and `git-receive-pack` for serving and receiving.

### 6.3 Repository migration workflow

A CLI workflow responsible for migrating direct flake inputs that should route through the relay.

Its responsibilities are:

- identify direct shorthand-based flake inputs,
- rewrite them to Git URLs according to policy,
- update lock files under the new transport semantics,
- surface a reviewable diff,
- and refuse unsafe mutations by default, such as mutating a dirty worktree without explicit confirmation.

This workflow is explicit. It is not hidden behind silent install-time file mutation.

## 7. Recommended Architecture

### 7.1 Technology choices

- **Implementation language:** intentionally deferred in this RFC
- **SSH ingress:** OpenSSH
- **Git server primitives:** system Git
- **HTTP Git support:** `git-http-backend`
- **Metadata and push journal:** SQLite
- **Object storage:** filesystem bare repositories

### 7.2 Why implementation language is deferred

This RFC is deciding the product boundary and protocol architecture, not the final implementation language.

The foundational decision is to use:

- system Git for Git correctness,
- OpenSSH for SSH ingress,
- and SQLite plus filesystem storage for local durability.

Go and Rust are both viable implementation options for the control plane. Language selection should follow the stabilization of protocol, migration, and durability requirements rather than precede them.

### 7.3 Why not a pure Git reimplementation

The product’s hardest requirement is correctness at the Git boundary. The safest MVP is to let Git itself handle:

- upload-pack,
- receive-pack,
- smart HTTP,
- hooks,
- pack negotiation,
- and repository maintenance.

### 7.4 Why not include tarball compatibility in the foundational architecture

Given the accepted product constraint that Git Relay may migrate `flake.nix` and `flake.lock`, a mandatory tarball plane is no longer the simplest path to product success.

Adding archive compatibility now would:

- add a second data plane,
- add a second cache domain,
- reintroduce compatibility questions around archive semantics,
- and distract the foundational RFC from the Git boundary that actually needs to be proven first.

Tarball compatibility may be revisited later if migrated direct inputs plus explicit transitive overrides are insufficient in practice.

## 8. Repository Model

Git Relay supports two repository modes.

### 8.1 Cache-only repository

Used for repositories that are read through the relay but not written through it.

Properties:

- local bare mirror,
- upstream is source of truth,
- relay may refresh on demand,
- suitable for public dependencies and read-only source access.

### 8.2 Authoritative repository

Used for repositories that accept client pushes through the relay.

Properties:

- local bare repository is canonical for relay clients,
- upstreams are replication targets,
- relay enforces push policy before ref updates,
- acknowledged pushes are durably journaled before success is returned,
- and replication proceeds after local acceptance.

This distinction is important because cache-only refresh semantics and write-accepting repository semantics are not the same thing.

## 9. Identity Model

Git Relay should separate at least three identities.

### 9.1 Repository identity

A canonical logical repository identity such as:

- `github.com/org/repo.git`
- `gitlab.com/group/repo.git`
- `git.example.com/team/repo.git`

Repository identity:

- excludes transport scheme,
- excludes username,
- normalizes optional `.git` suffix according to policy,
- and maps multiple ingress forms onto one canonical path.

### 9.2 Source-tree identity

A source-tree identity consists of:

- repository identity,
- resolved object identity such as commit SHA,
- and optional subtree selection when relevant.

This identity is what read caching and Nix migration ultimately care about.

### 9.3 Upstream auth identity

Different ingress paths may refer to the same repository while using different authentication mechanisms or credentials. Auth identity must therefore remain separate from repository identity.

## 10. Transparent Interception and Migration Model

### 10.1 Machine bootstrap: Git URL rewriting

For real Git traffic, the relay uses Git’s native URL rewriting support.

Example:

```ini
[url "ssh://git@127.0.0.1:4222/ssh/github.com/"]
    insteadOf = git@github.com:
    insteadOf = ssh://git@github.com/
    pushInsteadOf = git@github.com:
    pushInsteadOf = ssh://git@github.com/

[url "https://127.0.0.1:4318/https/github.com/"]
    insteadOf = https://github.com/
```

This is machine bootstrap. It is written to user or system Git configuration and makes Git Relay effectively invisible for Git traffic after setup.

### 10.2 Repository migration: direct flake input rewriting

Direct flake inputs that currently use shorthand archive-based references are migrated explicitly to Git URLs.

Example:

```nix
# Before
inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

# After
inputs.nixpkgs.url = "git+https://github.com/NixOS/nixpkgs?ref=nixos-unstable";
```

Alternative policy:

```nix
inputs.my-private-repo.url = "git+ssh://git@github.com/my-org/my-private-repo?ref=main";
```

The migration target is policy-driven:

- `git+https://` is a strong default for public repositories and CI portability,
- `git+ssh://` remains appropriate when SSH-based auth or developer parity is the stronger requirement.

### 10.3 Transitive shorthand behavior

MVP guarantees relay coverage for:

- Git operations,
- and direct flake inputs that have been migrated to Git URLs.

MVP does not guarantee full coverage for transitive third-party shorthand inputs embedded in downstream flakes. Those cases may be reduced with:

- `follows`,
- direct overrides,
- or explicit project policy.

They are not solved by the foundational architecture.

## 11. Read Path

### 11.1 Git clone/fetch over SSH or smart HTTP

1. Client connects to relay.
2. Relay parses requested service and repository identity.
3. Relay resolves repository policy and freshness policy.
4. Relay checks local repository state.
5. If required objects are present and freshness policy allows, serve locally.
6. If required objects are missing or freshness policy requires refresh, perform a singleflight upstream refresh when policy allows.
7. Persist new objects locally.
8. Serve response using native Git server-side commands.

Freshness policy must be explicit per repository class. The relay must not advertise ref state blindly without a defined refresh rule.

### 11.2 Migrated Nix input fetch path

1. Repository migration rewrites direct flake inputs to Git URLs.
2. Nix resolves those inputs through Git transport.
3. Git URL rewriting routes the traffic through the relay when local policy applies.
4. The relay serves the fetch through the same Git read path described above.
5. Locked metadata is derived from the Git fetch path and the post-migration lock state, not from the previous shorthand tarball semantics.

## 12. Write Path

### 12.1 Push acceptance model

Recommended default:

- accept locally first,
- then replicate asynchronously.

Flow:

1. Client pushes to relay.
2. Relay invokes native `git-receive-pack` against the local authoritative bare repository.
3. Hooks validate ACLs, protected refs, and fast-forward rules.
4. Local refs update if validation succeeds.
5. Relay persists a durable push journal entry that records the accepted ref changes and required replication work.
6. Success is returned only after the push journal is durable.
7. Background workers push the accepted ref updates to configured upstream remotes.

### 12.2 Acknowledgement policy

The default policy should be **durable-local**:

- success is returned after local acceptance and durable journaling of replication work.

Optional future policies:

- all-upstreams-must-succeed,
- selected-upstreams-must-succeed,
- branch-specific acknowledgement profiles.

### 12.3 Durability requirements

The foundational contract is:

- an **acknowledged** push must remain recoverable after relay restart or crash without requiring client retransmission,
- an **unacknowledged** partial accept must be detectable and quarantined for reconciliation.

The implementation may satisfy this with a journal-plus-recovery design, but the contract itself is mandatory.

## 13. Policy Enforcement

The relay should rely on Git-native enforcement points.

### 13.1 Hooks

Use:

- `pre-receive` for whole-push validation,
- `update` for per-ref validation,
- `post-receive` for non-critical notifications and worker wakeups.

The durability contract must not depend solely on best-effort `post-receive` behavior.

### 13.2 Default repository protections

Recommended defaults:

- deny deletes unless explicitly allowed,
- deny non-fast-forward updates unless explicitly allowed,
- enable object verification on receive,
- restrict access to Git-only server-side commands,
- and expose clear audit logs for ref updates.

### 13.3 Authoritative divergence policy

For authoritative repositories, direct upstream pushes should be treated as unsupported by default unless the repository is explicitly configured for shared-authority operation.

If divergence is detected, the repository should enter a degraded state until repaired intentionally.

## 14. Authentication and Authorization

### 14.1 Client to relay

Use normal user-facing Git auth.

- SSH keys for SSH traffic,
- standard HTTP auth mechanisms if HTTP is enabled.

### 14.2 Relay to upstream

Default to relay-owned machine credentials for read refreshes and background replication.

Reasons:

- background replication must work after the client disconnects,
- retries and reconciliation need stable credentials,
- and the relay must not depend on client credential presence after acceptance.

### 14.3 Attribution and audit

Even when relay-owned machine credentials are used upstream, the relay must record:

- authenticated client identity,
- repository identity,
- accepted ref changes,
- replication targets,
- and replication outcomes.

Per-user upstream delegation may be added later, but it is not the MVP default.

## 15. Nix Migration Model

### 15.1 Why this exists

Direct shorthand inputs that resolve to archive downloads cannot be intercepted through Git URL rewriting because they are not Git traffic.

Given that project-owned source and lock files may be updated, the simplest solution is to migrate those direct inputs to Git transports explicitly.

### 15.2 Supported migration targets

The migration tool should support:

- `git+https://` targets,
- `git+ssh://` targets,
- policy selection by host or repository class,
- and preservation of explicit branch or ref intent where representable.

### 15.3 Migration contract

Migration is an explicit command, not an implicit side effect.

The migration workflow should:

1. inspect direct flake inputs,
2. identify shorthand inputs covered by migration policy,
3. rewrite them to Git URLs,
4. re-lock the affected inputs,
5. show a reviewable diff,
6. and refuse unsafe mutation by default when the repository is dirty.

The migration workflow must not assume that `narHash` or lock metadata remains stable across transport change.

### 15.4 Direct versus transitive coverage

The migration model guarantees coverage only for direct inputs owned by the repository being migrated.

Transitive shorthand inputs may still bypass the relay unless:

- the adopting repository overrides them directly,
- or dependency relationships are tightened using mechanisms such as `follows`.

That remaining gap is accepted in MVP.

## 16. Storage Model

### 16.1 Git storage

One bare repository per logical repository.

Advantages:

- simple operator model,
- native Git maintenance,
- clear isolation,
- straightforward recovery.

### 16.2 Metadata and push journal

SQLite tables should cover:

- repository identity and ingress mappings,
- repository mode,
- upstream definitions,
- refresh policy,
- push journal entries,
- replication jobs,
- replication outcomes,
- and audit events.

### 16.3 Garbage collection and maintenance

The relay needs explicit policies for:

- Git maintenance scheduling,
- retention and pinning,
- cache eviction for cache-only repositories,
- reflog and audit retention,
- and failed replication cleanup.

## 17. Operations and Observability

The system should expose:

- cache hits and misses,
- upstream latency,
- object growth,
- replication lag,
- failed replications,
- per-repo freshness state,
- authoritative divergence state,
- migration activity,
- and authentication failures.

A minimal operator interface should include:

- `doctor`,
- `repo add`,
- `repo inspect`,
- `replication status`,
- `replication retry`,
- `cache pin`,
- `cache evict`,
- `migrate-flake-inputs`,
- and `migration inspect`.

## 18. Security Considerations

1. The relay becomes a high-value trust boundary.
2. Client-facing SSH access must be restricted to Git-only commands.
3. Upstream machine credentials must be isolated and auditable.
4. Project migration commands must be explicit and reviewable.
5. Private repositories and public repositories should be segregated logically in policy and credential scope.
6. Object verification and ref protection should be on by default.
7. Operator actions should be audited.
8. The relay should assume that repository migration is a privileged operation over project source, not a transparent network convenience.

## 19. Failure Modes and Recovery

### 19.1 Upstream fetch failure

If a repository is already cached and the requested objects are present, serve from cache. If required objects are absent, return a normal fetch failure.

### 19.2 Upstream replication failure

Do not roll back a locally accepted push. Queue retries, mark the repository degraded, and expose reconciliation tooling.

### 19.3 Crash or partial accept before acknowledgement

If refs were updated locally but the push was not acknowledged durably, the relay must detect that condition on restart and quarantine the repository for reconciliation.

The system must not silently treat an unacknowledged partial accept as a clean success.

### 19.4 Repository migration failure

If flake input migration fails partway through:

- leave a clear diagnostic trail,
- avoid silent lockfile corruption,
- and provide a straightforward rollback path through normal version control.

### 19.5 Divergence

Authoritative repositories should include divergence detection and explicit repair commands.

## 20. Configuration Model

The relay should have:

- one global daemon configuration file,
- one SQLite-backed metadata database,
- and a CLI for repo, policy, and migration management.

Example global configuration:

```toml
[git]
ssh_listen = "127.0.0.1:4222"
http_listen = "127.0.0.1:4318"
cache_root = "/var/lib/git-relay/git"

[replication]
default_push_ack = "durable-local"
retry_backoff = "exponential"

[policy]
default_repo_mode = "cache-only"
default_refresh = "ttl:60s"

[migration]
default_public_transport = "git+https"
default_private_transport = "git+ssh"
refuse_dirty_worktree = true
```

Example repository rules:

```toml
[[rule]]
match = "github.com/my-org/**"
mode = "authoritative"
read_upstream = "ssh://git@github.com/%repo%"
write_upstreams = ["ssh://git@github.com/%repo%"]
push_ack = "durable-local"

[[rule]]
match = "github.com/**"
mode = "cache-only"
read_upstream = "https://github.com/%repo%"
refresh = "ttl:60s"
```

## 21. MVP Scope

### Included

- Git over SSH
- optional smart HTTP support
- Git URL rewriting bootstrap helpers
- cache-only repositories
- authoritative repositories
- local durable push journaling
- asynchronous replication
- repository metadata in SQLite
- Nix direct-input migration command
- basic metrics, logs, and repair commands

### Excluded

- tarball compatibility plane
- Git LFS
- distributed cluster mode
- advanced multi-tenant auth
- generic full-traffic MITM as default behavior
- guaranteed coverage for all transitive shorthand-based Nix fetches
- full enterprise attribution of upstream writes

## 22. Alternatives Considered

### 22.1 Two-plane Git-plus-tarball architecture

**Rejected for the foundational RFC and MVP.**

Why:

- no longer required for direct-input coverage once repository migration is allowed,
- adds a second data plane and cache domain,
- adds compatibility complexity before the Git boundary has been proven,
- and weakens MVP discipline.

### 22.2 Generic interception as the primary mechanism

**Rejected as the default architecture.**

Why:

- larger trust boundary,
- higher operational risk,
- harder debugging,
- unnecessary for real Git URLs,
- and broader than the product needs.

### 22.3 Nix-first source cache with Git as a secondary feature

**Rejected.**

Why:

- weakens the Git-native model,
- pushes the system toward tool-specific behavior,
- and makes the product less generally useful outside Nix.

## 23. Rollout Plan

### Phase 1

- SSH ingress
- cache-only Git repositories
- explicit relay URLs
- repository identity and policy storage

### Phase 2

- Git URL rewrite bootstrap
- Nix direct-input migration tooling
- lockfile relock workflow

### Phase 3

- authoritative local accept for selected repositories
- push journal
- replication queue and repair tooling

### Phase 4

- smart HTTP ingress
- refined cache policies
- stronger operator tooling and metrics

### Phase 5

- revisit tarball compatibility only if validation shows that transitive shorthand gaps materially block adoption

## 24. Open Questions

1. What should be the default migration target for public repositories: `git+https://`, `git+ssh://`, or policy by host?
2. How much automation should the migration tool provide for transitive overrides such as `follows`?
3. Should authoritative repositories forbid direct upstream pushes operationally, or merely detect divergence?
4. What retention defaults keep cache growth practical without undermining offline expectations?
5. Should relay-owned upstream credentials be mandatory for authoritative repositories?
6. Which deployment defaults should differ between workstation-first and nearby shared installs?

## 25. Recommendation

Adopt the following product direction:

> **Git Relay should be built as a Git-first cache and replication edge, with explicit Nix direct-input migration and no tarball compatibility plane in the foundational architecture or MVP.**

This keeps the protocol boundary coherent, makes bootstrap and repository mutation explicit, allows direct Nix inputs to route through the relay using normal Git mechanisms, and keeps the MVP focused on the part of the system that must be correct first: the Git read/write boundary.
