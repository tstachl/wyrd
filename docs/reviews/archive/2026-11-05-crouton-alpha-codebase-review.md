# Wyrd Adversarial Codebase Quality Review — 2026-11-05 (crouton-alpha)

Reviewing model slug: crouton-alpha
Branch: pr/crouton-review
Commit: 438a2f3dd2464049d38e487b2fc2baf659fed13a
Date: 2026-11-05

## 1. Baseline

**Commit and tree.** Reviewed at commit `438a2f3dd2464049d38e487b2fc2baf659fed13a`
(`chore(gate): update to 940d3c and runner approver`, Nov 4 2026), branch
`pr/crouton-review`. `git status --short --branch` at review start was clean
(`## pr/crouton-review`, no dirty files). `git rev-parse HEAD` matched the
task commit. The review file itself is the only intended workspace change and
is left uncommitted.

**Crates and major modules inspected.** All 10 workspace members:
`wyrd-format`, `wyrd-sync`, `wyrd-namespace`, `wyrd-fuse`, `wyrd-core`,
`wyrd-daemon`, `wyrd-cli`, `wyrd-contracts`, `wyrd-embed`, `wyrd-app`
(399 `.rs` files, ~214,000 lines including tests). Inspected in depth:

- `wyrd-format`: `fs_store.rs` (store layout, crash discipline, stamp
  fast-path, sweep vs. retained walk), `store.rs`, `tree.rs`, `durable.rs`,
  `identity.rs`, error taxonomy (`FsStoreError`, `StoreFailure`).
- `wyrd-sync`: `lib.rs`, `serving/endpoint.rs`, `runtime/engine/` (authoring,
  carry, fetch), `transport/mailbox.rs`, `keys/`, `durable/store.rs`,
  `runtime/fetch/mod.rs`, `closure.rs`, `authorization/`, `membership/`.
- `wyrd-namespace` / `wyrd-fuse`: view value types, verified-head boundary,
  view grammar.
- `wyrd-core`: `node.rs`, `live/mod.rs`, `live/pass.rs` (`sync_pass`),
  `live/apply.rs`, `live/config.rs`, `mutation/` (`queue.rs`, `batch.rs`,
  `kind.rs`, `parents.rs`), `session.rs`, `want.rs`, `scrub.rs`,
  `quarantine.rs`, `wake.rs`, `policy.rs` (including `FakeView` test double),
  `status.rs`, `vault.rs`, `export/{mod,staging,walk}.rs`, `mailbox/`.
- `wyrd-daemon`: `vault.rs`, `owner/wire.rs`, `owner/server.rs`,
  `fuse/backend/{fold,ops,attr}.rs`, `fuse/inode.rs`, `posture.rs`,
  `lifecycle.rs`.
- `wyrd-cli`: `main.rs` (dispatch, `CliError::exit_code`), `commands/mount.rs`,
  `commands/sync.rs`, `args.rs`, `update/supervise.rs`.
- `wyrd-contracts`: `layer_contracts.rs` (DAG policy, nostr scanner,
  link-graph closure, proof-mint, KDF posture, release checks),
  `node_contracts.rs`, plus contract-test composition over `MemoryObjectStore`
  vs. `FsObjectStore`.
