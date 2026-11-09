//! The drive-owner interface: the single non-owner path to a held
//! drive (DG-1, option B). One process owns the durable store; every
//! other local consumer — the File Provider extension first — reaches
//! the drive through this socket interface, never by opening the store.
//!
//! The interface forwards and never interprets: namespace reads go to
//! the shared projection, the durable observation and liveness go to
//! the loop-published slots, mutations go to the single mutation
//! queue, and materialization policy goes to the loop-thread policy
//! executor. See `docs/contracts/owner-interface.md` for the protocol contract;
//! this module is its implementation.
//!
//! Three negative invariants pin the shape, enforced in
//! `wyrd-contracts` (not here, so the check survives this module's
//! own evolution):
//! - the interface exposes no store internals: no operation accepts
//!   or returns a `DurableStore`, a `RuntimeState`, an `Engine`, or a
//!   `wyrd_sync::runtime` type;
//! - the interface is not a second engine: no call site for
//!   `author_snapshot`, `carry_pending`, `admit_device`, or
//!   `announce_snapshot`;
//! - the interface never constructs a mailbox: the read side forwards
//!   the loop's observations, and a read leaves durable state
//!   byte-identical.

pub mod auth;
pub mod client;
pub mod mapping;
pub mod server;
#[cfg(test)]
mod tests_owner;
pub mod wire;

pub use client::{
    decode_read_bytes, ClientError, OwnerClient, CLIENT_WIRE_VERSION, DEFAULT_RESPONSE_TIMEOUT,
};
pub use mapping::{
    attr_from_wire, file_identity_from_wire, kind_from_wire, map_policy_error,
    mutation_kind_from_wire, node_from_wire, parent_token_from_wire, wire_attr,
    wire_conflict_version, wire_convergence_state, wire_dir_entry, wire_drain_report,
    wire_durability_level, wire_execute_report, wire_file_identity, wire_fold_disposition,
    wire_fold_forcer_outcome, wire_freshness, wire_freshness_state, wire_head_classes,
    wire_head_summary, wire_kind, wire_known_state, wire_mailbox_health, wire_mailbox_view,
    wire_materialization_summary, wire_mutation_error, wire_mutation_outcome, wire_node,
    wire_observation, wire_outbox_totals, wire_peer_handle, wire_pending_obligations,
    wire_policy_row, wire_presence, wire_quarantine_report, wire_queue_depth,
    wire_reconciliation_counters, wire_retention_policy, wire_scrub_report, wire_sync_report,
    wire_sync_status,
};
pub use server::{
    bind_listener, OwnerHandles, OwnerServer, DEFAULT_MAX_READ_BYTES, MAX_OWNER_CONNECTIONS,
    MAX_SUBMIT_CONTENT_BYTES, READ_DEMAND_TIMEOUT,
};
pub use wyrd_owner_client::{socket_path_for, MAX_FRAME_BYTES, OWNER_SOCKET_NAME};

pub use wire::{
    PathParams, RawRequest, SubmitParams, WireAttr, WireConflictVersion, WireConvergenceState,
    WireDirEntry, WireDrainReport, WireDurabilityLevel, WireError, WireErrorCode,
    WireExecuteReport, WireFileIdentity, WireFoldDisposition, WireFoldForcerOutcome,
    WireFoldMember, WireFreshness, WireFreshnessState, WireHeadClasses, WireHeadSummary, WireKind,
    WireLiveness, WireMailboxHealth, WireMailboxView, WireMaterializationSummary,
    WireMutationError, WireMutationKind, WireMutationOutcome, WireNode, WireObservation, WireOk,
    WireOutboxTotals, WireParentToken, WirePeerHandle, WirePendingObligations, WirePolicyRow,
    WirePolicyStatus, WirePresence, WireQuarantineReport, WireQueueDepth,
    WireReconciliationCounters, WireResponse, WireResult, WireRetentionPolicy, WireScrubReport,
    WireSyncReport, WireSyncStatus, OP_CAPTURE_PARENT, OP_FRESHNESS, OP_LIVENESS, OP_LOOKUP,
    OP_OBSERVE, OP_PAUSE, OP_POLICY_EVICT, OP_POLICY_PIN, OP_POLICY_STATUS, OP_POLICY_UNPIN,
    OP_READ, OP_READDIR, OP_RESUME, OP_STAT, OP_SUBMIT, WIRE_VERSION,
};
