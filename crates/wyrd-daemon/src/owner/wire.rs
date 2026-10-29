//! Interface-local wire types (OD-16-C option B): the drive-owner
//! interface's own declaration of every value that crosses the
//! socket, losslessly mapped from the core and namespace types, never
//! reusing them. A future `wyrd-namespace` change is therefore an
//! internal change until this mapping extends — never an implicit
//! wire change — and no consumer decoder compile-depends on the
//! namespace crate's internals.
//!
//! Every `From` below destructures its source exhaustively (the
//! disclosure-map habit): adding a field to a core or namespace type
//! fails the build here until the mapping decides what the wire says
//! about it. Renaming or re-meaning a wire field is a protocol
//! change, reviewed as one — the field-name snapshot test in
//! `tests_owner` pins the wire side so an accidental rename fails
//! the suite instead of drifting the contract.
//!
//! Identifiers cross as lowercase hex (`Display` form): the wire
//! never invents an identity encoding, and parsing back is how the
//! round-trip test proves the mapping lossless.

use serde::{Deserialize, Serialize};
use wyrd_core::view::{Attr, ConflictVersion, DirEntry, Kind, Node};
use wyrd_format::{ContentId, SnapshotId};
use wyrd_sync::runtime::ReconciliationCounters;
use wyrd_sync::transport::mailbox::MailboxSnapshot;

use wyrd_core::live::{OwnerObservation, PendingObligations, SyncReport};
use wyrd_core::projection::{FreshnessState, ProjectionFreshness};
use wyrd_core::quarantine::QuarantineReport;
use wyrd_core::scrub::ScrubReport;
use wyrd_core::status::{
    ConvergenceState, HeadClasses, HeadSummary, MailboxView, PeerHandle, QueueDepth, SyncStatus,
};
use wyrd_sync::membership::KnownState;
use wyrd_sync::runtime::{
    DrainReport, DurabilityLevel, ExecuteReport, MaterializationSummary, OutboxTotals,
};

/// The wire protocol version this module speaks. A request whose `v`
/// differs is refused with [`WireErrorCode::UnknownVersion`] — never
/// interpreted under the wrong schema, never silently accepted. A
/// future version adds optional fields and documents what an older
/// consumer does with unknown values; it never re-means a field.
pub const WIRE_VERSION: u32 = 1;

/// Operation names. Plain strings on purpose (see `RawRequest`): an
/// unknown name is a refusal with the name echoed back, not a parse
/// failure the consumer cannot match to its request.
pub const OP_LOOKUP: &str = "lookup";
pub const OP_STAT: &str = "stat";
pub const OP_READDIR: &str = "readdir";
pub const OP_READ: &str = "read";
pub const OP_OBSERVE: &str = "observe";
pub const OP_LIVENESS: &str = "liveness";
pub const OP_FRESHNESS: &str = "freshness";

/// Encode 32 raw identifier bytes as wire hex.
fn hex_id(bytes: &[u8; 32]) -> String {
    hex::encode(bytes)
}

/// Parse wire hex back into 32 raw identifier bytes.
fn parse_id(raw: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(raw).map_err(|error| format!("invalid id hex: {error}"))?;
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| format!("id hex decodes to {} bytes, want 32", bytes.len()))
}

// ---------------------------------------------------------------------------
// Namespace values
// ---------------------------------------------------------------------------

/// The wire form of [`Kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireKind {
    File,
    Dir,
    Symlink,
    Conflict,
}

impl From<Kind> for WireKind {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::File => WireKind::File,
            Kind::Dir => WireKind::Dir,
            Kind::Symlink => WireKind::Symlink,
            Kind::Conflict => WireKind::Conflict,
        }
    }
}

impl WireKind {
    /// Map back to the namespace kind. Total by construction — the
    /// match is exhaustive so a new wire variant fails here, not in
    /// a consumer.
    pub fn to_kind(self) -> Kind {
        match self {
            WireKind::File => Kind::File,
            WireKind::Dir => Kind::Dir,
            WireKind::Symlink => Kind::Symlink,
            WireKind::Conflict => Kind::Conflict,
        }
    }
}

/// The wire form of [`Attr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireAttr {
    pub kind: WireKind,
    pub size: u64,
    pub executable: bool,
}

