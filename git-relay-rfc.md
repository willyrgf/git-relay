# RFC: Git Relay — Git-First Edge Relay with Tarball Compatibility for Nix Fetchers

**Status:** Draft  
**Date:** 2026-04-02  
**Audience:** Product, platform, infrastructure, and implementation engineers

## 1. Summary

This RFC proposes **Git Relay**, a Git-first edge service that becomes the standard local or nearby endpoint for source retrieval and source publication.

Git Relay has two primary responsibilities:

1. **Git relay and cache** for native Git transports (`ssh`, `http`, `https` via smart HTTP).
2. **Tarball compatibility** for source fetchers that refer to Git-hosted repositories but do not use Git on the wire, especially Nix flake fetchers such as `github:`, `gitlab:`, and `sourcehut:`.

The service is designed to be as transparent as possible after one-time bootstrap. Users keep using normal Git remotes and normal Nix commands. The relay centralizes:

- read-through caching,
- offline reuse of previously seen source content,
- single-endpoint push acceptance,
- asynchronous replication to upstream Git remotes,
- and compatibility coverage for tarball-based forge fetchers.

The recommended architecture is **Git-first**, not Nix-first and not generic network interception. Git remains the core protocol boundary and the authoritative model for repositories. A small tarball compatibility module exists only to cover fetchers that never become Git traffic.

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

Some source consumers use real Git transports, while others use tarball-based fetchers for repositories hosted on Git forges. This means a pure Git relay does not fully cover all source access patterns.

## 3. Background and Constraints

### 3.1 Git over SSH is remote execution of Git server-side commands

For SSH transport, the Git client invokes `git-upload-pack` or `git-receive-pack` remotely and then speaks the pack protocol over that channel. This means the clean interception boundary is a Git-aware endpoint, not a generic TCP proxy.

### 3.2 Git already has native server-side building blocks

Git already ships restricted SSH command support via `git-shell`, smart HTTP server support via `git-http-backend`, and standard hooks for policy enforcement around pushes.

### 3.3 Git URL rewriting is the right transparency mechanism for real Git traffic

Git supports `url.<base>.insteadOf` and `url.<base>.pushInsteadOf`, which makes it possible to route normal-looking remote URLs through a relay without retraining users.

### 3.4 Nix forge shorthands are not always Git on the wire

Nix flake references such as `github:`, `gitlab:`, and `sourcehut:` refer to Git-hosted repositories, but the fetchers are documented as tarball-based for those shorthand forms. Their locked form records exact source-tree fetch information, including `narHash`.

### 3.5 Therefore, full coverage requires two planes

A pure Git transport relay covers all real Git URLs, but it does not see tarball-based forge fetchers. To provide full source transparency, the system must add a narrow compatibility layer for tarball fetchers while keeping Git as the main product boundary.

## 4. Goals

1. Provide a single standard local or nearby endpoint for source access.
2. Preserve normal Git semantics for clone, fetch, and push.
3. Make repeated source access local after the first successful fetch.
4. Improve offline and degraded-network behavior for already-seen source content.
5. Centralize push replication behind a single client-facing endpoint.
6. Keep the product Git-first while still covering tarball-only forge fetchers used by Nix.
7. Require only one-time bootstrap, after which the relay is mostly invisible.

## 5. Non-Goals

1. Reimplement Git protocol semantics in a custom server.
2. Replace Git with a Nix-specific source protocol.
3. Perform broad, generic TLS interception for all outbound traffic by default.
4. Provide full multi-tenant enterprise authorization in the first version.
5. Guarantee offline success for source content that has never been seen before.
6. Cover Git LFS in the MVP.
7. Build a distributed cluster in the MVP.

## 6. Proposed Solution

The recommended solution is:

> **A Git-first edge relay with a narrow tarball compatibility module.**

The product has one control plane and two data planes.

### 6.1 Control plane

A daemon responsible for:

- routing and identity resolution,
- repository policy,
- metadata storage,
- cache state,
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

### 6.3 Tarball compatibility plane

A small read-only HTTP service for source fetchers that name Git-hosted repositories but do not use Git on the wire.

Its responsibilities are:

- identify supported forge fetcher requests,
- resolve them to a logical repository and revision,
- fetch or replay compatible archive content,
- optionally hydrate the Git cache in the background,
- and provide a relay-owned tarball surface for controlled environments.

This module exists because some consumers fetch source trees as tarballs while still conceptually referring to Git-hosted repositories.

## 7. Recommended Architecture

### 7.1 Technology choices

- **Language:** Go
- **SSH ingress:** OpenSSH
- **Git server primitives:** system Git
- **HTTP Git support:** `git-http-backend`
- **Metadata and job state:** SQLite
- **Object storage:** filesystem bare repositories
- **Archive cache:** filesystem-backed content-addressed archive cache

### 7.2 Why Go

Go is a strong fit for the control plane:

- static binaries,
- strong standard library for network daemons,
- solid SQLite ecosystem,
- simple operational model,
- and good fit for filesystem and process orchestration.

Go should orchestrate. System Git should perform Git operations.

### 7.3 Why not a pure Git reimplementation

The product’s hardest requirement is correctness at the Git boundary. The safest MVP is to let Git itself handle:

- upload-pack,
- receive-pack,
- smart HTTP,
- hooks,
- pack negotiation,
- and repository maintenance.

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
- and replication proceeds after local acceptance.

This distinction is important because fetch-mirror semantics write upstream refs directly into local `refs/`, which is correct for mirrors but dangerous for write-accepting repositories.

## 9. Identity Model

The relay should normalize all inputs onto a logical repository identity such as:

- `github.com/org/repo.git`
- `gitlab.com/group/repo.git`
- `git.example.com/team/repo.git`

Transport-specific ingress is then mapped onto that identity.

Examples:

- `git@github.com:org/repo.git`
- `ssh://git@github.com/org/repo.git`
- `https://github.com/org/repo.git`
- tarball fetcher metadata for `github:org/repo`

All of these may refer to the same logical repository, but they do not necessarily share the same auth path.

## 10. Transparent Interception Model

### 10.1 Primary mechanism: Git URL rewriting

For real Git traffic, the relay should use Git’s native URL rewriting support.

Example:

```ini
[url "ssh://git@127.0.0.1:4222/ssh/github.com/"]
    insteadOf = git@github.com:
    insteadOf = ssh://git@github.com/

[url "https://127.0.0.1:4318/https/github.com/"]
    insteadOf = https://github.com/

[url "ssh://git@127.0.0.1:4222/ssh/github.com/"]
    pushInsteadOf = git@github.com:
    pushInsteadOf = ssh://git@github.com/
```

This makes the relay effectively invisible for Git users after bootstrap.

### 10.2 Secondary mechanism: scoped archive compatibility

For tarball-only fetchers, the relay cannot rely on Git URL rewriting because no Git session is created. The compatibility plane must therefore provide one of the following:

1. explicit relay-owned archive URLs for controlled environments, or
2. a narrowly scoped archive interception mode for the specific forge archive paths that the system needs to cover.

Generic network interception is not the main mechanism and should remain opt-in.

## 11. Read Path

### 11.1 Git clone/fetch over SSH or smart HTTP

1. Client connects to relay.
2. Relay parses requested service and logical repository.
3. Relay resolves policy.
4. Relay checks local repository state.
5. If content is already present, serve locally.
6. If content is missing and policy allows upstream access, fetch from upstream.
7. Persist new objects locally.
8. Serve response using native Git server-side commands.

### 11.2 Tarball fetch path