- Docs/contracts read as source of truth: `docs/architecture.md`,
  `docs/contracts/error-conventions.md`, `docs/contracts/write-path.md`,
  `README.md` (Status), `ROADMAP.md` (milestone table), `docs/design/cli.md`
  (diagnostics/exit codes). The architecture table, error rule, and write-path
  pipeline were treated as normative per `docs/architecture.md` ("the contracts
  below win any conflict").

**Build configurations.** Workspace resolver 2, toolchain 1.91.0
(`rust-toolchain.toml`). Features: only `wyrd-daemon`, `wyrd-cli`,
`wyrd-contracts` have features (`default = ["mount"]`, `mount =
["dep:fuser", "wyrd-daemon/mount"]`); all other crates build feature-free.
`insecure-fast-kdf` in `wyrd-sync` is dev-only (enabled solely via downstream
`[dev-dependencies]`). Nextest profiles in `.config/nextest.toml`:
`local` (edit loop), `default` (PR gate), `slow` (master supplement).
CI: `.ngit/nix/workflows/rust.yaml` (PR), `rust-slow.yaml` (master-only),
`nix.yaml`, `shell.yaml`, builds in `.ngit/nix/builds/default.nix`. Local hooks
in `devenv.nix`, flake `checks` in `flake.nix`, gate tasks in `gate.toml`.

**Checks actually run (and results).**

- `cargo check --workspace --all-targets` — PASS (28.0s).
- `cargo check --workspace --no-default-features` — PASS (18.3s).
- `cargo clippy --workspace --all-targets -- -D warnings` — PASS (11.2s).
- `cargo clippy --workspace --all-targets --no-default-features -- -D warnings`
  — PASS (13.9s).
- `cargo tree -e normal,features --workspace` grepped for `insecure-fast-kdf`
  — 0 hits (normal graph free, as CI asserts).
- `cargo nextest run --profile local -p wyrd-format` — 174/174 PASS (2.6s),
  sampled only.
- `cargo nextest list` counts (not runs): local 1,939 tests, default 2,150
  tests, slow 20 tests. These are `wyrd-*`-prefixed list entries, not measured
  suite times.

Not run for this review: full `cargo nextest run` (default or slow), doctests,
`cargo audit`/`deny`, Lima E2E, Incus/microVM gate E2E, any Linux/KVM legs.
No performance measurements were taken. Where a claim depends on unrun
evidence it is marked unverified below.

**Conventions used as evidence.** Current organization, module docs, and
contract comments were treated as claims to verify, not assumptions. Large
files, repeated code, and unfamiliar patterns were not flagged without a
demonstrable consequence. Every finding below includes an attempted
falsification note.

---

## 2. Concise assessment

Wyrd at this commit is unusually defensible for a pre-alpha Rust codebase:
error discipline is enforced by a normative convention, layering is
mechanically pinned by contract tests (not review vigilance alone), the
write-path pipeline makes durability/visibility/announceability order
explicit, and test lanes separate edit-loop, PR-gate, and expensive evidence
with honest budgets. The strongest structural properties are real:

- Non-test code returns typed `thiserror` errors; the 8,225 `.unwrap()` /
  1,420 `.expect()` counts are confined to `#[cfg(test)]` modules, test
  helpers, and three justified production `expect`s (bounds-checked decodes,
  a saturating closure, a delivery-ID guard). No production `todo!`.
- `wyrd-contracts` fails closed on any new member/edge/external dep,
  treats unresolvable edges as leaves only after the direct check fails
  closed, and pins the `unsafe` proof-mint count, the `nostr` sandbox, the
  `wyrd-fuse` link-graph, and KDF edges in one suite that the PR gate runs.
- The live loop (`live/pass.rs::sync_pass`, 560 lines) is long because it
  *is* the ordering contract — drain, quarantine, admit, walk, drain,
  fetch, mutate, publish — commented step by step against
  `docs/contracts/write-path.md`. That length is justified; splitting it
  would hide the order it exists to make reviewable (see Evaluated, §9).
- Feature posture is mostly sound: the workspace `wyrd-daemon` dep carries
  no defaults, each crate re-enables `mount` explicitly, and
  `insecure-fast-kdf` is dev-only with both a manifest check and a
  `cargo tree` backstop.

The reviewable defect risk is concentrated in a small number of places where
a failure mode is *coherent-looking but semantically empty*: poisoned locks
report as healthy emptiness, the object-store stamp fast-path reports success
without healing, the mount-free build reports green in CI without ever
compiling, and the edit loop reports green while silently dropping any
future test whose name matches a broad substring. Each is small, each has a
small fix, and each currently fails in the direction of false confidence
rather than loud failure. The ranked findings below prioritize those.

---

## 3. Ranked findings

### F1 (C1) — Poisoned locks report as healthy empty state; diagnostics cannot distinguish idle from stalled

**Classification:** C1, Correctness or contract risk. Confidence: **High**
(code-verified paths). Impact: **Medium** (requires a panic while a lock is
held; consequence is silent wrong diagnostics plus leaked accounting and
opaque EIOs, not silent data corruption — reads still fail closed).

**Evidence (exact symbols).**

- `crates/wyrd-core/src/session.rs:177-179`
  `WriteBudget::total()` → `self.lock().map(|s| s.total).unwrap_or(0)`.
  `dirty_handles()` at `session.rs:183-185` → `unwrap_or(0)`.
  `release()` at `session.rs:134-140` and `release_fold()` at
  `session.rs:170-174` → `if let Ok(mut state) = self.state.lock()` else
  silently drop. Contrast `reserve()` at `session.rs:113-130` and
  `reserve_fold()` at `session.rs:151-162`, which fail closed via
  `self.lock()?` → `BudgetError::Lock` (`session.rs:187-189`).
- `crates/wyrd-core/src/want.rs:209-214` `peek_pending()` → empty `Vec` on
  poison; `is_admitted()` at `want.rs:268-273` → `unwrap_or(false)`;
  `waiter_count()` at `want.rs:280-285` → `unwrap_or(0)`;
  `take_reopen_notes()` at `want.rs:311-318` → empty `Vec` on poison.
  Contrast `register()` (`want.rs:133-145`) and `register_unowned()`
  (`want.rs:157-168`), which raise `WantError::Lock`. The in-between ops
  (`release()` `want.rs:176-198`, `mark_admitted()` `want.rs:223-233`,
  `retire_where()` `want.rs:248-264`, `note_reopen_demand()`
  `want.rs:296-304`) silently no-op on poison, each with a local comment
  ("the next registration re-demands, and the loop never blocks on registry
  health").
- `crates/wyrd-core/src/scrub.rs:109-113` `submit()` drops on poison;
  `pop()` at `scrub.rs:117-125` → `None`; `len()` at `scrub.rs:128-130` →
  `unwrap_or(0)`; `pending()` at `scrub.rs:139-144` → `unwrap_or_default()`.
  Identical shape in quarantine (`crates/wyrd-core/src/quarantine.rs:141-145`,
  `149-157`, `160-162`, `171-180`).
- `crates/wyrd-core/src/mutation/parents.rs:135-151` `capture()` →
  `.ok()?` (poison reads as "blocked/over-limit" `None`);
  `validate()` at `parents.rs:153-158` → `.ok().is_some_and(...)` (poison
  reads as `false` = stale token); `invalidate_subtree()` at
  `parents.rs:160-175`, `invalidate_all()` at `parents.rs:177-185`,
  `publish()` at `parents.rs:187-193` silently return on poison.
- Inconsistent recovery elsewhere: `WakeSignal` (`crates/wyrd-core/src/wake.rs:63,100`)
  and `MutationQueue` (`crates/wyrd-core/src/mutation/queue.rs:136,179,404`)
  recover via `unwrap_or_else(|p| p.into_inner())`, treating the state as
  plain data. `SharedStore` maps poison to `SharedStoreError::Lock` (and
  proves it in `wyrd-format/src/store.rs::shared_store_maps_poison_to_lock_error`).
  `LiveNode` maps view/store poison to `LiveError::Lock`
  (`crates/wyrd-core/src/live/mod.rs:522,549,837,847,858`).

**Concrete defect / friction.** A poisoned registry, budget, or queue
reports exactly what a healthy idle one reports: zero buffered bytes, zero
dirty handles, zero pending wants, empty repair queues, no parent tokens,
no admitted marks. An operator (or a `SyncReport`/status consumer) cannot
distinguish "nothing to do" from "the bookkeeping is dead". The failure is
also *leaky by construction*: `WriteBudget::release()` and `release_fold()`
drop the decrement on poison, so bytes stay charged forever, while
`total()` simultaneously reports 0 — the two halves of the invariant
(conserved aggregate, honest introspection) contradict each other on the
same path. `WantRegistry` fails a different way: `peek_pending()` empty on
poison makes the pass skip admission, so demand stalls while every count
still reads healthy-empty. `ParentRegistry::capture()/validate()` turning
poison into `None`/`false` causes creates to refuse as `StaleParent` with
no lock-poison signal anywhere on the channel.

**Realistic bug example.** A FUSE backend thread panics while holding the
`WriteBudget` mutex (e.g. a formatting bug in a refusal-translation path).
From then on: buffered writes fail `ENOSPC` via `BudgetError::Lock` (via
`reserve()`), so the user sees spurious ENOSPC; `total()`/`dirty_handles()`
report 0/0 so dashboards read "clean"; `release()` on close silently keeps
the leaked bytes charged, so a *new* `WriteBudget` is the only recovery,
but nothing in the reports says the old one is dead. The same panic under
`WantRegistry` stalls fetch-on-open demand behind an empty peek while
`is_admitted()` reports false and `waiter_count()` reports 0, so the run
looks like "no demand" rather than "demand apparatus wedged".

**Why existing types/tests do not prevent it.** Types deliberately allow it:
`total()`, `dirty_handles()`, `peek_pending()`, `is_admitted()`,
`waiter_count()`, `len()`/`pending()` are all infallible (`usize` / `bool` /
`Vec`) and documented as "Diagnostics only, never semantics". The local
comments rationalize each site individually (fail-closed-without-blocking,
release paths must be infallible), but no contract owns the *global* rule:
error-conventions' "fail closed, then restore" and "raise vs count" do not
say when an infallible diagnostic may mask poison. Tests: `MutationQueue`
has `poisoned_state_lock_is_recovered`
(`crates/wyrd-core/src/mutation/tests_queue.rs:312-333`) and
`SharedStore` has a poison-mapping test; **no poison test exists for
`WriteBudget`, `WantRegistry`, `ScrubQueue`, `QuarantineQueue`, or
`ParentRegistry`** (grep finds only `tests_poison.rs` flood/restore tests,
which exercise *poison-message* handling in the mailbox, not lock poisoning).
So a refactor that widens the silent-drop surface (e.g. adding another
`unwrap_or(0)` accessor) stays green.

**Falsification attempt.** I checked whether all these accessors are
truly diagnostics-only. `peek_pending()` is *not* diagnostics: the live
pass gates admission on `!self.wants.peek_pending().is_empty()`
(`crates/wyrd-core/src/live/pass.rs`, admit-wants block), so its empty-on-
poison behavior directly suppresses admissions. `total()` is consumed via
budget accounting in the daemon fold path (reserve/lease), not just shown.
That strengthens the finding: the mask touches semantics, not just display.
I also checked whether poison is reachable: any thread panicking while
holding the lock poisons it; the codebase itself defensively handles poison
in several places, acknowledging reachability. Whether a *current* panic
path exists end-to-end (holder panics mid-call) is unverified without a
fault-injection run, so reachability is marked as **plausible but
unverified**; the defect (empty ≡ poison) holds regardless once poisoned.

**Smallest useful correction.** Do not change the fail-closed mutating ops.
Make the masking visible with the smallest API delta:

1. `WriteBudget::total/dirty_handles`, `WantRegistry::peek_pending/
   is_admitted/waiter_count/take_reopen_notes`, `Scrub/QuarantineQueue::
   len/pending`, `ParentRegistry::capture/validate` — return `Option` /
   `Result` so callers must name the poison case, **or** keep infallible
   signatures but add an explicit poison counter / `tracing::error!` on the
   silent paths (drop-path logs at `release`, `release_fold`, `submit`,
   `mark_admitted`, `peek_pending`). The `Result` variant is preferred
   for `peek_pending`/`capture`/`validate` because they gate semantics;
   the counter variant is acceptable for pure diagnostics (`len`, `total`).
   Keep `release`/drain-drop infallible, but log/count there too — currently
   the most silent path in the whole design.
2. State the global rule once in `docs/contracts/error-conventions.md`
   (§Fail closed, then restore): "Infallible diagnostics never mask
   poison; if a lock poisons, diagnostics either return `Err` or bump a
   counter the status surface can render."

**Regression test / guardrail.** Mirror
`poisoned_state_lock_is_recovered` for each masked type: spawn a panicking
holder, then assert (a) mutating ops still fail closed (`Err(Lock)`),
(b) diagnostics do *not* read as healthy (`Err` or counter > 0 / log
capture), (c) no silent accounting leak (`release` after poison leaves a
visible deficit, not 0). Add a `layer_contracts`-style source check only if
the team prefers a mechanical pin to new `unwrap_or(0)`-on-lock accessors;
the per-type tests are the primary guard.

---

### F2 (C1) — `FsObjectStore::insert` stamp fast-path can report success without healing mtime-preserving corruption

**Classification:** C1. Confidence: **High** on the code path; **Medium** on
field frequency. Impact: **Medium** (availability / contract-truth, not
integrity — `get` still fails closed, so corrupt bytes are never served).

**Evidence.** `crates/wyrd-format/src/fs_store.rs`:

- Module header (`fs_store.rs:17-24`): "Re-inserting identical content is a
  no-op when the live bytes still derive to the requested id, and heals them
  when they don't. A bounded in-process map of length-plus-mtime stamps skips
  the revalidation read …; any drift falls back to the full re-derive, and
  `get` retires a stamp when it observes corruption. The stamp assumes the
  filesystem reports mtime changes (coarse granularity weakens it): bitrot
  that leaves length and mtime untouched is invisible to it and is caught
  by the full re-derive (or by `get`)".
- Field comment (`fs_store.rs:128-134`): "Bitrot that leaves length and mtime
  untouched is invisible to this signal alone and is caught by the full
  re-derive **whenever the stamp is absent or stale**" (emphasis added).
- `insert()` (`fs_store.rs:497-568`): computes `stamped` from
  `stamp_for(&id).matches(meta)`; if `stamped` is true, `valid = stamped`
  short-circuits the `fs::read` + `ContentId::derive` full re-derive; if
  `valid`, the `else` branch only verifies the directory fsync and
  re-remembers the stamp — no heal, returns `Ok(id)`.
- `get()` (`fs_store.rs:583-610`): on hash mismatch, `self.forget(id)`
  retires the stamp and returns `FsStoreError::Corrupt` (fail-closed).
- Existing test `silent_corruption_heals_on_insert_after_get`
  (`fs_store.rs:1023-1050`): writes tampered bytes, **restores mtime**
  via `set_modified(pristine)`, asserts len+mtime equal, then does
  `get` (fails, retires stamp) **then** `insert` (heals). Sibling
  `reinsert_heals_corruption` (`fs_store.rs:1006-1020`) uses plain
  `fs::write` (mtime bumps, stamp misses, heal works).

**Concrete defect.** The header's claim "bitrot … is caught by the full
re-derive" is false for the insert-first order. Media bitrot characteristically
preserves length and mtime (mtime changes only on metadata updates via
syscalls, not on silent media flips), so the stamp still matches. If the next
operation is `insert` (re-authoring the same file, re-importing the same
chunk) *before* any `get` observes the rot, `insert` takes the stamped branch,
skips the read, returns `Ok(id)`, and leaves corrupt bytes under the live
name. The writer is told "healed" when nothing changed. Only a `get`-before-
`insert` sequence heals (because `get` retires the stamp). The field comment
actually draws the boundary correctly ("whenever the stamp is absent or
stale") — the module header overclaims relative to it.

**Realistic bug example.** A vault block silently flips a byte in a chunk
object file. The drive re-authors the same logical file (same content, same
`ContentId`) from another device; the local node calls `insert(kind, data)`
with the pristine bytes to publish the snapshot. `insert` sees length+mtime
match, no-ops, returns success, the snapshot commits. The next reader calls
`get` and gets `Corrupt` → quarantine → discard → re-fetch. Correctness is
preserved end-to-end (nothing corrupt is served; F1's repair loop works), but
the write path's "heal on re-insert" contract — the reason snapshots can be
authored over a bitrotted store without an explicit repair step — silently
did not happen, and the operator's log shows a clean insert.

**Why existing types/tests do not prevent it.** `insert()` returns
`Result<ContentId>` with no `Healed` vs. `Noop` distinction, so callers cannot
tell a verified no-op from a stamp-skipped one; the only accounting is
`#[cfg(test)]` `CacheStats` (`stamp_hits` vs. `full_checks`). The tests prove
the two heal orders that work (plain corrupt, corrupt-after-get) but not the
corrupt-then-insert-first order, which is exactly the order that fails.
Nothing in `ObjectStore`'s trait (`crates/wyrd-format/src/store.rs`) requires
`insert` to verify on the stamped path — the heal promise lives only in the
module header prose.

**Falsification attempt.** Could the stamp be sound because media corruption
*does* update mtime on these filesystems? No: mtime is VFS metadata updated by
write/truncate/utime syscalls; a torn write or silent flip at the block layer
does not touch it. The test itself (`silent_corruption_heals_on_insert_after_get`)
proves the team models mtime-restored corruption as the hard case. Could
`insert` be safe because the stamp is only remembered after verification?
After the first (clean) publish, the stamp *is* remembered (`insert` else/
publish branches call `remember`). So the second (bitrotted) insert is the
vulnerable one. Frequency (how often bitrot preserves mtime *and* is followed
by insert-before-any-read) is unverified — hence Medium impact, not higher.

**Smallest useful correction (pick one, document the other).**

- **(a) Tighten code (preferred if the heal promise is load-bearing):** do
  not short-circuit `insert` on stamp match; use the stamp only when the
  caller already knows the object is verified (i.e. keep `insert` always
  re-deriving on existing files, or only skip when the stamp was taken from
  a `get` in this process — track a separate "get-verified" set). Document
  the perf cost in the struct comment.
- **(b) Tighten contract (cheaper, honest):** rewrite the module header to
  state "insert heals corrupt-by-mtime-drift; mtime-preserving rot is healed
  only after a `get` retires the stamp; insert-first over such rot reports
  success without healing — `get` still fails closed". Then add the
  discriminating test below asserting the *actual* (documented) behavior so
  the boundary cannot silently move.

**Regression test.** `insert_over_mtime_preserving_corruption` — insert
`b"pristine"`, overwrite with same-length tampered bytes, restore pristine
mtime via `set_modified`, call `insert(b"pristine")` **without any
intervening `get`**, assert (under (a)) `get` returns pristine afterwards
(healed), or (under (b)) `get` returns `Corrupt` and the test name/docs
record the insert-first gap. The current suite passes without it; after
either fix it must fail if the behavior regresses.

---

### F3 (C2) — Mount-free (`--no-default-features`) build is never compiled in CI

**Classification:** C2, Test/validation gap. Confidence: **High**. Impact:
**Medium** (the mount-free build is a shipped configuration — "a machine
without system libfuse can still build and run everything above the mount",
`crates/wyrd-daemon/Cargo.toml` — and CI never proves it).

**Evidence.**

- `.ngit/nix/builds/default.nix:79` `rustCheck`: `cargo check --color never`
  (default features) + `cargo tree` grep. `:96` `rustClippy`:
  `cargo clippy --workspace --all-targets --color never -- -D warnings`
  (default features, all targets). `rustTests`/`rustSlowTests`/`rustDoctests`
  likewise run the default profile (mount on). No derivation passes
  `--no-default-features`. `.ngit/nix/workflows/rust.yaml` and
  `rust-slow.yaml` realise only those derivations.
- `devenv.nix:45-57`: local clippy hook runs **both**
  `cargo clippy --workspace --all-targets` **and**
  `cargo clippy --workspace --all-targets --no-default-features`, with the
  comment "Mirrors CI (`workflow.yml`): … in both feature configurations."
  There is no `workflow.yml` anymore (the pipeline is the `.ngit/nix`
  port — `rust.yaml` header says "Mirrors the act rust job step for step"),
  and the mirror claim is now false: the hook checks one more
  configuration than CI does. The comment itself records why both matter:
  "A mount-gated test import once shipped green locally and failed CI
  exactly here, because the default hook checked neither test targets nor
  the `--no-default-features` build."
- The gating it protects is delicate and documented as load-bearing:
  `crates/wyrd-daemon/Cargo.toml` ("`pub mod fuse` below is gated on the same
  feature, so no mount-only name leaks into a mount-free build"),
  `crates/wyrd-cli/Cargo.toml` (`pub mod mount` + `Mount` subcommand
  gated), `crates/wyrd-contracts/Cargo.toml` (`fuser` optional, no
  build script on a mount-free build). Verified locally that both
  configurations currently compile (this review ran both `cargo check` and
  both `cargo clippy` variants, all PASS).

**Concrete defect / regression example.** A future change widens a
mount-gated path — e.g. adding `use fuser::Errno` in a `wyrd-daemon`
non-fuse module, un-gating a `pub mod fuse` item, or adding a mount-only
import to a `wyrd-contracts` test that stays enabled without the feature.
Default-feature CI stays green (fuse present, import resolves); the
mount-free configuration, which is the only way a libfuse-less machine
builds, breaks. The failure surfaces at release-packaging time or for
exactly the users the feature exists for, not at PR time.

**Why existing tooling does not prevent it.** Layer-contract checks pin
*dependency edges* (manifest allow-lists), not *feature propagation*:
nothing asserts that `wyrd-daemon/mount` stays off under
`--no-default-features` or that no `fuser` name leaks. The local hook would
catch it, but hooks are opt-in per developer; CI is the gate that runs
without them. The 5-line fix is absent from the derivation set, so the gap
is structural, not a flake.

**Smallest useful correction.** Add two cheap derivations reusing the same
`rustDeps` artifacts (compile-once shape preserved): `rust-check-nomount`
(`cargo check --workspace --no-default-features`) and
`rust-clippy-nomount` (`cargo clippy --workspace --all-targets
--no-default-features -- -D warnings`), and realise them in
`.ngit/nix/workflows/rust.yaml` `gates-fast` alongside the existing steps.
Cost is one extra workspace-crate compile without fuser (measured locally
~14s for clippy); no new deps, no network. Update the `devenv.nix` comment
to point at the new derivation names instead of the retired `workflow.yml`.

**Regression guardrail.** The new derivations *are* the guardrail.
Additionally, a one-off mutation check: temporarily add
`use fuser::Filesystem;` to `wyrd-daemon/src/lib.rs` outside the feature
gate and confirm the new derivation fails while the old one passes; then
revert. (Not performed in this review; proposed as the PR's own evidence.)

---

### F4 (C2) — Local nextest profile's broad substring exclusions silently drop future fast tests

**Classification:** C2. Confidence: **High** (mechanism is certain; harm
depends on future naming, which is the point). Impact: **Medium**.

**Evidence.** `.config/nextest.toml:138` `[profile.local] default-filter`:

- `or test(~heterogeneous)` — excludes *any* test whose name contains
  "heterogeneous", not just the standing cover.
- `or test(~spam)` — the comment at `nextest.toml:15x` justifies this as
  "the only two `~spam` tests, so the substring has no fast collateral".
  Today that holds; the filter cannot know tomorrow.
- Compare `[profile.default]` (`nextest.toml:81`) and `[profile.slow]`
  (`nextest.toml:142`), which for the same heavy tests use full distinctive
  names (`test(~flood_beyond_channel_capacity)`,
  `test(~sustained_spam_commits_bounded)`, …) — the local profile is *less*
  precise than the profiles it subsets. The header's own maintenance rule
  says: "Broad substrings such as `~serving` or `~announc` would sweep those
  up and hollow out the very signal the edit loop exists for, so slow tests
  are named, not categorized." Yet `~spam` and `~heterogeneous` violate that
  rule.

**Regression example.** A future fast unit test named
`spam_filter_classifies_quiet` (or `heterogeneous_path_normalization`) is
added for a correctness fix. It is silently excluded from `cargo fast-*`
(the per-crate edit loop), the author's local signal says green, and the
regression it was meant to catch is only caught at PR time if at all —
or never, if the test itself was the only cover for the fix. The failure
mode is false confidence in exactly the loop developers trust most.

**Why existing tooling does not prevent it.** Nextest filters are string
matches; there is no test that asserts the local filter still runs a
known-good canary. `cargo nextest list --profile local | wc -l` would drop
by one on the accidental exclusion and nothing flags it.

**Smallest useful correction.** In `[profile.local]` only, replace
`test(~heterogeneous)` and `test(~spam)` with the exact default-profile
names (`test(~heterogeneous_relays_cover)`-style full names and
`test(~sustained_spam_commits_bounded)` / the two actual spam tests by full
name). Add a one-line `nextest list` canary to the local hook or a test:
the filter must still list `wyrd-core`'s `posture`/`quiescence` canary
tests (any stable fast test) — assert a floor count so a new broad
exclusion fails loudly. Cost: one list invocation (sub-second).

**Regression guardrail.** Add a new fast test named
`heterogeneous_helper_unit` *temporarily* in a throwaway branch with the
current filter to demonstrate it is dropped, then apply the fix and show it
runs. (Demonstration proposed, not performed here.)

---

### F5 (C3) — Two directory walks must agree "by comment" (`sweep` vs. `retained_bytes`)

**Classification:** C3, Maintenance friction (concrete divergence risk on
the durability/accounting path). Confidence: **High**. Impact: **Medium**
(the startup quota cross-check is the symptom; the cause is silent).

**Evidence.** `crates/wyrd-format/src/fs_store.rs`:

- `retained_bytes()` at `fs_store.rs:299-331` and `sweep()` at
  `fs_store.rs:374-407` each implement a ~30-line `read_dir` stack walk.
  `sweep` deletes `.tmp` entries and (when asked) totals published sizes;
  `retained_bytes` excludes entries whose extension is `tmp` and totals.
  Both recurse on `path.is_dir()`, both tolerate `NotFound` on `read_dir`,
  both propagate other I/O errors via `FsStoreError::io`.
- The coupling is documented only in prose: `fs_store.rs:352-367`
  ("The two totals in this file (here and [`Self::retained_bytes`]) must
  agree, or the startup cross-check cannot diagnose anything. They differ
  only in what they do with an entry: this one deletes temps, that one
  leaves them.") and `fs_store.rs:286-298` ("deliberately a separate walk
  from [`Self::sweep`]. Merging them would make this query delete `.tmp`
  debris, which no reader of a byte count expects"). The second note argues
  against a naive merge — correctly — but neither note is enforced.
- Tests `retained_bytes_counts_new_content_and_reseeds_on_reopen`
  (`fs_store.rs:688-728`) and `retained_bytes_subtract_matches_durable_removal_across_reopen`
  (`fs_store.rs:730-762`) prove counting and seeding agree on simple
  fixtures (fresh objects, reopen). Neither plants `.tmp` debris, symlinks,
  deeply nested fanouts, missing fanout dirs, or unreadable entries, and
  neither asserts `sweep( Some(&mut seeded)) == retained_bytes()` on the
  same tree state.

**Regression example.** A fix to `sweep`'s symlink handling (e.g. "do not
follow symlink dirs") is applied only there; `retained_bytes` keeps
following them (or vice versa for `.tmp` detection — `extension() ==
"tmp"` vs. `extension().is_some_and(|e| e == "tmp")` — same today, but two
spellings to keep in sync). At next boot, `open_with` seeds the accountant
from `sweep`'s walk while `check_startup_retention` cross-checks against
`retained_bytes()`'s walk; the two numbers differ, and the startup
diagnosis ("every write is ENOSPC" vs. quota) points the wrong way. Each
walk's unit tests still pass.

**Falsification attempt.** Is the separation semantically required? Yes —
`retained_bytes` must be read-only (it must not delete `.tmp` debris that
`open` alone owns, per the module rule at `fs_store.rs:14-15`). So the fix
is *not* "just merge them". The finding is that two hand-maintained
traversals encode one invariant (same tree, same recursion, same error
classification, opposite `.tmp` policy) with no mechanical agreement check.
A single walker with an explicit policy flag preserves read-only-ness while
removing the dual-maintenance.

**Smallest useful correction.** Extract a `walk(FnMut(&Path) -> WalkAction)`
helper (actions: count, sweep-temp) so the `read_dir` stack, `is_dir`
recursion, `NotFound` tolerance, and `FsStoreError::io` classification exist
once; `sweep` and `retained_bytes` become policy callers. If extraction is
deferred, add the agreement test below as the interim guard (it is the
higher-leverage half).

**Regression test.** `sweep_and_retained_agree_on_hostile_tree` — build a
store dir with: nested fanouts, one `.tmp` debris file, one symlink to a
fanout dir, one missing fanout entry (simulating concurrent removal),
one unreadable subdir (permission 0, Unix-only, skip on non-Unix). Assert
`retained_bytes()` equals `sweep(Some(&mut n))`'s total for the same tree,
and that `retained_bytes()` did not delete the `.tmp` file. Run it
`-- --test-threads=1`-safe (no shared global state; temp dir per test).

---

### F6 (C3) — Documentation drift that can misdirect implementation (three instances, one pattern)

**Classification:** C3 (low immediate runtime impact, concrete cost of
ordinary changes). Confidence: **High** (all three verified line-by-line
against code). Impact: **Low–Medium**.

Each instance has a clear source of truth (code) and a stale duplicate
(prose). Together they matter because Wyrd's docs are load-bearing —
`docs/architecture.md` explicitly says contracts win conflicts, and
contributors are told to read docs before code.

1. **Sync crate status understates what exists.**
   `crates/wyrd-sync/src/lib.rs:9-14`: "Implemented so far: the control
   plane …, the runtime engine …, the durable fact store, and the
   drive-local serving vault. The iroh endpoint wiring in the daemon and
   the mirror/vault peer roles are the remaining slices."
   Reality: `crates/wyrd-sync/src/serving/endpoint.rs:28-55` implements "A
   real-iroh serving surface over the durable vault", the daemon re-exports
   it (`crates/wyrd-daemon/src/lib.rs:45`, `vault.rs:38,60,285,371,545`),
   and `README.md` Status (lines 93-96) says the "real-iroh serving router
   … are in place and under test". A contributor trusting `lib.rs` would
   scope new endpoint work as if it did not exist, or misread the milestone
   gap. Smallest fix: rewrite the two sentences to name
   `serving::ServingEndpoint` as shipped and keep only the genuinely
   remaining slice (whatever it is — verify against ROADMAP before editing;
   this review marks the exact remaining work as unverified).
2. **CLI exit-code summary omits the lock exception.**
   `docs/design/cli.md:948` (the bullet-list source of truth operators
   script against): "Exit `0` on success; exit `2` on any failure".
   The same file's narrative at `cli.md:579-583` correctly states held-drive
   opens exit `78` (`EX_CONFIG`, supervisor must not retry), and code
   implements it (`crates/wyrd-cli/src/main.rs:209`
   `EXIT_DRIVE_LOCKED = 78`, `exit_code()` at `main.rs:216-224`). The bullet
   list is what `grep exit` surfaces first; a wrapper coded from it would
   retry held drives forever. Smallest fix: append ", except the
   drive-already-owned case, which exits `78`" to the bullet at line 948
   (and keep `main.rs` as the tiebreaker).
3. **Dev-loop comment points at a retired CI file.**
   `devenv.nix:45`: "Mirrors CI (`workflow.yml`)". There is no
   `workflow.yml`; CI is `.ngit/nix/workflows/*.yaml` + derivations in
   `.ngit/nix/builds/default.nix`. See F3 for the substantive half (the
   mirror is also *functionally* false today). Smallest fix: name the
   derivations (`rust-check` / `rust-clippy` in `.ngit/nix/builds/default.nix`)
   and, after F3 lands, add the nomount pair to the mirror claim.

**Why existing checks do not prevent it.** The `doc-links` and `page-check`
hooks verify link resolution and marketing pins, not code↔prose agreement
for status/exit claims. No test greps for the three phrases.

**Regression guardrail.** One small test in `wyrd-contracts` (or the doc
gate) asserting the three strings/behaviors agree: `lib.rs` does not contain
"endpoint wiring in the daemon" + "remaining", `docs/design/cli.md` bullet
mentions `78`, `devenv.nix` names `.ngit/nix/builds/default.nix`. Cheap,
brittle against intentional rewrites (which is the point — force a
conscious re-pin).

---

### F7 (C4) — Micro-duplication worth removing (identical semantics, no coupling)

**Classification:** C4, Cleanup opportunity. Confidence: **High**. Impact:
**Low**.

**Evidence.**

- `Ord` for `ScrubObservation` (`crates/wyrd-core/src/scrub.rs:77-92`)
  and `Ord` for `VerificationFailure`
  (`crates/wyrd-core/src/quarantine.rs:109-124`) are character-identical
  (content bytewise, then `kind.byte()`). `PartialOrd` delegates identically
  in both. The queuing *semantics* differ (F-evaluated below: do not merge
  the drains), but the ordering function has no Wyrd-specific meaning.
- Path tail extraction `path.rsplit('/').next().unwrap_or(path)` appears in
  `crates/wyrd-core/src/live/apply.rs:565,600,638,750` and
  `crates/wyrd-core/src/node.rs:297` (5 sites). Today all five mean "final
  path component for tree insertion". A future separator or normalization
  change must be made in five places; `rsplit('/')` never returns `None`
  (the `unwrap_or` is defensive dead code) which invites misreading.
- `scratch_dir` test helpers with per-file `static SEQ` + `temp_dir().join`
  appear in `status.rs:553`, `vault.rs:152`, `fs_store.rs:669`,
  `durable.rs:282`, `wyrd-contracts/src/support/rig.rs:24`. Harmless but
  the same collision-avoidance dance five times.

**Why this is C4 and not higher.** None of these can drift semantically
today (ordering is pure; tail extraction is local; scratch dirs are test-only
and pid+seq disambiguated). Removing them does not enable correctness; it
removes future edit cost.

**Smallest fix.** One `pub(crate) fn cmp_content_then_kind(a: &ContentId,
b: &ContentId, ka: ObjectKind, kb: ObjectKind) -> Ordering` in
`wyrd-core` (unit-tested once), two `Ord` impls delegating to it; one
`fn final_component(path: &str) -> &str` helper used by all five call sites
(with a `debug_assert!` instead of the silent `unwrap_or`); no change for
`scratch_dir` unless a shared test util already exists (`rig.rs` is the
candidate — move the pattern there if the team prefers). Total diff is
small and reviewable as a single behavior-preserving refactor.

---

## 4. Test-effectiveness table

| Invariant / behavior | Relevant test (file · symbol) | What it proves | What it fails to prove |
|---|---|---|---|
| Directory DAG stays downward; new edges fail closed | `wyrd-contracts/src/layer_contracts.rs` · DAG policy + `production_deps` tests | Manifest edges match allow-lists; alias/workspace-rename resolution; empty-list cannot pass vacuously (explicit `assert!(!members.is_empty())`) | Transitive third-party reachability (explicitly out of scope — `cargo tree` territory); runtime linking of new code paths that use only allowed crates |
| `nostr` confined to mailbox in `wyrd-core` | `layer_contracts.rs` · `code_text` + `line_uses_nostr` + `check_core_nostr_scope` (+ negative pure-function tests) | Textual `use nostr` / `nostr::` outside the sandbox fails; lexer strips comments/strings so prose cannot smuggle or false-alarmp | Lexer edge cases beyond the covered set (hand-rolled Rust lexer; residual cases named at `code_text` — a mis-strip could hide a use). Unverified: full edge coverage not enumerated in this review |
| `wyrd-fuse` never links transport | `layer_contracts.rs` · `fuse_link_graph_excludes_sync_and_iroh`, `fuse_dev_deps_stay_pinned` | Member-declared closure from the view crate reaches no `wyrd-sync`/`iroh*`; dev-deps pinned to `wyrd-core` | Full link-graph audit (third-party transitive deps invisible) — acknowledged in the module docs; needs `cargo tree` review |
| Unsafe proof-mint stays exactly at definition + authority call | `layer_contracts.rs` · `check_proof_mint` / `verification_proof_has_one_mint` | `from_verified_unchecked` text count is 2 (def + call) after comment stripping | Semantic single-minting (a second call *through* the counted site, or a new unsafe block elsewhere — caught only if it reuses the counted token) |
| `insecure-fast-kdf` never ships | `layer_contracts.rs` · `check_kdf_feature_edges` + `check_release_build_args` + CI `cargo tree` grep in `builds/default.nix:80-84` | Declared edges + release args exclude it; normal graph has 0 hits (verified in this review) | `--all-features` misuse by a downstream packager (no mechanism prevents a user from building with all features; only docs warn) |
| Production KDF params pinned | `wyrd-sync` `kdf_parameters_are_pinned` + `prod_kdf_params_round_trip` (in `default`, excluded from `local`) | Argon2id table is 64 MiB / t=3 in production | Nothing material — but note `prod_kdf_params` (9.5s) is `local`-excluded, so the edit loop cannot catch a params regression |
| Store: re-insert heals bitrot | `fs_store.rs:1006` `reinsert_heals_corruption`; `:1023` `silent_corruption_heals_on_insert_after_get` | Plain-tamper heals; mtime-restored tamper heals **after a `get`** retires the stamp; `get` fails closed (`Corrupt`, no identity in rendering) | Insert-first over mtime-preserving rot (F2) — no test calls `insert` before `get` on disguised bytes |
| Store: counted retained bytes agree across reopen | `fs_store.rs:688`, `:730` | New content charged once; reopen re-seeds; subtract-after-durable-removal matches | `sweep` vs. `retained_bytes()` agreement on hostile trees (F5): `.tmp`, symlinks, missing dirs |
| Scrub: claimed-but-absent re-drives fetch, claim-first ordering | `wyrd-core/src/scrub.rs` drains + `live/tests_*` scrub/restart legs; probe in `scrub.rs:184-` | Walk finds losses on a bounded cursor, drain re-verifies `has`, clears in one commit, subtracts accountant, policy untouched | `has`-as-validity (by design `has` is existence-only; a corrupt-but-present object is quarantine's job via `get` — proven by `get_detects_corruption` `fs_store.rs:1216` + quarantine drain). No test proves scrub ignores corrupt-present (it should, by separation) — separation is asserted by docs, not by a negative test |
| Quarantine: rejected bytes discarded, claim cleared, waiter re-demands | `wyrd-core/src/quarantine.rs` drains + tests | Diagnostic-before-delete, claim-clear precedes discard (claim-first crash order), `NowValid`/`Absent` handled, accountant subtracted only on `Discarded` | Drain-vs-scrub ordering interaction on the same identity in one pass (both drains clear claims; idempotence by `record_*_removed` is assumed, not shown on a shared identity — unverified) |
| Poisoned lock fails closed | `mutation/tests_queue.rs:312` `poisoned_state_lock_is_recovered`; `store.rs` `shared_store_maps_poison_to_lock_error` | Queue recovers; shared store maps poison to `Lock` error | `WriteBudget`/`WantRegistry`/`ScrubQueue`/`QuarantineQueue`/`ParentRegistry` poison paths (F1) — no poison tests exist |
| Mutation order: one snapshot per forcing event, store→commit→publish→announce | `live/` + `daemon/fuse/tests_*` + write-path scenario tests | Queued folds, stale/conflict per-member checks, durable commit before publish (dirty-flag republish on failure) | End-to-end POSIX-byte-range semantics beyond the modeled units (whole-file re-chunking is a documented v0 constraint; range reuse untested) |
| CLI exit codes: `78` locked, `2` other, `0` clean | `wyrd-cli/src/main.rs:216-224` + `tests_sync.rs` `mailbox_line_and_exit_agree_*`; `docs/design/cli.md:579-583` narrative | Locked maps to 78 (direct + wrapped `StoreLocked`), others to 2; degraded/quiet line↔exit agreement pinned | The bullet list at `cli.md:948` (F6) — a writer following the list would break the supervisor contract |
| Idle vs. stalled: quiet exit stays success, degraded stays non-zero | `tests_sync.rs` quiet/stall/cap-trip tests (instantiated in comments at `:636,676,998`, `~1307-1445`) | Loop convergence vs. exit path are tested *separately* (comments name the distinction) | Operator-level stall detection beyond exit codes (no live freshness SLA — `mount.posture` freshness bound is asserted in docs, not in a timed test here) |
| Node lifecycle needs no FUSE/daemon | `wyrd-embed/src/embedded_lifecycle.rs:81` `embedded_drive_lifecycle_needs_no_fuse_and_no_daemon` | Whole lifecycle through `wyrd-core` alone | Anything about daemon composition (covered separately — good separation, not a gap) |
| Heterogeneous relay interop | `tests_interop.rs` + `heterogeneous_relays_cover` (in `default`, excluded from `local`) | Fake + real in-process relay interoperate | Live public-relay behavior (opt-in hand evidence, never CI — documented) |

For each significant test-quality finding above, the regression that *could*
currently pass undetected and the discriminating test are: F1 (poison →
healthy-empty passes; discriminating test = poison each registry, assert
visible poison signal), F2 (insert-first heal gap passes; discriminating test
= disguised rot + insert-without-get), F3 (mount-free break passes CI;
guardrail = nomount derivations), F4 (new `*spam*`/`heterogeneous*` fast test
passes `fast-*` while never running; guardrail = exact names + canary
count), F5 (dual-walk drift passes; guardrail = hostile-tree agreement
test). The remaining rows above show where the suite is *strong* — notably
that updater/loop/exit tests deliberately separate convergence from exit
path, and contract checkers ship negative tests proving they bite.

---

## 5. Feature / configuration and CI coverage summary

| Configuration / gate | What it covers | Runs where | Gap |
|---|---|---|---|
| `default = ["mount"]` (daemon, cli, contracts) — `fuser` + `pub mod fuse/mount` | Full FUSE mount path, errno mapping, `FuseBackend` contracts | `cargo check/clippy` (default), `nextest default`, Nix `rust-check`/`rust-clippy`/`rust-tests` | None for the default path |
| `--no-default-features` (mount-free) | Libfuse-less build; no `fuser` build script; mount-free contract subset | `devenv.nix` clippy hook only (local) | **F3 — never in CI derivations** (`builds/default.nix` has no nomount step) |
| `insecure-fast-kdf` (dev-only) | Fast Argon2id for per-test `Engine` construction | Test profiles only; pinned by manifest checks + `cargo tree` backstop (verified 0 hits here) | `--all-features` papering-over (documented "never treat as production", no mechanical block — acceptable for pre-alpha) |
| `local` nextest (1,939 tests) | Cheap deterministic unit tests; excludes whole contracts binary, heterogeneous cover, flood/spam families, 9s+ state machines, prod-KDF proof | `cargo fast-*` aliases | **F4 — broad `~spam` / `~heterogeneous` substrings**; also any future slow-by-nature test not yet named |
| `default` nextest (2,150 tests) — PR gate | Cross-crate contracts (serial), flood/saturation (serial groups), heterogeneous relays (~30s standing cover), prod-KDF, mailbox/iroh loopback | `.ngit/nix/workflows/rust.yaml` `gates-tests` (8 GiB floor) | Live public-relay + kernel FUSE stay `#[ignore]`d (by design, never CI) |
| `slow` nextest (20 tests) — master supplement | >10s live-relay, ceiling-scale admission, measured ~20s+ heavyweights (32 MiB frame, ceiling manifest, vault fetch, spam/slice/deep-tip, large history) | `.ngit/nix/workflows/rust-slow.yaml`, master pushes only | PRs merge without slow evidence — intended, but a perf/scale regression can land on master before slow catches it |
| `nix` / `shell` / `dist` workflows | Eval + package builds, shell-harness stubs, release tarball (tag-only) | `nix.yaml`, `shell.yaml`, `dist.yaml` | Image assembly (`images.*`) only via `nix flake check` eval gate or KVM gate tasks, not per-PR Nix builds (cost-documented) |
| Gate tasks (`wyrd-gate-e2e`, `wyrd-drive-e2e` in `gate.toml`) | Incus 2-peer + relay convergence, drive-service supervision through the agent transport | Gate runner on a Linux KVM host (`/dev/kvm`), never direct | Not part of `rust.yaml`; requires dedicated hardware — reviewed as evidence, not as a per-PR merge gate |
| Lima E2E (`tests/lima/run-alpha.sh`) | macOS dev + refactor guard, real guest | Manual (`--keep` / `--step`) | Not CI; host needs macFUSE + guest setup (documented in README) |

Separation of fast / PR / expensive is otherwise correct: the `local`
editor loop (~fast, per-package), the PR gate (default profile + doctests,
the documented `cargo nextest run` + `cargo test --workspace --doc`), and
master-only slow + opt-in E2E are distinct with distinct resource floors
(4 GiB fast, 8 GiB tests). The `leak-timeout = "5s"` in both `default` and
`local` (`nextest.toml:99,139`) is a defensible triage for observer
starvation under the spawn storm, with the LEAK marker retained for genuine
leaks — noted here only because a genuine >5s leak would be masked; no
evidence of masking was found in this review. The `host-lifecycle` serial
group and `contracts-serial` exist for observed races, not speculation.

---

## 6. Prioritized improvement plan

### Immediate correctness / release-risk fixes (F1, F2, F3)

1. **F1:** Unmask poison. Smallest PR: `Result`-returning
   `peek_pending`/`capture`/`validate` + poison counter/log on
   `release`/`release_fold`/`submit`/`mark_admitted`, plus five
   per-type poison tests and one sentence in `error-conventions.md`. Do not
   touch the fail-closed mutating ops or the recover-by-`into_inner`
   semantics of `WakeSignal`/`MutationQueue` (they are documented and
   tested; unify the *diagnostic* rule, not the recovery strategy).
2. **F2:** Decide heal semantics and pin it. Smallest PR: either (a) stop
   short-circuiting `insert` on stamp match, or (b) correct the module
   header and add the insert-first discriminating test. Either way, the
   header, the field comment, and the test must agree on the same day.
3. **F3:** Add `rust-check-nomount` / `rust-clippy-nomount` derivations and
   wire them into `gates-fast`. Fix the `devenv.nix` mirror comment in the
   same diff. Verify with the temporary `use fuser` mutation described in
   F3 and paste the result into the PR.

### Small independent refactors (F4, F5, F6, F7 — each shippable alone)

4. **F4:** Exact names for `~spam` / `~heterogeneous` in `[profile.local]`
   + a `nextest list` canary floor. Five-minute config change; no Rust.
5. **F5:** Single walker + hostile-tree agreement test. Split into two
   diffs if desired: test first (fails on a deliberately diverged copy),
   then the extraction.
6. **F6:** Three doc edits + one string-agreement test. Keep code untouched;
   verify the "genuinely remaining slice" sentence against ROADMAP before
   editing `lib.rs`.
7. **F7:** `cmp_content_then_kind` + `final_component` helpers. One
   behavior-preserving diff; run `cargo fast-core` to prove no drift.

### Longer-term structural improvements (not blockers for v0.3)

- **Split `layer_contracts.rs` (3,392 lines).** The file now houses DAG
  policy, a hand-rolled Rust lexer (`code_text`), closure reachability,
  proof-mint counting, KDF/release checks, owner-run, and host-transport
  checks. It passes today, but every new contract pays the file's review
  cost. Extract the lexer/scanner into a `scanner` submodule with
  property tests over comment/string/char edge cases, leaving policy
  tables in place. Do not split by policy before the lexer is pinned —
  the scanner is the shared dependency.
- **Consider a single "diagnostics never mask poison" house rule** for all
  future `Mutex`/`RwLock` wrappers (today's five variants — fail-closed,
  recover-by-`into_inner`, silent-drop diagnostics, silent-drop mutation
  helpers, `Result` mapping — each grew locally). A one-page rule in
  `error-conventions.md` + a `clippy`-adjunct grep for
  `unwrap_or(0)`-on-lock in review checklists would prevent F1's shape
  from recurring faster than new tests accumulate.
- **Revisit `has`-as-presence callers as a class.** The trait correctly
  documents `has` = existence-only, and `get` fails closed, but
  `check_chunk` (`merge.rs:453`), `snapshot.rs:592`, `apply.rs:1437` and the
  scrub probe all treat `has==true` as "usable". Today the consequence is
  bounded (corrupt bytes fail at the next `get`, quarantine repairs), but
  a future caller that skips `get` on `has` would turn presence into
  validity. A one-line audit note in `store.rs` naming the three call
  sites as "presence checks only — never skip `get`" would harden the
  boundary without a type-level change.

---

## 7. Explicit review limitations

- **Scope.** Code, docs at this commit, and repo state only. Nothing under
  `docs/reviews/archive/` was read, listed, or opened, and
  `docs/reviews/README.md` was not read, per the task constraints. Past
  review conclusions, open issues, and `planning/` files were not consulted;
  where a decision is marked "Proposed / Open" in contracts (e.g. gossip
  framing, retentions, KDF pinning rationale), this review treats the
  normative text as given and does not re-litigate it.
- **Checks.** Only the commands listed in §1 were run. Full nextest
  (default/slow), doctests, audit/deny, Linux/KVM legs, Lima E2E, and the
  Incus gate were not run. Test counts are `nextest list` counts. Passing
  `cargo check`/`clippy` proves compilation and lint cleanliness, not
  behavioral correctness. No fuzzing, no miri, no timing runs.
- **Unverified claims (marked inline).** Poison reachability end-to-end
  (a live holder panic) is plausible but not demonstrated by a fault-
  injection run. F2's field frequency (mtime-preserving rot followed by
  insert-before-read) is not measured. The exact "genuinely remaining"
  slice in F6 needs a ROADMAP check outside this review's scope before
  editing. `code_text` lexer coverage is characterized by its own
  comments and negative tests, not by a mutation run in this review.
- **Platform.** Reviewed on macOS (aarch64-darwin). Linux-specific paths
  (libfuse, `DRIVE_DIR_MODE` non-Unix arms, `#[allow(unsafe_code)]`
  supervision) were read but not executed. The vendored `fuser` divergences
  (root `Cargo.toml` OD-07-D note, 4 divergences across 5 files) were not
  byte-diffed against crates.io 0.18.0.
- **What this review is not.** Not a security audit (crypto/capability
  claims are taken from contracts and code structure, not re-proven), not
  an API-stability review, and not a performance review. Style, naming
  preference, and line-count minimalism were not flagged.

---

## 8. Dimensions covered (index to findings)

- **A. Module/API boundaries:** no finding. The architecture table,
  `layer_contracts` DAG pin, `wyrd-fuse` link-graph closure, and
  `check_owner_runs_through_daemon` / `check_host_transport_free` enforce
  boundaries mechanically; `wyrd-sync` exposes 352 `pub` items by design
  (embeddable substrate), and `WyrdNode::parts()` is the explicit
  disjoint-borrow seam, not a leak. `has`-vs-`get` is the one boundary
  that relies on prose (see §6 longer-term note, not ranked).
- **B. Duplication:** F5 (dual walks), F7 (micro-duplication).
  Scrub/quarantine drain duplication explicitly evaluated and *not*
  flagged: the queue boilerplate is near-identical, but the drains differ
  in crash order, lock kind (read vs. write), and accountant rules by
  documented design (scub `drain_scrub` re-verify-then-clear under one read
  guard; quarantine `drain_quarantine` clear-then-discard under per-item
  write guards) — unifying them would couple two repair concepts to save
  ~40 lines of boilerplate. Same verdict for `sync_pass` length (§9).
- **C. Error handling / failure semantics:** F1. Otherwise exemplary:
  `FsStoreError::io` centralizes classification, `authoring_error`
  (`live/apply.rs`) keeps foreign text in traces, `CliError::exit_code`
  maps only lock contention to 78, and idle/stalled is distinguished by
  exit codes (`Unverified`/`Incomplete`/`ReconciliationOutstanding`).
- **D. State and mutation clarity:** no standalone finding. Command →
  validated intent → durable commit → publish → announce is traceable
  (`MutationQueue` → `sync_pass` → `apply_fold`/`author_snapshot` →
  `publish` → discharge), `Working` vs. `Committed`/`Published` matches
  `write-path.md` DG-1, and serving authorship holds `AuthorizedSnapshot`
  construction to `wyrd-sync` only. F2 is the one state-clarity gap
  (derived stamp masquerading as authoritative verification).
- **E. Test quality:** F1/F2/F3/F4 test halves + table in §4.
- **F. Build / feature / config:** F3 (+ F4 for test lanes). KDF
  dev-only posture and default-mount re-enablement dance are correct and
  pinned; the mount-free lane is the hole.
- **G. Documentation / code drift:** F6.
- **H. Changeability / reviewability:** F5, F7, and the `layer_contracts`
  note in §6. No giant-function finding: `sync_pass` (560 lines) and
  `apply_fold` (254 lines) are linear pipelines whose order *is* the
  invariant; splitting them would increase change risk.
- **I. Repository hygiene / developer feedback:** no standalone finding
  beyond F3/F4. `#[allow(dead_code)]` uses are documented and test-only;
  no TODO/FIXME/HACK production markers; fast/PR/slow separation matches
  its documented budgets. The two costliest smells — the 3392-line
  contracts file and the ~2,900-line `tests_sync.rs` — are cohesive test
  suites with deliberate weights (P0 baseline measured Oct-5), not dead
  scaffolding.

---

## 9. Evaluated but not flagged (discipline log)

To preserve the prompt's falsification discipline, the strongest
tempp-ration-to-flag candidates that were checked and *intentionally*
excluded, with reasons:

- **`sync_pass` 560 lines** — the ordering contract made explicit;
  early-returns are `?`-driven over phases, each phase is a named helper
  (`drain_quarantine`, `probe_presence`, `drain_scrub`, fetch, `apply_fold`).
  Splitting would distribute the order across modules and make the
  "walk before drain, drain before fetch" invariant harder to review.
- **Scrub vs. quarantine queues/drains (~600 lines each)** — duplicated
  queue *mechanics* (set, FIFO-by-identity, poison-drop) but opposite
  drain orders and different lock kinds. Generic queue extraction is the
  only candidate, and it buys ~60 lines at the cost of coupling two repair
  lifecycles the docs insist on keeping separate (OD-12/13). F7 fixes only
  the pure `Ord`, which has no lifecycle meaning.
- **Wide `pub` surface (`wyrd-sync` 352, `wyrd-core` 242 items)** —
  matches the crate-responsibility table and the embedder model; the
  machine-enforced DAG is the boundary, and `layer_contracts` proves it.
  Narrowing exports would break `wyrd-embed`/`wyrd-app` composition
  without removing a semantic risk.
- **8,225 `unwrap` / 1,420 `expect` / 220 `panic!`** — all but three
  `expect`s sit in tests; the three production ones are justified above.
  Production `unwrap_or`/`ok()` uses are dominated by the F1 patterns,
  which are ranked there, not here.
- **`has()` existence-only callers** — correct under the stated contract
  (fail-closed `get` + quarantine repair); ranked only as a documentation
  hardening note in §6, not as a defect.

---

*End of review. Commit `438a2f3dd2464049d38e487b2fc2baf659fed13a`, clean tree
at review start except this file, left uncommitted. No commits, pushes, or
stages were made.*
