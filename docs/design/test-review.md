# Test review

Status: Descriptive. This document records the 2026-11-09 review of
every Wyrd test. It does not change any contract. Each verdict
records one of five actions: **keep**, **merge**, **move** to a
different evidence budget, **remove** a duplicate, or **add** a missing
test. The section "What this branch does" names the small change
that implements the most valuable verdicts. The remaining merges
stay as follow-up work. This file is dated point-in-time
evidence. The five follow-up items in "Follow-up work" are the
canonical tracking surface once filed as issues; this file
records the decision, the issues track the work.

## Inventory

Rust tests by `#[test]` count, 2026-11-09, at `55160edf`:

| Crate | Test functions |
| --- | --- |
| `wyrd-sync` | 956 |
| `wyrd-core` | 357 |
| `wyrd-daemon` | 277 |
| `wyrd-cli` | 252 |
| `wyrd-contracts` | 218 |
| `wyrd-format` | 177 |
| `wyrd-view` | 49 |
| `wyrd-host` | 4 |
| `wyrd-embed` | 2 |
| `wyrd-namespace` | 0 |

Shell and end-to-end evidence: 17 `test-*.sh` harness suites and
4 libraries, 14 `tests/gate` bats files plus 1 driver, 1 `tests/drive`
bats file, the Lima scripts, and the 2,286-line
`tests/microvm/alpha-microvm-legs.sh`. Rust doctests are 6 fenced
examples in 3 files.

The baseline in `.config/nextest.toml` records 1,415 tests in 206
seconds on 2026-10-05. The current tree carries more tests than
that baseline. The increase is mostly new ceiling-scale admission,
spam, and merge tests. It is not a count regression. It is new
evidence budget that was placed without a fresh duration pass.

## How a test earns its place

A test stays if it pins one of these. When it pins more than one in
the same way, the second copy merges into the first.

- A normative sentence in a document under `docs/contracts/`.
- An architectural edge in `docs/architecture.md`, enforced by
  `wyrd-contracts`.
- A crash window in `docs/contracts/crash-consistency.md`.
- A resource bound in `docs/contracts/resource-limits.md`.
- A production decision that a compiler, a type, or a lint cannot
  enforce alone, such as secret hygiene or KDF posture.
- The only evidence for a topology that one process cannot show,
  such as relay wire opacity, partition, or `ENOSPC`.

A test goes if it restates a nearby stronger test with only a
parameter changed, asserts its own mock, or reads as a one-line
getter. Merges turn such pairs into one table test. They do not
delete the invariant.

## Executive summary

The suite is not massively redundant. Roughly 2,140 of 2,292 Rust
tests are **keep** without change. Roughly 152 are merge, move, or
remove candidates. Deleting them saves little run time. Most are
fast unit tests. The run-time cost is not the count. It is four
placements and two serial groups.

1. Doctests compile the whole workspace on every pull request for
   6 examples. They belong on `master`, not on the pull-request gate.
   The plain consequence: a doctest that stops compiling on a
   product branch now breaks on `master`, not on that branch.
   Local `cargo test --workspace --doc` runs plus the doc-links
   hook are the only branch-level backstop.
2. Eight multi-second live-relay tests still run on the
   pull-request gate. They belong in the `slow` profile with
   the floods. Their measured cost is at most the 28 to 36
   second critical path (see below). If the contracts serial
   tail dominates, the wall saving is smaller. The move still
   removes load and contention.
3. `wyrd-contracts` runs one binary serially (measured 52
   seconds for 215 tests on this machine, `default` profile)
   and the mailbox flood group runs serially. Both are correct
   load control, but the contracts serial group is the largest
   single tail in the `default` profile on this machine.
4. Two files each exceed 2,500 lines (`wyrd-cli` sync,
   `wyrd-daemon` owner). They are correct in content and hard to
   review. They need splits, not deletions.

The cross-host gate phases, the Lima suite, and the harness
suites are each correctly placed and need no cut. The microVM
gate runs only on Odin by hand. The harness suites run in
seconds on every pull request. The Lima suite guards the macOS
host path and never runs in CI.

## What must stay

These evidence classes are load-bearing. Each is small relative
to its risk if it goes quiet.

- Authorization and membership conformance in `wyrd-sync`
  (33 plus 40 tests). Each test maps to the conformance list in
  `docs/contracts/epochs.md`. Do not merge across it.
