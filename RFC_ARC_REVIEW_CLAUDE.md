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

- **No relay-specific URLs in source.** `flake.nix` must not reference `localhost`, relay ports, or relay-specific endpoints. Standard forge URLs (`git+ssh://git@github.com/...`) are acceptable because they work on both workstation and CI.
- **Changing flake input types is acceptable.** Converting `github:` shorthand to `git+ssh://` or `git+https://` is a source-level change, but it uses standard Git URLs that work everywhere — on the workstation (routed through relay via `insteadOf`), on CI (direct to forge), and on any other machine. This is not a relay-specific change; it is a transport preference.
- **Lock files will change when input types change.** Switching from `type: "github"` to `type: "git"` produces a different `narHash` (tarball extraction vs. Git checkout yield different source trees). This is a one-time re-lock cost per project, not an ongoing operational burden.
- **Interception is environment-level.** Git config (`insteadOf`) is the sole interception mechanism. It is per-user, per-machine, and invisible to CI.

This constraint shapes every architecture evaluated below. It rules out relay-specific URLs in committed code but accepts standard Git URL forms that happen to be interceptable by `insteadOf`.

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

One approach to fixing these weaknesses is to keep two protocol frontends but unify the storage layer behind them — a shared identity index mapping `(forge, owner, repo, rev)` to both Git objects and cached archives.

However, there is a simpler observation: **all four weaknesses disappear if the tarball plane is eliminated entirely.** If Nix inputs use `git+ssh://` instead of `github:`, there is only one plane, one cache, one identity resolution path, one bootstrap surface, and no hydration bridge. The weaknesses of Architecture A are not just fixable with a unified store — they are symptoms of a plane that may not need to exist.

This observation motivates Architecture C (Section 4), which eliminates the tarball plane by changing the input convention rather than bridging two protocols.

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

### 4.3 Trade-offs

1. **Existing lock files must be re-locked.** A `flake.lock` with `type: "github"` and a `narHash` computed from the GitHub tarball will not match the Git checkout tree. Switching to `git+ssh://` requires a one-time `nix flake update` to re-lock all inputs. This is a migration cost, not an ongoing burden.

2. **The `github:` shorthand is the dominant Nix convention.** Using `git+ssh://` is longer and less familiar. This is an ergonomic cost the user accepts in exchange for architectural simplicity — one transport, one interception mechanism, one cache.

3. **Nix's Git fetcher is slower for large repos.** The `github:` tarball fetcher downloads a single ~30MB archive for nixpkgs. The Git fetcher must negotiate pack protocol, which is significantly slower for the initial fetch. However, **with the relay caching Git objects locally**, subsequent fetches are fast incremental updates — the relay eliminates the performance disadvantage after the first fetch.

4. **`narHash` changes are a one-time event.** The hash changes when the input type changes, but once re-locked, the `type: "git"` `narHash` is stable across all environments (workstation, CI, other machines) because Git checkout is deterministic. There is no ongoing hash instability.

5. **Transitive dependencies use `github:` in their own `flake.nix`.** This is the most significant remaining issue. When your flake depends on a third-party flake that uses `github:` inputs internally, those transitive inputs are locked with `type: "github"` and fetched as tarballs — bypassing the relay. Mitigations:
   - Use `follows` to redirect transitive inputs through your own `git+ssh://` inputs where possible.
   - Accept that some transitive fetches go direct to forges. The relay still covers all direct inputs and all plain Git operations.
   - A future optional tarball compatibility plane could cover the remaining transitive cases if needed.

### 4.4 Verdict on Architecture C

**This is the recommended architecture.** It trades ecosystem convention for architectural simplicity:

1. **Compatible with the workstation-only constraint (Section 1.1).** `git+ssh://git@github.com/...` is a standard Git URL that works everywhere — on the workstation (intercepted by `insteadOf`), on CI (direct to GitHub via SSH), and on any machine with Git and SSH keys. No relay-specific URLs are committed. The `flake.nix` is portable.

