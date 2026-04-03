# RFC: Git Relay — Git-First Edge Relay with Explicit Nix Input Migration

**Status:** Draft  
**Date:** 2026-04-03  
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
8. Make replication status, divergence, and migration outcomes observable and debuggable.

## 5. Non-Goals

1. Reimplement Git protocol semantics in a custom server.
2. Preserve unmodified `github:` / `gitlab:` / `sourcehut:` direct inputs in place.
3. Guarantee interception of all transitive shorthand-based Nix fetches in MVP.
4. Perform broad, generic TLS interception for all outbound traffic by default.
5. Provide full multi-tenant enterprise authorization in the first version.
6. Guarantee offline success for source content that has never been seen before.
7. Cover Git LFS in the MVP.
8. Build a distributed cluster in the MVP.
9. Support smart HTTP push in the MVP.

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

### 7.1 Transport support matrix

Git Relay distinguishes client-facing ingress, upstream-facing egress, and Nix migration targets.

| Surface | Transport | MVP status | Notes |
|---|---|---|---|
| Ingress | SSH | Required | Primary read/write ingress using OpenSSH forced-command routing into system Git. |
| Ingress | Smart HTTPS | Optional | Read support may be enabled through `git-http-backend`. HTTP push is out of MVP. |
| Ingress | Smart HTTP | Dev/test only | Not a production default. |
| Ingress | `git://` | Unsupported | Fetch-only and unauthenticated. |
| Ingress | Dumb HTTP | Unsupported | Not part of the architecture. |
| Egress | SSH | Supported | Valid for read refresh and authoritative replication. |
| Egress | Smart HTTPS | Supported | Valid for read refresh and authoritative replication. |
| Egress | Smart HTTP | Limited | Internal or lab environments only. |
| Migration target | `git+ssh://` | Supported | Chosen per direct input by policy. |
| Migration target | `git+https://` | Supported | Chosen per direct input by policy. |

Git Relay does not require a single global transport preference. A deployment may support both SSH and HTTPS for upstream reads, upstream writes, and direct-input migration.

Each individual upstream attempt still uses one concrete URL. “Support all transports” means policy may select from multiple supported transports, not that a single fetch or push uses all transports simultaneously.

### 7.2 Required capability floor

This RFC defines capability requirements first and version floors second.

Required Git capabilities:

- `git-upload-pack`
- `git-receive-pack`
- smart HTTP via `git-http-backend`
- protocol v2 support for normal read-path operation
- receive quarantine behavior for incoming objects
- hooks: `pre-receive`, `update`, `reference-transaction`, `post-receive`
- detection of upstream push capabilities such as `atomic` and `push-options`
- repository maintenance commands and object verification

Required OpenSSH capabilities:

- `ForceCommand`
- `SSH_ORIGINAL_COMMAND`
- Git-only restricted account operation
- forwarding disablement such as `DisableForwarding`
- key lookup via `AuthorizedKeysFile` or `AuthorizedKeysCommand`

Required Nix capabilities:

- flake Git URLs using `git+ssh://` and `git+https://`
- lockfile update commands such as `nix flake lock --update-input` and `nix flake update`
- correct handling of transitive lock state for indirect dependencies

Exact minimum Git, Nix, and OpenSSH versions remain `needs verification`.

The RFC normatively requires the capabilities above. Version floors must be set only after conformance testing confirms that the chosen versions satisfy those capabilities in practice.

### 7.3 Foundational implementation choices

- **Implementation language:** intentionally deferred in this RFC
- **SSH ingress:** OpenSSH
- **Git server primitives:** system Git
- **HTTP Git support:** `git-http-backend`
- **Metadata and push journal:** SQLite
- **Object storage:** filesystem bare repositories

The foundational decision is to use:

- system Git for Git correctness,
- OpenSSH for SSH ingress,
- and SQLite plus filesystem storage for local durability.

Go and Rust are both viable implementation options for the control plane. Language selection should follow the stabilization of protocol, migration, and durability requirements rather than precede them.

### 7.4 Why not a pure Git reimplementation

The product’s hardest requirement is correctness at the Git boundary. The safest design is to let Git itself handle:

- upload-pack,
- receive-pack,
- smart HTTP,
- hooks,
- pack negotiation,
- reference updates,
- and repository maintenance.

### 7.5 Why not an in-process SSH server in the foundation

An in-process SSH server is not required to prove the architecture.

OpenSSH already provides:

- mature key handling,
- forced-command routing,
- stable operator expectations,
- and a smaller implementation burden at the trust boundary.

Custom SSH handling may be revisited later if the product needs protocol features that OpenSSH-based command routing cannot provide cleanly.

### 7.6 Why not include tarball compatibility in the foundational architecture

Given the accepted product constraint that Git Relay may migrate `flake.nix` and `flake.lock`, a mandatory tarball plane is no longer the simplest path to product success.

Adding archive compatibility now would:

- add a second data plane,
- add a second cache domain,
- reintroduce compatibility questions around archive semantics,
- and distract the foundational RFC from the Git boundary that actually needs to be proven first.

Tarball compatibility may be revisited later if migrated direct inputs plus explicit transitive overrides are insufficient in practice.

## 8. Repository Model

Git Relay supports two repository modes and a separate operational state machine.

### 8.1 Cache-only repository

Used for repositories that are read through the relay but not written through it.

Properties:

- local bare mirror,
- upstream is source of truth,
- relay may refresh on demand,
- cache eviction is allowed by policy,
- and stale serving is allowed only when freshness policy permits it.

### 8.2 Authoritative repository

Used for repositories that accept client pushes through the relay.

Properties:

- local bare repository is canonical for relay clients,
- local refs are the client-visible source of truth,
- upstreams are replication targets rather than read authority,
- acknowledged pushes are durably journaled before success is returned,
- and replication proceeds after local acceptance.

### 8.3 Repository operational states

Each repository has an operational state independent of its mode:

- `provisioning`
- `ready`
- `refreshing`
- `degraded`
- `divergent`
- `quarantined`
- `disabled`

`degraded` means the repository is still usable, but some background obligation such as replication has failed.

`divergent` means authoritative local refs and tracked upstream refs no longer agree under a configuration that expects relay authority.

`quarantined` means the relay detected a correctness risk and blocks normal write operation until an explicit repair action completes.

### 8.4 Authoritative repository invariants

For authoritative repositories, the following invariants are mandatory:

- client-visible refs live in the normal bare-repository namespace,
- upstream state is tracked under a separate internal namespace such as `refs/git-relay/upstreams/<upstream>/...`,
- local acceptance does not imply upstream success,
- direct upstream pushes are unsupported by default,
- and new writes are blocked while the repository is `divergent` or `quarantined`.

“Local canonical, upstream replicated later” therefore means:

- relay clients read accepted refs locally,
- upstreams are asynchronous replicas,
- and divergence with upstreams is a repair condition, not an alternate authority model.

## 9. Identity Model

Git Relay separates at least three identities.

### 9.1 Repository identity

A canonical logical repository identity such as:

- `github.com/org/repo.git`
- `gitlab.com/group/repo.git`
- `git.example.com/team/repo.git`

Repository identity rules:

- strip transport scheme,
- strip userinfo such as `git@`,
- lowercase the host component,
- preserve path case unless a provider-specific rule explicitly says otherwise,
- include a non-default port if present,
- normalize optional `.git` suffix according to policy,
- and treat host aliases, repository moves, and provider migrations as explicit alias mappings rather than implicit normalization.

### 9.2 Source-tree identity

A source-tree identity consists of:

- repository identity,
- resolved object identity such as commit SHA,
- and optional subtree selection when relevant.

This identity is what read caching and Nix migration ultimately care about.

### 9.3 Upstream auth identity

Different ingress paths may refer to the same repository while using different authentication mechanisms or credentials. Auth identity remains separate from repository identity.

Examples:

- client SSH identity used to push to the relay,
- relay-owned SSH key used to replicate upstream,
- relay-owned HTTPS token used to refresh reads from upstream.

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

### 10.2 Upstream transport selection policy

Git Relay supports multiple upstream transport options, but policy must choose concrete URLs for actual operations.

Per repository, policy may define:

- ordered read upstream URLs,
- ordered write upstream URLs,
- distinct auth profiles per upstream URL,
- and whether an upstream requires atomic multi-ref replay.

