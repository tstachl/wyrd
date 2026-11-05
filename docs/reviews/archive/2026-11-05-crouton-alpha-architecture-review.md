# Wyrd Adversarial Architecture Review — 2026-11-05 (crouton-alpha)

**Commit:** `438a2f3dd2464049d38e487b2fc2baf659fed13a` (branch `pr/crouton-review`)
**Working tree:** clean at that commit before this review file was written; this file is the only unstaged addition.
**Scope of evidence:** source code, contracts in `docs/contracts/`, `docs/architecture.md`, crate manifests, and contract/conformance tests read in this checkout. No `docs/reviews/archive/` input was read. No build, test run, or runtime experiment was performed.
**Method:** adversarial reading of the written architecture against the implemented architecture; every material claim below is tied to a file, a manifest edge, a call path, or a contract clause. Each finding states the intended property, the evidence, a realistic scenario, the consequence, the smallest correction, and the recurrence guard, plus an explicit attempt to disprove it.

---

## 1. Executive assessment

Wyrd's implemented architecture is *more disciplined than its prose*, and *more concentrated than its crate diagram suggests*.

The discipline is real. The two-identity model (payload-based `ContentId` in `wyrd-format` vs ciphertext `StorageId` in `wyrd-sync`) is enforced by distinct Rust types; the authorization state machine (valid/rooted/canonical/voided, snapshot classification REJECTED/PENDING/VOIDED/ELIGIBLE/SUPERSEDED/STRANDED) is a pure function of observed (membership log, snapshot DAG) sets; crash windows are inventoried per subsystem with named tests; layering is enforced mechanically by `wyrd-contracts::layer_contracts` (exact per-crate allow-lists, transitive link-graph closure for the view crate, proof-mint counting, KDF edge pinning). The four boundary kinds — durable / visible / servable / propagated — are a genuinely good vocabulary and the code largely honors them.

The concentration is the problem. Three facts dominate:

1. **Retention is charged but not refused on the receive path.** The v0.x retention contract requires local refusal before irreversible admission; the code enforces it only on the mounted write path, over the plaintext dimension only, with the production default unset (unlimited). Any peer can fill any device's disk by authoring content, and orphan bytes from torn or retried authoring are never reclaimable until post-v1 GC. (§3 F1, F3.)
2. **"One owner" is one filesystem lock, not one mutation authority.** The store lock serializes writers, but three commit authorities exist — the session-supervised mount loop (fold + mutation queue + quota), the headless composer (`wyrd vault` / `wyrd-app`, no presentation), and short-lived offline `Engine` opens (member/snapshot/policy/export) — and only the mount path exercises folding, queue order, and the quota gate. The drive-owner socket is served only by the headless run, not by the mount, which is the primary shipped owner. (§3 F2, F4.)
3. **Trust tokens prove signatures, not eligibility; mint authority does not gate installation.** `AuthorizedSnapshot` and `VerifiedSnapshot` both attest a drive-bound BIP-340 signature. Whether that snapshot is *currently eligible* to advance the live view is decided by convention at composition time (`refresh_live_heads`, the publication gate) rather than by the type system, so a future composer can install retained history as a live head with a fully type-correct program. Separately, capability installation is first-wins regardless of owner-proof legitimacy, and the accepted consequence — a first-committed conflicting vector anchors later minting — is documented but unremedied. (§3 F5, F6.)

None of these requires a rewrite. All admit small, targeted corrections (§6). The largest A1 is the receive-side retention gate (F1); the largest evolution risks are v0.4 correctness of the owner-interface composition (F4) and the v0.6–v0.8 dependency on core's relay-client coupling and the not-yet-decided fmt/version freeze items (§5).

The intended architecture (normative contracts) and the implemented architecture agree on protocol semantics — epochs, classification, carry, reconciliation, capability bindings — to a degree that is about right for a pre-alpha. They disagree mainly on *resource ownership* (who may refuse bytes, on which legs) and on *composition ownership* (who may own the drive and serve it to a second process). Those are operational boundaries, not protocol boundaries, which is precisely the part `architecture.md` claims stays open.

---

## 2. Baseline and dependency map

### 2.1 Crates and responsibilities (as implemented)

| Crate | Implemented responsibility (sources) | Production `wyrd-*` / notable 3rd-party deps |
|---|---|---|
| `wyrd-format` | Content addressing, chunking (FastCDC), canonical encodings, Merkle trees, snapshot DAG, object store (memory + fs), manifest plaintext schema, durable fs helpers. | (none) / `blake3, fastcdc, hex, thiserror, serde` |
| `wyrd-namespace` | Provider-neutral value types (`Node`, `OpenFile`, `Attr`), verified-head handle (`Head`/`AuthorizedSnapshot` token), `NamespaceView` read surface, symlink confinement policy. | `wyrd-format` / `thiserror` |
| `wyrd-sync` | Transport (iroh), encryption + keys + keystore, capabilities, authorization/classification, durable fact log, runtime engine (intake/plan/fetch/author/reconcile), serving vault + mirror, control framing, closure verifier. | `wyrd-format, wyrd-namespace` / iroh stack, `nostr`, `secp256k1`, crypto KDF/AEAD, `tokio, tracing, zeroize` |
| `wyrd-core` | Embeddable node (`WyrdNode`), live loop (`LiveNode`, pass pipeline, apply/fold), mutation queue, mailbox relay pool (`LiveMailbox`), policy/role/scrub/quarantine, export, status, budgets. **Also** the only concrete `Mailbox` implementation. | `wyrd-format, wyrd-namespace, wyrd-sync` / `nostr + nostr-sdk, tokio, futures-util` |
| `wyrd-fuse` | Filesystem-shaped read view over `NamespaceView`; `VerifiedSnapshot` unsafe capability; conflict-version lookup. No mutation/authorship. | `wyrd-format, wyrd-namespace` / `thiserror` (transitive closure reaches neither `wyrd-sync` nor `iroh*`, pinned by `fuse_link_graph_excludes_sync_and_iroh`) |
| `wyrd-daemon` | Composition root (shared headless owner/traffic tail), FUSE backend + write handles/fold lease, owner socket server + wire mapping, posture sidecar, process/supervision, vault runner. Its `core` module is a 42-line re-export shim of `wyrd-core`. | `wyrd-format, wyrd-sync, wyrd-fuse, wyrd-core` / `fuser (optional, mount)`, `serde, serde_json`, `libc`, `tracing` |
| `wyrd-app` | Developer desktop-host process: opens the drive through `Engine`, composes via `wyrd_daemon::compose_headless_owner`/`run_composed_owner`, runs the live loop. Credentials by environment (`WYRD_PASSPHRASE`, `WYRD_IDENTITY_HEX`). | `wyrd-format, wyrd-sync, wyrd-core, wyrd-daemon` / `hex, libc, tracing` |
| `wyrd-cli` | Process host (`wyrd` binary): argument parsing, hardened credential-file loading, `mount`/`vault`/`sync`/`member`/`snapshot`/`policy`/`device`/`export`/`update` commands. **Every command except `vault` opens `Engine` (and usually `FsObjectStore`) directly.** | `wyrd-format, wyrd-sync, wyrd-core, wyrd-daemon` / `clap, blake3, fuser (optional)`, `nostr`, `toml`, `serde_json` |
| `wyrd-embed` | Contract proving the core lifecycle runs with no daemon/CLI/presentation. | `wyrd-format, wyrd-sync, wyrd-fuse, wyrd-core` |
| `wyrd-contracts` | Cross-crate architectural contract suite (layering, node, fuse, owner, vault, upgrade, serving, etc.). Leaf of the DAG. | `wyrd-format, wyrd-sync, wyrd-fuse, wyrd-core, wyrd-daemon, wyrd-cli` / `fuser (optional, mount)` |

Total: 10 crates, 173,246 lines of Rust (source + tests as checked in).

### 2.2 Dependency direction and important call paths