2. **One mechanism, one cache, one bootstrap.** `insteadOf` in `~/.gitconfig` is the only interception needed. No tarball plane, no registry configuration, no separate archive cache. The relay is a pure Git relay.

3. **The re-lock cost is acceptable.** Switching inputs from `github:` to `git+ssh://` requires one `nix flake update`. This is a migration step, not an ongoing tax.

4. **The transitive dependency gap is real but bounded.** Third-party flakes that use `github:` internally still produce tarball-fetched transitive inputs. This can be mitigated with `follows` for key inputs, and the remaining gap is a candidate for a future optional tarball compatibility layer — not a blocker for the core architecture.

5. **The performance concern is addressed by the relay itself.** The Git fetcher is slower than tarball for initial fetches of large repos, but with the relay caching Git objects locally, subsequent fetches are fast incremental updates. The relay turns the Git fetcher's weakness into a strength.

## 5. The Core Architectural Insight

The two-mechanism approach (Architecture A) reflects a genuine protocol boundary — Git and tarball are different wire protocols. But **the protocol boundary exists because of a Nix ecosystem convention (`github:` shorthand), not because of a fundamental technical constraint.** Nix fully supports `git+ssh://` and `git+https://` inputs. The tarball path is a convenience, not a necessity.

The real question is: where should the complexity live?

| Approach | Where the complexity lives | Ongoing cost |
|---|---|---|
| A (two planes) | Relay: two frontends, unified store, cross-plane coherence | Permanent architectural complexity |
| B (MITM proxy) | Relay: TLS interception + two cache stores anyway | Permanent operational complexity |
| C (Git-only) | User: one-time re-lock, `follows` for transitive deps | One-time migration cost |

Architecture A and B push complexity into the relay — permanently. Every release, every bug fix, every new forge must consider two code paths. Architecture C pushes a bounded, one-time cost onto the user (re-lock inputs, add `follows` where needed) and then the relay is simple forever.

**Architecture C is the right call.** Accept the migration cost. Eliminate the tarball plane. Build a pure Git relay with one interception mechanism (`insteadOf`), one cache (bare Git repos), and one identity model (Git refs and SHAs). The transitive dependency gap (third-party flakes using `github:`) is real but bounded, and can be addressed with an optional tarball compatibility layer in a later phase if demand warrants it.

## 6. Recommendations for the RFC

### 6.1 Adopt Architecture C: Git-only relay with `git+ssh://` inputs

The RFC should be rewritten around a single-plane architecture:

- **One data plane:** Git over SSH (primary) and smart HTTP (secondary).
- **One interception mechanism:** `url.*.insteadOf` in `~/.gitconfig`.
- **One cache:** Local bare Git repositories.
- **One identity model:** `(forge, owner, repo)` mapped to Git refs and object SHAs.

The tarball compatibility plane (RFC Sections 6.3, 10.2, 11.2, 15) should be removed from the MVP scope entirely. The archive cache, the compatibility proxy, the forge-specific tarball fetcher logic — all of it is eliminated.

### 6.2 Nix inputs use `git+ssh://` instead of `github:` shorthand

Projects adopting Git Relay convert their `flake.nix` inputs:

```nix
# Before
inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

# After
inputs.nixpkgs.url = "git+ssh://git@github.com/NixOS/nixpkgs?ref=nixos-unstable";
```

This requires a one-time `nix flake update` to re-lock (the `narHash` changes because Git checkout and tarball extraction produce different source trees). After re-locking:

- On the workstation, `insteadOf` routes the fetch through the relay.
- On CI (GitHub Actions), the URL works directly — `git+ssh://git@github.com/...` is a standard Git remote. GHA provides SSH access to GitHub natively.
- The `flake.lock` is portable and contains no relay-specific information.

### 6.3 One bootstrap mechanism, one command

The MVP bootstrap is a single concern:

| What | Mechanism | Configuration surface |
|---|---|---|
| All Git traffic (including Nix `git+ssh://` inputs) | `url.*.insteadOf` in `~/.gitconfig` | One `git config --global` per forge |

