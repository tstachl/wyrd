## Overall assessment

This PR addresses Issue 32: projection freshness at the composition boundary. The change ensures that when a drive is composed (via `WyrdNode::into_live`), the view's projection is unconditionally refreshed from the engine's durable state -- both materialization and live heads -- so that the first served generation reflects all committed heads, even when the drive reopens with an empty view.

The implementation is minimal, focused, and well-tested. Five new contract tests in `wyrd-contracts` and one in `wyrd-embed` comprehensively cover the four projection states (fresh empty, fresh populated, refused, retained) and the `parts_mut` mutation gap. All tests pass, clippy is clean, and formatting is correct.

## Changed behavior

**`crates/wyrd-core/src/node.rs` -- `WyrdNode::into_live`**
- **Before**: `carry_pending` ran; `refresh_live_heads()` ran *only* if the carry changed queue state. The baseline revision was read before any refresh.
- **After**: `carry_pending` runs; then *both* `refresh_materialization()` and `refresh_live_heads()` run unconditionally; the baseline revision is recorded *after* the refresh. A refresh failure fails composition closed with the underlying error.

**`crates/wyrd-contracts/src/node_contracts.rs`**
Added six tests documenting and enforcing the new behavior:
1. `reopened_drive_serves_its_namespace_at_composition` -- reopen serves committed heads with no explicit refresh
2. `idle_pass_after_composition_does_not_regress` -- idle pass sees no change past the split revision
3. `engine_mutation_through_parts_mut_is_visible` -- `parts_mut` mutations are visible at composition
4. `composition_refusal_is_distinguishable_from_an_empty_view` -- refresh failure = `Err`, not empty `Ok`
5. `last_good_projection_is_retained_after_a_refresh_failure` -- failing refresh retains last good projection
6. `a_consumer_can_observe_projection_staleness` -- consumer distinguishes all four projection states

**`crates/wyrd-embed/src/embedded_lifecycle.rs`**
Added `embedded_lifecycle_needs_no_explicit_refresh` proving the embedded consumer path works without any `refresh_live_heads` call.

**`devenv.nix`**
Fixed the `clippy` pre-commit hook entry to wrap the `&&` chain in `bash -c` so it executes correctly under the `language: system` hook runner.

## Guardrails

| Guardrail | Verdict | Evidence |
|-----------|---------|----------|
| Composition boundary makes cache consistent (Issue 32, `docs/architecture.md` Current status) | Satisfied | `node.rs:394-403` unconditionally refreshes materialization and live heads after carry drain, before recording revision |
| Composition fails closed when no trustworthy projection exists (OD-32-C first row, `epochs.md` Post-corruption projection behavior) | Satisfied | `node.rs:402-403` -- `refresh_materialization()?; refresh_live_heads()?`; test `composition_refusal_is_distinguishable_from_an_empty_view` asserts `Err(NodeError::Runtime(EngineError::Durable(DurableError::CorruptCurrent)))` |
| Last good projection retained after refresh failure (OD-32-C second row, `epochs.md` Post-corruption projection behavior) | Satisfied | Test `last_good_projection_is_retained_after_a_refresh_failure` asserts `assert_serves_hello(node.view())` after `refresh_live_heads()` fails |
| Consumer distinguishes four projection states without core degraded mode (OD-32-C third row) | Satisfied | Test `a_consumer_can_observe_projection_staleness` exercises all four states via public API (`engine.live_heads()`, `into_live`, `refresh_live_heads`) |
| Embedded lifecycle needs no explicit refresh (architecture.md Embedded consumer boundary) | Satisfied | Test `embedded_lifecycle_needs_no_explicit_refresh` in `wyrd-embed` composes and serves with zero refresh calls |
| Carry drain precedes composition (architecture.md Current status, `epochs.md` Transition continuity) | Satisfied | `node.rs:387-393` -- carry drain runs in a scoped block before refreshes |
| Only `wyrd-sync` authorization mints `AuthorizedSnapshot` (master AGENTS.md Invariants) | Not applicable | Diff does not touch snapshot authorization or minting paths |
| Content IDs never appear in vault-visible metadata (master AGENTS.md Invariants) | Not applicable | Diff does not touch vault or transport serialization |
| No crypto primitive implementations (master AGENTS.md Invariants, `trust.md` T11) | Not applicable | Diff uses existing `Engine` and `ObjectStore` APIs only |
| `wyrd-format` free of networking/async/FUSE (master AGENTS.md Invariants) | Not applicable | Diff does not modify `wyrd-format` |
| `wyrd-fuse` links no iroh transitively (architecture.md Crate map, contract 34) | Not applicable | Diff does not modify `wyrd-fuse` or its dependencies |

## Findings

### Non-blocking

1. **Documentation gap: `devenv.nix` clippy hook fix rationale could be a code comment**
   The `bash -c` wrapper is a workaround for the `language: system` hook runner. A one-line comment above the `entry` explaining *why* (system hooks exec directly, no shell) would help future maintainers.
   *File:* `devenv.nix:52-55`
   *Introduced by this PR*

2. **Test helper `author_behind_the_view` is duplicated between `wyrd-contracts` and could be shared**
   The helper appears in `node_contracts.rs:784-809` and would be useful for other contract files. Consider moving to a `test_support` module if more tests need it.
   *File:* `crates/wyrd-contracts/src/node_contracts.rs:784`
   *Pre-existing but relevant*

