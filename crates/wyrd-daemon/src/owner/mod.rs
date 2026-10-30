//! The drive-owner interface: the single non-owner path to a held
//! drive (DG-1, option B). One process owns the durable store; every
//! other local consumer — the File Provider extension first — reaches
//! the drive through this socket interface, never by opening the store.
//!
//! The interface forwards and never interprets: namespace reads go to
//! the shared projection, the durable observation and liveness go to
//! the loop-published slots, and mutations go to the single mutation
//! queue. See `docs/contracts/owner-interface.md` for the protocol contract;
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
pub mod server;
#[cfg(test)]
mod tests_owner;
pub mod wire;

pub use server::{
    bind_listener, socket_path_for, OwnerHandles, OwnerServer, DEFAULT_MAX_READ_BYTES,
    MAX_FRAME_BYTES, OWNER_SOCKET_NAME, READ_DEMAND_TIMEOUT,
};

pub use wire::{
    PathParams, RawRequest, SubmitParams, WireAttr, WireConvergenceState, WireDirEntry,
    WireDrainReport, WireDurabilityLevel, WireError, WireErrorCode, WireExecuteReport,
    WireFileIdentity, WireFoldDisposition, WireFoldForcerOutcome, WireFoldMember, WireFreshness,
    WireFreshnessState, WireHeadClasses, WireHeadSummary, WireKind, WireLiveness,
    WireMailboxHealth, WireMailboxView, WireMaterializationSummary, WireMutationError,
    WireMutationKind, WireMutationOutcome, WireNode, WireObservation, WireOk, WireOutboxTotals,
    WireParentToken, WirePeerHandle, WirePendingObligations, WireQuarantineReport, WireQueueDepth,
    WireReconciliationCounters, WireResponse, WireResult, WireScrubReport, WireSyncReport,
    WireSyncStatus, OP_CAPTURE_PARENT, OP_FRESHNESS, OP_LIVENESS, OP_LOOKUP, OP_OBSERVE, OP_READ,
    OP_READDIR, OP_STAT, OP_SUBMIT, WIRE_VERSION,
};