No registry configuration. No proxy env vars. No DNS overrides. A single `git-relay bootstrap` command writes the `insteadOf` rules and the relay is operational.

### 6.4 Handle transitive `github:` dependencies with `follows`

When a direct dependency (e.g., nixpkgs) is also a transitive dependency of third-party flakes, use `follows` to ensure the transitive input resolves through your `git+ssh://` input:

```nix
inputs.nixpkgs.url = "git+ssh://git@github.com/NixOS/nixpkgs?ref=nixos-unstable";
inputs.home-manager.url = "git+ssh://git@github.com/nix-community/home-manager";
inputs.home-manager.inputs.nixpkgs.follows = "nixpkgs";
```

This eliminates redundant fetches and ensures the relay caches the shared dependency. For transitive inputs that cannot be redirected with `follows` (deep dependency trees with `github:` inputs in third-party flakes), those fetches go directly to the forge. This is an accepted gap — the relay still covers all direct inputs and all plain Git operations.

### 6.5 Simplify the storage model

With only one data plane, the storage model collapses to:

- **Git storage:** One bare repository per logical repository identity.
- **Metadata store:** SQLite for identity mappings, repository mode, upstream definitions, replication queues, cache TTLs, and audit events.
- **No archive cache.** No content-addressed blob store for tarballs. No `narHash` correctness contract. No forge-specific tarball format handling.

The identity index maps `(forge, owner, repo)` → local bare repo path + upstream URL(s) + repository mode (cache-only or authoritative). This is dramatically simpler than a unified index bridging Git objects and tarball archives.

### 6.6 Defer the tarball compatibility plane to a future phase

If the transitive dependency gap (Section 6.4) proves too painful in practice, a tarball compatibility plane can be added later as an **optional** module. But it should not be in the MVP. Build the Git relay first. Validate that `git+ssh://` inputs with `insteadOf` cover the primary use cases. Only add tarball support if real-world usage demonstrates the gap is significant enough to justify the architectural complexity.

### 6.7 Use Rust instead of Go (revising RFC Section 7)

The RFC recommends Go (Section 7.1–7.2). With the simplified Architecture C, Rust is the better choice. The relay is a long-running stateful daemon that orchestrates system Git processes, manages a SQLite database, and handles SSH/HTTP connections. The workload is I/O-bound, not compute-bound — the critical path is system Git, not the relay's own code.

**Why Rust over Go for this project:**

1. **SQLite without friction.** Go's best SQLite binding (`go-sqlite3`) requires CGO, which complicates Nix cross-compilation and makes builds less deterministic. The pure-Go alternative (`modernc.org/sqlite`) is a mechanical C-to-Go translation — it works but is unusual. Rust's `rusqlite` statically links `libsqlite3` and is the normal, well-trodden path in Nix. No CGO, no build-time surprises.

2. **Compile-time error handling.** The relay manages replication queues, ref updates, and bare Git repositories. An unhandled error during push replication or a missed failure from a subprocess can leave state inconsistent — a half-replicated push, a corrupt queue entry, a lock file left behind. In Go, `if err != nil` is easy to forget and the compiler does not enforce it. In Rust, `Result<T, E>` makes unhandled errors a compile error. For a stateful daemon where partial failures corrupt state, this is not a style preference — it is a correctness guarantee.

3. **No GC pauses.** The relay may handle bursts of concurrent Git fetches (e.g., Nix evaluating 10-30 inputs in parallel). Go's garbage collector is good but introduces latency variance. Rust has no GC. For a daemon that pipes data between network sockets and Git subprocesses, predictable latency matters.

4. **Nix packaging is equally mature.** `buildRustPackage` (nixpkgs) and `crane` (flake-native) are both well-maintained. Cargo's lockfile integrates cleanly with Nix's reproducibility model. Deterministic builds are the default path, not a special configuration.