- Durable crash and restart equivalence
  (`crash-consistency.md` rows, plus the cross-subsystem
  agreement test). Each crash stage is a distinct window.
- Fetch ledgers, strikes, terminal generations, and serving
  backpressure (`sync-and-peers.md`, `resource-limits.md`).
- The `wyrd-contracts` composition tests for one named
  invariant per file (heads, recovery, write path, queue
  pressure, upgrade invariants 1 to 10). Unit tests prove one
  crate. Contracts prove two crates agree.
- Gate phases 03, 06, 10, 11. Wire opacity, relay partition,
  `ENOSPC` fail-closed, and bounded unknown-parent heal are
  invisible to in-process tests.
- The single public-relay TLS proof (`external_relay_gift_wrap`
  over TLS, `#[ignore]`d). It is the only internet-relay
  evidence. Do not delete it.
- Secret hygiene inventory in `keys/hygiene.rs`. It names every
  scrubbed leaf. Three duplicate locks elsewhere remove
  cleanly (see below).

## Per-crate findings

### `wyrd-sync` (956 tests)

Keep almost all. The authorization, membership, durable,
reconciliation, fetch strike, and serving files each pin a
distinct contract clause. The verbosity is scenario setup, not
duplicate assertion.

Merge candidates (about 55):

- The three stale-obligation files plus the crash matrix
  (11 tests) share one `plant` parameter. They become one
  file with one table.
- Restart, crash, and reopen rows (about 25 tests) share one
  `scenario()` harness. They become one parameterized sweep
  plus the tripwire and the cross-subsystem agreement test.
- Seven capability tamper tests become one table. Four
  `ensure_owner_only` variants become one parameterized test.
  Three outbox round trips become one. Five rotation tamper
  and version tests become one table.
- About 15 codec round trips collapse to one per codec. Each
  keeps its known-answer or truncation rejection.
- `keys/root.rs` (2), `keys/ephemeral.rs` (1), and two device
  zeroize locks duplicate `keys/hygiene.rs`. They remove
  without loss (this branch removes 3).
- `close.rs` (5) stays. The zero-deadline sparing of an
  already-complete stop is the serving contract. It is not
  trivia.

Slow candidates still in `default`:

- `per_sender_quota_keeps_one_spammer_from_starving_others`
  (31s in the Oct-05 P0 comments; 5.4s in the Nov-09 local run).
  Moves to `slow` in this branch.
- `budgeted_walk_reaches_live_behind_hanging_candidates` (10.7s
  local) and `capped_walk_pages_dial_hangs_one_per_pass`
  (10.1s local). Move to `slow` in this branch.
- `oversize_flood_retains_nothing_per_delivery` (2.0s local,
  100,000 envelopes) and
  `torn_commit_in_any_pass_converges_like_a_clean_run`
  (3.6s local, 15 scenario builds). Move to `slow` in this branch.
  Grouping hygiene more than wall savings.
- `concurrent_senders_never_exceed_the_byte_bound` (0.9s local,
  256 MiB alloc) and `serving_mount_refuses_push` (4.8s local).
  Move to `slow` in this branch. Grouping hygiene more than
  wall savings.

### `wyrd-core`, `wyrd-format`, `wyrd-view`, `wyrd-namespace`

Zero removals. 583 tests stay.

- `live/tests_prereq.rs` (40 tests) is a catch-all sink. Split
  it by cluster into deferral, quota, head-gate, and quiet
  files. Move its shared `live_over_configured` fakes to
  `tests_harness.rs`.
- Five near-identical `NamespaceView` fakes (about 250 lines
  total) move to one shared fake parameterized by fetch
  status. No behavior changes.
- Error taxonomy across format, view, core, live, and
  contracts is intentional layering. Each layer maps one
  error variant. Do not collapse it.
- `wyrd-namespace` carries 0 tests for safety-critical
  surface (`AuthorizedSnapshot`, symlink confinement,
  budgets). Add 5 to 10 direct unit tests in its own crate.
  This is a gap, not a cut.

### `wyrd-daemon`, `wyrd-cli`, `wyrd-host`, `wyrd-embed`

Keep almost all. Two monoliths split. Four triads parameterize.
Helpers duplicate in four places and extract to shared
harnesses.