impl From<&Attr> for WireAttr {
    fn from(attr: &Attr) -> Self {
        let Attr {
            kind,
            size,
            executable,
        } = *attr;
        WireAttr {
            kind: WireKind::from(kind),
            size,
            executable,
        }
    }
}

impl WireAttr {
    /// Map back to the namespace value.
    pub fn to_attr(self) -> Attr {
        Attr {
            kind: self.kind.to_kind(),
            size: self.size,
            executable: self.executable,
        }
    }
}

/// The wire form of [`Node`]. Recursive through [`WireConflictVersion`],
/// like the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireNode {
    File {
        size: u64,
        executable: bool,
        chunks: Vec<String>,
    },
    Dir {
        subtree: String,
    },
    Symlink {
        target: String,
    },
    MergedDir {
        subtrees: Vec<(String, String)>,
    },
    Conflict {
        versions: Vec<WireConflictVersion>,
    },
}

/// The wire form of [`ConflictVersion`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireConflictVersion {
    pub snapshot: String,
    pub node: Box<WireNode>,
}

impl From<&Node> for WireNode {
    fn from(node: &Node) -> Self {
        match node {
            Node::File {
                size,
                executable,
                chunks,
            } => WireNode::File {
                size: *size,
                executable: *executable,
                chunks: chunks.iter().map(|id| hex_id(id.as_bytes())).collect(),
            },
            Node::Dir { subtree } => WireNode::Dir {
                subtree: hex_id(subtree.as_bytes()),
            },
            Node::Symlink { target } => WireNode::Symlink {
                target: target.clone(),
            },
            Node::MergedDir { subtrees } => WireNode::MergedDir {
                subtrees: subtrees
                    .iter()
                    .map(|(snapshot, content)| {
                        (hex_id(snapshot.as_bytes()), hex_id(content.as_bytes()))
                    })
                    .collect(),
            },
            Node::Conflict { versions } => WireNode::Conflict {
                versions: versions.iter().map(WireConflictVersion::from).collect(),
            },
        }
    }
}

impl From<&ConflictVersion> for WireConflictVersion {
    fn from(version: &ConflictVersion) -> Self {
        let ConflictVersion { snapshot, node } = version;
        WireConflictVersion {
            snapshot: hex_id(snapshot.as_bytes()),
            node: Box::new(WireNode::from(node)),
        }
    }
}

impl WireNode {
    /// Map back to the namespace value, parsing every identifier.
    /// The round-trip test proves this lossless for every variant,
    /// including multi-version conflicts.
    pub fn to_node(&self) -> Result<Node, String> {
        match self {
            WireNode::File {
                size,
                executable,
                chunks,
            } => {
                let mut ids = Vec::with_capacity(chunks.len());
                for raw in chunks {
                    ids.push(ContentId::from_bytes(parse_id(raw)?));
                }
                Ok(Node::File {
                    size: *size,
                    executable: *executable,
                    chunks: ids,
                })
            }
            WireNode::Dir { subtree } => Ok(Node::Dir {
                subtree: ContentId::from_bytes(parse_id(subtree)?),
            }),
            WireNode::Symlink { target } => Ok(Node::Symlink {
                target: target.clone(),
            }),
            WireNode::MergedDir { subtrees } => {
                let mut pairs = Vec::with_capacity(subtrees.len());
                for (snapshot, content) in subtrees {
                    pairs.push((
                        SnapshotId::from_bytes(parse_id(snapshot)?),
                        ContentId::from_bytes(parse_id(content)?),
                    ));
                }
                Ok(Node::MergedDir { subtrees: pairs })
            }
            WireNode::Conflict { versions } => {
                let mut out = Vec::with_capacity(versions.len());
                for version in versions {
                    out.push(ConflictVersion {
                        snapshot: SnapshotId::from_bytes(parse_id(&version.snapshot)?),
                        node: version.node.to_node()?,
                    });
                }
                Ok(Node::Conflict { versions: out })
            }
        }
    }
}

/// The wire form of [`DirEntry`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireDirEntry {
    pub name: String,
    pub node: WireNode,
}

impl From<&DirEntry> for WireDirEntry {
    fn from(entry: &DirEntry) -> Self {
        let DirEntry { name, node } = entry;
        WireDirEntry {
            name: name.clone(),
            node: WireNode::from(node),
        }
    }
}

// ---------------------------------------------------------------------------
// Durable status values
// ---------------------------------------------------------------------------

