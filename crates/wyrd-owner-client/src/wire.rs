//! Owned protocol types for the drive-owner interface (OD-16-C
//! option B): every value that crosses the socket, declared here and
//! nowhere else. This crate depends on no domain crate — conversions
//! to and from the core and namespace types live in `wyrd-daemon`,
//! which performs every domain-to-wire mapping. A future
//! `wyrd-namespace` change is therefore an internal change until the
//! daemon's mapping extends — never an implicit wire change — and no
//! consumer decoder compile-depends on any domain crate's internals.
//!
//! Renaming or re-meaning a wire field is a protocol change,
//! reviewed as one — the field-name snapshot test pins the wire side
//! so an accidental rename fails the suite instead of drifting the
//! contract.
//!
//! Identifiers cross as lowercase hex: the wire never invents an
//! identity encoding, and the daemon's mapping parses them back.

use serde::{Deserialize, Serialize};

/// The wire protocol version this module speaks. A request whose `v`
/// differs is refused with [`WireErrorCode::UnknownVersion`] — never
/// interpreted under the wrong schema, never silently accepted. v2
/// adds the required identity precondition on entry-addressed
/// mutations (see `WireMutationKind`); v1 payloads cannot express
/// it and v1 servers silently ignore it, so both directions refuse
/// across the boundary instead of running mixed. A future version
/// adds optional fields and documents what an older consumer does
/// with unknown values; it never re-means a field.
pub const WIRE_VERSION: u32 = 2;

/// Operation names. Plain strings on purpose (see `RawRequest`): an
/// unknown name is a refusal with the name echoed back, not a parse
/// failure the consumer cannot match to its request.
pub const OP_LOOKUP: &str = "lookup";
/// The name of the `stat` read operation on the wire.
pub const OP_STAT: &str = "stat";
/// The name of the `readdir` read operation on the wire.
pub const OP_READDIR: &str = "readdir";
/// The name of the `read` read operation on the wire.
pub const OP_READ: &str = "read";
/// The name of the `observe` durable-status operation on the wire.
pub const OP_OBSERVE: &str = "observe";
/// The name of the `liveness` run-report operation on the wire.
pub const OP_LIVENESS: &str = "liveness";
/// The name of the `freshness` projection-freshness operation on the wire.
pub const OP_FRESHNESS: &str = "freshness";

/// Policy operations: the four engine operations the interface
/// forwards to the loop-thread policy executor (not the mutation
/// queue). Names are plain strings like the read operations above:
/// an unknown name refuses with the name echoed back.
pub const OP_POLICY_PIN: &str = "policy-pin";
/// The name of the `policy-unpin` engine operation on the wire.
pub const OP_POLICY_UNPIN: &str = "policy-unpin";
/// The name of the `policy-evict` engine operation on the wire.
pub const OP_POLICY_EVICT: &str = "policy-evict";
/// The name of the `policy-status` engine operation on the wire.
pub const OP_POLICY_STATUS: &str = "policy-status";

// ---------------------------------------------------------------------------
// Namespace values
// ---------------------------------------------------------------------------

/// The wire form of `Kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireKind {
    /// A regular file entry.
    File,
    /// A directory entry.
    Dir,
    /// A symlink entry.
    Symlink,
    /// A conflicted entry with one version per head.
    Conflict,
}

/// The wire form of `Attr`. `identity` is the entry's stable
/// token as lowercase hex, or `None` for entries that predate
/// identity support and for synthetic states. It defaults for
/// mixed-version skew during rollout: a pre-identity reader sees
/// no token instead of failing the parse.
///
/// `Clone` but not `Copy`: the token rides as an owned hex
/// string, and attr answers are cheap to clone explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireAttr {
    /// The entry kind these attributes describe.
    pub kind: WireKind,
    /// The file size in bytes (zero for non-files).
    pub size: u64,
    /// Whether the entry is executable (files only).
    pub executable: bool,
    /// The entry's stable identity token as lowercase hex, if known.
    #[serde(default)]
    pub identity: Option<String>,
}

