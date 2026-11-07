//! Status surface: the person-visible sync vocabulary (OD-18-A/B/C).
//!
//! Three categories stay distinct in contract and presentation, and
//! no future protocol improvement may silently re-mean today's
//! fields:
//!
//! 1. Facts: what this device has observed ([`SyncStatus`], committed
//!    facts only, side-effect free).
//! 2. Local signals: what this device has deferred or failed to
//!    process, and why ([`IntakeDeferral`], reason-labelled, never
//!    aggregated across causes).
//! 3. Epistemic limits: what this device cannot establish about
//!    remote history (the standing sentences, permanent, not
//!    transient warnings).
//!
//! The surface reports what the device observes, discloses what it
//! cannot observe, and never turns a local operational signal into a
//! claim about remote state. In particular it never renders "in
//! sync", "up to date", a synced-files claim, or a peer as
//! connected: those would be remote-completeness verdicts the
//! protocol cannot deliver (a peer absent past relay retention
//! produces no local signal at all).
//!
//! Layout (OD-18-C option B): a five-group summary — local sync
//! activity, local head state, delivery obligations, intake
//! deferrals, peer/relay qualification — with the full observation
//! one disclosure away ([`render_status_detail`]). CLI parity is
//! factual, never textual: the summary derives from the same
//! `SyncStatus` the CLI renders, and the table test pins
//! fact-for-fact agreement without sharing a line of layout.
//!
//! When a host presenter exists to own person-visible copy, the
//! rendering below migrates there and this module keeps the state
//! vocabulary and predicates; until then the copy lives next to
//! the mapping its tests pin (same arrangement as `relay_posture`).

use wyrd_sync::runtime::DrainReport;

use super::relay_posture::RELAY_IDLE_LINE;
use super::status::SyncStatus;

/// Standing sentence one (OD-18-A): delivery is relay acceptance,
/// never recipient receipt. Permanent: renders on success, on an
/// empty outbox, and with no mailbox at all.
pub const DELIVERY_IS_RELAY_ACCEPTANCE: &str =
    "Delivery means the relay accepted it. It does not mean the recipient has it.\n";

/// Standing sentence two (OD-18-A): the blind spot, named. Permanent
/// for the same reason: a future "Synced" badge must not become an
/// unsupported guarantee by accident.
pub const CANNOT_TELL_MISSING_HISTORY: &str =
    "This device cannot tell you whether it is missing history from a peer. A peer that was away longer than the relay keeps messages produces no signal here.\n";

/// The typed intake-deferral observation (OD-18-B option B): the
/// semantically relevant reason-labelled count, never an aggregate.
/// The GUI never infers the reason from a count — the reason travels
/// in the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntakeDeferral {
    /// No intake pass has been observed yet: renders "Not observed",
    /// never zero. Absent cannot lie; a rendered zero would claim an
    /// observation that never happened.
    NotObserved,
    /// One intake pass observed; the count is messages held on a
    /// membership transition this device has never observed.
    Observed {
        /// Held on unseen membership only. The sibling causes —
        /// status-blocked waits and budget/quota sheds — are
        /// deliberately absent from this type: the first drains by
        /// itself, the second is relay-retained and revalidated next
        /// pass, and summing either into a freshness signal repeats
        /// the aggregate-`deferred` bug OD-18-B refuses.
        membership_unseen: usize,
    },
}

impl IntakeDeferral {
    /// Build the observation from one drain pass. Reads
    /// `deferred_unseen` only; every other cause counter stays out
    /// of the surface by construction, so no caller can re-aggregate
    /// them into a freshness metric from this type.
    pub fn from_drain(report: &DrainReport) -> IntakeDeferral {
        IntakeDeferral::Observed {
            membership_unseen: report.deferred_unseen,
        }
    }

    fn render(&self) -> String {
        match *self {
            IntakeDeferral::NotObserved => "intake deferred: Not observed\n".to_owned(),
            IntakeDeferral::Observed { membership_unseen } => {
                format!("intake deferred: {membership_unseen} (membership not observed)\n")
            }
        }
    }
}

