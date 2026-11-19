//! The recipient's reconciliation view: the durable-state evidence a
//! sender compares its outstanding control obligations against
//! (`docs/contracts/sync-and-peers.md` DG-3). The view is a projection of
//! durable facts — committed transitions, held snapshots, installed
//! capability epochs — never another authoritative store, and the
//! wire statement (21b) is its transport representation, never
//! equivalent to it.
//!
//! Provenance is load-bearing: retirement (21c) may proceed only on
//! evidence that reached the sender as an authenticated statement
//! over durable state. A view derived from live memory proves
//! nothing, so the retire gate refuses it here — the obligation
//! invariant (`docs/contracts/crash-consistency.md:142-146`) as a negative
//! test, before any retire path exists to misuse it.
//!
//! Scope, decided: the conservativeness rule is the *exact-set*
//! projection. A stored statement counts as evidence iff it is a
//! per-class subset of what `derive` computes over the same base
//! facts — nothing more. DG-3 admits ancestry-widened proof
//! predicates (a successor whose validated ancestry contains T), but
//! those live in 21c's comparison logic, not in stored statements: a
//! future predicate change must not reinterpret what an old `0x18`
//! record claimed. A statement outside today's projection is
//! dropped at load, never reinterpreted — widening the admissible
//! predicates versions the projection alongside the record.

use std::collections::BTreeSet;

use thiserror::Error;
use wyrd_format::{DeviceId, SnapshotId, TransitionId};

use super::replay::LoadedFacts;

/// The per-class durable evidence, as sets: committed transition
/// ids, held snapshot ids (announced or bodied), and installed
/// capability epochs keyed by device. Sorted sets, so encoding is
/// deterministic and set difference (21c) is structural.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconciliationEvidence {
    /// Committed transition ids evidenced by the view.
    pub transitions: BTreeSet<TransitionId>,
    /// Held snapshot ids (announced or bodied) evidenced by the view.
    pub snapshots: BTreeSet<SnapshotId>,
    /// Installed capability epochs per device evidenced by the view.
    pub capabilities: BTreeSet<(DeviceId, u64)>,
}

/// Max canonical evidence bytes in one reconciliation statement
/// page (`docs/contracts/resource-limits.md`). Pages split the
/// sorted evidence sets greedily in class order so every sealed page
/// lands near 33 KiB — under the 64 KiB mailbox ceiling by
/// construction with margin for envelope and relay framing. The
/// bound counts canonical bytes (three u32 section counts plus 32
/// per transition/snapshot id and 40 per capability entry), never
/// entries: the largest entry is 40 bytes, so every non-empty
/// evidence set cuts into at least one page and paging always
/// terminates.
pub const MAX_RECONCILIATION_PAGE_BYTES: usize = 32 * 1024;

/// Canonical bytes of one evidence entry class framing: the three
/// u32 LE section counts the canonical encoding always carries.
const PAGE_COUNT_OVERHEAD_BYTES: usize = 12;
/// Canonical bytes per transition id in a page.
const PAGE_TRANSITION_BYTES: usize = 32;
/// Canonical bytes per snapshot id in a page.
const PAGE_SNAPSHOT_BYTES: usize = 32;
/// Canonical bytes per capability entry (device 32 plus epoch u64
/// LE 8) in a page.
const PAGE_CAPABILITY_BYTES: usize = 40;

/// Where a view came from. Only a durably committed view — one the
/// recipient stated over state that survived its own commit protocol
/// — may retire an obligation. Everything else is a projection for
/// comparison or testing, never evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewProvenance {
    /// Committed over durable facts; eligible to retire obligations.
    Durable,
    /// Projected from live memory; comparison and testing only.
    Memory,
}

/// A reconciliation view: evidence plus its provenance. There are
/// exactly two constructors, and only replay mints durability (see
/// [`LoadedFacts::latest_stated_view`](super::LoadedFacts::latest_stated_view)):
/// [`ReconciliationView::derive`] builds memory projections, which
/// the retire gate refuses. Provenance is never caller-supplied, so
/// no caller can launder a live projection into evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciliationView {
    evidence: ReconciliationEvidence,
    provenance: ViewProvenance,
}