/// The wire form of `Node`. Recursive through [`WireConflictVersion`],
/// like the source. File, dir, and symlink nodes carry the entry's
/// stable token as lowercase hex (`None` for pre-identity entries);
/// merged and conflicted states are synthetic views and carry none
/// (each conflict version carries its head's token).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireNode {
    /// A file node with its content addressing.
    File {
        /// The file size in bytes.
        size: u64,
        /// Whether the file is executable.
        executable: bool,
        /// The content chunk ids as lowercase hex.
        chunks: Vec<String>,
        /// The entry's stable identity token as lowercase hex, if known.
        #[serde(default)]
        identity: Option<String>,
    },
    /// A directory node addressed by its subtree id.
    Dir {
        /// The subtree id as lowercase hex.
        subtree: String,
        /// The entry's stable identity token as lowercase hex, if known.
        #[serde(default)]
        identity: Option<String>,
    },
    /// A symlink node carrying its target.
    Symlink {
        /// The symlink target as stored.
        target: String,
        /// The entry's stable identity token as lowercase hex, if known.
        #[serde(default)]
        identity: Option<String>,
    },
    /// A merged directory view over the agreeing heads.
    MergedDir {
        /// The agreeing (snapshot, subtree) pairs as lowercase hex.
        subtrees: Vec<(String, String)>,
        /// The agreed directory token, or null where heads disagree
        /// or predate identity. Mirrors the `#[serde(default)]`
        /// tolerance on the Rust side (see `WireAttr`).
        #[serde(default)]
        identity: Option<String>,
    },
    /// A conflicted node with one version per head.
    Conflict {
        /// The per-head versions of the conflict.
        versions: Vec<WireConflictVersion>,
    },
}

/// The wire form of `ConflictVersion`. `timestamp` is the head
/// snapshot's authoring timestamp (ms, display only); it defaults to
/// `0` on parse so a pre-timestamp reader degrades to "unknown"
/// instead of failing, and both sides ship together so the default
/// only covers mixed-version skew during rollout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireConflictVersion {
    /// The head snapshot id as lowercase hex.
    pub snapshot: String,
    /// The head snapshot's authoring timestamp in ms (display only).
    #[serde(default)]
    pub timestamp: u64,
    /// The head's version of the conflicted node.
    pub node: Box<WireNode>,
}

/// The wire form of `DirEntry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireDirEntry {
    /// The entry name within its parent directory.
    pub name: String,
    /// The entry's node.
    pub node: WireNode,
}

// ---------------------------------------------------------------------------
// Durable status values
// ---------------------------------------------------------------------------

/// The wire form of `HeadSummary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireHeadSummary {
    /// The head snapshot id as lowercase hex.
    pub id: String,
    /// The membership epoch the head belongs to.
    pub epoch: u64,
    /// The authoring device key as lowercase hex.
    pub author: String,
}

/// The wire form of `HeadClasses`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireHeadClasses {
    /// Heads eligible for canonical history.
    pub eligible: usize,
    /// Heads in canonical history.
    pub canonical_history: usize,
    /// Heads superseded by newer history.
    pub superseded: usize,
    /// Heads stranded outside history.
    pub stranded: usize,
    /// Heads voided by membership changes.
    pub voided: usize,
    /// Heads pending fetch or reconciliation.
    pub pending: usize,
    /// Heads rejected as unusable.
    pub rejected: usize,
}

/// The wire form of `MailboxView`: configuration only, never liveness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMailboxView {
    /// The number of configured relays.
    pub configured_relays: usize,
}

/// The wire form of `PeerHandle`: the opaque handle crosses, never
/// the identity behind it — the wire keeps the status contract that
/// renderers name peers `peer-N` without learning who they are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WirePeerHandle {
    /// The opaque peer handle renderers name `peer-N`.
    pub handle: usize,
    /// The peer's display name (never its identity).
    pub peer: String,
    /// The queued messages behind this peer handle.
    pub pending: usize,
}

/// The wire form of `QueueDepth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireQueueDepth {
    /// The queued outbound announcements depth.
    pub outbox: usize,
    /// The queued carry messages depth.
    pub carries: usize,
    /// The queued fetch requests depth.
    pub fetch: usize,
}

// ---------------------------------------------------------------------------
// Durable status values, continued
// ---------------------------------------------------------------------------

/// The wire form of `PendingObligations`: itemized pairs with
/// identifiers as hex. The pairs name snapshot ids and recipient
/// devices — member-visible authorship and obligation facts (H5/H6),
/// the same rows the CLI renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WirePendingObligations {
    /// Pending (snapshot, recipient-device) announcement pairs as hex.
    pub announcements: Vec<(String, String)>,
    /// Pending (snapshot, recipient-device) transition pairs as hex.
    pub transitions: Vec<(String, String)>,
    /// Pending (epoch, recipient-device) capability pairs.
    pub capabilities: Vec<(u64, String)>,
}

/// The wire form of `OutboxTotals`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireOutboxTotals {
    /// Announcements queued for send.
    pub announcements_queued: usize,
    /// Announcements delivered.
    pub announcements_delivered: usize,
    /// Transitions queued for send.
    pub transitions_queued: usize,
    /// Transitions delivered.
    pub transitions_delivered: usize,
    /// Capabilities queued for send.
    pub capabilities_queued: usize,
    /// Capabilities delivered.
    pub capabilities_delivered: usize,
}