The enforced direction is downward within each stratum, but the *top is wider than `architecture.md` describes*. The table at `docs/architecture.md:18-29` claims `wyrd-daemon → wyrd-core, wyrd-fuse` and `wyrd-cli → wyrd-daemon`, `wyrd-app → wyrd-core, wyrd-daemon`. The enforced policy (`crates/wyrd-contracts/src/layer_contracts.rs`, `policy_for`) allows the daemon, CLI, and app to name `wyrd-format` and `wyrd-sync` directly, and every one of them does:

- **Mount path:** `wyrd-cli mount` → `Engine::open_keystore` (takes `DurableStore` LOCK) → `FsObjectStore::open` → `WyrdNode::new` → `open_serving` → `into_live` → `FuseBackend::shared_with_wants` + session thread + supervised loop thread. No owner socket (`crates/wyrd-cli/src/commands/mount.rs`).
- **Vault / app path (shared):** `compose_headless_owner` → `FsObjectStore::open` → `WyrdNode::new` → `refresh_live_heads` → `open_serving` → `into_live`; `run_composed_owner` binds `owner.sock`, spawns the loop, runs the shared transport tail (`crates/wyrd-daemon/src/vault.rs:560,598`).
- **Offline admin path:** `wyrd member|snapshot|policy|export|sync now` → `Engine::open_keystore` (+ `FsObjectStore::open` + `WyrdNode::new` for export/policy/sync) → direct `Engine` mutations or `export_tree` walk. Bypasses the daemon composer and the mutation queue (except `vault`, which uses the shared composer).
- **Trust crossings:** `Engine` authoring → vault import → `DurableStore::commit` (`SnapshotBody(AuthorizedSnapshot)`, `Manifest`, `AnnouncementQueued`); `AuthorizeSnapshot::authorize` (BIP-340 verify → `unsafe from_verified_unchecked`, exactly one production call site, pinned by `verification_proof_has_one_mint`) → `Head::new` → `NamespaceView::set_heads`; `ViewHead::new(impl VerifiedSnapshot)` → `DriveView` heads → FUSE backend reads.

### 2.3 Documents inspected

`docs/architecture.md`; all 14 files in `docs/contracts/` (`object-model`, `trust`, `epochs`, `capability-bindings`, `sync-and-peers`, `write-path`, `owner-interface`, `crash-consistency`, `resource-limits`, `retention-constraints`, `error-conventions`, `upgrade-contract`, `update`, `release-read`); `ROADMAP.md`. Not inspected (out of remit + not normative): `docs/design/`, `docs/platforms/`, `docs/releases/`, `docs/planning/` (which does not check in), and any prior review output.

### 2.4 Intended vs implemented

The *protocol* intended architecture is implemented. The *composition* intended architecture — "The `wyrd` binary lives in `wyrd-cli`; composes the node through the daemon" (`crates/wyrd-daemon/src/lib.rs`, `docs/architecture.md`) — is implemented only for `wyrd vault`. `mount` hand-rolls the same steps, and `wyrd-app` touches `wyrd_core::mailbox::LiveMailbox` and `wyrd_daemon::process::bind_bulk_source` directly (`crates/wyrd-app/src/main.rs`), contradicting the comment in `crates/wyrd-app/src/lib.rs` that the host "names no `wyrd_sync` transport machinery" (that assertion is true of the `wyrd-app` *library* sources checked by `check_host_transport_free`, not of the `wyrd-app` *binary*). The layer contracts enforce the wider, actual edges; the prose diagram enforces the narrower, intended ones. Doc drift, not a code violation — but it hides the amount of top-level duplication (§3 F8).

### 2.5 Areas not reviewed

Read-level detail of the FUSE backend (`crates/wyrd-daemon/src/fuse/backend.rs` and submodules beyond the manifest/grep-level checks), engine intake/fetch internals beyond their stated contracts, Nix packaging beyond the two structural tests in `layer_contracts.rs`, mobile/file-provider adapters (do not exist), and runtime behavior (no execution). The review is an *architecture* review: where code implementing a contract was sampled, it was sampled for ownership and boundary violations, not line-by-line for bugs.

---

## 3. Ranked findings

### F1 — A1 — Receive-side retention admission is not enforced: a peer can spend your disk

**Intended property.** `docs/contracts/retention-constraints.md` Invariant 5: *Remote admission is charged locally.* The pre-admission gate (A): *"decline any new durable admission that would exceed the local retention bound, whether locally authored or fetched. For fetched representations refusal occurs before local storage."* `docs/contracts/resource-limits.md` states the ceiling compares against the accountant and the refusal occurs pre-commit; the node is *"never entitled to unbounded work because remote input requests it"*; the mounted write path enforces `retained_bytes_quota` before its first write (`LiveConfig::with_retained_quota`).

**Evidence (code + contract).**
- The only production call site of `LiveConfig::with_retained_quota` is the contract test (`crates/wyrd-contracts/src/node_contracts.rs:254`). No CLI flag, config file, or composer wires a quota in production: `grep -rn "with_retained_quota" crates/` shows only `config.rs` (definition) + that test. `ResourceBudgets::default().retained_bytes_quota` is `None`, pinned by `resource_budgets_defaults_are_pinned` (*"Retained object bytes: unset, unlimited"*).
- Fetch/live receive paths admit bytes before any ceiling is consulted: `LiveNode::execute_plan` / `Engine::execute_plan` import verified bytes and commit the plan batch; no `retained_bytes_quota` check exists on that path (by contrast `crates/wyrd-core/src/live/apply.rs` checks it on the mutation path).
- The accountant itself is partial: it tallies `FsObjectStore` plaintext objects only (`FsObjectStore::open_with` seeds from the objects-dir walk; `retained_bytes()` walks that dir only). Vault ciphertext blobs, snapshot bodies/manifests, and the fact log are explicitly *"untallied"* (`docs/contracts/retention-constraints.md:30-31,53-57`).
- The contract itself records the gap as admitted, not fixed: *"A: receive-side admission ceiling — Decided, not implemented… until it lands, a remote author can still spend a peer's local-write headroom"* and *"Four refusal acceptance tests — Missing"* (`retention-constraints.md:130-135`).

**Scenario.** Member device B authors 5 GiB of content (or a churn loop of `unlink`+`create` paying the per-snapshot fixed cost) and syncs to victim A. A's object store grows with A never refusing; the vault and fact log grow untallied alongside. Eventually A's disk classifies `StorageFull` at the I/O boundary (`StoreFailure::of_io`, `resource-limits.md`): the pass aborts as a store-class failure and the mount terminates after a few passes against a "dead disk" that is merely full of peer content. A's own writes fail `ENOSPC` on a healthy drive — exactly the adversary named in `retention-constraints.md:83-85` ("Remote spending of local headroom").

**Consequence.** The boundedness architecture (Conventions first half of `resource-limits.md`) does not hold for the non-authoring replica, which is the majority topology (mirror/vault). One hostile or runaway author imposes unbounded retention on every replica; the only defense today is "the operator pre-configures a quota," but even a configured quota bounds only the author's own write path — the receive path still admits past it.

**Smallest correction.** Enforce the same pre-admission check on the fetch-import leg: before `Vault::import` / `FsObjectStore` insert in the plan path, compare (accounted + estimate) against the same `retained_bytes_quota` using a vault-seeing counter (the prerequisite already specified in `retention-constraints.md:162-167`, still missing), refusing *before* the bytes are stored and reporting *refused*, never *unavailable/lost* (Invariant 7). One-commit overshoot is acceptable only if stated; refusing atomically is preferable. Ship the default as `None` but make the knob real: one CLI/config surface that actually sets `with_retained_quota` for the shipped composers, with startup refusing a quota already exceeded (the existing `check_retained_ceiling` cross-check, already called by `check_startup_retention`, then actually bites).

**Recurrence guard.** Implement the four acceptance tests the contract names as missing: (1) receive-side refusal leaves nothing stored and nothing claimed servable; (2) refusal is distinguishable from loss/unavailability in `sync status` output; (3) refused bytes never enter loss/repair as lost; (4) boot rebuild preserves refusal (durable refusal state per §"Accounting prerequisites"). Add a contract test that `execute_plan` on a full accountant fails closed without importing.