The relay may try several configured upstream URLs across retries. Each individual refresh or replication attempt still uses one concrete URL and one concrete credential set.

### 10.3 Repository migration: direct flake input rewriting

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

There is no single mandatory migration target for every repository.

The migration target is policy-driven per host, repository class, or direct input:

- public direct inputs may prefer `git+https://`,
- private direct inputs may prefer `git+ssh://`,
- and deployments may support both simultaneously.

### 10.4 Transitive shorthand behavior

MVP guarantees relay coverage for:

- Git operations,
- and direct flake inputs that have been migrated to Git URLs.

MVP does not guarantee full coverage for transitive third-party shorthand inputs embedded in downstream flakes. Those cases may be reduced with:

- `follows`,
- direct overrides,
- or explicit project policy.

They are not solved by the foundational architecture.

## 11. Read Path

### 11.1 Read semantics

Git Relay distinguishes immutable object availability from ref freshness.

- If a client requests an object already present locally by object ID, the relay may serve it directly.
- If a client needs current ref advertisement such as branch heads or tags, the relay must apply repository freshness policy before advertising state.
- For authoritative repositories, locally accepted refs are immediately readable from the relay even if replication is still pending.

### 11.2 Freshness policy

Freshness policy must be explicit per repository class. Supported policy classes should include:

- `authoritative-local`
- `ttl:<duration>`
- `always-refresh`
- `manual-only`
- `stale-if-error`

`stale-if-error` is appropriate only where the repository is not authoritative for writes and where serving stale refs is better than failing.

The relay must not advertise ref state blindly without a defined freshness rule.

### 11.3 Git clone and fetch flow

1. Client connects to the relay over SSH or optional smart HTTP.
2. The relay parses the requested service and repository identity.
3. The relay resolves repository mode, operational state, freshness policy, and upstream policy.
4. The relay checks local repository state.
5. If required objects are present and freshness policy allows, serve locally.
6. If required objects are missing or freshness policy requires refresh, perform a singleflight upstream refresh when policy allows.
7. Persist new objects locally.
8. Serve the request using native `git-upload-pack` or `git-http-backend`.

Short negative caching may be used for repeated misses such as nonexistent repositories or nonexistent refs, but negative cache entries must expire quickly and must not outlive explicit repair or provisioning actions.

### 11.4 Migrated Nix input fetch path

1. Repository migration rewrites direct flake inputs to Git URLs.
2. Nix resolves those inputs through Git transport.
3. Git URL rewriting routes the traffic through the relay when local policy applies.
4. The relay serves the fetch through the same Git read path described above.
5. Locked metadata is derived from the Git fetch path and the post-migration lock state, not from the previous shorthand tarball semantics.

## 12. Write Path

### 12.1 Acceptance model

The recommended default is:

- accept locally first,
- acknowledge only after replication work is durable,
- then replicate asynchronously.

### 12.2 Client contract versus upstream replication contract

When a client push succeeds against an authoritative repository, Git Relay promises:

- local refs were accepted under Git’s normal receive rules,
- the accepted ref updates are durable and recoverable after crash or restart,
- replication work for each configured upstream was durably recorded,
- and relay clients can read the accepted refs locally.

Git Relay does **not** promise on client acknowledgement that:

- every upstream already contains the accepted refs,
- the original client’s transport or credentials were preserved upstream,
- upstream hooks observed the original client identity,
- or upstream push-certificate semantics were preserved under relay-owned replication credentials.

### 12.3 Push journal state machine

Every accepted push attempt is tracked as a durable state machine:

- `prepared`
- `aborted`
- `locally_committed`
- `ack_ready`
- `replicating`
- `replicated`
- `degraded`
- `quarantined`

State meaning:

- `prepared`: validation succeeded and the proposed ref updates are durably recorded before ref commit.
- `aborted`: the prepared transaction did not commit local refs.
- `locally_committed`: local refs were committed.
- `ack_ready`: replication jobs are durable and the push may be acknowledged.
- `replicating`: one or more upstream jobs are in flight.
- `replicated`: all required upstream jobs succeeded.
- `degraded`: local acceptance succeeded, but one or more background obligations failed.
- `quarantined`: local state may require reconciliation before further writes are safe.