/// The wire form of `ConvergenceState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireConvergenceState {
    /// Whether the replica has converged.
    pub converged: bool,
    /// Heads still pending fetch or reconciliation.
    pub pending_heads: usize,
    /// Heads that cannot be fetched.
    pub unfetchable_heads: usize,
}

/// The wire form of `ReconciliationCounters`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireReconciliationCounters {
    /// Statements received from peers.
    pub statements_received: usize,
    /// Transitions reconciled into history.
    pub transitions_reconciled: usize,
    /// Capabilities reconciled into history.
    pub capabilities_reconciled: usize,
    /// Announcements reconciled into history.
    pub announcements_reconciled: usize,
}

/// The wire form of `MaterializationSummary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMaterializationSummary {
    /// Files pinned to local retention.
    pub pinned: usize,
    /// Files cached locally without a pin.
    pub cached: usize,
    /// Local objects held in the store.
    pub local_objects: usize,
}

/// The wire form of `DurabilityLevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireDurabilityLevel {
    /// Working state: not yet committed.
    Working,
    /// Committed state: durable but unpublished.
    Committed,
    /// Published state: committed and announced.
    Published,
}

/// The wire form of `KnownState`: the membership tip with its roots
/// as hex. Roots are membership-tree content, member-visible like the
/// tip they anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireKnownState {
    /// The membership epoch of the tip.
    pub epoch: u64,
    /// The tip transition id as lowercase hex.
    pub transition_id: String,
    /// The members-tree root as lowercase hex.
    pub members_root: String,
    /// The owners-tree root as lowercase hex.
    pub owners_root: String,
    /// The readers-tree root as lowercase hex.
    pub readers_root: String,
}

/// The wire form of `SyncStatus`: the full durable observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSyncStatus {
    /// The membership tip, if the replica holds one.
    pub tip: Option<WireKnownState>,
    /// The epochs held locally.
    pub held_epochs: Vec<u64>,
    /// The itemized pending obligations.
    pub obligations: WirePendingObligations,
    /// The outbox queue/delivered totals.
    pub totals: WireOutboxTotals,
    /// The live head summaries.
    pub live_heads: Vec<WireHeadSummary>,
    /// The head class counts.
    pub head_classes: WireHeadClasses,
    /// The mailbox configuration view.
    pub mailbox: WireMailboxView,
    /// The number of known member devices.
    pub known_members: usize,
    /// The opaque peer handles.
    pub peers: Vec<WirePeerHandle>,
    /// The queue depths.
    pub queue: WireQueueDepth,
    /// The reconciliation counters.
    pub reconciliation: WireReconciliationCounters,
    /// The convergence judgment.
    pub convergence: WireConvergenceState,
    /// The materialization summary.
    pub materialization: WireMaterializationSummary,
    /// The durability level of the observation.
    pub durability: WireDurabilityLevel,
}

// ---------------------------------------------------------------------------
// Run-report values (OD-16-D)
// ---------------------------------------------------------------------------

/// The wire form of `MailboxSnapshot`: the transport health behind
/// one pass, as plain data. Renders alongside — never merged into —
/// the durable status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMailboxHealth {
    /// Whether the mailbox stream is alive.
    pub stream_alive: bool,
    /// The relays currently connected.
    pub connected_relays: usize,
    /// The relays configured in total.
    pub total_relays: usize,
    /// The saturation recoveries observed.
    pub saturation_recoveries: u64,
    /// The supervisor ticks observed.
    pub supervisor_ticks: u64,
    /// Whether a relay is currently attached.
    pub relay_attached: bool,
    /// The stream recovery attempts observed.
    pub stream_recovery_attempts: u64,
    /// The relay recovery attempts observed.
    pub relay_recovery_attempts: u64,
    /// The subscriptions closed by the relay.
    pub closed_subscriptions: u64,
}

impl WireMailboxHealth {
    /// No health reported: the mailbox behind the pass has no
    /// supervisor. Renders as unknown, never as healthy.
    pub fn unknown() -> Self {
        WireMailboxHealth {
            stream_alive: false,
            connected_relays: 0,
            total_relays: 0,
            saturation_recoveries: 0,
            supervisor_ticks: 0,
            relay_attached: false,
            stream_recovery_attempts: 0,
            relay_recovery_attempts: 0,
            closed_subscriptions: 0,
        }
    }

    /// Transport liveness over snapshot data. A relay-closed
    /// subscription keeps every transport flag green while killing
    /// intake, so consumers read `closed_subscriptions` alongside.
    pub fn is_live(&self) -> bool {
        self.stream_alive && (self.total_relays == 0 || self.connected_relays > 0)
    }
}