/// The wire form of [`HeadSummary`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireHeadSummary {
    pub id: String,
    pub epoch: u64,
    pub author: String,
}

impl From<&HeadSummary> for WireHeadSummary {
    fn from(head: &HeadSummary) -> Self {
        let HeadSummary { id, epoch, author } = head;
        WireHeadSummary {
            id: hex_id(id.as_bytes()),
            epoch: *epoch,
            author: hex_id(author.as_bytes()),
        }
    }
}

/// The wire form of [`HeadClasses`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireHeadClasses {
    pub eligible: usize,
    pub canonical_history: usize,
    pub superseded: usize,
    pub stranded: usize,
    pub voided: usize,
    pub pending: usize,
    pub rejected: usize,
}

impl From<&HeadClasses> for WireHeadClasses {
    fn from(classes: &HeadClasses) -> Self {
        let HeadClasses {
            eligible,
            canonical_history,
            superseded,
            stranded,
            voided,
            pending,
            rejected,
        } = classes;
        WireHeadClasses {
            eligible: *eligible,
            canonical_history: *canonical_history,
            superseded: *superseded,
            stranded: *stranded,
            voided: *voided,
            pending: *pending,
            rejected: *rejected,
        }
    }
}

/// The wire form of [`MailboxView`]: configuration only, never liveness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMailboxView {
    pub configured_relays: usize,
}

impl From<&MailboxView> for WireMailboxView {
    fn from(view: &MailboxView) -> Self {
        let MailboxView { configured_relays } = *view;
        WireMailboxView { configured_relays }
    }
}

/// The wire form of [`PeerHandle`]: the opaque handle crosses, never
/// the identity behind it — the wire keeps the status contract that
/// renderers name peers `peer-N` without learning who they are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WirePeerHandle {
    pub handle: usize,
    pub peer: String,
    pub pending: usize,
}

impl From<&PeerHandle> for WirePeerHandle {
    fn from(handle: &PeerHandle) -> Self {
        // Handles are opaque, not secret, but the identity behind
        // them stays server-side: the wire carries the handle's own
        // number and pending count, and the peer column renders the
        // same `peer-N` form the CLI prints. The destructure is
        // exhaustive so a new peer field fails here, not on the wire.
        let PeerHandle {
            handle,
            peer: _,
            pending,
        } = handle;
        WirePeerHandle {
            handle: *handle,
            peer: format!("peer-{handle}"),
            pending: *pending,
        }
    }
}

/// The wire form of [`QueueDepth`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireQueueDepth {
    pub outbox: usize,
    pub carries: usize,
    pub fetch: usize,
}

impl From<&QueueDepth> for WireQueueDepth {
    fn from(queue: &QueueDepth) -> Self {
        let QueueDepth {
            outbox,
            carries,
            fetch,
        } = *queue;
        WireQueueDepth {
            outbox,
            carries,
            fetch,
        }
    }
}

// ---------------------------------------------------------------------------
// Durable status values, continued
// ---------------------------------------------------------------------------

/// The wire form of [`PendingObligations`]: itemized pairs with
/// identifiers as hex. The pairs name snapshot ids and recipient
/// devices — member-visible authorship and obligation facts (H5/H6),
/// the same rows the CLI renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WirePendingObligations {
    pub announcements: Vec<(String, String)>,
    pub transitions: Vec<(String, String)>,
    pub capabilities: Vec<(u64, String)>,
}

impl From<&PendingObligations> for WirePendingObligations {
    fn from(obligations: &PendingObligations) -> Self {
        let PendingObligations {
            announcements,
            transitions,
            capabilities,
        } = obligations;
        WirePendingObligations {
            announcements: announcements
                .iter()
                .map(|(snapshot, recipient)| {
                    (hex_id(snapshot.as_bytes()), hex_id(recipient.as_bytes()))
                })
                .collect(),
            transitions: transitions
                .iter()
                .map(|(transition, recipient)| {
                    (hex_id(transition.as_bytes()), hex_id(recipient.as_bytes()))
                })
                .collect(),
            capabilities: capabilities
                .iter()
                .map(|(epoch, recipient)| (*epoch, hex_id(recipient.as_bytes())))
                .collect(),
        }
    }
}

/// The wire form of [`OutboxTotals`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireOutboxTotals {
    pub announcements_queued: usize,
    pub announcements_delivered: usize,
    pub transitions_queued: usize,
    pub transitions_delivered: usize,
    pub capabilities_queued: usize,
    pub capabilities_delivered: usize,
}