impl ReconciliationEvidence {
    /// Per-class subset: every stated transition, snapshot, and
    /// capability epoch is contained in the other evidence. The
    /// conservative direction — a stored statement that claims more
    /// than the base facts hold is an over-claim, and over-claims
    /// retire what was never evidenced. Load drops them with a
    /// warning (see `DurableStore::load`); under-claims stay
    /// committable and cost only retransmission.
    pub fn is_subset_of(&self, other: &ReconciliationEvidence) -> bool {
        self.transitions.is_subset(&other.transitions)
            && self.snapshots.is_subset(&other.snapshots)
            && self.capabilities.is_subset(&other.capabilities)
    }

    /// The local content identity of this evidence: domain-separated
    /// BLAKE3 over the canonical record bytes. An audit and
    /// supersession handle for stored statements — what a later
    /// statement supersedes, and what a `*Reconciled` fact's local
    /// anchor can name. It is not the authenticated reconciliation
    /// statement identity the forget contract requires retirement to
    /// reference: that identity belongs to the wire statement (21b),
    /// never to these bytes. The digest hashes the unbounded
    /// canonical bytes, never the ceiling-checked record: hashing
    /// must hold for evidence of any size, including what no single
    /// commit may state.
    pub fn digest(&self) -> [u8; 32] {
        let bytes = super::codec::encode_reconciliation_view_canonical(self);
        blake3::derive_key(RECONCILIATION_VIEW_CONTEXT, &bytes)
    }

    /// Whether this evidence names nothing in any class.
    fn is_empty(&self) -> bool {
        self.transitions.is_empty() && self.snapshots.is_empty() && self.capabilities.is_empty()
    }

    /// Split into deterministic bounded pages (`sync-and-peers.md`):
    /// per-class subsets in sorted order, each page's canonical
    /// bytes within [`MAX_RECONCILIATION_PAGE_BYTES`]. Greedy fill in
    /// class order (transitions, then snapshots, then capabilities);
    /// a page closes when the next entry would overflow it, so pages
    /// are disjoint and their union is exactly this evidence.
    /// Re-deriving the same facts yields the same pages byte-for-byte,
    /// so every page's wire-statement identity is stable across
    /// reseals and redeliveries. Always returns at least one page —
    /// empty evidence states as one empty page, exactly like today's
    /// single-statement send.
    pub(crate) fn pages(&self) -> Vec<ReconciliationEvidence> {
        let mut pages = Vec::new();
        let mut current = ReconciliationEvidence::default();
        let mut bytes = PAGE_COUNT_OVERHEAD_BYTES;
        // Close the page when the next entry would overflow it. The
        // emptiness guard keeps a first entry that alone exceeds the
        // bound on its own page instead of looping forever — with the
        // entry sizes above that arm never fires, but the bound is a
        // contract constant and the splitter must not trust it.
        let fit = |pages: &mut Vec<ReconciliationEvidence>,
                   current: &mut ReconciliationEvidence,
                   bytes: &mut usize,
                   entry: usize| {
            if *bytes + entry > MAX_RECONCILIATION_PAGE_BYTES && !current.is_empty() {
                pages.push(std::mem::take(current));
                *bytes = PAGE_COUNT_OVERHEAD_BYTES;
            }
        };
        for id in &self.transitions {
            fit(&mut pages, &mut current, &mut bytes, PAGE_TRANSITION_BYTES);
            current.transitions.insert(*id);
            bytes += PAGE_TRANSITION_BYTES;
        }
        for id in &self.snapshots {
            fit(&mut pages, &mut current, &mut bytes, PAGE_SNAPSHOT_BYTES);
            current.snapshots.insert(*id);
            bytes += PAGE_SNAPSHOT_BYTES;
        }
        for entry in &self.capabilities {
            fit(&mut pages, &mut current, &mut bytes, PAGE_CAPABILITY_BYTES);
            current.capabilities.insert(*entry);
            bytes += PAGE_CAPABILITY_BYTES;
        }
        pages.push(current);
        pages
    }
}

