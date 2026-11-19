//! The reconciliation-request trigger (OD-21-4, decided C+A with
//! coalescing): when the engine sends its reconciliation view, and
//! when it stays quiet.
//!
//! The rule: send a request when either a durable gap is observed or
//! a reconnect occurs; never on a timer. A reconnect is an explicit
//! opportunity to discover gaps that could not have been observed
//! while disconnected — "no durable gap currently observed" is not
//! evidence that no gap exists after an offline interval. The live
//! loop reports two reconnect-class edges into the one latch:
//! recovery-counter movement, and initial attachment (the from-zero
//! connected-relays transition, which moves no counter because
//! recovery only fires post-attachment).
//!
//! Triggers coalesce, they do not add: a reconnect edge and a gap
//! observation in the same evaluation produce one request, and a
//! latched edge evaluated twice still yields one. Repeated edges
//! (one per actual session) each fire once — the latch is consumed
//! by the evaluation, so "once per session" falls out of the caller
//! reporting edges, not levels.
//!
//! The send path never authors facts. It derives the view live,
//! splits it into bounded pages, seals each page, and fans every page
//! out; the only state it keeps is the digest of the last
//! relay-accepted full view, in memory. A trigger
//! evaluation must never advance the store sequence on its own:
//! spontaneous commits would republish the serving generation on
//! drives whose content did not change, breaking the
//! conflicted-drive inertness contract (a cross-crate test pins
//! it). Stated views (`0x18`) are audit committed explicitly, never
//! by the trigger; received statements (`0x19`) are the facts that
//! matter, and those arrive through intake. A crash loses the
//! volatile marker, so a restart re-probes once rather than
//! resuming silence — the benign direction, since requests are
//! idempotent. An accepted send is relay-retained, so the marked
//! digest needs no resend until the view changes or the next
//! reconnect; an unaccepted send (offline) marks nothing, so the
//! next armed trigger retries.
//!
//! Known race, accepted: an accepted request that expires from the
//! relay before the sender drains it leaves a static view with no
//! re-fire until the next view change or reconnect. No permanent
//! loss follows — the sender's own pending obligations keep their
//! existing retry machinery ("retries until evidence arrives"), so
//! the request stays an accelerator, never the sole path. A timer
//! would close the race and is forbidden (OD-21-4): background
//! traffic on a healthy peer is the failure being avoided.
//!
//! A membership-frozen drive reconciles nothing, even when a gap is
//! parked or an edge arrives: unresolved membership leaves both the
//! recipient set and the obligations ambiguous, and the
//! conflicted-drive inertness contract forbids new facts — let alone
//! new traffic — until resolution. The edge stays latched while
//! frozen, so unfreezing re-arms the probe without a new reconnect.

use std::collections::BTreeSet;

use wyrd_format::DeviceId;

use super::engine::{Engine, EngineError, PublishBudget};
use crate::control::{seal as seal_control, Message, ReconciliationRequestPayload};
use crate::durable::{
    encode_reconciliation_view_canonical, ReconciliationView, MAX_RECONCILIATION_PAGE_BYTES,
};
use crate::transport::mailbox::{check_outbound_size, seal_for_recipient, Mailbox};

/// What one trigger evaluation did. The live loop drops the value
/// (it stays free of reporting dependencies; 21d surfaces it) —
/// tests match on it to pin the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconciliationOutcome {
    /// No trigger armed: no durable gap, no reconnect edge. No
    /// traffic, no facts — the anti-background-work invariant.
    Quiet,
    /// Armed, but the current digest is already marked
    /// relay-accepted: resending would convey nothing new. Only a
    /// reconnect probe or a view change re-sends.
    AlreadyStated,
    /// A request left for these recipients, every page of the current
    /// view fanned out once per active recipient; `accepted` counts relay
    /// acceptances across the whole burst, `pages` the statements it
    /// took. Zero marks nothing — no relay holds the bytes (offline)
    /// — so the next armed trigger retries; acceptance on every page
    /// marks the full-view digest, while a budget trip that leaves
    /// recipients unattempted marks nothing either (deferred without
    /// loss, counted in the pass publish outcome).
    Requested {
        recipients: usize,
        accepted: usize,
        pages: usize,
    },
    /// Armed but membership-frozen: an unresolved conflict leaves
    /// recipients and obligations ambiguous, and the conflicted
    /// drive stays inert — no traffic, no facts — until resolution.
    /// The edge stays latched, so unfreezing re-arms the probe.
    Frozen,
    /// Triggered, but this device holds no epoch control key to seal
    /// under. A keyless device states nothing; capabilities and
    /// rotation install keys, and their arrival changes the view and
    /// re-arms the trigger.
    NoSealingKey,
    /// Triggered, but no other active device is known: the canonical
    /// tip names nobody to ask. A lone device reconciles with
    /// no one.
    NoRecipients,
    /// A constructed page still exceeds the mailbox ceiling, so
    /// nothing was sent and nothing marked. Defensive only: pages are
    /// cut under the ceiling by construction, so this never fires
    /// unless the page bound outgrows the transport. Retried on the
    /// next armed evaluation.
    ViewTooLarge,
}