impl From<&OutboxTotals> for WireOutboxTotals {
    fn from(totals: &OutboxTotals) -> Self {
        let OutboxTotals {
            announcements_queued,
            announcements_delivered,
            transitions_queued,
            transitions_delivered,
            capabilities_queued,
            capabilities_delivered,
        } = *totals;
        WireOutboxTotals {
            announcements_queued,
            announcements_delivered,
            transitions_queued,
            transitions_delivered,
            capabilities_queued,
            capabilities_delivered,
        }
    }
}

/// The wire form of [`ConvergenceState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireConvergenceState {
    pub converged: bool,
    pub pending_heads: usize,
    pub unfetchable_heads: usize,
}

impl From<&ConvergenceState> for WireConvergenceState {
    fn from(state: &ConvergenceState) -> Self {
        let ConvergenceState {
            converged,
            pending_heads,
            unfetchable_heads,
        } = *state;
        WireConvergenceState {
            converged,
            pending_heads,
            unfetchable_heads,
        }
    }
}

/// The wire form of [`ReconciliationCounters`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireReconciliationCounters {
    pub statements_received: usize,
    pub transitions_reconciled: usize,
    pub capabilities_reconciled: usize,
    pub announcements_reconciled: usize,
}

impl From<&ReconciliationCounters> for WireReconciliationCounters {
    fn from(counters: &ReconciliationCounters) -> Self {
        let ReconciliationCounters {
            statements_received,
            transitions_reconciled,
            capabilities_reconciled,
            announcements_reconciled,
        } = *counters;
        WireReconciliationCounters {
            statements_received,
            transitions_reconciled,
            capabilities_reconciled,
            announcements_reconciled,
        }
    }
}

/// The wire form of [`MaterializationSummary`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMaterializationSummary {
    pub pinned: usize,
    pub cached: usize,
    pub local_objects: usize,
}

impl From<&MaterializationSummary> for WireMaterializationSummary {
    fn from(summary: &MaterializationSummary) -> Self {
        let MaterializationSummary {
            pinned,
            cached,
            local_objects,
        } = *summary;
        WireMaterializationSummary {
            pinned,
            cached,
            local_objects,
        }
    }
}

/// The wire form of [`DurabilityLevel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireDurabilityLevel {
    Working,
    Committed,
    Published,
}

impl From<&DurabilityLevel> for WireDurabilityLevel {
    fn from(level: &DurabilityLevel) -> Self {
        match level {
            DurabilityLevel::Working => WireDurabilityLevel::Working,
            DurabilityLevel::Committed => WireDurabilityLevel::Committed,
            DurabilityLevel::Published => WireDurabilityLevel::Published,
        }
    }
}

/// The wire form of [`KnownState`]: the membership tip with its roots
/// as hex. Roots are membership-tree content, member-visible like the
/// tip they anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireKnownState {
    pub epoch: u64,
    pub transition_id: String,
    pub members_root: String,
    pub owners_root: String,
    pub readers_root: String,
}

impl From<&KnownState> for WireKnownState {
    fn from(known: &KnownState) -> Self {
        let KnownState {
            epoch,
            transition_id,
            members_root,
            owners_root,
            readers_root,
        } = known;
        WireKnownState {
            epoch: *epoch,
            transition_id: hex_id(transition_id.as_bytes()),
            members_root: hex::encode(members_root),
            owners_root: hex::encode(owners_root),
            readers_root: hex::encode(readers_root),
        }
    }
}

/// The wire form of [`SyncStatus`]: the full durable observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSyncStatus {
    pub tip: Option<WireKnownState>,
    pub held_epochs: Vec<u64>,
    pub obligations: WirePendingObligations,
    pub totals: WireOutboxTotals,
    pub live_heads: Vec<WireHeadSummary>,
    pub head_classes: WireHeadClasses,
    pub mailbox: WireMailboxView,
    pub known_members: usize,
    pub peers: Vec<WirePeerHandle>,
    pub queue: WireQueueDepth,
    pub reconciliation: WireReconciliationCounters,
    pub convergence: WireConvergenceState,
    pub materialization: WireMaterializationSummary,
    pub durability: WireDurabilityLevel,
}

