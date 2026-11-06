//! Per-session write budgets: the memory bound on mounted writable
//! handles.
//!
//! Normative in `docs/contracts/write-path.md` (Resource bounds). A writable handle
//! buffers a sparse overlay (dirtied ranges only, plus the append
//! sequence); those resident bytes are memory, so the daemon budgets
//! them explicitly and fails closed (`ENOSPC` at the POSIX boundary)
//! rather than allocating without limit. The logical length is never
//! charged: a large file with a one-byte edit reserves one byte, and
//! the full image exists only transiently at commit time, covered by
//! the fold lease.
//!
//! Accounting is by **current resident bytes**, not accumulated writes:
//! overwriting buffered bytes costs nothing, and shrinking, clearing,
//! or truncating a buffer releases budget. Presence in the budget's map
//! *is* the dirty-handle mark, so an `O_TRUNC` handle counts even with
//! an empty overlay.
//!
//! These are v0 daemon budgets, named constants rather than architectural
//! commitments: the contracts are the three inequalities, not the
//! particular numbers. They are independent of the protocol ingest limits
//! (`Limits::V0`), which still bound every committed object —
//! `MAX_COMMITTABLE_BYTES` below names exactly that protocol ceiling so
//! both the FUSE boundary and the live loop refuse unpublishable lengths
//! at the same value.
//!
//! This module depends only on std: it is the `wyrd-core` session
//! surface.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

/// Largest logical image one writable handle may buffer.
pub const MAX_WRITE_BUFFER_BYTES: usize = 64 * 1024 * 1024;
/// Largest aggregate of all buffered logical images at once.
pub const MAX_BUFFERED_BYTES: usize = 256 * 1024 * 1024;
/// Most handles that may be dirty (buffered) at once.
pub const MAX_DIRTY_HANDLES: usize = 64;
/// Largest logical file the mount can buffer or commit: the protocol
/// ingest ceiling (max chunks x max chunk bytes), the same bound
/// authoring enforces before any snapshot publishes. A logical length
/// past it can never publish, so writes, truncates, takes, and
/// path-addressed resizes refuse `EFBIG` before allocating or leasing
/// anything. Unlike the three budgets above — which are daemon memory
/// bounds, enforced at the FUSE boundary — this is a protocol bound,
/// shared with the live loop's `SetAttrs` apply.
pub const MAX_COMMITTABLE_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// A stable per-handle accounting key, minted by the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HandleId(u64);

impl std::fmt::Display for HandleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "handle-{}", self.0)
    }
}

/// Why a buffer reservation was refused. `ENOSPC` at the POSIX boundary:
/// the handle's state is unchanged and the write performs nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BudgetError {
    /// The handle's logical image would exceed the per-handle bound.
    #[error("write buffer exceeds the per-handle bound")]
    Handle,
    /// The aggregate of buffered images would exceed the global bound.
    #[error("aggregate write buffers exceed the global bound")]
    Aggregate,
    /// Another handle would exceed the dirty-handle bound.
    #[error("too many dirty handles")]
    DirtyHandles,
    /// A lock is poisoned: fail closed.
    #[error("write budget lock poisoned")]
    Lock,
}

#[derive(Debug, Default)]
struct BudgetState {
    /// Current resident buffered bytes per dirty handle. Presence marks the
    /// handle dirty (budgeted); absent handles are clean and unbudgeted.
    lengths: BTreeMap<HandleId, usize>,
    total: usize,
}

/// Shared budget for all writable handles in one live session. The FUSE
/// backend holds it in an `Arc`; handles reserve and release through it.
///
/// Lock poisoning is never silent here: every lock failure bumps the
/// [`WriteBudget::poison_count`] counter and logs at error level, so an
/// operator (or a `SyncReport` consumer) can distinguish "nothing
/// budgeted" from "the bookkeeping is dead". The infallible
/// diagnostics ([`WriteBudget::total`], [`WriteBudget::dirty_handles`])
/// still read the live state — they never invent poison data — but the
/// counter beside them names the poison case.
#[derive(Debug)]
pub struct WriteBudget {
    state: Mutex<BudgetState>,
    next: AtomicU64,
    per_handle: usize,
    aggregate: usize,
    dirty_handles: usize,
    /// Lock-poison observations. Bumped once per failed acquisition,
    /// never reset: a nonzero count means every reading since is
    /// suspect until the process restarts with a fresh budget.
    poison: AtomicU64,
}

