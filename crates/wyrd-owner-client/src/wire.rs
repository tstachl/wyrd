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

/// Policy operations: the four engine operations the interface
/// forwards to the loop-thread policy executor (not the mutation
/// queue). Names are plain strings like the read operations above:
/// an unknown name refuses with the name echoed back.
pub const OP_POLICY_PIN: &str = "policy-pin";
pub const OP_POLICY_UNPIN: &str = "policy-unpin";
pub const OP_POLICY_EVICT: &str = "policy-evict";
pub const OP_POLICY_STATUS: &str = "policy-status";

// ---------------------------------------------------------------------------
// Namespace values
// ---------------------------------------------------------------------------

/// The wire form of `Kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireKind {
    File,
    Dir,
    Symlink,
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
    pub kind: WireKind,
    pub size: u64,
    pub executable: bool,
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
    File {
        size: u64,
        executable: bool,
        chunks: Vec<String>,
        #[serde(default)]
        identity: Option<String>,
    },
    Dir {
        subtree: String,
        #[serde(default)]
        identity: Option<String>,
    },
    Symlink {
        target: String,
        #[serde(default)]
        identity: Option<String>,
    },
    MergedDir {
        subtrees: Vec<(String, String)>,
    },
    Conflict {
        versions: Vec<WireConflictVersion>,
    },
}

/// The wire form of `ConflictVersion`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireConflictVersion {
    pub snapshot: String,
    pub node: Box<WireNode>,
}

/// The wire form of `DirEntry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireDirEntry {
    pub name: String,
    pub node: WireNode,
}

// ---------------------------------------------------------------------------
// Durable status values
// ---------------------------------------------------------------------------

/// The wire form of `HeadSummary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireHeadSummary {
    pub id: String,
    pub epoch: u64,
    pub author: String,
}

/// The wire form of `HeadClasses`.
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

/// The wire form of `MailboxView`: configuration only, never liveness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMailboxView {
    pub configured_relays: usize,
}

/// The wire form of `PeerHandle`: the opaque handle crosses, never
/// the identity behind it — the wire keeps the status contract that
/// renderers name peers `peer-N` without learning who they are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WirePeerHandle {
    pub handle: usize,
    pub peer: String,
    pub pending: usize,
}

/// The wire form of `QueueDepth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireQueueDepth {
    pub outbox: usize,
    pub carries: usize,
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
    pub announcements: Vec<(String, String)>,
    pub transitions: Vec<(String, String)>,
    pub capabilities: Vec<(u64, String)>,
}

/// The wire form of `OutboxTotals`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireOutboxTotals {
    pub announcements_queued: usize,
    pub announcements_delivered: usize,
    pub transitions_queued: usize,
    pub transitions_delivered: usize,
    pub capabilities_queued: usize,
    pub capabilities_delivered: usize,
}

/// The wire form of `ConvergenceState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireConvergenceState {
    pub converged: bool,
    pub pending_heads: usize,
    pub unfetchable_heads: usize,
}

/// The wire form of `ReconciliationCounters`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireReconciliationCounters {
    pub statements_received: usize,
    pub transitions_reconciled: usize,
    pub capabilities_reconciled: usize,
    pub announcements_reconciled: usize,
}

/// The wire form of `MaterializationSummary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMaterializationSummary {
    pub pinned: usize,
    pub cached: usize,
    pub local_objects: usize,
}

/// The wire form of `DurabilityLevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireDurabilityLevel {
    Working,
    Committed,
    Published,
}

/// The wire form of `KnownState`: the membership tip with its roots
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

/// The wire form of `SyncStatus`: the full durable observation.
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

// ---------------------------------------------------------------------------
// Run-report values (OD-16-D)
// ---------------------------------------------------------------------------

/// The wire form of `MailboxSnapshot`: the transport health behind
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

/// The wire form of `ExecuteReport`.
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
    pub observed: u64,
    pub claims_cleared: u64,
    pub bytes_discarded: u64,
    pub failures: u64,
}

/// The wire form of `ScrubReport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireScrubReport {
    pub observed: u64,
    pub claims_cleared: u64,
    pub bytes_subtracted: u64,
    pub failures: u64,
}

/// The wire form of `SyncReport`: one pass's intake, fetch, repair,
/// and publication counts. The publish-clip fields default like
/// `paused` does: a v1 reader from before the publish budget degrades
/// to "no clip reported" instead of failing the parse; both sides of
/// this change ship together, so the default only covers
/// mixed-version skew during rollout.
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
    #[serde(default)]
    pub publish_clipped: bool,
    #[serde(default)]
    pub publish_deferred: usize,
    #[serde(default)]
    pub publish_elapsed_ms: u64,
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
    pub health: WireMailboxHealth,
    pub report: Option<WireSyncReport>,
    pub passes: u64,
    #[serde(default)]
    pub paused: bool,
}

// ---------------------------------------------------------------------------
// Freshness values (OD-32-C)
// ---------------------------------------------------------------------------

/// The wire form of `FreshnessState`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireFreshnessState {
    FreshEmpty,
    FreshNonEmpty,
    RetainedLastGood { detail: String },
}