impl From<&SyncStatus> for WireSyncStatus {
    fn from(status: &SyncStatus) -> Self {
        // Exhaustive destructure: a new status row fails the build
        // here until the mapping decides where it belongs on the
        // wire. No row is silently dropped.
        let SyncStatus {
            tip,
            held_epochs,
            obligations,
            totals,
            live_heads,
            head_classes,
            mailbox,
            known_members,
            peers,
            queue,
            reconciliation,
            convergence,
            materialization,
            durability,
        } = status;
        WireSyncStatus {
            tip: tip.as_ref().map(WireKnownState::from),
            held_epochs: held_epochs.clone(),
            obligations: WirePendingObligations::from(obligations),
            totals: WireOutboxTotals::from(totals),
            live_heads: live_heads.iter().map(WireHeadSummary::from).collect(),
            head_classes: WireHeadClasses::from(head_classes),
            mailbox: WireMailboxView::from(mailbox),
            known_members: *known_members,
            peers: peers.iter().map(WirePeerHandle::from).collect(),
            queue: WireQueueDepth::from(queue),
            reconciliation: WireReconciliationCounters::from(reconciliation),
            convergence: WireConvergenceState::from(convergence),
            materialization: WireMaterializationSummary::from(materialization),
            durability: WireDurabilityLevel::from(durability),
        }
    }
}

// ---------------------------------------------------------------------------
// Run-report values (OD-16-D)
// ---------------------------------------------------------------------------

/// The wire form of [`MailboxSnapshot`]: the transport health behind
/// one pass, as plain data. Renders alongside — never merged into —
/// the durable status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMailboxHealth {
    pub stream_alive: bool,
    pub connected_relays: usize,
    pub total_relays: usize,
    pub saturation_recoveries: u64,
    pub supervisor_ticks: u64,
    pub relay_attached: bool,
    pub stream_recovery_attempts: u64,
    pub relay_recovery_attempts: u64,
    pub closed_subscriptions: u64,
}

impl From<&MailboxSnapshot> for WireMailboxHealth {
    fn from(health: &MailboxSnapshot) -> Self {
        let MailboxSnapshot {
            stream_alive,
            connected_relays,
            total_relays,
            saturation_recoveries,
            supervisor_ticks,
            relay_attached,
            stream_recovery_attempts,
            relay_recovery_attempts,
            closed_subscriptions,
        } = *health;
        WireMailboxHealth {
            stream_alive,
            connected_relays,
            total_relays,
            saturation_recoveries,
            supervisor_ticks,
            relay_attached,
            stream_recovery_attempts,
            relay_recovery_attempts,
            closed_subscriptions,
        }
    }
}

impl WireMailboxHealth {
    /// No health reported: the mailbox behind the pass has no
    /// supervisor. Renders as unknown, never as healthy — mirrors
    /// [`MailboxSnapshot::unknown`].
    pub fn unknown() -> Self {
        WireMailboxHealth::from(&MailboxSnapshot::unknown())
    }

    /// Transport liveness over snapshot data. A relay-closed
    /// subscription keeps every transport flag green while killing
    /// intake, so consumers read `closed_subscriptions` alongside.
    pub fn is_live(&self) -> bool {
        self.stream_alive && (self.total_relays == 0 || self.connected_relays > 0)
    }
}

/// The wire form of [`DrainReport`]. One deliberate reduction:
/// `peers_observed` crosses as a count, not identities — observed
/// peer devices stay opaque on the wire the way the status peer
/// handles stay opaque, while the count preserves the load signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireDrainReport {
    pub accepted: usize,
    pub duplicates: usize,
    pub deferred: usize,
    pub deferred_unseen: usize,
    pub deferred_status_blocked: usize,
    pub deferred_shed: usize,
    pub deferred_shed_unseen: usize,
    pub deferred_shed_status_blocked: usize,
    pub skipped: usize,
    pub discarded: usize,
    pub peers_observed: usize,
}

/// The wire form of [`ExecuteReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireExecuteReport {
    pub manifests: usize,
    pub snapshot_bodies: usize,
    pub objects: usize,
    pub unfulfilled: usize,
    pub transport_errors: usize,
    pub deadlines: usize,
    pub missing: usize,
    pub invalid: usize,
    pub unavailable_keys: usize,
    pub local_failures: usize,
}

/// The wire form of [`QuarantineReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireQuarantineReport {
    pub observed: u64,
    pub claims_cleared: u64,
    pub bytes_discarded: u64,
    pub failures: u64,
}