impl Engine {
    /// Latch a reconnect edge: the next trigger evaluation treats it
    /// as a fresh opportunity to discover gaps unobservable while
    /// disconnected. The caller reports edges (one per actual
    /// connected session, including session start), never levels —
    /// a latched edge evaluated twice still yields one request. The
    /// latch is consumed even when the send fails: a parked gap
    /// re-arms the next evaluation on its own, and a real outage
    /// re-edges through the supervisor — but a one-off failure
    /// while the transport stays up does not, so an edge-only probe
    /// lost to a blip waits for the next genuine reconnect, gap, or
    /// view change. A budget trip is the exception: the fan-out
    /// re-arms the latch itself when recipients go unattempted, so
    /// the retry needs no new edge.
    pub fn note_reconnected(&mut self) {
        self.reconnect_latched = true;
    }

    /// Evaluate the trigger once: gap, reconnect edge, or both (one
    /// request either way), else quiet. The gap signal today is the
    /// parked deferral queue — every held entry names a transition
    /// absent from the durable log or a blocked standing, so the
    /// signal is durable-grounded even though the queue itself is
    /// volatile (the relay retains every held envelope). OD-21-4's
    /// second gap source, the unfetchable head, is not wired: fetch
    /// produces no durable verdict to trigger on, and a drive whose
    /// only gap is an unfetchable head does not probe. That is
    /// deferred, not dropped — the predicate gains a disjunct when
    /// the producer exists. A frozen drive
    /// short-circuits to `ReconciliationOutcome::Frozen` before any
    /// seal or send, with the edge left latched.
    pub fn maybe_request_reconciliation(
        &mut self,
        mailbox: &mut impl Mailbox,
        budget: &mut PublishBudget,
    ) -> Result<ReconciliationOutcome, EngineError> {
        let gap = self.pending.len() > 0;
        if !gap && !self.reconnect_latched {
            return Ok(ReconciliationOutcome::Quiet);
        }
        if self.log.frozen_at().is_some() {
            return Ok(ReconciliationOutcome::Frozen);
        }
        // Gap-only repeat on a static store: derivation is a pure
        // function of the facts, so an unchanged sequence re-derives
        // the recorded digest — and a marked digest means the full
        // path would AlreadyState. Skip the replay; the outcome is
        // identical. Edges always evaluate (a probe is owed after
        // every reconnect), and unmarked digests always evaluate
        // (offline attempts must retry, oversize views must re-seal).
        if !self.reconnect_latched {
            if let Some((eval_seq, eval_digest)) = self.last_trigger_eval {
                if eval_seq == self.current() && self.last_requested_digest == Some(eval_digest) {
                    return Ok(ReconciliationOutcome::AlreadyStated);
                }
            }
        }
        let reconnected = std::mem::replace(&mut self.reconnect_latched, false);
        let facts = self.store.load()?;
        let view = ReconciliationView::derive(&facts);
        self.last_trigger_eval = Some((self.current(), view.evidence().digest()));
        self.request_once(mailbox, view, reconnected, budget)
    }