### 12.4 Push acceptance flow

The write path for authoritative repositories is:

1. Client pushes to the relay.
2. The relay invokes native `git-receive-pack` against the local authoritative bare repository under a wrapper process that remains responsible for final success or failure.
3. Incoming objects land in Git receive quarantine.
4. `pre-receive` validates ACLs, protected refs, fast-forward rules, and write policy.
5. During the same pre-commit phase, the relay durably records a push-journal row and the full set of proposed ref updates with state `prepared`.
6. If validation fails, the push aborts and no local refs update.
7. If validation succeeds, Git commits the local ref transaction.
8. `reference-transaction` marks the push `locally_committed` when the ref transaction commits, or `aborted` if the transaction aborts.
9. After `git-receive-pack` completes successfully, the wrapper durably creates per-upstream replication jobs and marks the push `ack_ready`.
10. Only after `ack_ready` is durable may the wrapper return overall success to the client.
11. Background workers replay the accepted ref updates to configured upstreams.

The protocol-level implementation detail of how the wrapper gates final client success must be conformance-tested. The RFC requirement is normative: a push is not acknowledged until `ack_ready` is durable.

### 12.5 Upstream replication semantics

Replication is defined per repository and per upstream:

- one replication job per accepted push and upstream,
- in-order replay per repository and upstream,
- exact replay of the accepted ref update set,
- and explicit handling of multi-ref atomicity requirements.

Each upstream configuration must declare whether multi-ref atomic replay is required.

- If `require_atomic = true`, the relay must use upstream atomic push when supported and must treat lack of atomic support as a failed replication obligation.
- If `require_atomic = false`, the relay may attempt best-effort replay, but any partial upstream application is a degraded state that requires repair tooling.

This is necessary because “one client push to N upstream servers” is not one semantic contract. It is N independent remote contracts behind one local acknowledgement boundary.

### 12.6 Acknowledgement policy

The MVP acknowledgement policy is **durable-local**:

- success is returned after local acceptance and durable journaling of replication work.

Future policies may include:

- all-upstreams-must-succeed,
- selected-upstreams-must-succeed,
- or branch-specific acknowledgement profiles.

### 12.7 Durability requirements

The foundational contract is:

- an **acknowledged** push must remain recoverable after relay restart or crash without requiring client retransmission,
- an **unacknowledged** partial accept must be detectable and quarantined for reconciliation,
- and local acceptance must never be silently rewritten to match a failed upstream outcome.

## 13. Policy Enforcement

The relay relies on Git-native enforcement points.

### 13.1 Hooks and transactions

Use:

- `pre-receive` for whole-push validation and pre-commit journal preparation,
- `update` for per-ref validation,
- `reference-transaction` for commit or abort transition recording,
- `post-receive` for non-critical notifications and worker wakeups.

The durability contract must not depend on best-effort `post-receive` behavior.

### 13.2 Default repository protections

Recommended defaults:

- deny deletes unless explicitly allowed,
- deny non-fast-forward updates unless explicitly allowed,
- enable object verification on receive,
- hide internal tracking refs from clients,
- restrict access to Git-only server-side commands,
- and expose clear audit logs for ref updates.

### 13.3 Authoritative divergence policy

For authoritative repositories, direct upstream pushes should be treated as unsupported by default unless the repository is explicitly configured for shared-authority operation.

Divergence detection must compare:

- local authoritative refs,
- tracked upstream refs under the relay’s internal namespace,
- and the configured authority model for that repository.

If unsupported divergence is detected, the repository enters `divergent` state and blocks new writes until repaired intentionally.

## 14. Authentication and Authorization

### 14.1 Client to relay

Use normal user-facing Git authentication.

- SSH keys for SSH traffic,
- optionally SSH certificates later,
- and standard HTTP auth mechanisms only if HTTP ingress is enabled.

If HTTP ingress is enabled, the web server or reverse proxy is responsible for authenticating the user before `git-http-backend` is invoked.

### 14.2 Relay to upstream

Default to relay-owned machine credentials for read refreshes and background replication.

Reasons:

- background replication must work after the client disconnects,
- retries and reconciliation need stable credentials,
- and the relay must not depend on client credential presence after acceptance.

Each upstream URL uses an explicit auth profile. That profile may be:

- an SSH key,
- an HTTPS token,
- or another transport-specific machine credential supported by policy.

### 14.3 Attribution and audit

Even when relay-owned machine credentials are used upstream, the relay must record:

- authenticated client identity,
- repository identity,
- accepted ref changes,
- replication targets,
- upstream credential profile used,
- and replication outcomes.

Per-user upstream delegation may be added later, but it is not the MVP default.

### 14.4 Credential handling

Credentials are runtime secrets, not declarative source inputs.

- secrets must not be stored in the Nix store,
- secrets must be isolated by auth profile and repository scope,
- and operator access to credential material must be auditable.

## 15. Nix Migration Model

### 15.1 Why this exists

Direct shorthand inputs that resolve to archive downloads cannot be intercepted through Git URL rewriting because they are not Git traffic.

Given that project-owned source and lock files may be updated, the simplest solution is to migrate those direct inputs to Git transports explicitly.

### 15.2 Supported rewrite scope

The MVP migration tool is intentionally bounded.

It should support direct flake inputs that are expressed as literal URL strings in forms such as:

- `inputs.<name>.url = "github:owner/repo";`
- `inputs.<name>.url = "github:owner/repo/ref";`
- `inputs.<name>.url = "gitlab:group/project";`
- `inputs.<name>.url = "sourcehut:~user/project";`

Host-specific query parameters must be preserved where representable in the target Git URL.

Unsupported forms must fail closed, including:

- dynamically constructed URLs,
- non-literal expressions that require evaluation to understand,
- or shorthand forms the tool cannot map unambiguously.

### 15.3 Supported migration targets

The migration tool must support:

- `git+https://` targets,
- `git+ssh://` targets,
- policy selection by host, repository class, or direct input name,
- and preservation of explicit branch or ref intent where representable.

There is no single universal migration target. The tool chooses one concrete target transport per rewritten direct input.

### 15.4 Migration contract

Migration is an explicit command, not an implicit side effect.

The migration workflow should:

1. inspect direct flake inputs,
2. identify shorthand inputs covered by migration policy,
3. produce a rewrite plan before mutation,
4. rewrite direct inputs to concrete Git URLs,
5. re-lock the affected inputs,
6. show a reviewable diff,
7. report any remaining transitive shorthand nodes visible in the resulting lock graph,
8. and refuse unsafe mutation by default when the repository is dirty.

The migration workflow must not assume that `narHash` or lock metadata remains stable across transport change.

The preferred relock behavior is targeted relocking of rewritten direct inputs. If the tool cannot isolate the lock update safely for the current Nix version or dependency graph, it must fail and require an explicit broader relock command rather than silently widening the update scope.

### 15.5 Direct versus transitive coverage

The migration model guarantees coverage only for direct inputs owned by the repository being migrated.

Transitive shorthand inputs may still bypass the relay unless:

- the adopting repository overrides them directly,
- or dependency relationships are tightened using mechanisms such as `follows`.

That remaining gap is accepted in MVP and must be reported clearly when detected.

### 15.6 CI and portability implications

Rewritten inputs remain standard Git URLs.

- `git+https://` is portable where HTTPS access is available,
- `git+ssh://` is portable where SSH access is available,
- and neither form should contain relay-specific hostnames in committed source.

## 16. Storage Model

### 16.1 Git storage

Git Relay stores one bare repository per logical repository.

Advantages:

- simple operator model,
- native Git maintenance,
- clear isolation,
- straightforward recovery,
- and direct reuse of Git’s own object and ref machinery.

For authoritative repositories, upstream-tracking refs must live in a separate internal namespace such as `refs/git-relay/upstreams/<upstream>/...`.

### 16.2 Metadata and push journal

SQLite stores metadata and durable work state, not Git objects.

Tables should cover:

- repository identity and alias mappings,
- repository mode and operational state,
- upstream definitions and auth-profile bindings,
- refresh policy,
- push journal entries,
- push ref-update rows,
- replication jobs,
- replication attempts and outcomes,
- and audit events.