/// The wire form of `DrainReport`. One deliberate reduction:
/// `peers_observed` crosses as a count, not identities — observed
/// peer devices stay opaque on the wire the way the status peer
/// handles stay opaque, while the count preserves the load signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireDrainReport {
    /// Envelopes accepted into the queue.
    pub accepted: usize,
    /// Envelopes already seen (duplicates).
    pub duplicates: usize,
    /// Envelopes deferred to a later pass.
    pub deferred: usize,
    /// Deferred envelopes for unseen senders.
    pub deferred_unseen: usize,
    /// Deferred envelopes blocked on status.
    pub deferred_status_blocked: usize,
    /// Deferred envelopes shed under load.
    pub deferred_shed: usize,
    /// Shed deferred envelopes for unseen senders.
    pub deferred_shed_unseen: usize,
    /// Shed deferred envelopes blocked on status.
    pub deferred_shed_status_blocked: usize,
    /// Envelopes skipped without intake.
    pub skipped: usize,
    /// Envelopes discarded as unusable.
    pub discarded: usize,
    /// Peer devices observed this pass (count only, never identities).
    pub peers_observed: usize,
}

/// The wire form of `ExecuteReport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireExecuteReport {
    /// Manifests fetched.
    pub manifests: usize,
    /// Snapshot bodies fetched.
    pub snapshot_bodies: usize,
    /// Content objects fetched.
    pub objects: usize,
    /// Fetch items left unfulfilled.
    pub unfulfilled: usize,
    /// Fetch items lost to transport errors.
    pub transport_errors: usize,
    /// Fetch items lost to expired deadlines.
    pub deadlines: usize,
    /// Fetch items missing at the source.
    pub missing: usize,
    /// Fetch items invalid at the source.
    pub invalid: usize,
    /// Fetch items missing decryption keys.
    pub unavailable_keys: usize,
    /// Fetch items lost to local failures.
    pub local_failures: usize,
    /// Retention-gate refusals (DG-4 A): distinct from every other
    /// class — refused is not missing, invalid, or a local failure.
    /// Defaults for mixed-version skew during rollout, like the
    /// publish-clip fields below: a pre-gate reader degrades to "no
    /// refusals reported" instead of failing the parse.
    #[serde(default)]
    pub refused: usize,
}

/// The wire form of `QuarantineReport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireQuarantineReport {
    /// Bytes observed by quarantine.
    pub observed: u64,
    /// Claims cleared by quarantine.
    pub claims_cleared: u64,
    /// Bytes discarded by quarantine.
    pub bytes_discarded: u64,
    /// Quarantine failures.
    pub failures: u64,
}

/// The wire form of `ScrubReport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireScrubReport {
    /// Bytes observed by scrub.
    pub observed: u64,
    /// Claims cleared by scrub.
    pub claims_cleared: u64,
    /// Bytes subtracted by scrub.
    pub bytes_subtracted: u64,
    /// Scrub failures.
    pub failures: u64,
}

/// The wire form of `PoisonCounts`: per-registry lock-poison
/// observations behind one pass. Every field defaults to zero (no
/// poison observed) so a v1 reader from before the poison surface
/// degrades to "healthy" instead of failing the parse; both sides
/// of this change ship together, so the defaults only cover
/// mixed-version skew during rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WirePoisonCounts {
    /// `WantRegistry` poison observations.
    #[serde(default)]
    pub wants: u64,
    /// `QuarantineQueue` poison observations.
    #[serde(default)]
    pub quarantine: u64,
    /// `ScrubQueue` poison observations.
    #[serde(default)]
    pub scrub: u64,
    /// Mutation parent-table poison observations.
    #[serde(default)]
    pub parents: u64,
}

/// The wire form of `SyncReport`: one pass's intake, fetch, repair,
/// and publication counts, plus the poison observations behind the
/// pass. The publish-clip and poison fields default like
/// `paused` does: a v1 reader from before them degrades
/// to "no clip reported, no poison observed" instead of failing the
/// parse; both sides of this change ship together, so the defaults
/// only cover mixed-version skew during rollout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSyncReport {
    /// The intake drain behind the pass.
    pub drained: WireDrainReport,
    /// The fetch execution behind the pass.
    pub fetched: WireExecuteReport,
    /// The quarantine repair behind the pass.
    pub quarantined: WireQuarantineReport,
    /// The scrub repair behind the pass.
    pub scrubbed: WireScrubReport,
    /// Whether the pass published.
    pub published: bool,
    /// Envelopes sent by the pass.
    pub sent: usize,
    /// The pass generation count.
    pub generation: u64,
    /// Heads still pending after the pass.
    pub pending_heads: usize,
    /// Whether the publish budget clipped the pass.
    #[serde(default)]
    pub publish_clipped: bool,
    /// Publications deferred by the publish budget.
    #[serde(default)]
    pub publish_deferred: usize,
    /// Publish-phase elapsed time in ms.
    #[serde(default)]
    pub publish_elapsed_ms: u64,
    /// Lock-poison observations behind the pass, by registry.
    #[serde(default)]
    pub poison: WirePoisonCounts,
}