- `core/tests_coalescing.rs` (20), `fuse/tests_write.rs` (33),
  and `core/tests_handles.rs` (22) each prove one layer of
  the fold (`write-path.md` DG-1). Keep all three. Rename
  does not change evidence.
- `owner/tests_owner.rs` (33, 2,525 lines) is the normative
  `owner-interface.md` contract. Split it into wire,
  dispatch, socket, submit, and framing files. No deletion.
- `cli/tests_sync.rs` (70, 3,048 lines) mixes status spine,
  headless loop, render, and vault status. Split it into
  four files. Keep the status and headless spine. Convert
  fragile positive `contains` prose to structural parse.
  Keep every negative leak assert (`no membership data`,
  `no content ids`, `no paths`, `no secrets`) and the
  `every_storage_figure_maps_to_a_dg5_row` projection.
- `cli/tests_member.rs` four `*_carries_files_forward`
  tests become one parameterized test. `core/tests_backend.rs`
  nine `queued_create_*` tests become one table.
  `fuse/tests_write.rs` three disappeared-path tests become
  one. `fuse/tests_want.rs` three terminal tests become one.
- Eight CLI arg-validation tests in `tests_cli.rs` move to
  `args.rs` unit tests or drop under `help_contract.rs`.
- `core/tests_mount_free.rs` (3) moves to `wyrd-view` or
  merges into `tests_mount.rs`. `core/tests_sync.rs` want
  trio moves to `fuse/tests_want.rs`. The vault discharge
  pair merges into `tests_publish.rs`.
- `host_lifecycle.rs` (4) and `embedded_lifecycle.rs` (2)
  stay. Each pins a distinct public-API close-order claim.
  Cross-crate duplication there is deliberate.

### `wyrd-contracts` and shell

No removal except the layer helper corpus.

- `layer_contracts.rs` is 4,126 lines: 21 live-repo tests
  plus 86 private checker unit tests. The checker negatives
  are needed. A checker that always says "ok" proves nothing,
  so the live tests would pass vacuously. The corpus is
  over-factored: manifest shapes times KDF versus fixtures
  times flake tokenisation. Convert it to about 20 high-value
  negatives plus one parameterized table (and optionally a
  proptest for the lexer). Split checkers from live tests.
  This is a maintenance tax, not a run-time tax.
- `sync_contracts`, `node_contracts`, `shared_drive_semantics`,
  `upgrade_contracts`, `join_contracts`, `view_contracts`,
  `serving_contracts`, `terminal_contracts`, `vault_contracts`,
  `ownership_contracts`, `egress_contracts`,
  `removal_contracts`, `policy_contracts`, and `owner_contracts`
  all stay. Each names a reviewer-visible invariant or
  composes at least two crates. The apparent overlaps with
  unit tests are named and deliberate (for example,
  `drive_ownership` exists so the gate selects it by name).
- Gate bats phases 01 to 11 plus smoke and epilogue stay.
  Phases 03, 06, 10, and 11 prove wire-level facts. The
  other phases prove cross-host integration at 50 to 100
  times the cost of the matching contract. That trade is
  correct for manual Odin evidence. It is wrong for every
  pull request. The current placement (manual only) is
  correct. Log `--step` prefix selection remains the lever
  if an Odin run is too long.
- All 17 harness `test-*.sh` suites stay. Each pins one
  grammar or timeout discipline whose regression would
  surface only after a 30-minute Odin run. Total pull-request
  cost is seconds.
- Lima and microVM stay as two stacks. Lima proves the macOS
  host path. microVM proves cross-host convergence. They
  are not duplicates.

## Slow lane and pull-request wall

The `nextest.toml` lanes are the right shape: `local` for the
edit loop, `default` for the pull-request gate, `slow` for
`master` only. The maintenance rule (name every test at 9
seconds or more that stays in `default`) is correct.

This branch changes the `default` filter to exclude eight
additional substrings and restates them in `slow` verbatim
(profiles do not inherit filters):

- `per_sender_quota` (spam-scale quota proof)
- `heterogeneous` (cross-implementation cover)
- `budgeted_walk_reaches_live`
- `capped_walk_pages_dial_hangs`
- `oversize_flood_retains_nothing`
- `torn_commit_in_any_pass`
- `concurrent_senders_never_exceed`
- `serving_mount_refuses_push` (push-denied pin; now `slow`
  with the rest of serving evidence)

Measured Nov-09, local `slow` profile, one run (serial, as
`slow` runs):