SQLite must be configured for local durability:

- `journal_mode = WAL`
- `synchronous = FULL`
- foreign keys enabled
- and bounded transaction scope in the write path

### 16.3 Garbage collection and maintenance

The relay needs explicit policies for:

- Git maintenance scheduling,
- retention and pinning,
- cache eviction for cache-only repositories,
- reflog retention for authoritative repositories,
- and failed replication cleanup.

Cache eviction must never apply to authoritative repositories while they are configured as write-accepting.

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

Structured logs must carry stable identifiers such as:

- `request_id`
- `repo_id`
- `push_id`
- `upstream_id`
- `attempt_id`
- and authenticated client identity

A minimal operator interface should include:

- `doctor`
- `repo add`
- `repo inspect`
- `repo repair`
- `replication status`
- `replication retry`
- `cache pin`
- `cache evict`
- `migrate-flake-inputs`
- and `migration inspect`

## 18. Security Considerations

1. The relay becomes a high-value trust boundary.
2. Client-facing SSH access must be restricted to Git-only commands through OpenSSH forced-command routing.
3. Upstream machine credentials must be isolated and auditable.
4. Project migration commands must be explicit and reviewable.
5. Private repositories and public repositories should be segregated logically in policy and credential scope.
6. Object verification and ref protection should be on by default.
7. Operator actions should be audited.
8. Repository migration is a privileged operation over project source, not a transparent network convenience.
9. If HTTP ingress is enabled, authentication and authorization boundaries between the web tier and `git-http-backend` must be explicit.
10. Secrets must not be materialized in the Nix store.

## 19. Failure Modes and Recovery

### 19.1 Upstream fetch failure

If a repository is already cached and the requested objects are present, the relay may serve from cache if freshness policy allows it.

If required objects are absent, return a normal fetch failure.

### 19.2 Upstream replication failure

Do not roll back a locally accepted push.

Instead:

- queue retries,
- preserve the accepted local refs,
- mark the repository `degraded`,
- and expose reconciliation tooling.

### 19.3 Crash or partial accept before acknowledgement

If refs were updated locally but the push was not acknowledged durably, the relay must detect that condition on restart and quarantine the repository for reconciliation.

The system must not silently treat an unacknowledged partial accept as a clean success.

Startup reconciliation must inspect push-journal rows and handle at least:

- `prepared` without local commit,
- `locally_committed` without `ack_ready`,
- `ack_ready` without completed replication,
- and in-flight replication attempts interrupted by crash.

### 19.4 Repository migration failure

If flake input migration fails partway through:

- leave a clear diagnostic trail,
- avoid silent lockfile corruption,
- and provide a straightforward rollback path through normal version control.

### 19.5 Divergence

Authoritative repositories must include divergence detection and explicit repair commands.

### 19.6 Metadata-store corruption or unavailability

If the metadata database cannot provide the durability guarantees required for the write path, authoritative write acceptance must fail closed.

## 20. Configuration Model

Git Relay has:

- one static daemon configuration file,
- one SQLite-backed metadata database for mutable repository state,
- and a CLI for repo, policy, repair, and migration management.

Static configuration covers:

- listen addresses,
- filesystem paths,
- database durability settings,
- default policy classes,
- and operator-safe feature toggles such as HTTP read enablement.

Mutable repository configuration in SQLite covers:

- repository mode,
- upstream URLs,
- auth profile bindings,
- per-upstream atomicity requirements,
- and current operational state.

Example static configuration:

```toml
[listen]
ssh = "127.0.0.1:4222"
https = "127.0.0.1:4318"
enable_http_read = false
enable_http_write = false

[paths]
state_root = "/var/lib/git-relay"
repo_root = "/var/lib/git-relay/repos"
db_path = "/var/lib/git-relay/metadata.sqlite3"

[database]
journal_mode = "wal"
synchronous = "full"
busy_timeout_ms = 5000

[replication]
default_push_ack = "durable-local"
retry_backoff = "exponential"

[policy]
default_repo_mode = "cache-only"
default_refresh = "ttl:60s"
negative_cache_ttl = "5s"

[migration]
supported_targets = ["git+https", "git+ssh"]
refuse_dirty_worktree = true
prefer_targeted_relock = true
```