**Attempt to disprove.** One could object that charging (without refusal) satisfies Invariant 5, that the default-unlimited stance is deliberate for dogfood (`retention-constraints.md:196-202`), and that disk-full already fails closed at the store boundary. All three are true and do not refute the finding: the gate's *purpose* per the contract's own Table is pre-irreversibility; the store-full path terminates the mount (operator-visible, but the damage — unbounded growth and mount death — is done); and "deliberate default" does not excuse "unwired knob." The finding's premise is the project's own stated contract, not an imported standard.

**Confidence:** high (self-admitted gap).

---

### F2 — A1 — `WyrdNode::put_file/remove` is a second mutation authority that bypasses folding, the queue, and the quota

**Intended property.** `architecture.md:35`: *"All mutation flows through the owner's single mutation queue."* `write-path.md` DG-1: folding (one snapshot per forcing event), the bounded synchronous queue, and the pre-commit retained-bytes gate define when bytes become durable. The quota gate lives in the live apply path (`crates/wyrd-core/src/live/apply.rs`), not in `Engine`.

**Evidence.** `crates/wyrd-core/src/node.rs:292-347` (`put_file`, `remove`) performs: take `store_write()` guard → `Engine::begin_write` → chunk + `mutation::put/remove` → `Engine::author_pending_write` → `install_authored_head` (installs the new snapshot as the sole head without classification/closure gating). It enforces neither the `retained_bytes_quota` check nor any fold/budget estimate, takes no queue slot, and can run while the live loop holds its own authorship. Production callers: none found in `wyrd-cli`/`wyrd-app`/`wyrd-daemon` — the only callers are contract tests (`node_contracts`, `shared_drive_semantics`, `sync_contracts/write_path`). But it is a public, documented node API at exactly the layer `wyrd-embed` claims to prove for embedders, and `crates/wyrd-core/src/view.rs` exposes the same `Store: ObjectStore` surface the live loop mutates.

**Scenario.** A future mobile/file-provider embedder (v0.4–v0.6) uses the "simple" `put_file` API for the lifecycle proof: imported file → one snapshot per call, unbounded retention (bypassing F1's gate even when the live path is fixed), no `max_pending_mutations`/budget accounting, and silent head overwrite races against concurrent live-loop publication because `install_authored_head` sets the head set without partition/closure checks. Two components mutate what is logically the same store (the live loop's `apply_mutation` and `put_file`) with different invariants — exactly what boundary C ("can two components independently mutate what is logically the same state?") asks this review to catch.

**Consequence.** Every correct invariant implemented in the live apply path (quota, fold estimation, pending-set semantics) silently does not apply to this path. Product work that reaches for the simplest API gets a different durability story from FUSE.

**Smallest correction.** Either (a) route `put_file`/`remove` through the same `LiveNode` mutation submission ( preferred): make them queue submissions on the live loop rather than direct `Engine` calls, so quota/fold/staleness share one code path; or (b) restrict them to `#[cfg(test)]` fixtures and delete them from the non-test public surface (the embed lifecycle test can construct snapshots via the live loop instead). What must not remain is a public non-test mutation entry point with weaker invariants.

**Recurrence guard.** A layer-contract test asserting that every `pub fn` on `WyrdNode`/`LiveNode` that author snapshots goes through `MutationQueue::submit` / the live pass (mechanical scan like `check_owner_runs_through_daemon`), or: no production `wyrd-core` export authoring API outside `live/apply.rs` + `Engine::author_pending_write` call sites enumerated by the test.

**Attempt to disprove.** The live loop is the only *shipped* composer; `put_file` is a test harness (its callers are all tests), so there is no live bypass today. If the team deprecates it from the default (non-test) surface, this downgrades to A4. As shipped (public, non-test), it is a loaded API waiting for a caller, so A1 for its presence, A3 for its impact.

**Confidence:** high on the code facts, medium on severity (no current production caller).

---

### F3 — A3 — The three durable stores have sequential, not atomic, commit; orphan bytes accrue without accounting

**Intended property.** `write-path.md` step 3: *"The object store, the vault, and the fact log become durable together at this boundary."* `crash-consistency.md` "Snapshot authoring and heads": vault imports land before the facts that name them.

**Evidence.** The two halves of step 3 are both true and mutually inconsistent in wording: objects are durably inserted at step 1 (`FsObjectStore` temp+fsync+rename per object, `crates/wyrd-format/src/fs_store.rs`), vault envelopes land at step 2 before the fact commit, and the fact commit is the durable boundary. A crash between step 2's vault imports and the fact commit leaves sealed envelopes with no naming fact; a crash between chunk inserts and authoring leaves unreferenced chunks. The contract calls each orphan situation *"unreferenced (append-only, no GC)"* and pins convergence rather than reclamation. `retention-constraints.md` acknowledges vault bytes escaped the accountant (`retention-constraints.md:164-167` prerequisite: a vault-seeing counter *"It lives beside mirror accounting… not as a widened `RetainedBytes`"*) — still absent from the live codebase (the counter grep shows `Mirror*` queue constants only).

**Scenario.** A retry storm or repeated authoring failures (disk pressure, mirror-full backpressure, `Conflicted` folds) accumulates orphan chunk+vault bytes across weeks of unattended vault operation — precisely the headline dogfood workload (`wyrd vault` "forgettable"). The accountant (once F1 lands) will charge a growing orphan mass that no refusal path can prevent or evict, and the unattended operator sees `StorageFull` with no actionable object to delete.

**Consequence.** Not a correctness violation (orphans are inert and restart-convergent), but growth accounting lies by omission: every post-commit failure inflates retained bytes permanently. It converts F1 from "refuse at the gate" into "refuse at the gate while leaking through the side door."

**Smallest correction.** Implement the already-specified vault-seeing counter beside mirror accounting and charge orphans against it (seeding from a vault-dir walk at open, like the plaintext accountant — one traversal at open, never per commit). Defer reclamation (post-v1 GC), but stop *lying* about residency. No protocol or format change; local accounting only.

**Recurrence guard.** A crash matrix test asserting orphan bytes are charged to the same accountant that gates admission (the existing `crash_matrix_never_hybrid` + `serving_reopen_rebuilds_the_mirror_from_the_vault` suite extended by a charged-orphan assertion).

**Attempt to disprove.** The crash sections explicitly accept orphans and the invariants pinned (before-or-full per subsystem; subsystems agree after reopen, `engine/tests_reopen_agreement.rs`) confirm no torn-state correctness issue. Downgrading to A4 would be fair if orphan volume were bounded; it is not — retries are unbounded by design (obligation retries "refuses permanently" per `sync-and-peers.md:82-91`). So A3 stands.

**Confidence:** high.

---

### F4 — A2 — The mount owner serves no owner-interface socket; the headless owner does. Two lifecycles, one lock

**Intended property.** `architecture.md:35`: *"A drive has exactly one active process owner of its durable store. All mutation flows through the owner's single mutation queue; other consumers submit operations through the drive-owner interface."* `owner-interface.md` is normative for that cross-process surface (socket at `<drive_dir>/owner.sock`, peer-uid check, `v: 1` framing, 96 MiB frame cap, read/observe/liveness/freshness/mutation/pause/resume operations).

**Evidence.** `run_composed_owner` (`crates/wyrd-daemon/src/vault.rs:598`) binds the socket and serves `OwnerServer`. `crates/wyrd-cli/src/commands/mount.rs` never constructs one (grep: no `OwnerServer`/`bind_listener` in the mount path). `layer_contracts.rs:1448-1480` deliberately scopes the headless-composition check away from the mount: *"The mount's session supervision is a legitimate second caller … a shape the headless check must not flatten."* So the two owners are intentional, but their surfaces differ by the socket. The `wyrd-app` host and `wyrd vault` get a socket-serving owner; the `wyrd mount` owner does not. Offline CLI commands (`member invite`, `snapshot`, `policy`) do not use the socket either; they become transient owners via `Engine::open_keystore` (and fail `StoreLocked` when a long-lived owner holds the drive, e.g. requiring the operator to stop the service first — stated explicitly for `update`, `docs/contracts/update.md`, and implicit everywhere else).