| Test | Wall |
| --- | --- |
| `heterogeneous_relays_cover` | 28.1s (36.1s on rerun) |
| `budgeted_walk_reaches_live` | 10.7s |
| `capped_walk_pages_dial_hangs` | 10.1s |
| `serving_mount_refuses_push` | 4.8s |
| `per_sender_quota` | 5.4s |
| `torn_commit_in_any_pass` | 3.6s |
| `oversize_flood_retains_nothing` | 2.0s |
| `concurrent_senders_never_exceed` | 0.9s |
| Total, 8 serial | 65.6s |

In `default` these run in parallel, so the saving is at most
the longest single test (28 to 36s), not the serial sum. If
the 52s contracts serial tail dominates, the wall saving is
smaller and the move reads as load reduction. The heterogeneous
test is the honest trade: it was the only `default` proof of
the real-relay path. On `master` it still runs. On a pull
request it does not. If reviewers want it on every pull
request again, restore the one `heterogeneous` substring to
`default` (remove it from the `default` exclusion, keep it in
`slow` and `local`). Do not re-add the whole tail.

Doctests move with the tail. `rust.yaml` no longer builds
`rust-doctests` on the pull-request gate. `rust-slow.yaml`
(`on: push: branches: [master]`) builds it on `master`
pushes. The derivation stays so `nix flake check` still
evaluates it. No KDF coverage is lost: `rust-check` still
runs the production feature-graph gate
(`cargo tree -e normal,features`).

## What this branch does

1. `.config/nextest.toml`: exclude the names above from
   `default`, include them in `slow`, restate them in
   `local`. Update the profile comments to name the new
   measurement.
2. `.ngit/nix/workflows/rust.yaml`: remove the `doctests`
   step from `gates-tests`. Update the header comment.
   `.ngit/nix/workflows/rust-slow.yaml`: add a `doctests`
   job with the same memory floor as `slow-tests`.
3. `crates/wyrd-sync/src/keys/root.rs`: remove the two
   hygiene duplicates (`root_does_not_leak_through_debug`,
   `root_implements_zeroize_on_drop`). Keep
   `root_keys_are_fresh_random`.
   `crates/wyrd-sync/src/keys/ephemeral.rs`: remove the
   duplicate zeroize lock. Keep the public-key wiring test.
   `crates/wyrd-sync/src/keys/hygiene.rs` already pins both.

No contract text changes. No test file moves. Each removal
is byte-for-byte covered by the hygiene inventory test.

## Follow-up work (not in this branch)

Track each item as a separate issue. Do not bundle them into
this branch.

1. Convert 86 `layer_contracts.rs` checker tests to about
   20 negatives plus one table. Split checkers from live
   tests.
2. Merge the 11 stale-obligation and crash-matrix tests into
   one table file. Collapse about 25 restart rows to one
   sweep.
3. Split `tests_prereq.rs` (40), `owner/tests_owner.rs`
   (33), `cli/tests_sync.rs` (70), and `fuse/tests_write.rs`
   (33). Deduplicate five view fakes and four mailbox fakes.
4. Parameterize the triads named above and convert
   `cli/tests_sync.rs` plus `tests_snapshot.rs` prose
   asserts to structural parses.
5. Add direct `wyrd-namespace` unit tests. Re-measure the
   `default` profile wall and re-apply the 9-second rule.

## Method and limits

Each verdict came from static file review plus one
`cargo nextest run --profile slow` timing run for the eight
moved tests (Nov-09, local machine, single run; durations
above). Duration claims otherwise come from the comments in
`.config/nextest.toml` (2026-10-05 P0, 2026-10-30
heavyweight pass) and from size signals in code (sleep,
relay spawn, 100,000-envelope loops, 256 MiB allocations,
Argon2 production params). Timings vary by machine and load
(`per_sender_quota` is 31s in the P0 comments and 5.4s
locally). The next full `cargo nextest run` with timing
output should replace single-run numbers. If a moved test
proves cheap on that run, move it back.

Hygiene verification: `keys/hygiene.rs` names
`DriveRootKey` and `EphemeralScalar` in both
`secret_leaves_implement_zeroize_on_drop` (lines 70 to 77)
and the inventory table. `secret_leaves_redact_through_debug`
covers `DriveRootKey(REDACTED)`. The deleted trait-bound
asserts are covered byte-for-byte.