1. Client requests a supported archive representation.
2. Compatibility plane resolves logical repository, revision, and source-tree identity.
3. If archive or source tree is cached and valid, serve locally.
4. Otherwise fetch from upstream archive endpoint or derive a compatible archive from the corresponding Git state when safe to do so.
5. Store compatibility metadata and archive content.
6. Optionally hydrate or refresh the Git cache for the same repository.

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
5. Replication jobs are durably queued.
6. Background workers push the accepted ref updates to configured upstream remotes.

### 12.2 Acknowledgement policy

The default policy should be **durable-local**:

- success is returned after local acceptance and durable queueing of replication work.

Optional future policies:

- all-upstreams-must-succeed,
- selected-upstreams-must-succeed,
- branch-specific acknowledgement profiles.

### 12.3 Why durable-local is preferred

It best matches the product goals:

- offline tolerance,
- resilience to transient upstream failures,
- simple client contract,
- and cleaner operator recovery.

## 13. Policy Enforcement

The relay should rely on Git-native enforcement points.

### 13.1 Hooks

Use:

- `pre-receive` for whole-push validation,
- `update` for per-ref validation,
- `post-receive` for enqueueing replication and notifications.

### 13.2 Default repository protections

Recommended defaults:

- deny deletes unless explicitly allowed,
- deny non-fast-forward updates unless explicitly allowed,
- enable object verification on receive,
- restrict access to Git-only server-side commands,
- and expose clear audit logs for ref updates.

## 14. Authentication and Authorization

### 14.1 Client to relay

Use normal user-facing Git auth.

- SSH keys for SSH traffic,
- standard HTTP auth mechanisms if HTTP is enabled.

### 14.2 Relay to upstream

Default to relay-owned machine credentials.

Reasons:

- background replication must work after the client disconnects,
- retries and reconciliation need stable credentials,
- and tarball fetch compatibility often requires host-scoped tokens.

### 14.3 Optional future mode

Per-user upstream delegation can be added later for environments that need upstream attribution, but it should not be the MVP default.

## 15. Tarball Compatibility Details

### 15.1 Why this exists

Nix flake shorthand fetchers such as `github:` and `gitlab:` may refer to repositories conceptually, but those shorthand fetchers are archive-based rather than Git-transport based.

### 15.2 Required properties

The compatibility module must preserve source-tree semantics expected by the consumer.

This means the module must key cache entries by properties such as:

- provider,
- host,
- owner/group,
- repo,
- revision or ref,
- subdirectory,
- and integrity identity such as `narHash` when available.

### 15.3 Two operating modes

#### A. Compatibility proxy mode

Use upstream-compatible archive requests and cache the results.

Best for:

- existing projects,
- existing lock files,
- minimal user-visible changes.

#### B. Relay-native tarball mode

Expose relay-owned tarball URLs and implement a stable, lockable tarball contract.

Best for:

- controlled environments,
- internal standardization,
- future simplification.

### 15.4 Recommendation

Ship **compatibility proxy mode** first, and keep relay-native tarball serving as a later optimization.

## 16. Storage Model

### 16.1 Git storage

One bare repository per logical repository.

Advantages:

- simple operator model,
- native Git maintenance,
- clear isolation,
- straightforward recovery.

### 16.2 Metadata store

SQLite tables should cover:

- repository identity and ingress mappings,
- repository mode,
- upstream definitions,
- replication queues,
- cache pins and TTLs,
- archive cache metadata,
- and audit events.

### 16.3 Archive storage

Store cached archives content-addressably and separately from Git object stores.

### 16.4 Garbage collection

The relay needs explicit policies for:

- retention,
- pinning,
- eviction,
- repository maintenance,
- archive expiry,
- and failed replication clean-up.

## 17. Operations and Observability

The system should expose:

- cache hits and misses,
- upstream latency,
- object growth,
- archive cache growth,
- replication lag,
- failed ref replications,
- per-repo health,
- and authentication failures.

A minimal operator interface should include:

- `doctor`,
- `repo add`,
- `repo inspect`,
- `replication status`,
- `replication retry`,
- `cache pin`,
- `cache evict`,
- `archive inspect`.