/// The liveness answer: the transport health and the last pass
/// report behind it, pinned to the pass count. `report` is `None`
/// before the first pass completes — an explicit "no report yet",
/// never a healthy-looking default. `paused` is the loop's pause
/// switch as it reads back: a paused owner is reachable and quiet by
/// choice, distinct from an unreachable owner (refused connection)
/// and from a reachable owner with no pass yet.
///
/// `paused` defaults to `false` on the wire so a v1 reader from
/// before the pause operations degrades to "running" instead of
/// failing the parse; both sides of this change ship together, so
/// the default only covers mixed-version skew during rollout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireLiveness {
    /// The transport health behind the last pass.
    pub health: WireMailboxHealth,
    /// The last pass report, if a pass has completed.
    pub report: Option<WireSyncReport>,
    /// The pass count the answer is pinned to.
    pub passes: u64,
    /// Whether the loop's pause switch reads back as paused.
    #[serde(default)]
    pub paused: bool,
}

// ---------------------------------------------------------------------------
// Freshness values (OD-32-C)
// ---------------------------------------------------------------------------

/// The wire form of `FreshnessState`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireFreshnessState {
    /// The projection is fresh and empty.
    FreshEmpty,
    /// The projection is fresh and non-empty.
    FreshNonEmpty,
    /// The projection keeps its last good state across a refresh failure.
    RetainedLastGood {
        /// The refresh failure behind the retained projection.
        detail: String,
    },
}

/// The wire form of `ProjectionFreshness`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFreshness {
    /// The freshness judgment for the projection.
    pub state: WireFreshnessState,
    /// The durable revision the judgment was served from.
    pub revision: u64,
    /// The serving generation the judgment was served from.
    pub generation: u64,
}

/// The wire form of `OwnerObservation`: the durable status plus
/// the run report behind it, pinned to the pass count. The two
/// halves stay separate fields so no consumer can render a live
/// socket as a durable fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireObservation {
    /// The durable status half of the observation.
    pub status: WireSyncStatus,
    /// The run-report half of the observation.
    pub report: WireSyncReport,
    /// The transport health behind the pass.
    pub health: WireMailboxHealth,
    /// The pass count the observation is pinned to.
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
    /// The wire protocol version of the request.
    pub v: u32,
    /// The client-matched request id echoed in the answer.
    pub id: u64,
    /// The operation name to dispatch.
    pub op: String,
    /// The per-operation parameters.
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Parameters for the path-addressed read operations (`lookup`,
/// `stat`, `readdir`, `read`). `identity` is the caller's expected
/// entry token (lowercase hex) and binds `read` only: `None` is a
/// path-only operation with the long-standing behavior, `Some`
/// holds the `read` to the entry the caller verified — a
/// delete-plus-recreate behind the path refuses as `StaleEntry`
/// instead of serving the replacement. The other three operations
/// refuse a present token as `Malformed` rather than serving an
/// unbound answer under a bound request. Absent stays absent for
/// mixed-version skew, exactly like the node identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathParams {
    /// The namespace path the operation addresses.
    pub path: String,
    /// The caller's expected entry token as lowercase hex (binds `read` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
}

/// A response: the protocol version, the echoed request id, and
/// exactly one of a result or a refusal. The id echoes even on
/// refusals the server could parse (unknown version, unknown
/// operation) so the client always matches the answer; an
/// unparseable frame is the only answer without an id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireResponse {
    /// The wire protocol version of the answer.
    pub v: u32,
    /// The echoed request id the answer matches.
    pub id: u64,
    /// The per-operation result or explicit refusal.
    pub result: WireResult,
}

/// Either a per-operation result or an explicit refusal. No silent
/// success, no silent ignore: unsupported operations fail here with
/// the name echoed in the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireResult {
    /// A per-operation result.
    #[serde(rename = "ok")]
    Ok(Box<WireOk>),
    /// An explicit refusal.
    #[serde(rename = "err")]
    Err(WireError),
}

