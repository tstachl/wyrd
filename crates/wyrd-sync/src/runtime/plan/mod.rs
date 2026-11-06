//! Fetch-plan orchestration for the runtime sync engine.
//!
//! The engine owns durable state and intake. This module owns the repeated
//! reconcile/fetch/commit pass that turns that state into local manifests and
//! objects. The individual fetch validators remain on `Engine` for now so the
//! security-sensitive state access stays explicit during the decomposition.

use wyrd_format::{ObjectStore, SnapshotId};

use super::engine::{Engine, EngineError, ExecuteReport, FetchKey};
use super::fetch::FetchOutcome;
use super::retention::snapshot_body_unit;
use super::{PendingObjectFetch, RuntimeState};
use crate::bulk::BulkSource;
use crate::control::SnapshotAnnouncement;
use crate::durable::{AuthorizeSnapshot, DurableError, Fact};

/// Look up the announcement a planned body derives from. The plan's
/// pending bodies come from the same projection, so a miss is a
/// reconcile/projection disagreement — an internal bug, not sender
/// data — and fails the pass instead of panicking the process.
fn planned_announcement<'a>(
    runtime: &'a RuntimeState,
    snapshot: &SnapshotId,
) -> Result<&'a SnapshotAnnouncement, EngineError> {
    runtime
        .announcement(snapshot)
        .ok_or(EngineError::AnnouncementUnavailable(*snapshot))
}

/// Execute the current fetch plan to convergence.
pub(super) fn execute(
    engine: &mut Engine,
    bulk: &mut impl BulkSource,
    objects: &mut impl ObjectStore,
) -> Result<ExecuteReport, EngineError> {
    execute_inner(engine, bulk, objects, None)
}

/// The same run under a wall-clock budget: the plan stops starting
/// fetch work once `deadline` passes, and every attempt started before
/// it is capped at the remaining time, so a stalled provider cannot
/// push a caller's timeout decision past its bound. Work not started
/// stays pending and the next run picks it up; the attempt cap is
/// cleared on the way out, including on error.
pub(super) fn execute_sliced(
    engine: &mut Engine,
    bulk: &mut impl BulkSource,
    objects: &mut impl ObjectStore,
    deadline: Option<std::time::Instant>,
) -> Result<ExecuteReport, EngineError> {
    bulk.set_attempt_deadline(deadline);
    let result = execute_inner(engine, bulk, objects, deadline);
    bulk.set_attempt_deadline(None);
    result
}

/// Whether a wall-clock budget is spent: every start point checks, so
/// a run owes at most the one in-flight attempt (itself capped) past
/// the deadline.
fn spent(deadline: Option<std::time::Instant>) -> bool {
    deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline)
}

/// Commit `facts` through the existing landing path, splitting an
/// over-ceiling batch into admissible commits instead of refusing the
/// whole batch identically every pass (zero progress past either
/// durable ceiling). Each half lands before the next is attempted, so
/// a giant pass converges in several commits. Returns whether the
/// batch landed: a torn commit reports `Ok(false)` and the caller must
/// rebuild from disk before trusting its in-memory projection — the
/// landed prefix stays durable and the remainder re-derives on the
/// next pass, preserving the torn-commit rebuild semantics. A single
/// fact that refuses on its own is genuinely inadmissible and errors
/// exactly like today's whole-batch refusal.
fn commit_admissible(engine: &mut Engine, facts: &[Fact]) -> Result<bool, EngineError> {
    if facts.is_empty() {
        return Ok(true);
    }
    match engine.commit_facts_landing(facts) {
        Ok(landed) => Ok(landed),
        Err(DurableError::TooManyRecords { .. }) | Err(DurableError::CommitTooLarge { .. })
            if facts.len() > 1 =>
        {
            // Over either ceiling: split the batch and retry, as the
            // refusal itself prescribes. A torn half stops the split —
            // the landed prefix stays durable — and the caller rebuilds.
            let mid = facts.len() / 2;
            if !commit_admissible(engine, &facts[..mid])? {
                return Ok(false);
            }
            commit_admissible(engine, &facts[mid..])
        }
        Err(error) => {
            let _ = engine.resync();
            Err(error.into())
        }
    }
}

