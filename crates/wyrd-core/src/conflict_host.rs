//! Conflict host acquisition: the read-only mapping from a live
//! engine to the conflict surface vocabulary.
//!
//! The surface (`conflict_surface`) owns the words; this module owns
//! the one join the surface cannot do itself — eligible heads from
//! `snapshot_heads`, branch facts from `live_heads`, the frozen epoch
//! from the membership log, and a real `merge_plan` over the store —
//! so a host presenter (today the `snapshot diverged` CLI rendering,
//! tomorrow the daemon status path) feeds the surface real engine
//! state instead of reimplementing the mapping per host.
//!
//! Read-only by construction: every input is a shared reference and
//! nothing here takes an engine, a store, or anything mutable. The
//! inspection runs only when the drive has actually diverged (two or
//! more eligible heads); a converged drive renders the standing
//! verdict and the not-requested comparison line, never an empty
//! plan-as-evidence.

use std::collections::BTreeMap;

use wyrd_format::{DeviceId, ObjectStore, SnapshotId};
use wyrd_sync::runtime::{Engine, EngineError};

use super::conflict_surface::{
    inspect_differences, render_conflict_screen, BranchHead, DivergenceState, FreezeState,
};

/// Default root-path budget for the on-demand comparison: the same
/// small number the surface tests exercise, so the CLI default and
/// the pinned behavior agree without a second constant.
pub const DEFAULT_INSPECTION_BUDGET: usize = 64;

/// Render the diverged-drive screen from live engine state: the
/// standing indicator, the freeze half with its remedy, and — only
/// on a diverged drive — the bounded what-differs comparison over
/// the real merge plan.
///
/// Eligible heads come from `snapshot_heads` classification (id and
/// epoch are durable facts); the author comes from the `live_heads`
/// projection joined by id. An eligible head with no live body — an
/// internal disagreement both projections fail safe on — keeps its
/// row under its fact epoch and the zero author, never dropped and
/// never evidence of absence. Plan failures propagate as engine
/// errors: a read-only report fails closed rather than rendering a
/// partial screen as complete.
pub fn diverged_report<S: ObjectStore>(
    engine: &Engine,
    store: &S,
    budget: usize,
) -> Result<String, EngineError>
where
    S::Error: std::fmt::Debug,
{
    use wyrd_sync::authorization::Classification;

    let mut eligible: Vec<(SnapshotId, u64)> = engine
        .snapshot_heads()?
        .iter()
        .filter(|head| matches!(head.classification, Classification::Eligible))
        .map(|head| (head.id, head.epoch))
        .collect();
    eligible.sort();

    let authors: BTreeMap<SnapshotId, DeviceId> = engine
        .live_heads()?
        .iter()
        .map(|head| {
            let snapshot = head.snapshot();
            (snapshot.snapshot_id(), snapshot.author)
        })
        .collect();
    let branches: Vec<BranchHead> = eligible
        .iter()
        .map(|(id, epoch)| BranchHead {
            id: *id,
            epoch: *epoch,
            author: authors
                .get(id)
                .copied()
                .unwrap_or(DeviceId::from_bytes([0; 32])),
        })
        .collect();

    let divergence = DivergenceState::from_branches(&branches);
    let freeze = FreezeState::from_frozen_epoch(engine.membership_log().frozen_at());
    let inspection = if divergence.is_diverged() {
        let ids: Vec<SnapshotId> = eligible.iter().map(|(id, _)| *id).collect();
        let plan = engine.merge_plan(store, ids)?;
        Some(inspect_differences(&plan, &branches, budget))
    } else {
        None
    };
    Ok(render_conflict_screen(
        &divergence,
        &freeze,
        inspection.as_ref(),
    ))
}

#[cfg(test)]
#[path = "tests_conflict_host.rs"]
mod tests_conflict_host;