/// Per-operation results. Every read result carries the serving
/// generation and durable revision it was served from, so the
/// consumer correlates answers with the freshness judgment instead
/// of assuming recency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireOk {
    /// Answer to `lookup`: the node with its serving generation and revision.
    Lookup {
        /// The looked-up node.
        node: WireNode,
        /// The serving generation the node was served from.
        generation: u64,
        /// The durable revision the node was served from.
        revision: u64,
    },
    /// Answer to `stat`: the attributes with serving generation and revision.
    Stat {
        /// The entry attributes.
        attr: WireAttr,
        /// The serving generation the attributes were served from.
        generation: u64,
        /// The durable revision the attributes were served from.
        revision: u64,
    },
    /// Answer to `readdir`: the entries with serving generation and revision.
    Readdir {
        /// The directory entries.
        entries: Vec<WireDirEntry>,
        /// The serving generation the entries were served from.
        generation: u64,
        /// The durable revision the entries were served from.
        revision: u64,
    },
    /// Answer to `read`: the file bytes as hex with generation and revision.
    Read {
        /// The file bytes as lowercase hex.
        bytes_hex: String,
        /// The serving generation the bytes were served from.
        generation: u64,
        /// The durable revision the bytes were served from.
        revision: u64,
    },
    /// Answer to `observe`: the durable status.
    Observe {
        /// The durable status observation.
        status: WireSyncStatus,
    },
    /// Answer to `liveness`: the run report behind the pass count.
    Liveness {
        /// The liveness answer.
        liveness: WireLiveness,
    },
    /// Answer to `freshness`: the projection freshness judgment.
    Freshness {
        /// The freshness judgment.
        freshness: WireFreshness,
    },
    /// Answer to `submit`: the queue's terminal mutation outcome.
    Submitted {
        /// The terminal outcome of the forwarded mutation.
        outcome: WireMutationOutcome,
    },
    /// Answer to `capture-parent`: the token a create sends against.
    ParentToken {
        /// The captured parent token.
        token: WireParentToken,
    },
    /// Answer to `pause`/`resume`: the switch as it reads back.
    Paused {
        /// Whether the loop reads back as paused.
        paused: bool,
    },
    /// Answer to `policy-pin`: the pin counts.
    PolicyPinned {
        /// Chunks newly pinned.
        pinned: u64,
        /// Chunks already pinned.
        already_pinned: u64,
        /// Files walked.
        files: u64,
        /// Directories walked.
        dirs: u64,
    },
    /// Answer to `policy-unpin`: the release counts.
    PolicyUnpinned {
        /// Chunks released.
        released: u64,
        /// Chunks already unpinned.
        already_unpinned: u64,
        /// Files walked.
        files: u64,
        /// Directories walked.
        dirs: u64,
    },
    /// Answer to `policy-evict`: the eviction counts.
    PolicyEvicted {
        /// Chunks released to remote-only.
        released: u64,
        /// Chunks already remote-only.
        already_remote: u64,
        /// Files walked.
        files: u64,
        /// Directories walked.
        dirs: u64,
    },
    /// Answer to `policy-status`: the rows with generation and revision.
    PolicyStatus {
        /// The reachable policy rows and walk coverage.
        status: WirePolicyStatus,
        /// The serving generation the walk ran on.
        generation: u64,
        /// The durable revision the walk ran on.
        revision: u64,
    },
}

/// An explicit refusal: a machine-readable code plus a human message.
/// Transport-level failures (broken connection, unparseable frame)
/// never produce one of these — a refusal is always a terminal
/// answer to a parsed request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireError {
    /// The machine-readable refusal code.
    pub code: WireErrorCode,
    /// The human-readable refusal detail.
    pub message: String,
}

/// Refusal codes. Unknown and unsupported operations, versions, and
/// values fail here — never as success, never as silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireErrorCode {
    /// The request speaks a wire version the server does not.
    UnknownVersion,
    /// The request parsed but cannot be interpreted.
    Malformed,
    /// The operation name is not implemented.
    UnsupportedOperation,
    /// The path addresses nothing.
    NotFound,
    /// A directory was required where the path is not one.
    NotADirectory,
    /// A file was required where the path is not one.
    NotAFile,
    /// The entry is conflicted and the operation needs one version.
    Conflicted,
    /// The content is not materialized locally.
    NotMaterialized,
    /// The content or peer needed is currently unreachable.
    Unavailable,
    /// Stored bytes failed validation.
    Corrupt,
    /// The representation is refused by policy.
    RejectedRepresentation,
    /// The representation is no longer available.
    LostRepresentation,
    /// The durable store failed.
    Store,
    /// The read exceeds the interface read bound.
    TooLarge,
    /// The operation exceeded its deadline.
    Timeout,
    /// The server shed the request under load.
    Saturated,
    /// A file was required where the path is a directory.
    IsDirectory,
    /// The create targets an existing entry.
    AlreadyExists,
    /// The removed directory is not empty.
    DirectoryNotEmpty,
    /// The rename cannot be expressed to the queue.
    InvalidRename,
    /// The entry changed content base under the caller.
    Stale,
    /// The parent changed between capture and submit, or the
    /// capture itself was refused because the parent-token table is
    /// blocked or saturated (retry the capture).
    StaleParent,
    /// The path resolved to a different entry than the caller's
    /// expected identity (delete-plus-recreate, path reuse, kind
    /// change, or a legacy entry that never had one). Distinct from
    /// `Stale` (same entry, changed content base), from
    /// `Conflicted`, and from `NotFound` — even where a
    /// presentation surface renders two with the same error.
    StaleEntry,
    /// The fold exceeds its member bound.
    TooMany,
    /// The commit needs content the caller did not send.
    NeedContent,
    /// The owner is shutting down.
    Shutdown,
    /// The policy executor failed.
    Engine,
    /// No pass has completed yet.
    Unobserved,
    /// The instance state is unusable after a break.
    Poisoned,
    /// A local I/O failure.
    Io,
    /// Evict refused: part of the subtree is pinned. Unpin first.
    /// Nothing was committed. Distinct from every other refusal —
    /// refused is not missing, invalid, or a local failure.
    PinnedPresent,
    /// The namespace path exceeds the format's depth bound.
    TooDeep,
}