**Scenario.** v0.4 thesis: *"The UI talks to the drive-owner interface, not FUSE"* (`ROADMAP.md` v0.4). Any desktop composition that runs the drive under `wyrd mount` (today's default, read-write FUSE) leaves the future window with no interface to talk to: starting the window's host would fail `StoreLocked` (or require stopping the mount — operationally identical to "mount FUSE breaks the desktop app"). The File Provider extension described in `crates/wyrd-daemon/src/owner/mod.rs` ("the File Provider extension first") is likewise incompatible with a mounted owner. The alternative — always run the app host, never the mount — is workable, but then mount is not a product candidate and the roadmap's FUSE-first history reads differently.

**Consequence.** The primary shipped owner (mount) cannot host a second local consumer through the contract's only sanctioned surface. The office product loop must remember which owner binary is running to know whether the socket exists.

**Smallest correction.** Make the owner server part of the shared composition: hoist `OwnerServer::new` + `bind_listener` + thread lifecycle out of `run_composed_owner` into a helper both the headless runner and the mount path call (the socket claims the store lock *while held*, stale-socket reclaim already under that lock). The mount's session supervision stays mount-specific (don't flatten `spawn_loop`, per the layer contract) but shares the socket binding. Separately, document that offline CLI commands require the long-lived owner stopped (stating the same "stop the service first" rule update uses, in `member`/`snapshot`/`policy` help).

**Recurrence guard.** A layer-contract test asserting every path that opens the store for a long-lived run binds the owner socket except the explicit headless composers (or one test binding the shared helper into both paths); a composition test that `mount` + a second process over `owner.sock` can `lookup`/`read`/`submit` while mounted.

**Attempt to disprove.** The exemption is deliberate and pinned in comments; the desktop host (`wyrd-app`) *does* provide the socket, so v0.4 is not blocked if desktop always runs the app host. That's the strongest counter and the reason this is A2 (evolution risk to the stated v0.4 thesis) rather than A1 (broken today). The classification still holds as A2 because the thesis says "not FUSE" without recognizing mount's missing socket as a constraint.

**Confidence:** high on the facts, medium on the v0.4 impact (depends on which binary runs the desktop loop).

---

### F5 — A3 — Eligibility is a convention, not a type: the token proves a signature, the composer must remember to check eligibility

**Intended property.** `trust.md` "View-head audit rule" + `epochs.md` classification: the live view admits *eligible heads only*; the `VerifiedSnapshot` capability is the trust assertion that the wrapped snapshot was verified by the trust authority; *"Every future `unsafe impl` of that trait is therefore a trust-boundary change."*

**Evidence.** The token chain proves less than the name implies. `AuthorizedSnapshot::from_verified_unchecked` asserts *"the body's BIP-340 signature verified"* (`crates/wyrd-namespace/src/view.rs:64-76`); `verify_snapshot` in `crates/wyrd-sync/src/durable/mod.rs` performs exactly that check. The `VerifiedSnapshot` trait asserts the same (*"the wrapped snapshot's signature has been verified"*, `crates/wyrd-fuse/src/view/head.rs:35-39`); its own docs add: *"The token proves a drive-bound signature only: head eligibility is decided at composition (`WyrdNode::refresh_live_heads`), so a view built from otherwise-obtained tokens can project retained history."* Both production install paths assume the compiler will catch ineligible heads: `WyrdNode::refresh_live_heads` (does partition + closure gate) and `install_authored_head` (skips both, relying on "member-authored under canonical membership ⇒ eligible"). The audit marker is genuinely mechanical (the proof-mint crossing count test), but nothing type-level stops a new composer (desktop window, File Provider, mobile) from calling `ViewHead::new` with a signature-valid but superseded/stranded snapshot and rendering retained history as the live view.

**Scenario.** A v0.5 time-travel UI or a v0.4 conflict indicator reuses `DriveView`, constructs heads from older `AuthorizedSnapshot`s it legitimately holds (for history browsing), and — because installation is unchecked — the merged view now treats a superseded snapshot as live. Reads serve history as current state; a subsequent write parents onto the wrong head. The user-visible symptom is data regression, not a crash, and the type system blessed it.

**Consequence.** The load-bearing invariant "only eligible heads advance the live view" rests on every composer calling `refresh_live_heads` and none calling `set_heads` directly. Today the set of composers is small and the comments are good; the invariant is one new call site away from silent violation.

**Smallest correction.** Collect eligibility into the type: introduce an `EligibleHead` newtype minted only by the classification+closure path (`LiveNode` publication), so `set_heads` accepts `Vec<EligibleHead>`. Keep `AuthorizedSnapshot` (signature) for history browsing explicitly; make installing retained history require an explicit `install_history_head` that cannot be confused with live publication (e.g. a separate `HistoryView` constructor). This is a small, local change: move the "eligibility decision" into the minting function signature instead of a comment.

**Recurrence guard.** The proof-mint-style mechanical test generalized: every call site of `set_heads` / `ViewHead::new` outside the live publication module fails the suite (same source-scan pattern as `check_owner_runs_through_daemon`).

**Attempt to disprove.** The docs *explicitly* state the token proves signature only and that eligibility is composer's job; this is a known, documented split, not a hidden bug. The production code honors it (both install paths are documented as eligibility/correctness-verified). So this is not a demonstrated defect today — hence A3, not A1. The risk is forward-looking composers, which is why it is ranked for v0.5 history work.

**Confidence:** high on the facts, medium on likelihood (needs a future composer error).

---

### F6 — A3 — Capability first-wins installs forgery as operative, then re-mints it under an owner proof

**Intended property.** `trust.md`: *"owner authorization authenticates the origin of the secret material; member authorization authenticates only its delivery."* Installation of a capability *"may only add secrets for epochs not yet held; it must never roll back"* and *"Conflicting vectors resolve first-wins over local durable commit order."*

**Evidence.** `trust.md`'s Capability-construction section and `epochs.md` "Capability installation is monotonic": the winning vector *"is operative (installed, minted from) without being legitimate (proof-bound)"* (`trust.md:751-759`, `epochs.md:343-362`). *"the next rotation re-mints whatever the keyring holds under a valid owner proof, so a first-committed conflicting vector travels to every member"* and *"Preferring the root escrow sidecar over the keyring when minting past epochs would stop that propagation …; that narrowing is future protocol work."*