/// Domain context for the stated-view identity.
const RECONCILIATION_VIEW_CONTEXT: &str = "wyrd reconciliation view v1";

/// Domain context for the wire-statement identity: what the forget
/// contract requires retirement to reference (21c's anchor).
const RECONCILIATION_STATEMENT_CONTEXT: &str = "wyrd reconciliation statement v1";

/// The wire-statement identity: domain-separated BLAKE3 over the
/// requester plus the canonical evidence bytes. Stable across
/// reseals (the seal nonce never enters it) and redeliveries, so it
/// is the content-dedupe key the envelope id cannot be: the same
/// statement commits the same fact no matter how many bytes carried
/// it. Distinct from the stated-view digest on purpose — that one
/// names local evidence for audit and supersession, this one names
/// the authenticated statement retirement must reference.
pub fn reconciliation_statement_digest(
    requester: &DeviceId,
    evidence: &ReconciliationEvidence,
) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(requester.as_bytes());
    bytes.extend_from_slice(&super::codec::encode_reconciliation_view_canonical(
        evidence,
    ));
    blake3::derive_key(RECONCILIATION_STATEMENT_CONTEXT, &bytes)
}

/// Retirement refused before it can happen: the evidence was never
/// committed durably.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReconciliationError {
    /// Retirement requires durable evidence, not a memory projection.
    #[error(
        "retirement requires a durably committed reconciliation view, not an in-memory projection"
    )]
    InMemoryView,
}

impl ReconciliationView {
    /// Project the evidence from loaded durable facts. Memory
    /// provenance: for comparison and testing, never for retirement.
    /// View facts themselves are statements about these base facts,
    /// not base facts, so derivation ignores the views bucket — a
    /// committed view never changes what derivation computes.
    pub fn derive(facts: &LoadedFacts) -> Self {
        let mut evidence = ReconciliationEvidence::default();
        evidence
            .transitions
            .extend(facts.transitions.iter().map(|t| t.transition_id()));
        evidence
            .snapshots
            .extend(facts.announcements.iter().map(|a| a.snapshot));
        evidence
            .snapshots
            .extend(facts.snapshot_bodies.iter().map(|s| s.snapshot_id()));
        for cap in &facts.capabilities {
            let epochs = cap.secrets.len() as u64;
            evidence
                .capabilities
                .extend((1..=epochs).map(|epoch| (cap.device, epoch)));
        }
        ReconciliationView {
            evidence,
            provenance: ViewProvenance::Memory,
        }
    }

    /// Wrap evidence that arrived as a replayed fact. Visible only
    /// inside `durable`, so only replay mints it, through
    /// [`LoadedFacts::latest_stated_view`](super::LoadedFacts::latest_stated_view):
    /// the only views the retire gate accepts. Confined this far
    /// because 21c's retire path lives elsewhere in `wyrd-sync`, and
    /// one visibility step is all that separates "the replayed
    /// bucket" from "any module".
    pub(super) fn from_replayed(evidence: ReconciliationEvidence) -> Self {
        ReconciliationView {
            evidence,
            provenance: ViewProvenance::Durable,
        }
    }

    /// The evidence sets.
    pub fn evidence(&self) -> &ReconciliationEvidence {
        &self.evidence
    }

    /// Where this view came from.
    pub fn provenance(&self) -> ViewProvenance {
        self.provenance
    }

    /// The obligation invariant as a gate: retirement on an
    /// in-memory-only view is refused. A crash between observing
    /// reconciliation and committing the reconciled fact resurrects
    /// the obligation (harmless retransmission); retiring on memory
    /// would lose it.
    pub fn check_retire_eligible(&self) -> Result<(), ReconciliationError> {
        match self.provenance {
            ViewProvenance::Durable => Ok(()),
            ViewProvenance::Memory => Err(ReconciliationError::InMemoryView),
        }
    }
}
