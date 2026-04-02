# Architectural Review: Two Mechanisms vs. Unified Interception

**Status:** Review  
**Date:** 2026-04-02  
**Reviewing:** `git-relay-rfc.md`  
**Reviewer:** Claude (Opus 4.6), prompted by project author

## 1. Context

The Git Relay RFC proposes two data planes because it observes two wire protocols:

| Consumer | Wire protocol | Proposed interception |
|---|---|---|
| Git client (clone/fetch/push) | Git pack protocol over SSH/HTTP | `url.*.insteadOf` (native) |
| Nix `github:`/`gitlab:` fetchers | HTTPS tarball download | Separate HTTP compatibility service |

This review examines whether a single interception mechanism could replace the two-plane design, and whether the two-plane design is the right call.

### 1.1 Key constraint: workstation-only tool

Git Relay is strictly a workstation tool. CI environments (GitHub Actions, etc.) must work normally from the same repository without any relay-specific configuration. This means:

- **No source-level changes.** `flake.nix`, `*.nix` files, and lock files must remain unchanged. The relay cannot require relay-specific URLs or inputs in committed code.
- **Interception is environment-level only.** Git config (`insteadOf`), Nix registries, and environment variables are acceptable. Anything committed to the repo is not.
- **The same `flake.lock` must work on both workstation (through relay) and CI (direct to forge).**

This constraint shapes every architecture evaluated below and definitively rules out approaches that require modifying project files.

### 1.2 Candidate architectures

Three candidate architectures are evaluated:

- **Architecture A:** Two mechanisms (the RFC's proposal)
- **Architecture B:** Unified HTTP(S)/SSH transparent proxy
- **Architecture C:** Eliminate the tarball plane entirely by making Nix use Git transport

## 2. Architecture A: Two Mechanisms (the RFC's Proposal)

**Git plane:** `insteadOf` rewrites to relay SSH/HTTP, local bare repos, upstream fetch-on-miss.  
**Tarball plane:** separate HTTP service, archive cache, upstream forge tarballs.

### 2.1 Strengths

1. **Git `insteadOf` is the cleanest interception mechanism that exists.** It is native, zero-overhead, does not touch TLS, and has been stable for over 15 years. It is hard to improve on.

2. **Small trust boundary per plane.** The Git plane never terminates or inspects TLS for non-Git traffic. The tarball plane is a scoped HTTP cache, not a general proxy.

3. **Each plane can fail independently.** A tarball cache bug does not break Git pushes. A Git receive-pack regression does not break Nix evaluations.

### 2.2 Weaknesses (as originally proposed)

1. **Two cache stores with weak coherence.** A `git clone` and a Nix `github:` fetch of the same repo at the same rev populate two completely separate caches. The RFC acknowledges this with "optionally hydrate the Git cache in the background" (Section 11.2, step 6), but this is a band-aid. Cross-plane cache consistency becomes an ongoing operational concern.

2. **Two identity resolution paths.** Both planes need to resolve `github.com/org/repo` to a logical identity, but they receive it in different forms (SSH command argument vs. HTTP path). That is duplicated logic with subtle divergence risk.

3. **Two bootstrap surfaces.** Users configure `insteadOf` for Git, and then separately configure something (proxy env vars? DNS? explicit URLs? Nix registry?) for tarballs. The RFC is vague on tarball bootstrap (Section 10.2), which is a signal that it is architecturally awkward.

4. **The "hydration" bridge is a design smell.** When Architecture A needs an explicit mechanism to propagate state from the tarball plane into the Git plane, that is evidence the two planes are artificially separated.

### 2.3 Addressing the weaknesses: unified storage model

The weaknesses in Section 2.2 are not inherent to having two interception planes. They arise from having two **data models**. The fix is to keep two protocol frontends but unify the storage layer behind them.

```
┌──────────────┐     ┌──────────────┐
│  Git plane   │     │ Tarball plane│
│  (insteadOf) │     │ (Nix registry│
│              │     │  redirect)   │
└──────┬───────┘     └──────┬───────┘
       │                    │
       ▼                    ▼
┌──────────────────────────────────┐
│       Unified Relay Store        │
│  ┌────────────┐ ┌─────────────┐  │
│  │ bare Git   │ │ archive     │  │
│  │ objects    │ │ cache       │  │
│  └─────┬──────┘ └──────┬──────┘  │
│        │               │         │
│  ┌─────▼───────────────▼──────┐  │
│  │   Identity + mapping index │  │
│  │  (forge, owner, repo, rev) │  │
│  └────────────────────────────┘  │
└──────────────────────────────────┘
```

The design principles:

1. **One identity index.** A single component maps `(forge, owner, repo, rev)` to both Git objects and cached archives. Both planes read from and write to the same index. No duplicated identity resolution logic.

2. **Cross-plane warming is automatic, not optional.** When the tarball plane caches an archive for `(github.com, NixOS, nixpkgs, abc123)`, it records the mapping in the shared index. The Git plane can see that rev `abc123` is known. When the Git plane fetches a repo, it updates the shared index with all available refs, so the tarball plane knows which revs are locally resolvable.

3. **The archive cache stores real upstream tarballs.** For `narHash` correctness (see Section 6.5), the relay must serve the actual upstream tarball, not a `git archive` derivative. The archive cache is a content-addressed blob store indexed by the shared identity layer. It is physically separate from the Git object store but logically part of the same unified model.

4. **Hydration becomes index maintenance, not a separate mechanism.** There is no "hydration bridge" because both planes contribute to the same index. The cross-plane awareness that the original RFC treated as optional becomes an inherent property of the storage layer.

This resolves weaknesses 1, 2, and 4 while keeping the two-plane architecture's strengths (independent failure domains, no TLS interception, native Git `insteadOf`).

## 3. Architecture B: Unified HTTP(S)/SSH Interception (Transparent Proxy)

**Idea:** One proxy endpoint intercepts all outbound connections to known forge hosts, inspects the request type, and routes accordingly:

- Git smart HTTP requests go to relay Git cache.
- Git SSH requests go to relay Git cache (via SSH proxy or `insteadOf`).
- Tarball requests go to relay archive cache (or are derived from Git cache).
- Everything else is passed through.

### 3.1 What This Actually Requires

For HTTPS tarballs (the Nix case), the relay must terminate TLS to inspect the request path. This means:

1. A relay-owned CA certificate.
2. That CA trusted by the Nix daemon and any other tarball consumer.
3. The relay acting as a CONNECT proxy with selective MITM for configured hosts.

For SSH, `insteadOf` or an SSH proxy is still needed. SSH MITM is possible but operationally hostile because host key verification breaks. Realistically, **even the "unified" approach keeps `insteadOf` for SSH**. It cannot be escaped.

### 3.2 Strengths

1. **Single cache store is theoretically possible.** If both Git HTTPS and tarball HTTPS are intercepted, the underlying Git object store could be shared, with tarballs derived from it.

2. **Single bootstrap for HTTP traffic.** Set `https_proxy` or configure DNS once, and both Git-over-HTTPS and tarball fetches are covered.

### 3.3 Weaknesses

1. **TLS MITM is the wrong default for a developer tool.** The RFC is right to reject this (Section 22.2). Asking users to install a custom CA and trust a local process to intercept HTTPS connections to GitHub creates a large security surface, a larger debugging surface, and opaque failure modes. Certificate pinning in future clients could break it silently.

2. **SSH still requires `insteadOf`.** Git-over-SSH is the dominant developer transport. It cannot be transparently proxied without either `insteadOf` (which is Architecture A's mechanism) or SSH MITM (which breaks host key verification). The approach has not actually unified; it has added TLS interception on top of `insteadOf` and called it one mechanism.

3. **Tarballs cannot be reliably derived from Git state.** This is the critical correctness constraint. GitHub's `/archive/<rev>.tar.gz` and `git archive` do not produce byte-identical output. Nix's `narHash` in existing lock files is computed from the unpacked GitHub tarball. The NAR serialization is deterministic over file content, but:
   - GitHub may apply `.gitattributes` `export-ignore` differently than `git archive`.
   - GitHub strips the `.git` directory; `git archive` does too, but the prefix path differs.
   - Submodule handling differs.
   - Some forges normalize line endings or apply other transforms.

   If a tarball is derived from Git state instead of fetching the real upstream tarball, it will produce a **different `narHash`** for some repositories, breaking existing lock files. Even with unified interception, real upstream tarballs must still be fetched and cached separately for correctness.

4. **`https_proxy` is not scoped the way it needs to be.** Setting it affects all HTTPS traffic from that process, not just forge hosts. Using `no_proxy` for everything except target hosts is fragile and inverted from the desired default. DNS-level interception requires controlling `/etc/hosts` or a local DNS resolver, which adds more moving parts, not fewer.

5. **Operational complexity is higher, not lower.** Debugging "why did my `nix build` fail" now requires understanding the proxy's TLS termination, certificate chain, connection routing, and caching behavior. With Architecture A, the tarball plane is just an HTTP cache, trivial to debug with `curl`.

### 3.4 Verdict on Architecture B

Architecture B does not actually eliminate the two-plane problem. It hides it behind a proxy and adds TLS interception complexity while still requiring two cache stores for `narHash` correctness. The result is **two cache stores plus MITM infrastructure**, which is strictly worse than Architecture A.

## 4. Architecture C: Eliminate the Tarball Plane Entirely

**Idea:** Instead of intercepting or proxying tarball fetches, configure Nix to use Git transport for everything. Then `insteadOf` covers 100% of traffic and only one plane is needed.

### 4.1 How It Would Work

Nix supports `git+ssh://` and `git+https://` flake inputs directly. Instead of:

```nix
inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
```

Use:

```nix
inputs.nixpkgs.url = "git+ssh://git@github.com/NixOS/nixpkgs?ref=nixos-unstable";
```

With `insteadOf`, this becomes transparent:

```
# Nix sees git+ssh://git@github.com/...
# Git rewrites to ssh://git@relay:4222/ssh/github.com/...
# Relay serves from cache or fetches upstream
```

One mechanism. One cache. One bootstrap. No tarballs.

### 4.2 Why This Is Tempting

- Truly unified: everything is Git everywhere.
- No archive compatibility module needed at all.
- No `narHash` mismatch risk from tarball derivation.
- Cache coherence is trivial because there is only one cache.
- The relay is genuinely just a Git relay.

### 4.3 Why This Does Not Fully Work

1. **Existing lock files break.** A `flake.lock` that has `type: "github"` with a `narHash` computed from the GitHub tarball will fail verification when switched to `type: "git"`. The `narHash` will be different because Git checkout and tarball extraction produce different source trees. Every downstream project must re-lock.

2. **The `github:` shorthand is the dominant Nix convention.** Telling users not to use `github:` in favor of `git+ssh://` is a non-starter for adoption. It fights the ecosystem.

3. **Nix's Git fetcher is slower for large repos.** The `github:` tarball fetcher downloads a single archive. The Git fetcher must negotiate pack protocol, which for large repos like nixpkgs is significantly slower, even with shallow clones.

4. **Some Nix features depend on tarball semantics.** `narHash` stability across evaluation environments is important for Nix's reproducibility guarantees. Changing the fetcher type changes the hash, which ripples through the entire dependency graph.

5. **Third-party flakes use `github:`.** Even if a user's own flakes use `git+ssh://`, their transitive dependencies use `github:`, and those lock files cannot be rewritten.

### 4.4 Verdict on Architecture C

It is the purist's answer but it fails on two fronts:

1. **Incompatible with the workstation-only constraint (Section 1.1).** Rewriting `flake.nix` inputs from `github:` to `git+ssh://` is a source-level change. The same `flake.nix` must work unchanged on CI (no relay) and on a workstation (with relay). Architecture C requires modifying committed project files, which is ruled out.

2. **Incompatible with the existing Nix ecosystem.** It cannot be adopted incrementally and cannot be adopted for transitive dependencies whose `flake.nix` files are not under the user's control.

It would work for a greenfield, Nix-free, pure-Git environment, but then the tarball plane is unnecessary anyway.

## 5. The Core Architectural Insight

The two-mechanism approach is not a weakness. It is a reflection of a **genuine protocol boundary** in the ecosystem. Git traffic and tarball traffic are different protocols with different identity models, different integrity properties, and different client expectations. No amount of architectural cleverness makes `narHash` and Git SHA the same thing.

The real question is not "can we unify the mechanism?" but rather "where should the complexity of bridging two identity models live?"

| Approach | Where the bridge complexity lives |
|---|---|
| A (RFC, as-is) | Explicit but optional: hydration step + per-plane identity resolution |
| A (refined) | Implicit and structural: unified storage model with shared identity index |
| B (MITM proxy) | Implicit in proxy routing, but still needs two cache stores for correctness |
| C (Git-only) | Pushed onto users: re-lock everything, change conventions, modify source files |

Architecture B does not eliminate the two-plane problem. It adds TLS interception complexity while still requiring two cache stores for `narHash` correctness. Architecture C is clean but violates the workstation-only constraint (Section 1.1) and is incompatible with the existing Nix ecosystem.

**Architecture A with a unified storage model is the right call.** Two protocol frontends, one data model. The interception planes stay separate (independent failure, no TLS MITM, native Git mechanisms), but the storage layer is shared so that cross-plane coherence is structural rather than bolted on.

## 6. Recommendations for the RFC

### 6.1 Adopt the unified storage model (Section 2.3)

Replace the RFC's implicit two-store design with an explicit unified storage layer:

- **One identity index** mapping `(forge, owner, repo, rev)` to both Git refs and cached archives.
- **Both planes read from and write to the same index.** A tarball fetch updates the index; a Git fetch updates the index. Cross-plane awareness is structural, not optional.
- **The archive cache is physically separate from Git objects but logically part of the same model.** Real upstream tarballs are stored for `narHash` correctness (see Section 6.5), but they are indexed by the same identity layer.
- **Drop "optionally hydrate" language.** Cross-plane warming is a natural consequence of the shared index, not an optional background job.

This is the single most impactful change. It resolves the weak coherence, duplicated identity resolution, and hydration bridge issues (Section 2.2) without adding any interception complexity.

### 6.2 Use Nix flake registries as the primary tarball bootstrap mechanism

Section 10.2 is the weakest part of the RFC. The workstation-only constraint (Section 1.1) narrows the options significantly: the bootstrap mechanism must be environment-level configuration, not source-level. Nix flake registries are the cleanest fit.

A relay-provided registry that maps `github:org/repo` to `tarball+http://relay:4320/github/org/repo/archive/<rev>.tar.gz` would:

- Require one configuration step: add the registry to `~/.config/nix/registry.json`.
- Not require TLS interception.
- Not require proxy configuration or DNS overrides.
- Work with existing `github:` shorthand syntax in `flake.nix`.
- Keep lock file compatibility (the resolved URL is still a tarball, `narHash` is preserved).
- **Be invisible to CI.** On a workstation, Nix resolves `github:NixOS/nixpkgs` through the relay registry. On GitHub Actions, the default registry resolves it directly to GitHub. The same `flake.nix` and `flake.lock` work in both environments.

This directly satisfies the workstation-only constraint and should be the recommended bootstrap path, not one option among several.

### 6.3 Be concrete about bootstrap: one recommended path per plane

For the MVP, do not present a menu of options. Recommend one path per plane:

| Plane | Bootstrap mechanism | Configuration surface |
|---|---|---|
| Git | `url.*.insteadOf` in `~/.gitconfig` | One `git config --global` per forge |
| Tarball (Nix) | Nix flake registry | One entry in `~/.config/nix/registry.json` |

Both are environment-level, per-user, and invisible to CI. Both can be set up by a single `git-relay bootstrap` command.

Other options (proxy env vars, DNS overrides, explicit URLs) can be documented as alternatives for advanced use cases, but the default path should be this.

### 6.4 Drop generic network interception from the MVP

The RFC lists it as a possibility (Section 10.2.2). For the MVP, having two well-defined, environment-level mechanisms is better than having those plus an optional third mode that changes the trust model entirely. TLS MITM may be useful in controlled CI environments, but this contradicts the workstation-only scope. Add it in Phase 4 if there is demand.

### 6.5 Clarify the narHash correctness contract

The RFC says the tarball module must "preserve source-tree semantics" (Section 15.2) but does not state the key constraint explicitly:

> **The relay must serve the real upstream tarball (or a byte/content-identical reproduction), not a relay-derived archive, because `narHash` in existing lock files was computed from the upstream tarball.**

This is the single hardest correctness requirement for the tarball plane. It should be stated upfront, not implied. It is also the reason the archive cache must be a separate content store from Git objects (you cannot substitute `git archive` output for GitHub's tarball endpoint), even though both stores are indexed by the same identity layer.

## 7. Summary

**Verdict: Architecture A (two interception planes) is correct, but the storage layer must be unified.**

The unified interception alternative (Architecture B) does not actually unify the underlying problem; it adds TLS interception complexity while still requiring two cache stores for `narHash` correctness. The pure Git approach (Architecture C) violates the workstation-only constraint and is incompatible with the existing Nix ecosystem.

The refined Architecture A provides:

- **Two protocol frontends** (Git via `insteadOf`, tarball via Nix flake registry redirect), each with independent failure domains and no TLS interception.
- **One unified storage model** with a shared identity index mapping `(forge, owner, repo, rev)` to both Git objects and cached archives. Cross-plane coherence is structural.
- **Environment-level bootstrap only** (`~/.gitconfig` + `~/.config/nix/registry.json`), invisible to CI. The same source files and lock files work on both workstation and GitHub Actions.

The RFC's main gaps to address:

1. **Adopt a unified storage model** (Section 2.3). Replace optional hydration with a shared identity index.
2. **Use Nix flake registries as the tarball bootstrap** (Section 6.2). This is the cleanest mechanism that satisfies the workstation-only constraint.
3. **State the `narHash` correctness contract explicitly** (Section 6.5). The relay must serve real upstream tarballs, not `git archive` derivatives.
4. **Recommend one bootstrap path per plane** (Section 6.3), not a menu of options. `insteadOf` for Git, flake registry for Nix.
5. **Drop generic network interception from MVP scope** (Section 6.4).

Tighten those five areas and the architecture is solid.