impl Default for WriteBudget {
    fn default() -> Self {
        WriteBudget::with_limits(
            MAX_WRITE_BUFFER_BYTES,
            MAX_BUFFERED_BYTES,
            MAX_DIRTY_HANDLES,
        )
    }
}

impl WriteBudget {
    /// A budget with explicit bounds; production uses the constants via
    /// [`Default`], tests use small bounds to exercise the caps without
    /// allocating the real limits.
    pub fn with_limits(per_handle: usize, aggregate: usize, dirty_handles: usize) -> Self {
        WriteBudget {
            state: Mutex::new(BudgetState::default()),
            next: AtomicU64::new(0),
            per_handle,
            aggregate,
            dirty_handles,
            poison: AtomicU64::new(0),
        }
    }

    /// Mint a fresh handle key.
    pub fn next_handle(&self) -> HandleId {
        HandleId(self.next.fetch_add(1, Ordering::Relaxed))
    }

    /// Reserve (or extend) `handle`'s resident buffered bytes to
    /// `new_len` bytes. Shrinking is always allowed and releases budget.
    /// Refusal leaves the budget and the handle's accounting unchanged.
    pub fn reserve(&self, handle: HandleId, new_len: usize) -> Result<(), BudgetError> {
        if new_len > self.per_handle {
            return Err(BudgetError::Handle);
        }
        let mut state = match self.lock() {
            Ok(state) => state,
            Err(error) => {
                self.note_poison("reserve");
                return Err(error);
            }
        };
        let old = state.lengths.get(&handle).copied();
        let new_dirty = old.is_none();
        if new_dirty && state.lengths.len() >= self.dirty_handles {
            return Err(BudgetError::DirtyHandles);
        }
        let adjusted = state.total - old.unwrap_or(0) + new_len;
        if adjusted > self.aggregate {
            return Err(BudgetError::Aggregate);
        }
        state.lengths.insert(handle, new_len);
        state.total = adjusted;
        Ok(())
    }

    /// Release `handle`'s budget entirely (commit success, failed commit,
    /// or close). Idempotent. A poisoned lock drops the release — the
    /// holder panicked, so the daemon is coming down — and counts it
    /// loudly instead of vanishing the bytes silently.
    pub fn release(&self, handle: HandleId) {
        let Ok(mut state) = self.state.lock() else {
            self.note_poison("release");
            return;
        };
        if let Some(len) = state.lengths.remove(&handle) {
            state.total = state.total.saturating_sub(len);
        }
    }

