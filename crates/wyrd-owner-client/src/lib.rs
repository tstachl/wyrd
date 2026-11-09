//! The non-owning side of the drive-owner interface: owned protocol
//! types, the socket transport constants, and the shared Rust client.
//! Every consumer in this language uses this client — Tauri first —
//! so framing, request ids, refusal mapping, and the unconfirmed
//! answer live in exactly one place instead of one hand-roll per
//! consumer. A consumer in another language implements the same
//! framing against the golden vectors (`vectors.json`), which pin
//! the bytes both sides must agree on.
//!
//! The client forwards and never interprets: it exposes one typed
//! method per interface operation, carries refusals through as data,
//! and treats every broken connection as unconfirmed — never success,
//! never failure. It owns no mutation path of its own: `submit`
//! forwards to the owner's single queue, and reconciliation re-reads
//! through the owner rather than deciding outcomes locally. See
//! `docs/contracts/owner-interface.md` for the protocol contract;
//! this crate is its Rust-side transport.
//!
//! Deliberately dependency-free of the domain: no `wyrd-core`,
//! `wyrd-sync`, `wyrd-view`, `wyrd-format`, `wyrd-daemon`, or
//! presentation edge, direct or transitive. The daemon keeps the
//! server and performs every domain-to-wire conversion.

pub mod client;
pub mod transport;
pub mod wire;

pub use client::{
    decode_read_bytes, ClientError, OwnerClient, CLIENT_WIRE_VERSION, DEFAULT_RESPONSE_TIMEOUT,
};
pub use transport::{socket_path_for, MAX_FRAME_BYTES, OWNER_SOCKET_NAME};
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