/// The wire form of `ProjectionFreshness`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFreshness {
    pub state: WireFreshnessState,
    pub revision: u64,
    pub generation: u64,
}

/// The wire form of `OwnerObservation`: the durable status plus
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
    Submitted {
        outcome: WireMutationOutcome,
    },
    ParentToken {
        token: WireParentToken,
    },
    Paused {
        paused: bool,
    },
    PolicyPinned {
        pinned: u64,
        already_pinned: u64,
        files: u64,
        dirs: u64,
    },
    PolicyUnpinned {
        released: u64,
        already_unpinned: u64,
        files: u64,
        dirs: u64,
    },
    PolicyEvicted {
        released: u64,
        already_remote: u64,
        files: u64,
        dirs: u64,
    },
    PolicyStatus {
        status: WirePolicyStatus,
        generation: u64,
        revision: u64,
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
    IsDirectory,
    AlreadyExists,
    DirectoryNotEmpty,
    InvalidRename,
    Stale,
    StaleParent,
    TooMany,
    NeedContent,
    Shutdown,
    Engine,
    Unobserved,
    Poisoned,
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
    pub size: u64,
    pub executable: bool,
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
    pub instance: u64,
    pub counter: u64,
}

/// One folded member: a non-`Fold` operation plus whether it is the
/// commit-forcing event. Nesting a fold inside a fold is refused at
/// the wire, before the queue's own closed-fold rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFoldMember {
    pub kind: Box<WireMutationKind>,
    pub forcer: bool,
}

/// The wire form of `MutationKind`: all nine operations the owner
/// forwards verbatim to the single queue. The interface interprets
/// nothing — the parent's precondition is captured by the consumer
/// (`capture-parent`) and re-checked by the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireMutationKind {
    Mkdir {
        path: String,
    },
    CreateFile {
        path: String,
        parent: WireParentToken,
    },
    CommitFile {
        path: String,
        base: WireFileIdentity,
        executable: bool,
        content_hex: String,
    },
    AppendFile {
        path: String,
        content_hex: String,
    },
    Unlink {
        path: String,
    },
    Rmdir {
        path: String,
    },
    Rename {
        from: String,
        to: String,
        no_replace: bool,
    },
    SetAttrs {
        path: String,
        size: Option<u64>,
        executable: Option<bool>,
        base: Option<WireFileIdentity>,
    },
    Fold {
        members: Vec<WireFoldMember>,
    },
}

/// The wire form of `FoldForcerOutcome`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireFoldForcerOutcome {
    Applied(Box<WireMutationOutcome>),
    Failed(WireMutationError),
}

/// The wire form of `FoldDisposition`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireFoldDisposition {
    Committed(WireFileIdentity),
    Done,
    Created(WireFileIdentity),
    Failed(WireMutationError),
    Restored,
}

/// A mutation refusal as data: the queue's vocabulary carried
/// through as a distinct outcome, never collapsed to a boolean. The
/// consumer maps these to its own errors; the transport never
/// reinterprets them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMutationError {
    pub code: WireErrorCode,
    pub message: String,
}

/// The wire form of `MutationOutcome`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireMutationOutcome {
    Done,
    Created(WireFileIdentity),
    Committed(WireFileIdentity),
    Fold {
        forcer: WireFoldForcerOutcome,
        members: Vec<WireFoldDisposition>,
    },
}

/// The two mutation-side operations, beside the seven read
/// operations above. `capture-parent` returns the token a create
/// sends against; `submit` forwards one of the nine mutation kinds.
pub const OP_SUBMIT: &str = "submit";
pub const OP_CAPTURE_PARENT: &str = "capture-parent";

/// The pause switch, beside the mutation-side operations. `pause`
/// defers network work and obligation discharge, `resume` returns to
/// an ordinary pass; local authoring and serving continue either way.
/// Both take no parameters and answer with how the switch reads back.
pub const OP_PAUSE: &str = "pause";
pub const OP_RESUME: &str = "resume";

/// Parameters for `submit`: one of the nine mutation kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitParams {
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
    #[serde(rename = "PINNED")]
    Pinned,
    #[serde(rename = "REMOTE_ONLY")]
    RemoteOnly,
}

/// The wire form of `LocalPresence`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WirePresence {
    #[serde(rename = "PRESENT")]
    Present,
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
    pub path: String,
    pub policy: WireRetentionPolicy,
    pub presence: WirePresence,
}

/// The on-demand status answer: every reachable row plus the walk
/// coverage, without the serving generation and durable revision —
/// those ride the `WireOk::PolicyStatus` envelope beside it, the
/// way read results carry theirs, so the consumer correlates the
/// answer with the freshness judgment instead of assuming recency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WirePolicyStatus {
    pub files: Vec<WirePolicyRow>,
    pub conflicts: Vec<String>,
    pub dirs: u64,
    pub symlinks_skipped: u64,
    /// Whether the traversal budget covered the subtree: `false` is
    /// a prefix, never the whole, and the consumer narrows the path
    /// or accepts the aggregate as partial.
    pub complete: bool,
}