// ---------------------------------------------------------------------------
// Mutation submission values (child 2)
// ---------------------------------------------------------------------------

/// The wire form of `FileIdentity`: the exact precondition a
/// commit or guarded setattr sends against, and the exact identity
/// a successful outcome reports. The consumer learns identities
/// from lookups and outcomes, never by parsing anything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFileIdentity {
    /// The file size in bytes.
    pub size: u64,
    /// Whether the file is executable.
    pub executable: bool,
    /// The content chunk ids as lowercase hex.
    pub chunks: Vec<String>,
}

/// A parent token on the wire: the queue counter bound to the
/// owner instance that minted it. Tokens are meaningful only
/// within their owning queue and process; a restart mints a new
/// instance, and any token from a previous instance refuses as
/// stale before reaching the queue. No durable format is involved:
/// the binding lives and dies with the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireParentToken {
    /// The owner instance that minted the token.
    pub instance: u64,
    /// The queue counter the token binds.
    pub counter: u64,
}

/// One folded member: a non-`Fold` operation plus whether it is the
/// commit-forcing event. Nesting a fold inside a fold is refused at
/// the wire, before the queue's own closed-fold rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFoldMember {
    /// The member operation (never a nested fold).
    pub kind: Box<WireMutationKind>,
    /// Whether this member is the commit-forcing event.
    pub forcer: bool,
}

/// The wire form of `MutationKind`: all nine operations the owner
/// forwards verbatim to the single queue. The interface interprets
/// nothing — the parent's precondition is captured by the consumer
/// (`capture-parent`) and re-checked by the owner. Entry-addressed
/// operations additionally carry the caller's expected entry
/// identity as lowercase hex (`expected_identity`, required since
/// v2): the owner validates it at execution, inside the serialized
/// queue, and refuses a mismatch instead of landing on the wrong
/// entry. Creations address absent paths and carry no entry
/// precondition; their parent token covers the parent instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireMutationKind {
    /// Create a directory at an absent path.
    Mkdir {
        /// The directory path to create.
        path: String,
    },
    /// Create a file guarded by a captured parent token.
    CreateFile {
        /// The file path to create.
        path: String,
        /// The parent token captured for the containing directory.
        parent: WireParentToken,
    },
    /// Replace file content guarded by base and entry identity.
    CommitFile {
        /// The file path to commit.
        path: String,
        /// The file identity the commit builds on.
        base: WireFileIdentity,
        /// The executable bit the commit sets.
        executable: bool,
        /// The new file content as lowercase hex.
        content_hex: String,
        /// The caller's expected entry token as lowercase hex.
        expected_identity: String,
    },
    /// Append content to a file entry.
    AppendFile {
        /// The file path to append to.
        path: String,
        /// The appended content as lowercase hex.
        content_hex: String,
        /// The caller's expected entry token as lowercase hex.
        expected_identity: String,
    },
    /// Remove a file or symlink entry.
    Unlink {
        /// The entry path to remove.
        path: String,
        /// The caller's expected entry token as lowercase hex.
        expected_identity: String,
    },
    /// Remove an empty directory entry.
    Rmdir {
        /// The directory path to remove.
        path: String,
        /// The caller's expected entry token as lowercase hex.
        expected_identity: String,
    },
    /// Move an entry, optionally refusing replacement.
    Rename {
        /// The source path to move.
        from: String,
        /// The destination path to move to.
        to: String,
        /// Whether a replacement at the destination refuses.
        no_replace: bool,
        /// The caller's expected entry token as lowercase hex.
        expected_identity: String,
    },
    /// Change size and/or executable bit, guarded by base and entry identity.
    SetAttrs {
        /// The file path to change.
        path: String,
        /// The new size, if changed.
        size: Option<u64>,
        /// The new executable bit, if changed.
        executable: Option<bool>,
        /// The file identity the change builds on, if content-guarded.
        base: Option<WireFileIdentity>,
        /// The caller's expected entry token as lowercase hex.
        expected_identity: String,
    },
    /// Apply a closed fold of member operations.
    Fold {
        /// The fold members in order.
        members: Vec<WireFoldMember>,
    },
}