    /// Reserve transient fold-submission memory: a fold of B buffered
    /// bytes holds ~2B while the submission is in flight (the taken
    /// images plus their queue-kind clones, plus one restore clone per
    /// restored member at settle). Only the aggregate bound applies —
    /// a fold has no per-handle key — so this is the honest
    /// counterpart to [`reserve`](Self::reserve) for memory the
    /// handles' own reservations do not cover. Refusal leaves the
    /// budget unchanged; the caller restores the taken images and
    /// reports the pressure instead of submitting unaccounted.
    pub fn reserve_fold(&self, bytes: usize) -> Result<FoldLease<'_>, BudgetError> {
        let mut state = match self.lock() {
            Ok(state) => state,
            Err(error) => {
                self.note_poison("reserve_fold");
                return Err(error);
            }
        };
        let adjusted = state.total.saturating_add(bytes);
        if adjusted > self.aggregate {
            return Err(BudgetError::Aggregate);
        }
        state.total = adjusted;
        Ok(FoldLease {
            budget: self,
            bytes,
        })
    }

    /// Release transient fold-submission memory. Called by the
    /// lease's [`Drop`]; the saturating arithmetic keeps a logic
    /// error in lease scope from taking the aggregate negative.
    /// A refused reservation constructs no lease and changes nothing,
    /// so the refusal path restores images against exactly the budget
    /// it found.
    fn release_fold(&self, bytes: usize) {
        let Ok(mut state) = self.state.lock() else {
            self.note_poison("release_fold");
            return;
        };
        state.total = state.total.saturating_sub(bytes);
    }

    /// Introspection for providers and tests: the aggregate buffered bytes.
    /// A poisoned lock reads 0 — the live state is unreachable — and
    /// counts the observation, so 0 beside a nonzero
    /// [`WriteBudget::poison_count`] reads as "bookkeeping dead",
    /// never as "nothing buffered".
    pub fn total(&self) -> usize {
        match self.lock() {
            Ok(state) => state.total,
            Err(_) => {
                self.note_poison("total");
                0
            }
        }
    }

    /// Introspection for providers and tests: the number of dirty
    /// (budgeted) handles. Poison reads 0 and counts, like
    /// [`WriteBudget::total`].
    pub fn dirty_handles(&self) -> usize {
        match self.lock() {
            Ok(state) => state.lengths.len(),
            Err(_) => {
                self.note_poison("dirty_handles");
                0
            }
        }
    }

    /// Lock-poison observations since construction. Any nonzero value
    /// means the state behind the infallible diagnostics is suspect:
    /// reservations still fail closed, but totals may under-report.
    pub fn poison_count(&self) -> u64 {
        self.poison.load(Ordering::Relaxed)
    }

    /// Record one poisoned acquisition, loudly: the counter names it
    /// for status consumers, the log names it for the operator. Only
    /// ever called with a lock already known poisoned.
    fn note_poison(&self, op: &'static str) {
        self.poison.fetch_add(1, Ordering::Relaxed);
        tracing::error!(op, "write budget lock poisoned; bookkeeping unreachable");
    }

    fn lock(&self) -> Result<MutexGuard<'_, BudgetState>, BudgetError> {
        self.state.lock().map_err(|_| BudgetError::Lock)
    }
}

/// A transient fold-submission reservation: releases its bytes back
/// to the [`WriteBudget`] aggregate on drop, so no settle path can
/// leak the accounting. The backend holds it across the submit and
/// the settle; creating it is the only way to hold accounted fold
/// memory.
pub struct FoldLease<'a> {
    budget: &'a WriteBudget,
    bytes: usize,
}