/// Render the five-group summary (OD-18-C option B). Every figure
/// derives from the same `SyncStatus` the CLI renders; nothing here
/// is independently reconstructed state.
pub fn render_status_summary(status: &SyncStatus, intake: IntakeDeferral) -> String {
    let mut out = String::new();
    // 1. Local sync activity: what this device still has to do, never
    // whether it is current with anyone.
    if status.convergence.converged {
        out.push_str("sync: converged (nothing outstanding locally)\n");
    } else {
        out.push_str(&format!(
            "sync: active ({} outstanding)\n",
            status.queue.total()
        ));
    }
    // 2. Local head state: what this device holds. Never summarized
    // as remote completeness; held epochs are not capabilities.
    match &status.tip {
        Some(tip) => {
            let classes = &status.head_classes;
            out.push_str(&format!(
                "heads: {} live (epoch {} tip); {} eligible, {} pending, {} rejected\n",
                status.live_heads.len(),
                tip.epoch,
                classes.eligible,
                classes.pending,
                classes.rejected,
            ));
        }
        None => {
            // No membership tip observed. Live heads normally require
            // a tip, but the summary never assumes it: if heads exist
            // without a tip, their count still renders rather than
            // collapsing to "none" and disagreeing with the detail.
            if status.live_heads.is_empty() {
                out.push_str("heads: none observed (no membership tip)\n");
            } else {
                out.push_str(&format!(
                    "heads: {} live (no membership tip)\n",
                    status.live_heads.len()
                ));
            }
        }
    }
    // 3. Delivery obligations: queued vs relay-accepted vs pending.
    // "Delivered" never renders bare: the fact's whole meaning is
    // relay acceptance, so the word says so every time.
    let totals = &status.totals;
    let pending = &status.obligations;
    let relay_accepted = totals.announcements_delivered
        + totals.transitions_delivered
        + totals.capabilities_delivered;
    out.push_str(&format!(
        "obligations: {} pending ({} announcements, {} transitions, {} capabilities); {} relay-accepted\n",
        pending.len(),
        pending.announcements.len(),
        pending.transitions.len(),
        pending.capabilities.len(),
        relay_accepted,
    ));
    // 4. Intake deferrals: the local signal, labelled, never a peer
    // verdict.
    out.push_str(&intake.render());
    // 5. Peer/relay qualification: denominators and limits. Liveness
    // belongs to run reports; status states the omission.
    out.push_str(&format!(
        "peers: {} known ({} members)\n",
        status.peers.len(),
        status.known_members,
    ));
    if status.mailbox.configured_relays == 0 {
        out.push_str(RELAY_IDLE_LINE);
    } else {
        out.push_str(&format!(
            "mailbox: {} relays configured (liveness visible on sync now or mount)\n",
            status.mailbox.configured_relays
        ));
    }
    out.push_str("connectivity: not observed (this command does not connect)\n");
    out.push_str(DELIVERY_IS_RELAY_ACCEPTANCE);
    out.push_str(CANNOT_TELL_MISSING_HISTORY);
    out
}