## 18. Security Considerations

1. The relay becomes a high-value trust boundary.
2. Client-facing SSH access must be restricted to Git-only commands.
3. Upstream machine credentials must be isolated and auditable.
4. Compatibility archive interception must be narrow in scope if enabled.
5. Private source archives and public source archives should be segregated logically.
6. Object verification and ref protection should be on by default.
7. Operator actions should be audited.

## 19. Failure Modes and Recovery

### 19.1 Upstream fetch failure

If a repository is already cached and the requested objects are present, serve from cache. If required objects are absent, return a normal fetch failure.

### 19.2 Upstream replication failure

Do not roll back a locally accepted push. Queue retries, mark the repository degraded, and expose reconciliation tooling.

### 19.3 Archive fetch failure

If a matching archive or validated source tree is cached, serve it. Otherwise fail clearly and preserve diagnostic state.

### 19.4 Divergence

Authoritative repositories should include divergence detection and explicit repair commands.

## 20. Configuration Model

The relay should have:

- one global daemon configuration file,
- one SQLite-backed metadata database,
- and a CLI for repo and policy management.

Example global configuration:

```toml
[git]
ssh_listen = "127.0.0.1:4222"
http_listen = "127.0.0.1:4318"
cache_root = "/var/lib/git-relay/git"

[archive]
mode = "compat-proxy"
listen = "127.0.0.1:4320"
cache_root = "/var/lib/git-relay/archive"
providers = ["github", "gitlab", "sourcehut", "tarball"]

[replication]
default_push_ack = "durable-local"
retry_backoff = "exponential"

[policy]
default_repo_mode = "cache-only"
default_refresh = "ttl:60s"
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
- smart HTTP support
- Git URL rewriting bootstrap helpers
- cache-only repositories
- authoritative repositories
- local durable push acceptance
- asynchronous replication
- repository metadata in SQLite
- archive compatibility module for GitHub/GitLab/SourceHut/tarball fetchers
- basic metrics, logs, and repair commands

### Excluded

- Git LFS
- distributed cluster mode
- advanced multi-tenant auth
- cross-repo object deduplication
- generic full-traffic MITM as default behavior
- full enterprise attribution of upstream writes

## 22. Alternatives Considered

### 22.1 Pure Git-only relay

**Rejected as insufficient.**

Why:

- correct for real Git traffic,
- incorrect for archive-based forge fetchers,
- fails the “works for tarball shorthand too” requirement.

### 22.2 Generic interception as the primary mechanism

**Rejected as the default architecture.**

Why:

- larger trust boundary,
- higher operational risk,
- harder debugging,
- unnecessary for real Git URLs,
- and conceptually broader than the product needs.

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
- authoritative local accept for selected repositories
- replication queue

### Phase 2

- Git URL rewrite bootstrap
- smart HTTP ingress
- operator tooling and metrics

### Phase 3

- archive compatibility module for forge shorthand coverage
- background Git hydration from archive activity

### Phase 4

- refined cache policies
- optional relay-native tarball endpoints
- advanced auth and policy features

## 24. Open Questions

1. Should authoritative repositories forbid direct upstream pushes operationally, or merely detect divergence?
2. Which archive compatibility mode should be the default in workstation installs?
3. How aggressive should automatic Git hydration be after archive-only fetches?
4. What retention defaults keep cache growth practical without undermining offline expectations?
5. Should relay-owned upstream credentials be mandatory for authoritative repositories?

## 25. Recommendation

Adopt the following product direction:

> **Git Relay should be built as a Git-first cache and replication edge, with a narrowly scoped tarball compatibility sidecar for archive-based forge fetchers.**

This preserves the cleanest protocol boundary, keeps Git as the authoritative model, gives users the transparency they want after one-time bootstrap, and still covers the Nix shorthand cases that a pure Git relay cannot see.