5. **The ecosystem covers the requirements:**

   | Concern | Rust crate | Notes |
   |---|---|---|
   | SSH server | `russh` | Actively maintained, async, used by Thrussh/Lapce |
   | HTTP server | `hyper` / `axum` | Production-grade, async |
   | SQLite | `rusqlite` | Static linking, well-maintained |
   | Process orchestration | `tokio::process` | Async subprocess management |
   | Singleflight / coalescing | `tokio::sync` | `OnceCell`, `broadcast`, `watch` channels |
   | CLI | `clap` | Derive macros, shell completions |
   | Serialization (TOML config) | `toml` / `serde` | Standard |

6. **Static binary, single artifact.** Like Go, Rust produces a single static binary. The operational model is identical: one binary, one config file, one SQLite database, one directory of bare Git repos.

**What Rust costs:**

- **Slower iteration in early development.** Compile times are longer than Go. For a project this size (a focused daemon, not a large application), this is noticeable but not blocking — incremental builds are fast.
- **Async complexity.** The relay needs async I/O (concurrent SSH/HTTP connections, subprocess pipes). Rust's async model (`tokio`) has a steeper learning curve than Go's goroutines. But the relay's concurrency patterns are straightforward (accept connection → resolve repo → spawn Git → pipe I/O), not complex state machines.
- **Smaller hiring pool.** Irrelevant for a workstation tool maintained by its author.

**Revised technology choices (replacing RFC Section 7.1):**

- **Language:** Rust
- **Async runtime:** `tokio`
- **SSH ingress:** `russh` (relay-side) + OpenSSH (upstream)
- **HTTP ingress:** `axum` (or `hyper` directly)
- **Git server primitives:** system `git` (spawned as subprocesses)
- **Metadata and job state:** SQLite via `rusqlite`
- **Object storage:** filesystem bare repositories
- **Configuration:** TOML via `serde` + `toml`
- **CLI:** `clap`
- **Packaging:** Nix flake with `crane` or `buildRustPackage`

## 7. Summary

**Verdict: Architecture C (Git-only relay) is the right foundation.**

The two-plane approach (Architecture A) is not wrong — it correctly identifies the protocol boundary. But it permanently encodes the complexity of bridging Git and tarball semantics into the relay's architecture: two frontends, two cache stores (even with a unified index), forge-specific tarball format handling, and a `narHash` correctness contract that depends on upstream forges producing stable archives.

Architecture C eliminates all of that by pushing a one-time migration cost onto the user: convert `github:` inputs to `git+ssh://`, re-lock, and use `follows` for shared transitive dependencies. After that migration:

- **One interception mechanism:** `insteadOf` in `~/.gitconfig`.
- **One cache:** Local bare Git repositories.
- **One identity model:** Git refs and SHAs.
- **One bootstrap command:** `git-relay bootstrap`.
- **Portable source files:** `git+ssh://git@github.com/...` works on workstation (through relay), CI (direct), and any other machine.
- **Technology stack:** Rust + SQLite + Nix. Compile-time error handling for a stateful daemon, `rusqlite` with static linking for clean Nix builds, deterministic packaging via Nix flake.

The RFC should be revised to:

1. **Drop the tarball compatibility plane from the MVP** (Sections 6.3, 10.2, 11.2, 15). Build a pure Git relay.
2. **Recommend `git+ssh://` inputs for Nix flakes** (Section 6.2). Document the migration path from `github:` shorthand.
3. **Simplify the storage model** (Section 6.5). One bare repo per logical identity, SQLite for metadata. No archive cache.
4. **Use `follows` to cover transitive dependencies** (Section 6.4). Accept the remaining gap for deep third-party dependency trees.
5. **Defer tarball compatibility to a future phase** (Section 6.6). Only build it if real-world usage proves the transitive dependency gap is a blocker.
6. **Use Rust instead of Go** (Section 6.7). `rusqlite` avoids Go's CGO problem, `Result<T, E>` enforces error handling at compile time, and the Nix packaging story (`crane` / `buildRustPackage`) is clean and deterministic.