/// Execute the current fetch plan to convergence, optionally under a
/// wall-clock budget.
fn execute_inner(
    engine: &mut Engine,
    bulk: &mut impl BulkSource,
    objects: &mut impl ObjectStore,
    deadline: Option<std::time::Instant>,
) -> Result<ExecuteReport, EngineError> {
    let mut report = ExecuteReport::default();
    engine.fetch_run += 1;
    // The retention gate reads live counters through shared handles,
    // so one gate per run sees every import this run lands: vault
    // bytes charge on import, plaintext on store insert, and each
    // check compares the fresh totals.
    let gate = engine.retention_gate();
    // One durable replay per run, not per pass: this executor commits
    // only snapshot-body, manifest, and local-object facts, and it
    // applies each of those to the in-memory projection through the
    // same mutator a replay uses (`record_snapshot_body`,
    // `record_manifest`, `mark_local_object`), so after a landed
    // commit the held state equals what a rebuild would produce and
    // the next pass reconciles from it directly. Nothing else can
    // commit while this loop holds `engine`, and the membership log
    // and keyring the executor reads are untouched by those fact
    // types, so only a torn commit forces the replay back.
    let mut rebuilt = engine.store.rebuild(engine.device)?;
    loop {
        let runtime = &mut rebuilt.runtime;
        let keyring = &rebuilt.keyring;
        let plan = runtime.reconcile();
        // Items this pass has not yet fulfilled, counted down as work
        // lands: the unfulfilled total covers everything planned and
        // not delivered, whether the run exhausted the plan or stopped
        // at the wall-clock budget.
        let mut remaining = plan.pending_snapshot_bodies.len()
            + plan.pending_snapshots.len()
            + plan.pending_manifests.len()
            + plan.pending_objects.len();
        let mut facts = Vec::new();
        for snapshot in &plan.pending_snapshot_bodies {
            if spent(deadline) {
                report.unfulfilled = remaining;
                // Checkpoint the fulfilled prefix before the normal
                // return: the vault imports already happened, and
                // dropping the batch would re-request it every tight
                // slice. A torn checkpoint leaves nothing durable (the
                // next run rebuilds from disk anyway); a genuine
                // refusal or store failure surfaces like the
                // end-of-pass commit.
                commit_admissible(engine, &facts)?;
                return Ok(report);
            }
            // Backoff: a repeatedly invalid body stops being attempted
            // while cooled. Bodies strike by snapshot: the announcement
            // carries only the id.
            let body_key = FetchKey::Body(*snapshot);
            if !engine.fetch_eligible(&body_key) {
                // Forensics for headless stalls: a cooled item is
                // never attempted, so its unfulfilled count survives
                // passes that otherwise look idle. The key names the
                // cooled representation; strikes cool it, fulfillment
                // clears it.
                tracing::debug!(key = ?body_key, "fetch skipped: backoff cooling");
                continue;
            }
            match super::fetch::snapshot_body(bulk, runtime, snapshot) {
                FetchOutcome::Fulfilled(body) => {
                    // The cross-record binding: the body must be the one
                    // the accepted announcement describes. The snapshot
                    // id covers the bytes, but a lying announcement can
                    // still name a real body under wrong metadata — and
                    // that metadata drives manifest-key selection while
                    // the body's own binding drives authorization. Only
                    // an agreeing pair commits; a disagreement is the
                    // sender's invalid data, not a transport failure.
                    let announced = planned_announcement(runtime, snapshot)?;
                    let agrees = announced.author == body.author
                        && announced.epoch == body.epoch
                        && announced.membership == body.membership;
                    if !agrees {
                        report.invalid += 1;
                        engine.note_fetch_invalid(&body_key);
                        continue;
                    }
                    match crate::durable::AuthorizedSnapshot::authorize(body, &engine.drive) {
                        Ok(authorized) => {
                            // Residency precedes the durable record: the
                            // verified body lands in the serving vault
                            // before the fact commits, so the recorded
                            // snapshot never names a missing body. The
                            // retention gate precedes the import: refused
                            // bytes are never stored (DG-4 A).
                            let encoded = authorized.snapshot().encode();
                            if let Err(refusal) = gate.check(encoded.len() as u64, 0) {
                                let unit = snapshot_body_unit(snapshot);
                                let _ = engine.refusals.record(&unit, refusal.quota);
                                tracing::debug!(
                                    unit,
                                    quota = refusal.quota,
                                    retained = refusal.retained,
                                    vault_seen = refusal.vault_seen,
                                    incoming = refusal.incoming_vault,
                                    "retention gate refused snapshot body before storage; nothing stored, nothing committed"
                                );
                                report.refused += 1;
                                continue;
                            }
                            match engine.vault.import(&encoded) {
                                Ok(_) => {
                                    runtime.record_snapshot_body(authorized.snapshot().clone())?;
                                    facts.push(crate::durable::Fact::SnapshotBody(authorized));
                                    report.snapshot_bodies += 1;
                                    remaining -= 1;
                                    engine.note_fetch_fulfilled(&body_key);
                                }
                                Err(error) => {
                                    // A full or unwritable disk aborts the
                                    // pass; anything else is a local I/O
                                    // condition that never strikes.
                                    if let Some(fatal) = super::fetch::fatal_vault(&error) {
                                        return Err(EngineError::Store(fatal));
                                    }
                                    report.local_failures += 1;
                                }
                            }
                        }
                        Err(_) => {
                            report.invalid += 1;
                            engine.note_fetch_invalid(&body_key);
                        }
                    }
                }
                FetchOutcome::Invalid => {
                    report.invalid += 1;
                    engine.note_fetch_invalid(&body_key);
                }
                FetchOutcome::Missing => report.missing += 1,
                FetchOutcome::UnavailableKey => report.unavailable_keys += 1,
                FetchOutcome::Transport => {
                    report.transport_errors += 1;
                    engine.note_fetch_transport_failure(&body_key);
                }
                // A sliced attempt is budget evidence, never
                // representation evidence: it always counts, but only
                // a nonzero grant burns — a zero grant means the walk
                // stopped before asking, which carries no information
                // about the representation. The item stays pending for
                // the next run either way.
                FetchOutcome::Deadline { slice } => {
                    report.deadlines += 1;
                    if !slice.is_zero() {
                        engine.note_fetch_deadline(&body_key);
                    }
                }
                FetchOutcome::Local => report.local_failures += 1,
                // Retention refusal: counted separately, never struck,
                // never terminal — retry re-evaluates the same ceiling.
                FetchOutcome::Refused => report.refused += 1,
                FetchOutcome::Store(fatal) => return Err(EngineError::Store(fatal)),
            }
        }
        for snapshot in &plan.pending_snapshots {
            if spent(deadline) {
                report.unfulfilled = remaining;
                // Checkpoint the fulfilled prefix before the normal
                // return: the vault imports already happened, and
                // dropping the batch would re-request it every tight
                // slice. A torn checkpoint leaves nothing durable (the
                // next run rebuilds from disk anyway); a genuine
                // refusal or store failure surfaces like the
                // end-of-pass commit.
                commit_admissible(engine, &facts)?;
                return Ok(report);
            }
            // Backoff: a repeatedly invalid root stops being attempted
            // while cooled (the snapshot stays pending). Roots strike by
            // snapshot: no storage address exists before the fetch.
            let root_key = FetchKey::Root(*snapshot);
            if !engine.fetch_eligible(&root_key) {
                tracing::debug!(key = ?root_key, "fetch skipped: backoff cooling");
                continue;
            }
            match super::fetch::root(
                &engine.drive,
                bulk,
                keyring,
                runtime,
                &engine.vault,
                snapshot,
                &gate,
                &engine.refusals,
            ) {
                FetchOutcome::Fulfilled(record) => {
                    runtime.record_manifest(record.clone())?;
                    facts.push(crate::durable::Fact::Manifest(record));
                    report.manifests += 1;
                    remaining -= 1;
                    engine.note_fetch_fulfilled(&root_key);
                }
                FetchOutcome::Invalid => {
                    report.invalid += 1;
                    engine.note_fetch_invalid(&root_key);
                }
                FetchOutcome::Missing => report.missing += 1,
                FetchOutcome::UnavailableKey => report.unavailable_keys += 1,
                FetchOutcome::Transport => {
                    report.transport_errors += 1;
                    engine.note_fetch_transport_failure(&root_key);
                }
                // Budget evidence, not representation evidence: always
                // counts, burns only on a nonzero grant (a zero grant
                // was never attempted); the item stays pending.
                FetchOutcome::Deadline { slice } => {
                    report.deadlines += 1;
                    if !slice.is_zero() {
                        engine.note_fetch_deadline(&root_key);
                    }
                }
                FetchOutcome::Local => report.local_failures += 1,
                // Retention refusal: counted separately, never struck,
                // never terminal — retry re-evaluates the same ceiling.
                FetchOutcome::Refused => report.refused += 1,
                FetchOutcome::Store(fatal) => return Err(EngineError::Store(fatal)),
            }
        }
        for (id, link) in &plan.pending_manifests {
            if spent(deadline) {
                report.unfulfilled = remaining;
                // Checkpoint the fulfilled prefix before the normal
                // return: the vault imports already happened, and
                // dropping the batch would re-request it every tight
                // slice. A torn checkpoint leaves nothing durable (the
                // next run rebuilds from disk anyway); a genuine
                // refusal or store failure surfaces like the
                // end-of-pass commit.
                commit_admissible(engine, &facts)?;
                return Ok(report);
            }
            // Backoff: a repeatedly invalid child representation is
            // skipped while cooled — no attempt, item stays pending.
            let child_key = FetchKey::Storage(link.storage);
            if !engine.fetch_eligible(&child_key) {
                tracing::debug!(key = ?child_key, "fetch skipped: backoff cooling");
                continue;
            }
            engine.note_fetch_attempted(id);
            match super::fetch::child(
                &engine.drive,
                bulk,
                keyring,
                runtime,
                &engine.vault,
                id,
                link,
                &gate,
                &engine.refusals,
            ) {
                FetchOutcome::Fulfilled(record) => {
                    runtime.record_manifest(record.clone())?;
                    facts.push(crate::durable::Fact::Manifest(record));
                    report.manifests += 1;
                    remaining -= 1;
                    engine.note_fetch_fulfilled(&child_key);
                }
                FetchOutcome::Invalid => {
                    report.invalid += 1;
                    engine.note_fetch_invalid(&child_key);
                }
                FetchOutcome::Missing => report.missing += 1,
                FetchOutcome::UnavailableKey => report.unavailable_keys += 1,
                FetchOutcome::Transport => {
                    report.transport_errors += 1;
                    engine.note_fetch_transport_failure(&child_key);
                }
                // Budget evidence, not representation evidence: always
                // counts, burns only on a nonzero grant (a zero grant
                // was never attempted); the item stays pending.
                FetchOutcome::Deadline { slice } => {
                    report.deadlines += 1;
                    if !slice.is_zero() {
                        engine.note_fetch_deadline(&child_key);
                    }
                }
                FetchOutcome::Local => report.local_failures += 1,
                // Retention refusal: counted separately, never struck,
                // never terminal — retry re-evaluates the same ceiling.
                FetchOutcome::Refused => report.refused += 1,
                FetchOutcome::Store(fatal) => return Err(EngineError::Store(fatal)),
            }
        }
        for (content, candidates) in &plan.pending_objects {
            if spent(deadline) {
                report.unfulfilled = remaining;
                // Checkpoint the fulfilled prefix before the normal
                // return: the vault imports already happened, and
                // dropping the batch would re-request it every tight
                // slice. A torn checkpoint leaves nothing durable (the
                // next run rebuilds from disk anyway); a genuine
                // refusal or store failure surfaces like the
                // end-of-pass commit.
                commit_admissible(engine, &facts)?;
                return Ok(report);
            }
            // Backoff: cooled representations are not attempted. A
            // content with every representation cooled makes no attempt
            // at all — the item just stays pending.
            let eligible: Vec<PendingObjectFetch> = candidates
                .iter()
                .filter(|candidate| engine.fetch_eligible(&FetchKey::Storage(candidate.storage_id)))
                .cloned()
                .collect();
            if eligible.is_empty() {
                // Forensics for headless stalls: every representation
                // cooled, so the item makes no attempt at all — the
                // quiet predicate sees an idle pass while unfulfilled
                // survives. One line per content identity, not per
                // cooled candidate.
                tracing::debug!(content = ?content, "fetch skipped: all representations cooling");
                continue;
            }
            // The generation attempted: terminal evaluation may only
            // complete generations with evidence, so every issued
            // fetch marks its identity even when the walk fails.
            engine.note_fetch_attempted(content);
            let attempt = super::fetch::object(
                &engine.drive,
                bulk,
                keyring,
                objects,
                &engine.vault,
                content,
                &eligible,
                &gate,
                &engine.refusals,
            );
            // Strike representations whose bytes arrived and failed
            // validation regardless of the aggregate verdict: a corrupt
            // candidate keeps earning strikes even when a later
            // candidate fulfilled. Transport failures strike on the
            // same ledger (an unreachable route backs off instead of
            // retrying every pass and starving the items behind it).
            // Sliced attempts never strike: the deadline belongs to the
            // waiter, and a sliced timeout carries no fault information
            // about the provider. They burn instead: repeated slices
            // back the representation off exactly like strikes, on a
            // separate ledger, so a hanging route stops being retried
            // every pass without ever being branded faulty. Absent,
            // key-less, locally-refused, and retention-refused
            // candidates are attributed nowhere: none of them is
            // evidence against the representation.
            for storage in &attempt.invalid {
                engine.note_fetch_invalid(&FetchKey::Storage(*storage));
            }
            for storage in &attempt.transport_failed {
                engine.note_fetch_transport_failure(&FetchKey::Storage(*storage));
            }
            // Counted per sliced representation, not per item: the
            // aggregate cannot undercount, so a burn always has its
            // deadline counted beside it. Zero grants count too (dry
            // budget is still budget pressure) but never burn.
            report.deadlines += attempt.deadline_sliced.len();
            for (storage, slice) in &attempt.deadline_sliced {
                if !slice.is_zero() {
                    engine.note_fetch_deadline(&FetchKey::Storage(*storage));
                }
            }
            match attempt.aggregate {
                FetchOutcome::Fulfilled(()) => {
                    runtime.mark_local_object(*content);
                    facts.push(crate::durable::Fact::LocalObject(*content));
                    report.objects += 1;
                    remaining -= 1;
                    // Only the representation that served valid bytes
                    // clears its backoff state; other candidates keep
                    // their accumulated strikes.
                    if let Some(storage) = attempt.fulfilled {
                        engine.note_fetch_fulfilled(&FetchKey::Storage(storage));
                    }
                }
                FetchOutcome::Invalid => {
                    report.invalid += 1;
                }
                // No count here: sliced representations are counted
                // per candidate above, so the aggregate cannot
                // undercount them.
                FetchOutcome::Deadline { .. } => {}
                FetchOutcome::Missing => report.missing += 1,
                FetchOutcome::UnavailableKey => report.unavailable_keys += 1,
                FetchOutcome::Transport => report.transport_errors += 1,
                FetchOutcome::Local => report.local_failures += 1,
                // Retention refusal: counted separately, never struck,
                // never terminal — retry re-evaluates the same ceiling.
                // The item stays pending, so `remaining` is untouched.
                FetchOutcome::Refused => report.refused += 1,
                FetchOutcome::Store(fatal) => return Err(EngineError::Store(fatal)),
            }
        }

        if facts.is_empty() {
            report.unfulfilled = remaining;
            return Ok(report);
        }
        // An over-ceiling batch splits into admissible commits (each
        // lands before the next is attempted) instead of refusing
        // identically every pass. A torn commit rebuilds from disk —
        // the landed prefix stays durable and the loop re-derives the
        // remainder — preserving the torn-commit rebuild semantics.
        if !commit_admissible(engine, &facts)? {
            rebuilt = engine.store.rebuild(engine.device)?;
        }
        // The pass's batch is durable (or the projection was rebuilt
        // around a tear), so the per-pass accumulation starts over and
        // the deadline exit below cannot recommit it.
        facts.clear();
        // The convergence pass the commits unlocked starts only if the
        // budget allows: stopping here leaves the rest planned and
        // pending for the next run.
        if spent(deadline) {
            report.unfulfilled = remaining;
            return Ok(report);
        }
    }
}

// Plan behavior tests live beside the executor, one file per theme:
// plan execution and convergence, and rejection/byte accounting.
#[cfg(test)]
mod tests_accounting;
#[cfg(test)]
mod tests_bounded;
#[cfg(test)]
mod tests_convergence;
#[cfg(test)]
mod tests_retention;