Example repository rules:

```toml
[[rule]]
match = "github.com/my-org/**"
mode = "authoritative"
refresh = "authoritative-local"
push_ack = "durable-local"
migration_transport = "git+ssh"

read_upstreams = [
  { name = "github-https", url = "https://github.com/%repo%", auth_profile = "github-read-https" },
  { name = "github-ssh", url = "ssh://git@github.com/%repo%", auth_profile = "github-read-ssh" },
]

write_upstreams = [
  { name = "github-primary", url = "ssh://git@github.com/%repo%", auth_profile = "github-write-ssh", require_atomic = true },
  { name = "backup-mirror", url = "https://git.example.com/%repo%.git", auth_profile = "backup-write-token", require_atomic = false },
]

[[rule]]
match = "github.com/**"
mode = "cache-only"
refresh = "ttl:60s"
migration_transport = "git+https"

read_upstreams = [
  { name = "github-https", url = "https://github.com/%repo%", auth_profile = "github-read-https" },
  { name = "github-ssh", url = "ssh://git@github.com/%repo%", auth_profile = "github-read-ssh" },
]
```

## 21. MVP Scope

### Included

- Git over SSH
- optional smart HTTP read support
- Git URL rewriting bootstrap helpers
- cache-only repositories
- authoritative repositories
- local durable push journaling
- asynchronous replication to multiple upstreams
- per-upstream atomic replay policy
- repository metadata in SQLite
- Nix direct-input migration command
- basic metrics, logs, and repair commands

### Excluded

- tarball compatibility plane
- smart HTTP push
- Git LFS
- distributed cluster mode
- advanced multi-tenant auth
- generic full-traffic MITM as default behavior
- guaranteed coverage for all transitive shorthand-based Nix fetches
- full enterprise attribution of upstream writes
- preservation of original client upstream identity or push-certificate semantics

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

### 22.4 In-process SSH server as the foundational ingress

**Rejected for the foundational architecture.**

Why:

- not required to prove the Git boundary,
- expands the initial trust boundary,
- and replaces mature OpenSSH controls before there is evidence that doing so is necessary.

## 23. Rollout Plan

### Phase 1

- SSH ingress
- cache-only Git repositories
- explicit relay URLs
- repository identity and policy storage
- freshness policy and singleflight refresh

### Phase 2

- Git URL rewrite bootstrap
- Nix direct-input migration tooling
- lockfile relock workflow
- unresolved transitive shorthand reporting

### Phase 3

- authoritative local accept for selected repositories
- push journal state machine
- crash recovery and reconciliation tooling
- replication queue and multi-upstream replay

### Phase 4

- optional smart HTTP read ingress
- refined cache policies
- stronger operator tooling and metrics

### Phase 5

- revisit tarball compatibility only if validation shows that transitive shorthand gaps materially block adoption

## 24. Open Questions

1. Which exact Git version floor satisfies the required hook, protocol, and atomicity-detection behavior across the supported platforms? `needs verification`
2. Which exact Nix version floor provides the required flake Git URL and targeted relock behavior across the supported platforms? `needs verification`
3. Should smart HTTP push ever enter scope, or should authoritative writes remain SSH-only by design?
4. How much automation should the migration tool provide for transitive remediation suggestions such as `follows`?
5. What retention defaults keep cache growth practical without undermining offline expectations?
6. Should shared-authority operation ever be supported, or should authoritative repositories remain relay-authority-only?

## 25. Recommendation

Adopt the following product direction:

> **Git Relay should be built as a Git-first cache and replication edge, with explicit Nix direct-input migration and no tarball compatibility plane in the foundational architecture or MVP.**

The relay’s core contract is:

- normal Git read and write semantics at the relay boundary,
- durable-local acknowledgement for authoritative pushes,
- asynchronous replication to one or more upstream Git servers,
- and explicit, reviewable migration of direct Nix flake inputs to concrete Git transports.

This keeps the protocol boundary coherent, makes bootstrap and repository mutation explicit, allows direct Nix inputs to route through the relay using normal Git mechanisms, and keeps the MVP focused on the part of the system that must be correct first: the Git read/write boundary.