/// Render the full observation (the disclosure target): every
/// `SyncStatus` field has a row here, itemized obligations
/// preserved per snapshot / transition / epoch for diagnosis, and
/// the same relay-accepted wording and standing sentences as the
/// summary. Factual parity with `sync_status_render`
/// (`wyrd-cli/src/commands/sync.rs`): same facts, never the same
/// layout — the CLI keeps its own copy and this renderer never
/// shares a line of it by construction.
pub fn render_status_detail(status: &SyncStatus, intake: IntakeDeferral) -> String {
    use std::collections::BTreeMap;

    fn group_pending<T: Ord + Copy>(pairs: &[(T, wyrd_format::DeviceId)]) -> BTreeMap<T, usize> {
        let mut grouped = BTreeMap::new();
        for (id, _) in pairs {
            *grouped.entry(*id).or_insert(0) += 1;
        }
        grouped
    }

    let mut out = String::new();
    match &status.tip {
        Some(tip) => {
            out.push_str(&format!("epoch {} tip {}\n", tip.epoch, tip.transition_id));
            let held = status
                .held_epochs
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            out.push_str(&format!("held secrets: {held}\n"));
        }
        None => out.push_str("membership: none observed\n"),
    }
    out.push_str(&format!("durability: {}\n", status.durability.as_str()));
    let totals = &status.totals;
    let pending = &status.obligations;
    out.push_str(&format!(
        "outbox announcements: {} queued, {} relay-accepted, {} pending\n",
        totals.announcements_queued,
        totals.announcements_delivered,
        pending.announcements.len(),
    ));
    for (snapshot, count) in group_pending(&pending.announcements) {
        out.push_str(&format!("  snapshot {snapshot}: {count} pending\n"));
    }
    out.push_str(&format!(
        "outbox transitions: {} queued, {} relay-accepted, {} pending\n",
        totals.transitions_queued,
        totals.transitions_delivered,
        pending.transitions.len(),
    ));
    for (id, count) in group_pending(&pending.transitions) {
        out.push_str(&format!("  transition {id}: {count} pending\n"));
    }
    out.push_str(&format!(
        "outbox capabilities: {} queued, {} relay-accepted, {} pending\n",
        totals.capabilities_queued,
        totals.capabilities_delivered,
        pending.capabilities.len(),
    ));
    for (epoch, count) in group_pending(&pending.capabilities) {
        out.push_str(&format!("  epoch {epoch}: {count} pending\n"));
    }
    let reconcile = &status.reconciliation;
    out.push_str(&format!(
        "reconciliation: {} statements received, {} transitions + {} capabilities + {} announcements retired\n",
        reconcile.statements_received,
        reconcile.transitions_reconciled,
        reconcile.capabilities_reconciled,
        reconcile.announcements_reconciled,
    ));
    if status.live_heads.is_empty() {
        out.push_str("live heads: none\n");
    } else {
        out.push_str(&format!("live heads: {}\n", status.live_heads.len()));
        for head in &status.live_heads {
            out.push_str(&format!(
                "  {} epoch {} author peer-{}\n",
                head.id,
                head.epoch,
                status.peer_handle(&head.author),
            ));
        }
    }
    out.push_str(&format!(
        "peers: {} known ({} members)\n",
        status.peers.len(),
        status.known_members,
    ));
    for entry in &status.peers {
        out.push_str(&format!(
            "  peer-{}: {} pending\n",
            entry.handle, entry.pending
        ));
    }
    let queue = &status.queue;
    out.push_str(&format!(
        "queue: {} outstanding ({} outbox, {} fetch)\n",
        queue.total(),
        queue.outbox,
        queue.fetch,
    ));
    if queue.carries > 0 {
        out.push_str(&format!(
            "  carries: {} staged (counted in outbox)\n",
            queue.carries
        ));
    }
    let convergence = &status.convergence;
    if convergence.converged {
        out.push_str("convergence: converged\n");
    } else {
        out.push_str(&format!(
            "convergence: not converged ({} outbox, {} fetch, {} heads pending, {} unfetchable)\n",
            queue.outbox, queue.fetch, convergence.pending_heads, convergence.unfetchable_heads,
        ));
    }
    out.push_str(&format!(
        "materialization: {} cached, {} pinned, {} local objects\n",
        status.materialization.cached,
        status.materialization.pinned,
        status.materialization.local_objects,
    ));
    let classes = &status.head_classes;
    out.push_str(&format!(
        "heads classified: {} eligible, {} canonical-history, {} superseded, {} stranded, {} voided, {} pending, {} rejected\n",
        classes.eligible,
        classes.canonical_history,
        classes.superseded,
        classes.stranded,
        classes.voided,
        classes.pending,
        classes.rejected,
    ));
    if status.mailbox.configured_relays == 0 {
        out.push_str(RELAY_IDLE_LINE);
    } else {
        out.push_str(&format!(
            "mailbox: {} relays configured (liveness visible on sync now or mount)\n",
            status.mailbox.configured_relays
        ));
    }
    out.push_str("connectivity: not observed (this command does not connect)\n");
    out.push_str(&intake.render());
    out.push_str(DELIVERY_IS_RELAY_ACCEPTANCE);
    out.push_str(CANNOT_TELL_MISSING_HISTORY);
    out
}

#[cfg(test)]
#[path = "tests_status_surface.rs"]
mod tests_status_surface;