/// The wire form of `FoldForcerOutcome`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireFoldForcerOutcome {
    /// The forcing commit applied; carries its outcome.
    Applied(Box<WireMutationOutcome>),
    /// The forcing commit failed; carries the refusal.
    Failed(WireMutationError),
}

/// The wire form of `FoldDisposition`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireFoldDisposition {
    /// The member committed content; carries the resulting identity.
    Committed(WireFileIdentity),
    /// The member applied with no content result.
    Done,
    /// The member created a file; carries the resulting identity.
    Created(WireFileIdentity),
    /// The member failed; carries the refusal.
    Failed(WireMutationError),
    /// The member restored prior state after a later failure.
    Restored,
}

/// A mutation refusal as data: the queue's vocabulary carried
/// through as a distinct outcome, never collapsed to a boolean. The
/// consumer maps these to its own errors; the transport never
/// reinterprets them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMutationError {
    /// The machine-readable refusal code.
    pub code: WireErrorCode,
    /// The human-readable refusal detail.
    pub message: String,
}

/// The wire form of `MutationOutcome`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireMutationOutcome {
    /// The mutation applied with no content result.
    Done,
    /// The mutation created a file; carries the resulting identity.
    Created(WireFileIdentity),
    /// The mutation committed content; carries the resulting identity.
    Committed(WireFileIdentity),
    /// The fold outcome: the forcer result plus per-member dispositions.
    Fold {
        /// The outcome of the commit-forcing member.
        forcer: WireFoldForcerOutcome,
        /// The per-member dispositions in order.
        members: Vec<WireFoldDisposition>,
    },
}

/// The two mutation-side operations, beside the seven read
/// operations above. `capture-parent` returns the token a create
/// sends against; `submit` forwards one of the nine mutation kinds.
pub const OP_SUBMIT: &str = "submit";
/// The name of the `capture-parent` token operation on the wire.
pub const OP_CAPTURE_PARENT: &str = "capture-parent";

/// The pause switch, beside the mutation-side operations. `pause`
/// defers network work and obligation discharge, `resume` returns to
/// an ordinary pass; local authoring and serving continue either way.
/// Both take no parameters and answer with how the switch reads back.
pub const OP_PAUSE: &str = "pause";
/// The name of the `resume` pause-switch operation on the wire.
pub const OP_RESUME: &str = "resume";

/// Parameters for `submit`: one of the nine mutation kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitParams {
    /// The mutation kind to forward to the owner's queue.
    pub kind: WireMutationKind,
}

// ---------------------------------------------------------------------------
// Policy operation values (selective sync)
// ---------------------------------------------------------------------------

/// The wire form of `RetentionPolicy`.
/// Serialized as the canonical words — the same two strings the CLI
/// prints — so the wire and the surface share one vocabulary. The
/// `policy_words_match_surface_copy` test pins the rename to the
/// surface consts, so a re-wording fails the suite instead of
/// drifting the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireRetentionPolicy {
    /// Keep every chunk under the path local.
    #[serde(rename = "PINNED")]
    Pinned,
    /// Keep only metadata local; content stays remote.
    #[serde(rename = "REMOTE_ONLY")]
    RemoteOnly,
}

/// The wire form of `LocalPresence`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WirePresence {
    /// The content is present locally.
    #[serde(rename = "PRESENT")]
    Present,
    /// The content is absent locally (remote only).
    #[serde(rename = "ABSENT")]
    Absent,
}

/// One reachable file's policy/presence row. Reachable content only —
/// the per-file table the `policy status` surface renders. File-level
/// only by choice: per-chunk identities stay out of the wire (the
/// aggregate counts answer the retention question without listing
/// content addressing over the socket).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WirePolicyRow {
    /// The reachable file path.
    pub path: String,
    /// The retention policy on the path.
    pub policy: WireRetentionPolicy,
    /// The local presence of the path.
    pub presence: WirePresence,
}

/// The on-demand status answer: every reachable row plus the walk
/// coverage, without the serving generation and durable revision —
/// those ride the `WireOk::PolicyStatus` envelope beside it, the
/// way read results carry theirs, so the consumer correlates the
/// answer with the freshness judgment instead of assuming recency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WirePolicyStatus {
    /// The reachable file rows.
    pub files: Vec<WirePolicyRow>,
    /// The conflicted paths skipped by the walk.
    pub conflicts: Vec<String>,
    /// The directories walked.
    pub dirs: u64,
    /// The symlinks skipped by the walk.
    pub symlinks_skipped: u64,
    /// Whether the traversal budget covered the subtree: `false` is
    /// a prefix, never the whole, and the consumer narrows the path
    /// or accepts the aggregate as partial.
    pub complete: bool,
}