/// The wire form of [`ScrubReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireScrubReport {
    pub observed: u64,
    pub claims_cleared: u64,
    pub bytes_subtracted: u64,
    pub failures: u64,
}

/// The wire form of [`SyncReport`]: one pass's intake, fetch, repair,
/// and publication counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSyncReport {
    pub drained: WireDrainReport,
    pub fetched: WireExecuteReport,
    pub quarantined: WireQuarantineReport,
    pub scrubbed: WireScrubReport,
    pub published: bool,
    pub sent: usize,
    pub generation: u64,
    pub pending_heads: usize,
}

/// The liveness answer: the transport health and the last pass
/// report behind it, pinned to the pass count. `report` is `None`
/// before the first pass completes — an explicit "no report yet",
/// never a healthy-looking default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireLiveness {
    pub health: WireMailboxHealth,
    pub report: Option<WireSyncReport>,
    pub passes: u64,
}

// ---------------------------------------------------------------------------
// Freshness values (OD-32-C)
// ---------------------------------------------------------------------------

/// The wire form of [`FreshnessState`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireFreshnessState {
    FreshEmpty,
    FreshNonEmpty,
    RetainedLastGood { detail: String },
}

impl From<&FreshnessState> for WireFreshnessState {
    fn from(state: &FreshnessState) -> Self {
        match state {
            FreshnessState::FreshEmpty => WireFreshnessState::FreshEmpty,
            FreshnessState::FreshNonEmpty => WireFreshnessState::FreshNonEmpty,
            FreshnessState::RetainedLastGood { detail } => WireFreshnessState::RetainedLastGood {
                detail: detail.clone(),
            },
        }
    }
}

/// The wire form of [`ProjectionFreshness`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFreshness {
    pub state: WireFreshnessState,
    pub revision: u64,
    pub generation: u64,
}

impl From<&ProjectionFreshness> for WireFreshness {
    fn from(freshness: &ProjectionFreshness) -> Self {
        let ProjectionFreshness {
            state,
            revision,
            generation,
        } = freshness;
        WireFreshness {
            state: WireFreshnessState::from(state),
            revision: *revision,
            generation: *generation,
        }
    }
}

/// The wire form of [`OwnerObservation`]: the durable status plus
/// the run report behind it, pinned to the pass count. The two
/// halves stay separate fields so no consumer can render a live
/// socket as a durable fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireObservation {
    pub status: WireSyncStatus,
    pub report: WireSyncReport,
    pub health: WireMailboxHealth,
    pub passes: u64,
}

// ---------------------------------------------------------------------------
// Protocol envelope
// ---------------------------------------------------------------------------

/// A raw inbound request: version, client-matched id, operation name,
/// and per-operation parameters decoded by the dispatcher. Plain
/// strings and an untyped params object on purpose — an unknown `op`
/// is an explicit refusal echoing the name, not a parse failure the
/// client cannot match to its request, and a future version adds
/// operations without breaking this parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawRequest {
    pub v: u32,
    pub id: u64,
    pub op: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Parameters for the path-addressed read operations (`lookup`,
/// `stat`, `readdir`, `read`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathParams {
    pub path: String,
}

/// A response: the protocol version, the echoed request id, and
/// exactly one of a result or a refusal. The id echoes even on
/// refusals the server could parse (unknown version, unknown
/// operation) so the client always matches the answer; an
/// unparseable frame is the only answer without an id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireResponse {
    pub v: u32,
    pub id: u64,
    pub result: WireResult,
}

/// Either a per-operation result or an explicit refusal. No silent
/// success, no silent ignore: unsupported operations fail here with
/// the name echoed in the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireResult {
    #[serde(rename = "ok")]
    Ok(Box<WireOk>),
    #[serde(rename = "err")]
    Err(WireError),
}

/// Per-operation results. Every read result carries the serving
/// generation and durable revision it was served from, so the
/// consumer correlates answers with the freshness judgment instead
/// of assuming recency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireOk {
    Lookup {
        node: WireNode,
        generation: u64,
        revision: u64,
    },
    Stat {
        attr: WireAttr,
        generation: u64,
        revision: u64,
    },
    Readdir {
        entries: Vec<WireDirEntry>,
        generation: u64,
        revision: u64,
    },
    Read {
        bytes_hex: String,
        generation: u64,
        revision: u64,
    },
    Observe {
        status: WireSyncStatus,
    },
    Liveness {
        liveness: WireLiveness,
    },
    Freshness {
        freshness: WireFreshness,
    },
}