    /// One send attempt for the derived view: page the evidence into
    /// bounded statements, seal each under the newest held epoch key,
    /// fan every page out to every active recipient, and mark the
    /// full-view digest only when every recipient was attempted and
    /// every page was relay-accepted. A budget trip anywhere in the
    /// attempt defers the unattempted recipients without loss,
    /// leaves the digest unmarked, and re-arms the reconnect latch
    /// so the next pass retries even with no new gap or edge. The
    /// mark is volatile by design (see the module docs): a restart
    /// re-probes once.
    fn request_once(
        &mut self,
        mailbox: &mut impl Mailbox,
        view: ReconciliationView,
        reconnected: bool,
        budget: &mut PublishBudget,
    ) -> Result<ReconciliationOutcome, EngineError> {
        let digest = view.evidence().digest();
        if !reconnected && self.last_requested_digest == Some(digest) {
            return Ok(ReconciliationOutcome::AlreadyStated);
        }
        // Newest held epoch key: both sides of an active membership
        // hold recent keys, so the sender opens what the requester
        // seals. A device that never held a key states nothing (see
        // the outcome); a sender missing the epoch skips for relay
        // redelivery through the existing unknown-epoch path.
        let epoch = match self.epoch_keys.keys().next_back() {
            Some(epoch) => *epoch,
            None => return Ok(ReconciliationOutcome::NoSealingKey),
        };
        let recipients = self.request_recipients();
        if recipients.is_empty() {
            return Ok(ReconciliationOutcome::NoRecipients);
        }
        // Bound the whole attempt before sealing: a budget that
        // already tripped (or that trips below) sends nothing and
        // marks nothing — the next armed evaluation retries whole.
        // Sealing is CPU-only, so skipping it on a dead budget is
        // pure savings, never a semantic difference.
        let pages = view.evidence().pages();
        if !budget.check() {
            budget.defer(recipients.len());
            self.reconnect_latched = true;
            return Ok(ReconciliationOutcome::Requested {
                recipients: recipients.len(),
                accepted: 0,
                pages: pages.len(),
            });
        }
        // Page first, seal second: every page fits the mailbox ceiling
        // by construction, so an oversize full view fans out as
        // several bounded statements instead of sending nothing.
        // Pre-seal every page before any send: a page that still
        // exceeds the ceiling sends nothing and marks nothing — the
        // same fail-fast the single-statement path always had.
        let mut sealed_pages = Vec::with_capacity(pages.len());
        for page in &pages {
            let evidence_bytes = encode_reconciliation_view_canonical(page);
            debug_assert!(
                evidence_bytes.len() <= MAX_RECONCILIATION_PAGE_BYTES,
                "pages are cut under the page bound by construction"
            );
            let sealed = seal_control(
                &self.epoch_keys[&epoch],
                &self.drive,
                epoch,
                &Message::ReconciliationRequest(ReconciliationRequestPayload {
                    requester: self.device,
                    evidence: evidence_bytes,
                }),
            )?;
            let sealed_bytes = sealed.encode();
            if check_outbound_size(&sealed_bytes).is_err() {
                return Ok(ReconciliationOutcome::ViewTooLarge);
            }
            sealed_pages.push(sealed_bytes);
        }
        // Fan every page out to every active recipient,
        // recipient-major: one recipient's full page set leaves
        // before the next recipient's begins, so a tripped budget
        // defers whole recipients (the documented defer unit)
        // instead of half-fanning everyone. The budget is checked
        // between sends, never mid-send: one in-flight send can
        // overshoot the deadline by up to the per-send relay
        // timeout. Pages stay independent statements with
        // independent wire identities, answered and retired per
        // page under the existing caps. The full-view digest marks
        // only when every recipient was attempted and every page
        // was relay-accepted — a partial fan-out (rejection or
        // budget trip) retries whole, exactly like a partial send.
        let mut accepted = 0usize;
        let mut page_accepted = vec![false; sealed_pages.len()];
        let mut attempted = 0usize;
        'fan: for recipient in &recipients {
            for (index, sealed_bytes) in sealed_pages.iter().enumerate() {
                if !budget.check() {
                    // Publish budget tripped: this recipient and
                    // the rest stay unattempted for the next armed
                    // evaluation — deferred without loss, never
                    // retired, never marked. The current recipient
                    // counts as unattempted too (its page set is
                    // incomplete), so a retry fans it out whole.
                    // Re-arm the reconnect latch: without a parked
                    // gap this is the only arm the retry has, and
                    // the pre-seal check above re-arms the same
                    // way.
                    budget.defer(recipients.len() - attempted);
                    self.reconnect_latched = true;
                    break 'fan;
                }
                let envelope = seal_for_recipient(&self.identity_secret, *recipient, sealed_bytes)?;
                let report = mailbox.send(envelope)?;
                accepted += report.accepted;
                if report.accepted > 0 {
                    page_accepted[index] = true;
                }
            }
            attempted += 1;
        }
        if attempted == recipients.len() && page_accepted.iter().all(|ready| *ready) {
            self.last_requested_digest = Some(digest);
        }
        Ok(ReconciliationOutcome::Requested {
            recipients: recipients.len(),
            accepted,
            pages: sealed_pages.len(),
        })
    }

    /// Who to ask: the canonical tip's active members and readers,
    /// minus self — admitted readers keep converging past admission.
    /// Removed devices receive nothing further (`epochs.md`): they
    /// hold no later epoch secret, so they could neither open the
    /// request nor compose a useful response, while their sends (and
    /// per-relay timeouts) stall the live members behind them. No
    /// canonical tip (genesis conflict) names nobody: without a
    /// canonical roster there is no active set to ask. Deterministic
    /// ascending order: the send sequence must not depend on hash
    /// iteration.
    fn request_recipients(&self) -> Vec<DeviceId> {
        let Some(tip) = self.log.known_state().map(|state| state.transition_id) else {
            return Vec::new();
        };
        let mut recipients = BTreeSet::new();
        if let Some(members) = self.log.members_of(&tip) {
            recipients.extend(members);
        }
        if let Some(readers) = self.log.readers_of(&tip) {
            recipients.extend(readers);
        }
        recipients.remove(&self.device);
        recipients.into_iter().collect()
    }
}