**Scenario.** A malicious or buggy member seals a well-formed capability vector for epoch N with attacker-chosen secrets (it legitimately holds N's control keys; the envelope proves nothing about the mint). Victim V (a fresh admit, or a restored device whose keyring is empty for N) commits the forgery before the owner's honest vector arrives. V now derives control keys from the forgery; honest epoch-N traffic fails to open. Worse, when V (or an owner whose keyring anchors the forgery) later mints, the forgery is re-minted under a *valid owner proof*, converting one victim's poisoned keyring into every member's poisoned vector. First-wins keeps one node working; it does not quarantine the forgery.

**Consequence.** A single adversarial first commit achieves persistent cross-member poisoning that survives honest redistribution. This is precisely the "mint authority" boundary the owner-proof construction was built to defend — the proof attests the vector's *origin*, but installation is first-wins *regardless of the proof*, so the defense is applied at delivery time, not at the durability boundary where it matters.

**Smallest correction.** Prefer escrow-derived secrets when minting past epochs (the documented narrowing), or, more narrowly for v0: treat a capability vector as *staged* until a second corroborating source (owner-proof rotation delivery, or the escrow sidecar) confirms it, while keeping first-wins only as an availability fallback that is never used as a minting base. The documents name the first option as reserved work; the minimal version is to use the escrow preference *inside minting* without changing the wire format.

**Recurrence guard.** A conformance test that plants a member-forged pre-commit vector and asserts the next rotation does not re-mint it (i.e. the minting base is the proof-bound vector). Until then, the test suite must at minimum pin the current accepted behavior so any change is a deliberate protocol decision.

**Attempt to disprove.** The windows are narrow: honest operation never produces conflicts; an already-membered owner already holds its honest vector, so a forgery for an occupied epoch is ignored (first-wins fills only unoccupied). Propagation requires the *minter's* keyring to be poisoned *before* it mints, which for the owner normally cannot happen because the owner mints honestly first. Counterexample: a fresh device. Fresh devices don't mint (no owner authority), so propagation requires owner poisoning, which is hard via this exact path. Counter-counter: owner restore from escrow (`EscrowConflict` fail-closed on disagreeing sidecar — but that protects the *sidecar*, not a poisoned pre-existing keyring) or an owner with an empty keyring receiving mail before re-minting. The residual risk is small but the affected asset (the minting base for all future epochs) is total. A3 is the honest classification; promoting to A1 would overstate liveness.

**Confidence:** medium (docs acknowledge; narrow trigger, total impact).

---

### F7 — A3 — `wyrd-core` owns the relay client: the embeddable node embeds Nostr

**Intended property.** `architecture.md`: `wyrd-sync` owns iroh transport, encryption, capabilities; `wyrd-core` is the embeddable node; `sync-and-peers.md`: *"anything implementing `Mailbox` … drives the same engine… The `Mailbox` trait is the boundary."*

**Evidence.** The boundary *is* correctly placed at the trait level: `Mailbox` lives in `wyrd-sync`, `LiveMailbox` in `wyrd-core/src/mailbox/`. But the only production `Mailbox` implementation drags in `nostr-sdk`, `nostr`, `tokio` (`crates/wyrd-core/Cargo.toml`, `use nostr_sdk::prelude::{Client, Filter, RelayLimits}` in `mod.rs`). The layer contract pins `nostr` use to this one subsystem dir, confirming the coupling is known. The live loop (`LiveNode`) is then composed *with* a `LiveMailbox` in both shipped long-lived paths (`mount.rs`, `app/main.rs`); there is no shipped composer that runs the node with anything else (a wake-sleep mailbox, an in-memory fake beyond tests, or nothing).

**Scenario.** v0.6 mobile: lifecycle-aware sync must *"wake, inspect durable state, perform bounded work, persist, sleep, never keep the daemon alive forever"* (`ROADMAP.md` v0.6). A mobile embedder reusing `wyrd-core` inherits a relay subscription model (persistent filter, backoff drainer, seen-id dedupe log), a 5-second idle pacing loop, and background drainer supervision — all of which assume a long-lived process. The trait allows a wake-sleep implementation, but no such implementation exists as evidence that the trait is sufficient; the first data point (persistent subscription + CLOSE+REQ resubscribe + health polling) shapes the assumptions everywhere.

**Consequence.** v0.6 product work must either amortize the relay-stack lifecycle inside a mobile sandbox (battery, background limits) or refactor the node/tests against a new `Mailbox` implementation under schedule pressure — the exact "protocol semantics never depend on consumer" correction the roadmap's project rule warns against.

**Smallest correction.** No code move: write the wake-sleep `Mailbox` implementation *as a test/contract implementation* before v0.6 (`wyrd-embed` or a small `wyrd-core` example that drives the engine with a fake mailbox through wake/inspect/work/persist/sleep, asserting bounded work without a persistent subscription). If it cannot be expressed as a `Mailbox`, that is the architecture signal — discovered now, not in 2027.

**Recurrence guard.** A `wyrd-embed` lifecycle case that never constructs `LiveMailbox` (already the embed crate's thesis: "no daemon, CLI or presentation" — extend it to "no relay client").

**Attempt to disprove.** `sync-and-peers.md` explicitly documents the placement ("It lives in `wyrd-core` because it composes node concerns") and the trait boundary is genuinely clean (tests exercise only the fake). Mobile could vendor a `Mailbox` impl later without touching sync. So this is A3/A4, not a violation. The remaining gap is the missing proof of the second implementation.

**Confidence:** medium.

---

### F8 — A4 — Architecture prose describes a narrower crate DAG than the enforced policy; CLI/App duplicate top-level composition

**Intended property.** `architecture.md:18-29` + `wyrd-daemon` description ("Composes the core, view and presentation backends"; CLI "over the daemon's public API").

**Evidence.** Enforced policy (`layer_contracts.rs`) allows — and sources use — direct `wyrd-format`/`wyrd-sync` edges from daemon, CLI, and app. CLI commands `member`, `snapshot`, `policy`, `device`, `export`, `sync` each re-implement the open sequence (`Engine::open_keystore` + `FsObjectStore::open` + `WyrdNode::new` + `refresh_live_heads` + transport binding) rather than calling one composition helper; mount re-implements `vault`'s supervision around the session. The shared pieces (`install_serving`, `install_role_dir`, `bind_bulk_source`, `SHUTDOWN` latch, `compose_headless_owner` for headless only) are genuinely shared, so the duplication is top-level sequencing, not tail behavior.

**Scenario.** A mailbox/transport/bulk change (e.g. rotating how relays are bound or how the bulk source is constructed) must be replayed in mount, vault, sync, app, plus each offline command — five places reasoning about the same order of operations, with checks (`check_owner_runs_through_daemon`, `check_host_transport_free`) that exclude the mount by design.

**Consequence.** Change amplification confined to sequencing; per-command review must confirm it re-derived the same order (posture claim, retention check, heads refresh, serving install) as every other composer. Drift risk is moderate, blast radius is annoyance + occasional divergence, not corruption.

**Smallest correction.** Reconcile the prose table with the enforced policy (one paragraph: daemon/CLI/app name format+sync directly by design; the DAG constraint is *direction*, not adjacency). Optionally factor the offline-command open sequence into one `open_policy_node`-style helper shared by member/snapshot/policy/export (it already exists for policy/export — extend it).

**Recurrence guard.** Extend the existing owner-run/composition source scans to the offline commands (today both checks exempt them or don't scan them).

**Attempt to disprove.** The policy comments state the edges are deliberate ("the host crates are its own") and the layer contracts match them exactly. This is doc drift + minor duplication, not a boundary violation. A4, not higher.

**Confidence:** high.

---

### F9 — A4 — Credentials: two shipped paths (`--identity-file`, env) outside the stated boundary; NIP-46 scope enforcement is local-session-only

**Intended property.** `architecture.md` "Platform mechanisms stay at the boundary": *"Identity and vault logic consume secret material (`DeviceIdentitySecret`), never files, modes, or OS facilities."* `trust.md` "NIP-46 remote signing": per-domain authorization at the signer; *"the daemon never receives the nsec"* in NIP-46 mode. `trust.md` "Custody modes": *"Local-key mode — the only mode implemented … the daemon does hold the nsec."*

**Evidence.** `trust.md` is honest that no shipped build satisfies the NIP-46 target. Two additional observations beyond the documented gap: (a) `wyrd-app` adds a third credential transport (environment variables, `crates/wyrd-app/src/main.rs:53-54`), while the architecture's platform-neutral statement names exactly one backend (the hardened Unix file reader in `wyrd-cli`'s `credentials.rs`); env-sourced `WYRD_PASSPHRASE`/`WYRD_IDENTITY_HEX` are visible in `/proc/<pid>/environ` and any post-mortem dump. (b) The owner-proof domain (`OwnerProofV1`, 0x02) is *"unscoped by design"* for the only shipped session ("today the domain names the operation without constraining it", `trust.md:450-455`) — correct per the documented "enforcement is remote-session behavior," but it means a compromised local signer client today can re-label any digest, and the domain work that makes NIP-46 mode safe has no production assertion to measure against.

**Scenario.** The v0.8 threat-model review ("device-compromise scenarios," `ROADMAP.md` v0.8) must re-derive custody assumptions from *three* shipped credential paths (file, env, local signer key held for life) instead of one, or discover env exposure via `/proc` on a supervisor-reported incident.

**Consequence.** For the pre-alpha this is acceptable (app is labeled "developer scaffolding, not the product surface"), but the flatpak manifest promotes `command: wyrd-app` (`layer_contracts.rs:856`) — a shipping artifact running the scaffolding credential path.

**Smallest correction.** Gate `wyrd-app`'s env-credential main behind a feature or `#[cfg(not(feature = "ship"))]`-equivalent, or move env reading into `wyrd-cli`'s credential file path (single reader, single zeroization story); file `docs/contracts/trust.md` "Custody modes" to name env as an additional mode with explicit exposure notes. Defer the NIP-46 session client but assert the signer-domain refusal with a remote-shape test double (already exists as `FakeSignerSession` — label the shipped-session path as explicitly unscoped in `main.rs` usage output).

**Attempt to disprove.** `DeviceIdentitySecret` *does* zeroize on drop (`ZeroizeOnDrop` derived, `crates/wyrd-sync/src/keys/device.rs`), `credentials.rs` is the hardened reader, and the roadmap explicitly books NIP-46 as absent. The finding is only that env is a second path and the shipped flatpak runs it — cheap to normalize before v0.8.

**Confidence:** medium.

---

## 4. Invariant-ownership table

| Invariant (contract) | Authoritative owner (code) | Enforcement point(s) | Durable evidence | Recovery / rebuild | Test coverage (named) |
|---|---|---|---|---|---|
| Payload identity: `ContentId = BLAKE3-derive_key("wyrd content v1/<kind>", payload)` — identity is payload, not envelope (`object-model.md` D30) | `wyrd-format` (`identity.rs`, `envelope.rs`) | `ObjectStore::insert` / `insert_verified` (store derives + verifies); `seal.rs:132-144` derives from encrypted payload bytes | Content-addressed objects (`objects/<kind>/<fanout>/<hex>`) | Scrub on read; quarantine discard of verification failures; `FsObjectStore::open` sweeps stale temps | `read` verification; store round-trip tests; `seal` derivation tests |
| Storage identity: `StorageId` over sealed bytes, distinct type from `ContentId` | `wyrd-sync` (`seal.rs`, `keys/`) | AEAD tag over `version ‖ kind ‖ ContentId` AAD + plaintext-hash check (two checks) | Vault files (`vault/<transport-root-hex>`) | Fetch fallback to next representation; mirror rebuild from vault (`serving_reopen_rebuilds_the_mirror_from_the_vault`) | `kdf_parameters_are_pinned`, `prod_kdf_params_round_trip`; AEAD binding tests in `capability-bindings` matrix |
| Manifest schema + canonical plaintext encoding | `wyrd-format` (`manifest.rs`) | `Manifest::decode`; `Limits::V0` structural ceilings in `ingest.rs` | Encrypted manifests in vault + `ManifestRecord` facts | Authoring self-check + daemon head gate (`verify_snapshot_manifest` / `verify_head_closure`) | Closure verifier tests; sync admission tests |
| Manifest/shipping-closure correspondence (tree ↔ manifests) | `wyrd-sync` (`closure.rs`) | Authoring self-check; daemon head gate (`verify_head_closure`) | — (verifies other evidence) | Pending head waits for closure fetch; damaged closure fails pass closed | `verify_snapshot_manifest` call-site tests (see F5) |
| Membership log: valid / rooted / canonical / voided; conflicts freeze | `wyrd-sync` membership module (`MembershipLog`) | `MembershipLog::observe` + chain analysis (signature, roots, pre-state owner authority, `apply` semantics) | `Fact::Transition` (canonical bytes = preimage‖sig) | Freeze + owner-signed resolution (`resolves`); voided retained forever | Membership conformance suite (genesis/conflict/resolution/reader/retirement cases, `epochs.md:646-673`) |
| Snapshot authorization + classification (eligible vs superseded/stranded) | `wyrd-sync` authorization (`classify.rs`, `predicates.rs`) | `Engine::live_heads` + `partition_heads`; `epochs.md` predicates | `SnapshotBody(AuthorizedSnapshot)` + `Announcement*` facts | Bounded-fork: stale snapshots degrade on log advance; stranded content only via owner recovery snapshot | Authorization conformance (`conformance.rs`, `properties.rs`); `partition_heads` gate tests |
| Capability secrecy + five-value binding (`DriveId/DeviceId/EncKey/Transition/Epoch`) | `wyrd-sync` keys (`capability/`) | `AuthorizedCapability::authorize` before commit; replay re-authorizes | `Fact::Capability` (sealed at rest) | Monotonic install (no rollback); first-wins conflicts (see F6); escrow sidecars (`<store>/escrow/<epoch>`) | `capability/tests.rs`, `tests_capability.rs`, binding-matrix tests |
| Signing authority for new snapshots/transitions | Device identity key (`DeviceIdentitySecret`, `trust.md` T14) | `SignerSession::sign_message` (local session only); drive-bound preimage + BIP-340 challenge | Signed bodies/transitions in fact log | NIP-46 remote session: NOT implemented (booked open); local key held for process life (see F9) | `transport/signer.rs` fake-signer tests; conformance suite |
| Durable fact commit: one batch per op, torn commit invisible (seq ≤ CURRENT) | `wyrd-sync` durable (`store.rs`, `codec/`) | `DurableStore::commit_until` + 8-stage crash matrix; `check_commit_fits` (65,536 record / 64 MiB ceiling) | Commit files + `CURRENT`; hash chain over `1..=CURRENT` | Replay + `resync`; torn commit returns `Ok` with nothing durable; orphan files ignored | `crash_matrix_never_hybrid`, `orphan_files_are_ignored`, `commit_fit_boundaries_match_the_load_ceilings`, reopen-agreement suite |
| Object residency (what this device holds) | **Split:** fact log (`LocalObject`/`ObjectRemoved` flags) + `FsObjectStore` (actual bytes) | Plan/fetch commits `LocalObject` after import; scrub/quarantine clear stale claims | Object bytes + `LocalObject` facts (must agree; scrub reconciles) | Scrub repair (re-verify → clear claim → subtract); quarantine claim-clear-before-unlink; restart re-seeds accountant from disk walk | `missing_chunk_clears_claim_and_subtracts_size`, `record_objects_removed_batches_many_identities`; failure-open question: which source is authoritative is documented as fact = claim, bytes = truth (F3) |
| Serving residency (what is offered) | `wyrd-sync` serving (`VaultSource`) | `VaultSource::from_state` rebuilt per call; mirror flush before announcement discharge | Vault files (hex-named sealed roots) | Rebuild from `RuntimeState` on every `from_state` (no derived cache as sole copy) | `restart_equivalence_serving_residency_matches`, `maps_for_test` comparison, peer-fetch-over-rebuilt-source test |
| Publication projection (live head set served by views) | `wyrd-core` (`WyrdNode::refresh_live_heads`, `LiveNode` publication gate) | Partition (publishable vs pending) + closure gate; stale-slot retention on damaged projection | In-memory `Projection` (generation-indexed, lock-cloned per operation) | Post-corruption rule: preserve last installed heads, surface error, never auto-repair (`epochs.md:557-568`); next pass retries | `failed_projection_preserves_the_last_installed_heads` + surface/never-clears trio |
| Mutation order + fold semantics (DG-1) | `wyrd-core` (`mutation/queue.rs`, `live/apply.rs`) | Bounded queue (`max_pending_mutations`), total order, stale-handle checks, quota gate | Ephemeral queue (volatile by design) | Shutdown resolves `Shutdown`; torn folds leave no half-state (monotonic commit pipeline) | Queue/fold tests (`tests_queue`, `tests_batch`, `tests_coalescing`); daemon fold suite |
| Resource budgets (live operation bounds) | `wyrd-core` (`budgets.rs`, `ResourceBudgets`) | Queue admission, want registry, pass caps, handle+capture ceilings | — (process-local) | Refusal (raise-vs-count: fatal commit fails raise, per-item fails counted) | `resource_budgets_defaults_are_pinned` (doc≡default); per-cap tests |
| Peer credential for owner socket | `wyrd-daemon` owner (`auth.rs`) | `owner_uid` (geteuid) vs `peer_uid` (`SO_PEERCRED`/`getpeereid`), fail-closed on error | None (runtime) | Drop + log each refusal; manager-owned refusal | Owner tests (`tests_owner.rs`); contract in `owner-interface.md` |
| Release-read freeze (byte layouts unchanged) | `wyrd-sync` (`public_read.rs`) | Authorship rule (`author==maintainer && membership==genesis && epoch==1`), scope order, `SnapshotMismatch`-first backstop | Release metadata (public) + clear announcements | Fails closed (`NotMaintainer`, `NotGenesis`, `WrongEpoch`, …); no membership fallback | `public_announcement_encoding_is_frozen`, `release_key_derivations_are_frozen`, `release_metadata_encoding_is_fixed_length_and_versioned` |
| Format versions / upgrade boundary | `wyrd-format` (`envelope::VERSION`), `wyrd-sync` (`SEAL_/ROTATION_/ESCROW_/KEYSTORE_/NODE_ADDR_/BOOTSTRAP_/COMMIT_/CONTROL_VERSION`) | Version checks at open/verify/decode; unknown envelope/control versions refuse (`UnknownVersion`) | Versioned records; skippable unknown fact tags (forward-compat exception, `upgrade-contract.md` Inv 10) | Derived state rebuilds; immutable data never rewritten; torn upgrades reopen | `upgrade_contracts.rs` catalog 23–33; fixture replay (`dev`, `v0.2.0-alpha`, `v0.3.0-alpha` stores); two ignored tests named (fact-payload replay, version matrix) |

**Read across the table:** each invariant has exactly one owner and a named test, except residency (two sources reconciled by scrub/quarantine) and publication (one owner but two install call sites — see F5). That is the architecture's actual ownership shape: protocol invariants are single-owned; operational invariants (residency, publication) are *reconciled between two sources* by repair loops. The repair loops are well-built (claim-clear-before-unlink, re-verify-before-clear, rotating sweep cursor, batched commits), so "reconciliation, not single ownership, for possession" is defensible — except that the reconciliation was not extended to the *charging* side (F1/F3).

---

## 5. Future-evolution assessment

### v0.3 (current) — durable operation and content recovery

*Roadmap thesis: "one machine can safely operate it."*
- **Supported cleanly:** immutable objects, append-only fact log, crash windows, carry-across-transition recovery (the staged `CarryQueued` → drain before `into_live`), escrow sidecars, peer-repair/scrub/quarantine loop, `Release`-read subset pinned by freeze tests. The worst v0.3 gap is not protocol but operability: content-recovery snapshot authoring is **public at the library layer only, with no CLI/daemon/embed entry point** (`ROADMAP.md` v0.3: "no sanctioned way back until v0.3"), so the v0.3 thesis rides on an unshipped workflow. No architectural blocker — a CLI surface over the existing `Engine::author_recovery_snapshot` suffices. The *recovery-snapshot authoring path* should be exercised through the same mutation queue as other commits (see F2).
- **Genuinely blocks safe unattended operation:** F1 (unbounded retention), F3 (orphan accrual), and the single-machine divergence between the mount lifecycle and the headless lifecycle (F4). All are dogfood-correctness issues, not new-protocol issues.

### v0.4 — cross-platform desktop product

*Failure modes to avoid, in order:*
1. *"UI talks to FUSE"* — the roadmap's test requires the UI to talk to the owner interface instead. Only the app host serves that interface (F4). If the desktop host is the app, this holds; if any desktop composition runs the mount, it fails. **Decision needed before UI code: which binary owns the drive in the desktop product?** The owner socket + pause/resume + observation surface is otherwise complete (wire types are exhaustive destructures, versioned `v: 1`, peer-credentialed).
2. *Member-only sharing with no role picker* (`epochs.md` OD-15-A/B): already decided and enforced; the reader role stays queryable but unadvertised. No architectural pressure.
3. *Conflict indicators (detection only; resolution is v0.5):* the DAG supports this natively (heads + path-level differencing in `wyrd-namespace` `MergedDir`/`Conflict`). The risk is F5: a conflict-UI composer hand-installing heads. The `Head` + `NamespaceView` abstraction is genuinely product-neutral (no FUSE types in the view signatures) — desktop benefits from the `wyrd-namespace` separation without change.
4. *Process/credential differences across platforms:* `architecture.md` fixed scope already names this (Windows out of v0.4, platform audit before native presentation). The app's env credential is developer scaffolding (F9); production desktop should consume the file-reader contract. No protocol change needed.

### v0.5 — history and conflict resolution

*Supported cleanly:* snapshots are immutable, retained, and addressable; `snapshot heads` lists superseded/stranded with classification. Browsing arbitrary history is a **view problem, not a format problem** — the tree/schema supports it today; only the live projection restricts to eligible heads. Resolution authoring as a merge snapshot over all heads already works architecturally (epochs.md); only navigation/merge UX is missing.
*Future-break risk:* F5 is maximized here. A history composer that starts serving historical heads through the same `DriveView` will conflate "installed" with "eligible" unless the `EligibleHead` type change (§3 F5) lands *before* the first historical view. This is the single easiest provocation to decide now in docs before code: history views must be a separate constructor/namespace.

### v0.6 — mobile synchronization

*Supported cleanly:* protocol semantics are consumer-independent (roadmap project rule); `wyrd-embed` + `wyrd-fuse` (transport-free view) demonstrate the shape; `Mailbox` trait admits a wake-sleep implementation.
*Concrete retarders:* F7 (no second `Mailbox` proof; `LiveMailbox` assumes a persistent process); `FsObjectStore` + `LOCK` flock are POSIX filesystem assumptions with no storage-capability abstraction (`architecture.md` fixed scope: "credential loading, file locking, storage durability, process ownership, filesystem presentation" audited together before Windows — mobile is the same class of audit); resource budgets are process-lifetime ceilings, not per-wake budgets (mobile needs the latter). None blocks starting v0.6 design, but each *is* a redesign if discovered inside mobile lifecycle code rather than via the `wyrd-embed` wake-sleep contract. The cheapest insurance is F7's correction (fake-mailbox lifecycle case) before any mobile target.

### v0.7 — production vault infrastructure and disaster recovery

*Supported cleanly:* vaults are ordinary protocol participants (keyless replica restricted by the trust matrix); `wyrd vault` runs as the shared headless owner; escrow records + custody contracts make root-recovery design space compatible with the epoch model.
*Concrete gaps that are design, not just code:*
- **Escrow sidecars are not replicated** ("vault replication of records rides later transport work," `trust.md` T13; `crash-consistency.md` escrow section). A disaster scenario that loses the owner device loses the sidecars the escrow exists to survive — until the replication leg lands, escrow is local custody, not disaster recovery. v0.7's existence depends on that sentence being scheduled.
- **Vault process vs vault role is a live confusion hazard.** The contract distinguishes role (local intent, one string in a `role` file, not replicated) from process (`wyrd vault` as a way to run it) — but discoverability (`wyrd policy role status`, CLI help, docs) must keep them distinct under incident pressure. No architectural blocker; the distinction is documented at the right layer (`sync-and-peers.md` role sections) and enforced as "never replicated, never authorization" (a `Role` file is not a fact-log entry — verified `role_path` beside `keystore`, `role.rs:194`).
- **Guardian/Shamir/root reconstruction is reserved, not modeled.** The membership-log "reserved fields" promise means the epoch model accommodates guardians; the *authorization* of a guardian-assisted reconstruction (who may restore root custody and mint the post-recovery capability, cross-checked against the chain) is a v0.7 design input that must exist on paper before the code. This is the only genuine "decide in documents before code" item on the list.

### v0.8 — protocol compatibility and security freeze

*Exit condition:* no consumer-facing change requires moving identity/serialization/authorization/epoch semantics.
*Items that threaten the freeze if left open:*
1. **Fact-payload versioning** (open to v0.9.0 milestone, `upgrade-contract.md` Inv 2 exception: *"payloads are versioned before the v1 freeze … replay normalizes only the envelope layer"*). The freeze requires deciding the payload versioning *now* (or explicitly freezing "envelope-only replay" as the v0.8 compatibility claim — either is discussable; ambiguity is not).
2. **Capability negotiation** ("`CONTROL_VERSION` equality is the pre-v1 gate; before v1 it grows into advertised capabilities," `upgrade-contract.md` Inv 5) — absent; release-read v1 is pinned as the floor it must never go below. The negotiation envelope is a protocol decision for v0.8 paper, not a surprise.
3. **First-wins mint propagation** (F6) — a protocol decision for the freeze (escrow-preference in minting vs. documented acceptance). The freeze review needs the malicious-member scenario on paper; the current docs assert it is future work, which is correct for a freeze review to *settle*.
4. **Retention/refusal wire semantics** — F1's correction is deliberately local (no wire signal, per `retention-constraints.md` Inv 6: no control-plane refusal in v0.3). The freeze must bless that scoping or design the wire form before v1.
5. **NIP-46 session negotiation** (the only custody mode with domain enforcement). The freeze review ("full threat-model review," `ROADMAP.md` v0.8) must state whether local-key mode is the frozen custody story or until when it is not. All three are manageable; none is a rewrite.

### v0.9–v1.0

No new subsystem by hard rule. The listed gaps are all remediable before v0.9: F1/F3 (local accounting + gate), F4 (socket in the shared composition), F5 (history separation), F7 (wake-sleep mailbox proof), F9 (credential normalization). GC stays post-v1 by decision (`retention-constraints.md`: what v0.x preserves so GC stays possible) — respected here, not proposed.

**Genuine blockers vs not-yet-implemented:** F1 and F4 are the only items that materially block a *stated* capability on a near horizon (unbounded retention blocks safe unattended vault operation; socket scope blocks any mount-based desktop composition). NIP-46, guardians/Shamir, GC, capability negotiation, and mobile wake-sleep are specified-or-absent features, not blockers of today's operating story, and are labeled as such above.

---

## 6. Remediation sequence

### Release blockers (before the next dogfood cut / v0.3 completion)

1. **Receive-side admission gate (F1) + charged orphans (F3).** Implement the vault-seeing counter, enforce the quota pre-import on the fetch leg, and expose the knob through the shipped composers (`wyrd vault`/`mount` flags or config). Land the four refusal acceptance tests. This closes the stated boundedness contract; everything else is local.
2. **Decide the desktop owner binary (F4).** One paragraph in `ROADMAP.md`/architecture: desktop runs the app host (which serves the socket). Then either (a) bind the owner socket in the shared composition for both mount *and* headless, or (b) explicitly mark `wyrd mount` as non-product (not desktop) so the roadmap's "UI talks to the interface" has an unambiguous owner. Ship the mount documentation change if (b).
3. **Retire or route `put_file`/`remove` (F2).** Route them through the live mutation path or restrict to `#[cfg(test)]`. No new behavior; just remove the second authority from the public surface.

### Before v0.4 UI code

4. **History separation (F5).** Land the `EligibleHead` (or equivalent) type split before the first composer that serves non-live heads, so v0.5 history work cannot conflate installed with eligible.
5. **Wake-sleep mailbox proof (F7).** A `wyrd-embed` lifecycle test with a fake mailbox — no persistent subscription — asserting the engine converges on mailbox delivery alone. Discovers whether the trait shapes the assumptions, cheaply.

### Before v0.8 freeze paper

6. **Settle the wire/version decisions:** fact-payload versioning, capability negotiation floor (release-read v1 as floor), first-wins mint (F6 escrow-preference or documented acceptance), retention-refusal scoping (no wire in v0.3; decision for the freeze). Each is a document change with a specified code tail; none should be discovered during freeze review.
7. **Credential normalization (F9)** and **architecture prose reconciliation (F8)** — docs + small guards so the freeze review audits one custody story and one dependency diagram.
8. **Escrow replication leg** scheduled as an explicit v0.7 protocol task (the sentence "rides later transport work" becomes a tracked obligation with a wire identity, not a parenthetical).

### Deliberately deferred (not now)

- Vault/resident-refusal (B) advertisement wiring beyond the accounting; the docs already label its status unverified ("'residency' in code currently means materialization possession, not refusal-to-advertise") — inspect the advertisement paths first, don't invent semantics.
- Cross-platform presentation (Windows) and OS credential-store integration — out of v0.4 by fixed scope; the platform audit is the right vehicle.
- Relay-node `put_file` budget symmetry beyond gating — DG-1 already scopes waiting budgets; no change without evidence.

---

## 7. Coverage limitations

- **No execution.** No `cargo check`, `cargo test`, or binary was run; claims about test *coverage* rest on test *names and sites* read in source, not on green/red evidence. Where the review asserts a test exists, it asserts the source contains it; where it asserts a behavior, the behavior is the code's statically readable semantics.
- **Not line-by-line.** 173k lines were navigated by contract-guided sampling (layer policies, owner/security/token surfaces, publication/mutation/retention paths, CLI/App composition). The FUSE backend, the full fetch/intake pipeline, `transport/` Nostr plumbing, bulk/iroh serving, and the Nix/Flake distribution beyond its two structural tests were sampled, not audited.
- **No runtime probing.** Crash windows, lock behavior across restarts, peer convergence, relay delivery guarantees, and perf pressure were reasoned from the crash/restart-equivalence tables and their named tests; no fault was reproduced.
- **Review-output quarantine honored.** Only source, contracts, `architecture.md`, `ROADMAP.md`, and this checkout's non-review files were used. Prior reviews were not read.

---

## Appendix — checked-and-cleared (attempted to disprove)

- **Privilege already at least as strong as expected.** `FsStoreError::IdentityMismatch`/`Corrupt` carry no identity (deliberate anti-leak, `error-conventions.md`); `insert_verified` keeps verification inside the store; `Discarded`/`Absent`/`NowValid` are narrow. No violation.
- **Protocol/core independence (A):** `wyrd-format` has no networking/async/FUSE edges; `wyrd-contracts` proves the view crate's transitive closure reaches neither `wyrd-sync` nor `iroh*`; conflict UI + time-travel are view-layer extensions over the DAG without format changes. Satisfied, with the caveat that core's `LiveMailbox` coupling narrows the "embeddable" claim (F7).
- **Manifest ownership split (AGENTS.md placement rule):** schema + canonical plaintext in `wyrd-format/manifest.rs` (identity), encryption + StorageId + capability semantics in `wyrd-sync` (`seal.rs`, `keys/`). Decision 25's prose once contradicted the code; the decision was corrected (object-model D25) and the manifest/ContentId-over-plaintext rule is implemented (`seal.rs:132-144`). Satisfied.
- **Gossip/authorship:** snapshots bind to TransitionId + epoch; only `wyrd-sync` may mint `AuthorizedSnapshot` (single `from_verified_unchecked` call site, mechanically counted). Satisfied.
- **Owner auth:** peer-credential check is `SO_PEERCRED`/`getpeereid`-based, fail-closed, uid-equality at bind; socket is bind-once under the store lock; stale-socket reclaim is under the same lock. Platform-compile-fails on non-Linux/macOS Unix. Satisfied *for the runs that bind it* (see F4).
- **Retirement/reconciliation:** retirement reserves live ONLY on durable-evidence statements (forget condition), with the explicit safety invariant *"optimizations may not enlarge what is considered reconciled"* and tests that no test retires an obligation the reference comparison would keep. The synthesized announcement queue and the 32-sends-per-statement cap honor the operational bound without weakening it. Satisfied.
- **Upgrade invariants 1/3/4/8/9** are structural (content-addressed immutability, derived-state rebuild, repr-separability, interrupted-upgrade reopen) and pinned by fixture replays (`upgrade_previous_release_store_replays`, etc.). Invariants 2 (payload clause) and 5 (version matrix) are honestly named as open/ignored until fact payloads are versioned and negotiation lands — stated as gaps, not claimed as coverage. Satisfied as disclosed.