impl Drop for FoldLease<'_> {
    fn drop(&mut self) {
        self.budget.release_fold(self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overwrite_and_shrink_do_not_accumulate() {
        let budget = WriteBudget::default();
        let handle = budget.next_handle();
        assert_eq!(budget.reserve(handle, 10_000), Ok(()));
        assert_eq!(budget.total(), 10_000);
        // Rewriting the same span costs nothing.
        assert_eq!(budget.reserve(handle, 10_000), Ok(()));
        assert_eq!(budget.total(), 10_000);
        // Shrinking releases budget.
        assert_eq!(budget.reserve(handle, 1_000), Ok(()));
        assert_eq!(budget.total(), 1_000);
        budget.release(handle);
        assert_eq!(budget.total(), 0);
        assert_eq!(budget.dirty_handles(), 0);
    }

    #[test]
    fn per_handle_and_dirty_handle_bounds() {
        let budget = WriteBudget::default();
        let handle = budget.next_handle();
        assert_eq!(
            budget.reserve(handle, MAX_WRITE_BUFFER_BYTES + 1),
            Err(BudgetError::Handle)
        );
        assert_eq!(budget.reserve(handle, MAX_WRITE_BUFFER_BYTES), Ok(()));
    }

    /// The budget is by resident buffered bytes: extending to the cap is
    /// allowed, one byte past is `ENOSPC`, and overwriting an existing
    /// span costs nothing.
    #[test]
    fn budget_tracks_resident_bytes_not_writes() {
        let mib = 1024 * 1024;
        let budget = WriteBudget::with_limits(64 * mib, 256 * mib, 64);
        let handle = budget.next_handle();
        assert_eq!(budget.reserve(handle, 60 * mib), Ok(()));
        assert_eq!(budget.reserve(handle, 63 * mib), Ok(()));
        assert_eq!(budget.reserve(handle, 64 * mib), Ok(()));
        assert_eq!(
            budget.reserve(handle, 64 * mib + 1),
            Err(BudgetError::Handle)
        );
        // A full-size overwrite does not accumulate.
        assert_eq!(budget.reserve(handle, 64 * mib), Ok(()));
        assert_eq!(budget.total(), 64 * mib);
    }

    #[test]
    fn aggregate_and_dirty_bounds_apply_across_handles() {
        let budget = WriteBudget::with_limits(100, 150, 1);
        let first = budget.next_handle();
        let second = budget.next_handle();
        assert_eq!(budget.reserve(first, 100), Ok(()));
        // Dirty-handle cap refuses a second before its bytes are even
        // considered.
        assert_eq!(budget.reserve(second, 10), Err(BudgetError::DirtyHandles));
        budget.release(first);
        // With room for dirty handles, the aggregate still bounds.
        let budget = WriteBudget::with_limits(100, 150, 4);
        let first = budget.next_handle();
        let second = budget.next_handle();
        assert_eq!(budget.reserve(first, 100), Ok(()));
        assert_eq!(budget.reserve(second, 60), Err(BudgetError::Aggregate));
        assert_eq!(budget.reserve(second, 50), Ok(()));
        assert_eq!(budget.total(), 150);
    }

    #[test]
    fn a_clean_handle_is_not_dirty_and_release_is_idempotent() {
        let budget = WriteBudget::default();
        let handle = budget.next_handle();
        budget.release(handle);
        budget.release(handle);
        assert_eq!(budget.dirty_handles(), 0);
        assert_eq!(budget.reserve(handle, 0), Ok(()), "O_TRUNC counts dirty");
        assert_eq!(budget.dirty_handles(), 1);
    }

    /// Poison the budget lock the way `poisoned_state_lock_is_recovered`
    /// does for the queue: mutating ops still fail closed, diagnostics
    /// never read healthy, and a dropped release leaves a visible
    /// deficit instead of a quiet zero.
    #[test]
    fn poisoned_budget_lock_is_visible_not_healthy() {
        let budget = std::sync::Arc::new(WriteBudget::default());
        let handle = budget.next_handle();
        assert_eq!(budget.reserve(handle, 1_000), Ok(()));
        assert_eq!(budget.poison_count(), 0, "no poison observed yet");
        let poisoner = std::sync::Arc::clone(&budget);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.state.lock().unwrap();
            panic!("poison the budget state");
        })
        .join();
        // Mutating ops fail closed, and every failure counts.
        assert_eq!(budget.reserve(handle, 2_000), Err(BudgetError::Lock));
        assert!(matches!(budget.reserve_fold(1), Err(BudgetError::Lock)));
        assert!(budget.poison_count() >= 2, "failed reservations count");
        // A release that cannot run drops loudly instead of vanishing
        // the bytes silently: the counter records the dropped
        // accounting, so the zero below reads as "dead, not idle".
        budget.release(handle);
        assert!(budget.poison_count() >= 3, "the dropped release counts");
        assert_eq!(budget.total(), 0);
        assert_eq!(budget.dirty_handles(), 0);
        assert!(
            budget.poison_count() >= 5,
            "reads beside a nonzero count are dead, not idle"
        );
    }

    /// A poisoned empty budget reads zero the way a healthy idle one
    /// does — but the counter distinguishes them.
    #[test]
    fn poisoned_empty_budget_reads_zero_with_count() {
        let budget = std::sync::Arc::new(WriteBudget::default());
        let poisoner = std::sync::Arc::clone(&budget);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.state.lock().unwrap();
            panic!("poison the budget state");
        })
        .join();
        assert_eq!(budget.total(), 0);
        assert_eq!(budget.dirty_handles(), 0);
        assert!(
            budget.poison_count() > 0,
            "zero with a count is dead, not idle"
        );
    }
}