/// An explicit refusal: a machine-readable code plus a human message.
/// Transport-level failures (broken connection, unparseable frame)
/// never produce one of these — a refusal is always a terminal
/// answer to a parsed request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireError {
    pub code: WireErrorCode,
    pub message: String,
}

/// Refusal codes. Unknown and unsupported operations, versions, and
/// values fail here — never as success, never as silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireErrorCode {
    UnknownVersion,
    Malformed,
    UnsupportedOperation,
    NotFound,
    NotADirectory,
    NotAFile,
    Conflicted,
    NotMaterialized,
    Unavailable,
    Corrupt,
    RejectedRepresentation,
    LostRepresentation,
    Store,
    TooLarge,
    Timeout,
    Saturated,
    Unobserved,
    Poisoned,
    Io,
}

impl From<&DrainReport> for WireDrainReport {
    fn from(report: &DrainReport) -> Self {
        let DrainReport {
            accepted,
            duplicates,
            deferred,
            deferred_unseen,
            deferred_status_blocked,
            deferred_shed,
            deferred_shed_unseen,
            deferred_shed_status_blocked,
            skipped,
            discarded,
            peers_observed,
        } = report;
        WireDrainReport {
            accepted: *accepted,
            duplicates: *duplicates,
            deferred: *deferred,
            deferred_unseen: *deferred_unseen,
            deferred_status_blocked: *deferred_status_blocked,
            deferred_shed: *deferred_shed,
            deferred_shed_unseen: *deferred_shed_unseen,
            deferred_shed_status_blocked: *deferred_shed_status_blocked,
            skipped: *skipped,
            discarded: *discarded,
            // Identities stay opaque: the count preserves the load
            // signal, the devices behind it never cross the wire.
            peers_observed: peers_observed.len(),
        }
    }
}

impl From<&ExecuteReport> for WireExecuteReport {
    fn from(report: &ExecuteReport) -> Self {
        let ExecuteReport {
            manifests,
            snapshot_bodies,
            objects,
            unfulfilled,
            transport_errors,
            deadlines,
            missing,
            invalid,
            unavailable_keys,
            local_failures,
        } = *report;
        WireExecuteReport {
            manifests,
            snapshot_bodies,
            objects,
            unfulfilled,
            transport_errors,
            deadlines,
            missing,
            invalid,
            unavailable_keys,
            local_failures,
        }
    }
}

impl From<&QuarantineReport> for WireQuarantineReport {
    fn from(report: &QuarantineReport) -> Self {
        let QuarantineReport {
            observed,
            claims_cleared,
            bytes_discarded,
            failures,
        } = *report;
        WireQuarantineReport {
            observed,
            claims_cleared,
            bytes_discarded,
            failures,
        }
    }
}

impl From<&ScrubReport> for WireScrubReport {
    fn from(report: &ScrubReport) -> Self {
        let ScrubReport {
            observed,
            claims_cleared,
            bytes_subtracted,
            failures,
        } = *report;
        WireScrubReport {
            observed,
            claims_cleared,
            bytes_subtracted,
            failures,
        }
    }
}

impl From<&SyncReport> for WireSyncReport {
    fn from(report: &SyncReport) -> Self {
        let SyncReport {
            drained,
            fetched,
            quarantined,
            scrubbed,
            published,
            sent,
            generation,
            pending_heads,
        } = report;
        WireSyncReport {
            drained: WireDrainReport::from(drained),
            fetched: WireExecuteReport::from(fetched),
            quarantined: WireQuarantineReport::from(quarantined),
            scrubbed: WireScrubReport::from(scrubbed),
            published: *published,
            sent: *sent,
            generation: *generation,
            pending_heads: *pending_heads,
        }
    }
}

impl From<&OwnerObservation> for WireObservation {
    fn from(observation: &OwnerObservation) -> Self {
        let OwnerObservation {
            status,
            report,
            health,
            passes,
        } = observation;
        WireObservation {
            status: WireSyncStatus::from(status),
            report: WireSyncReport::from(report),
            health: WireMailboxHealth::from(health),
            passes: *passes,
        }
    }
}