3. **`composition_refusal_is_distinguishable_from_an_empty_view` corrupts `CURRENT` directly**
   The test writes `b"rot"` to `CURRENT` to simulate durable damage. This is a valid corruption injection for the contract, but the test could clarify that this simulates a specific on-disk corruption scenario (bitrot after commit).
   *File:* `crates/wyrd-contracts/src/node_contracts.rs:937`
   *Introduced by this PR*

## Architecture

The change correctly identifies the **composition boundary** (`into_live`) as the single point where the view's projection must be made consistent with the engine's durable state. This aligns with the architecture:

- `wyrd-core` is the embeddable node; composition is its boundary to presentation backends (`wyrd-daemon`, `wyrd-embed`, future mobile surfaces).
- The engine holds durable heads (classified via `epochs.md` authorization); the view serves the projected namespace.
- The revision gate in the live loop (`published_revision` vs `engine.current()`) would otherwise treat an unrefreshed baseline as current and never publish the engine's heads -- the fix ensures the baseline is recorded *after* refresh.

The ordering (carry drain -> refresh materialization -> refresh live heads -> read revision) is correct:
- Carry drain publishes staged heads so they're visible to the refresh
- Materialization refresh rebuilds residency from the fresh runtime
- Live heads refresh installs eligible heads from the same store
- Revision is recorded *after* both, so any `parts_mut` mutation between refresh and revision read appears as a mismatch the loop repairs

This matches the crash-consistency model in `epochs.md` Transition continuity: carry drain at composition completes pending obligations idempotently.

## Tests

| Test | Package | Status | Notes |
|------|---------|--------|-------|
| `reopened_drive_serves_its_namespace_at_composition` | wyrd-contracts | PASS | Core contract: reopen serves heads at composition |
| `idle_pass_after_composition_does_not_regress` | wyrd-contracts | PASS | Revision gate works: idle pass sees no change |
| `engine_mutation_through_parts_mut_is_visible` | wyrd-contracts | PASS | `parts_mut` gap healed at composition |
| `composition_refusal_is_distinguishable_from_an_empty_view` | wyrd-contracts | PASS | Fail-closed: `Err` not empty `Ok` |
| `last_good_projection_is_retained_after_a_refresh_failure` | wyrd-contracts | PASS | Retention of last good projection |
| `a_consumer_can_observe_projection_staleness` | wyrd-contracts | PASS | Four states distinguishable via public API |
| `embedded_lifecycle_needs_no_explicit_refresh` | wyrd-embed | PASS | Embedded consumer path verified |
| All existing wyrd-contracts tests | wyrd-contracts | PASS | 165 tests, no regressions |
| All existing wyrd-core tests | wyrd-core | PASS | 277 tests, no regressions |
| All existing wyrd-embed tests | wyrd-embed | PASS | 2 tests, no regressions |

**Test gaps**: None identified. The new tests cover all four projection states from OD-32-C plus the `parts_mut` mutation gap and the embedded lifecycle. The existing suite passes without modification.

## Code quality

- **Clippy**: Clean (no warnings on changed packages)
- **Formatting**: `cargo fmt --check` passes
- **Naming**: Clear, consistent with existing conventions (`refresh_materialization`, `refresh_live_heads`, `into_live`)
- **Comments**: The new doc comment on `into_live` (lines 372-381) correctly explains the composition-boundary freshness invariant and the fail-closed rationale
- **Error handling**: `?` propagation on both refreshes; failure fails composition closed with the underlying `EngineError` -- matches the `NodeError::Runtime` variant
- **Scope discipline**: The carry drain runs in a scoped block (`{ ... }`) to drop the store read guard before the refreshes -- correct lock discipline

One minor note: the old conditional `if recovery.queue_changed() { refresh_live_heads() }` is removed entirely. The comment correctly states it's "subsumed" because the unconditional refresh covers all cases. This is correct but the comment could explicitly note that the carry drain's queue-change signal is now observed by the refresh itself (since the carry snapshots become eligible heads).

## Performance

No performance regression. The change adds one unconditional `refresh_materialization()` call at composition (previously only called on `drain`, `execute_plan`, and write paths). Composition is a one-time operation at drive open/reopen, not a hot path. The live loop's revision gate still prevents redundant publications: an idle pass after composition sees the same revision and publishes nothing.

## Suggested changes

1. **Non-blocking**: Add a comment in `devenv.nix` explaining the `bash -c` wrapper for the clippy hook.
2. **Non-blocking**: Consider extracting `author_behind_the_view` to a shared test utility if other contract files need engine-behind-view mutations.
3. **Non-blocking**: In `composition_refusal_is_distinguishable_from_an_empty_view`, add a comment clarifying that corrupting `CURRENT` simulates bitrot after durable commit.

## Review conclusion

The PR correctly implements the composition-boundary freshness fix for Issue 32. The change is minimal, architecturally sound, and comprehensively tested. All guardrails are satisfied. The five new contract tests plus the embed test provide strong evidence that the four projection states are correctly handled and the embedded consumer boundary works without explicit refresh.

0 blocking findings, 3 non-blocking findings, 0 guardrails unverified.
Reviewing model: Nemotron 3 Ultra