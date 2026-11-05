//! Per-pass publish budget: a wall-clock deadline plus clip
//! accounting shared by every send leg of one publish phase
//! (reconciliation answers, transition/capability delivery,
//! announcement discharge). The loops check the budget before each
//! obligation and stop when it trips; untried obligations stay
//! pending durably for the next pass — never retired, never
//! dropped. The live loop sizes the deadline per pass (config
//! budget clipped to the nearest held mutation); bounded test and
//! single-send paths use [`PublishBudget::unbounded`].
use std::time::Instant;

/// Clip state for one publish phase. Counts only obligations the
/// clip itself skipped — obligations skipped for missing keys,
/// orphans, or epoch gating are not deferrals, they were never
/// attempted for independent reasons and stay pending on their
/// own terms.
#[derive(Debug)]
pub struct PublishBudget {
    deadline: Instant,
    clipped: bool,
    deferred: usize,
}

impl PublishBudget {
    /// A budget tripping at `deadline`: checks after that instant
    /// fail and count the skipped obligation as deferred.
    pub fn new(deadline: Instant) -> Self {
        PublishBudget {
            deadline,
            clipped: false,
            deferred: 0,
        }
    }

    /// No deadline: every check passes, nothing ever clips. For
    /// single-send paths (bounded by the per-send relay timeout
    /// already) and tests that pin behavior unrelated to pacing.
    pub fn unbounded() -> Self {
        // Far-future, not `Instant::now() + Duration::MAX`: adding
        // the max duration to `now` can overflow on some platforms.
        PublishBudget::new(Instant::now() + std::time::Duration::from_secs(3600 * 24 * 365 * 100))
    }

    /// Check before attempting one obligation send. `true` means
    /// attempt it; `false` means the budget tripped — the caller
    /// breaks out of its send loop with its remainder (see
    /// [`PublishBudget::defer`]) and the obligation stays pending.
    /// Marks clipped; counting belongs to `defer`, so a trip the
    /// caller mishandles still reads as clipped with no phantom
    /// deferral.
    pub fn check(&mut self) -> bool {
        if Instant::now() >= self.deadline {
            self.clipped = true;
            false
        } else {
            true
        }
    }

    /// Record `n` obligations left unattempted after the budget
    /// tripped (the loop's remainder on break, current obligation
    /// included). Marks clipped like [`PublishBudget::check`]: a
    /// remainder exists only past the trip.
    pub fn defer(&mut self, n: usize) {
        if n > 0 {
            self.clipped = true;
            self.deferred += n;
        }
    }

    /// Whether any check tripped this phase.
    pub fn clipped(&self) -> bool {
        self.clipped
    }

    /// Obligations unattempted because the budget tripped.
    pub fn deferred(&self) -> usize {
        self.deferred
    }
}
