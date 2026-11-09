//! Domain-to-wire conversions for the drive-owner interface: every
//! mapping between the core/sync/format domain types and the owned
//! protocol types in `wyrd_owner_client`. The conversions live here —
//! in the daemon that owns the server — never in the client crate, so
//! the client crate's dependency closure stays domain-free.
//!
//! The disclosure-map habit from the old inline impls is preserved:
//! each conversion destructures its source exhaustively, so adding a
//! field to a domain type fails the build here until the mapping
//! decides what the wire says about it.

use wyrd_core::live::{OwnerObservation, PendingObligations, SyncReport};
use wyrd_core::mutation::{
    FileIdentity, FoldDisposition, FoldForcerOutcome, MutationError, MutationKind, MutationOutcome,
    ParentToken,
};
use wyrd_core::policy::{FileResidency, LocalPresence, PolicyError, RetentionPolicy};
use wyrd_core::projection::{FreshnessState, ProjectionFreshness};
use wyrd_core::quarantine::QuarantineReport;
use wyrd_core::scrub::ScrubReport;
use wyrd_core::status::{
    ConvergenceState, HeadClasses, HeadSummary, MailboxView, PeerHandle, QueueDepth, SyncStatus,
};
use wyrd_core::view::{Attr, ConflictVersion, DirEntry, Kind, Node};
use wyrd_format::{ContentId, SnapshotId};
use wyrd_sync::membership::KnownState;
use wyrd_sync::runtime::{
    DrainReport, DurabilityLevel, ExecuteReport, MaterializationSummary, OutboxTotals,
    ReconciliationCounters,
};
use wyrd_sync::transport::mailbox::MailboxSnapshot;

use wyrd_owner_client::wire::*;

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

/// Map `Kind` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_kind(kind: Kind) -> WireKind {
    match kind {
        Kind::File => WireKind::File,
        Kind::Dir => WireKind::Dir,
        Kind::Symlink => WireKind::Symlink,
        Kind::Conflict => WireKind::Conflict,
    }
}

pub fn kind_from_wire(kind: WireKind) -> Kind {
    match kind {
        WireKind::File => Kind::File,
        WireKind::Dir => Kind::Dir,
        WireKind::Symlink => Kind::Symlink,
        WireKind::Conflict => Kind::Conflict,
    }
}
/// Map `Attr` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_attr(attr: &Attr) -> WireAttr {
    let Attr {
        kind,
        size,
        executable,
    } = *attr;
    WireAttr {
        kind: wire_kind(kind),
        size,
        executable,
    }
}

pub fn attr_from_wire(attr: WireAttr) -> Attr {
    Attr {
        kind: kind_from_wire(attr.kind),
        size: attr.size,
        executable: attr.executable,
    }
}
/// Map `Node` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_node(node: &Node) -> WireNode {
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
            versions: versions.iter().map(wire_conflict_version).collect(),
        },
    }
}

/// Map `ConflictVersion` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_conflict_version(version: &ConflictVersion) -> WireConflictVersion {
    let ConflictVersion { snapshot, node } = version;
    WireConflictVersion {
        snapshot: hex_id(snapshot.as_bytes()),
        node: Box::new(wire_node(node)),
    }
}

pub fn node_from_wire(node: &WireNode) -> Result<Node, String> {
    match node {
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
                    node: node_from_wire(&version.node)?,
                });
            }
            Ok(Node::Conflict { versions: out })
        }
    }
}
/// Map `DirEntry` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_dir_entry(entry: &DirEntry) -> WireDirEntry {
    let DirEntry { name, node } = entry;
    WireDirEntry {
        name: name.clone(),
        node: wire_node(node),
    }
}

/// Map `HeadSummary` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_head_summary(head: &HeadSummary) -> WireHeadSummary {
    let HeadSummary { id, epoch, author } = head;
    WireHeadSummary {
        id: hex_id(id.as_bytes()),
        epoch: *epoch,
        author: hex_id(author.as_bytes()),
    }
}

/// Map `HeadClasses` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_head_classes(classes: &HeadClasses) -> WireHeadClasses {
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

/// Map `MailboxView` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_mailbox_view(view: &MailboxView) -> WireMailboxView {
    let MailboxView { configured_relays } = *view;
    WireMailboxView { configured_relays }
}

/// Map `PeerHandle` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_peer_handle(handle: &PeerHandle) -> WirePeerHandle {
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

/// Map `QueueDepth` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_queue_depth(queue: &QueueDepth) -> WireQueueDepth {
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

/// Map `PendingObligations` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_pending_obligations(obligations: &PendingObligations) -> WirePendingObligations {
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

/// Map `OutboxTotals` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_outbox_totals(totals: &OutboxTotals) -> WireOutboxTotals {
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

/// Map `ConvergenceState` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_convergence_state(state: &ConvergenceState) -> WireConvergenceState {
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

/// Map `ReconciliationCounters` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_reconciliation_counters(
    counters: &ReconciliationCounters,
) -> WireReconciliationCounters {
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

/// Map `MaterializationSummary` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_materialization_summary(
    summary: &MaterializationSummary,
) -> WireMaterializationSummary {
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

/// Map `DurabilityLevel` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_durability_level(level: &DurabilityLevel) -> WireDurabilityLevel {
    match level {
        DurabilityLevel::Working => WireDurabilityLevel::Working,
        DurabilityLevel::Committed => WireDurabilityLevel::Committed,
        DurabilityLevel::Published => WireDurabilityLevel::Published,
    }
}

/// Map `KnownState` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_known_state(known: &KnownState) -> WireKnownState {
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

/// Map `SyncStatus` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_sync_status(status: &SyncStatus) -> WireSyncStatus {
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
        tip: tip.as_ref().map(wire_known_state),
        held_epochs: held_epochs.clone(),
        obligations: wire_pending_obligations(obligations),
        totals: wire_outbox_totals(totals),
        live_heads: live_heads.iter().map(wire_head_summary).collect(),
        head_classes: wire_head_classes(head_classes),
        mailbox: wire_mailbox_view(mailbox),
        known_members: *known_members,
        peers: peers.iter().map(wire_peer_handle).collect(),
        queue: wire_queue_depth(queue),
        reconciliation: wire_reconciliation_counters(reconciliation),
        convergence: wire_convergence_state(convergence),
        materialization: wire_materialization_summary(materialization),
        durability: wire_durability_level(durability),
    }
}

/// Map `MailboxSnapshot` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_mailbox_health(health: &MailboxSnapshot) -> WireMailboxHealth {
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

/// Map `FreshnessState` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_freshness_state(state: &FreshnessState) -> WireFreshnessState {
    match state {
        FreshnessState::FreshEmpty => WireFreshnessState::FreshEmpty,
        FreshnessState::FreshNonEmpty => WireFreshnessState::FreshNonEmpty,
        FreshnessState::RetainedLastGood { detail } => WireFreshnessState::RetainedLastGood {
            detail: detail.clone(),
        },
    }
}

/// Map `ProjectionFreshness` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_freshness(freshness: &ProjectionFreshness) -> WireFreshness {
    let ProjectionFreshness {
        state,
        revision,
        generation,
    } = freshness;
    WireFreshness {
        state: wire_freshness_state(state),
        revision: *revision,
        generation: *generation,
    }
}

/// Map `DrainReport` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_drain_report(report: &DrainReport) -> WireDrainReport {
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

/// Map `ExecuteReport` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_execute_report(report: &ExecuteReport) -> WireExecuteReport {
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
        refused,
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
        refused,
    }
}

/// Map `QuarantineReport` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_quarantine_report(report: &QuarantineReport) -> WireQuarantineReport {
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

/// Map `ScrubReport` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_scrub_report(report: &ScrubReport) -> WireScrubReport {
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

/// Map `SyncReport` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_sync_report(report: &SyncReport) -> WireSyncReport {
    let SyncReport {
        drained,
        fetched,
        quarantined,
        scrubbed,
        published,
        sent,
        publish_clipped,
        publish_deferred,
        publish_elapsed_ms,
        generation,
        pending_heads,
    } = report;
    WireSyncReport {
        drained: wire_drain_report(drained),
        fetched: wire_execute_report(fetched),
        quarantined: wire_quarantine_report(quarantined),
        scrubbed: wire_scrub_report(scrubbed),
        published: *published,
        sent: *sent,
        publish_clipped: *publish_clipped,
        publish_deferred: *publish_deferred,
        publish_elapsed_ms: *publish_elapsed_ms,
        generation: *generation,
        pending_heads: *pending_heads,
    }
}

/// Map `OwnerObservation` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_observation(observation: &OwnerObservation) -> WireObservation {
    let OwnerObservation {
        status,
        report,
        health,
        passes,
    } = observation;
    WireObservation {
        status: wire_sync_status(status),
        report: wire_sync_report(report),
        health: wire_mailbox_health(health),
        passes: *passes,
    }
}

/// Map `FileIdentity` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_file_identity(identity: &FileIdentity) -> WireFileIdentity {
    WireFileIdentity {
        size: identity.size(),
        executable: identity.executable(),
        chunks: identity
            .chunks()
            .iter()
            .map(|id| hex_id(id.as_bytes()))
            .collect(),
    }
}

pub fn file_identity_from_wire(identity: &WireFileIdentity) -> Result<FileIdentity, String> {
    let mut chunks = Vec::with_capacity(identity.chunks.len());
    for raw in &identity.chunks {
        chunks.push(ContentId::from_bytes(parse_id(raw)?));
    }
    Ok(FileIdentity::new(
        identity.size,
        identity.executable,
        chunks,
    ))
}
pub fn parent_token_from_wire(
    token: &WireParentToken,
    owner_instance: u64,
) -> Result<ParentToken, (WireErrorCode, String)> {
    if token.instance != owner_instance {
        return Err((
            WireErrorCode::StaleParent,
            "parent token from a previous owner instance: capture again".to_string(),
        ));
    }
    Ok(wyrd_core::mutation::ParentToken::from_wire_value(
        token.counter,
    ))
}
pub fn mutation_kind_from_wire(
    kind: &WireMutationKind,
    owner_instance: u64,
) -> Result<MutationKind, (WireErrorCode, String)> {
    use wyrd_core::mutation::{FoldMember, MutationKind};
    let malformed = |message: String| (WireErrorCode::Malformed, message);
    match kind {
        WireMutationKind::Mkdir { path } => Ok(MutationKind::Mkdir { path: path.clone() }),
        WireMutationKind::CreateFile { path, parent } => Ok(MutationKind::CreateFile {
            path: path.clone(),
            parent: parent_token_from_wire(parent, owner_instance)?,
        }),
        WireMutationKind::CommitFile {
            path,
            base,
            executable,
            content_hex,
        } => Ok(MutationKind::CommitFile {
            path: path.clone(),
            base: file_identity_from_wire(base)
                .map_err(|message| (WireErrorCode::Malformed, message))?,
            executable: *executable,
            content: hex::decode(content_hex)
                .map_err(|error| malformed(format!("commit content is not hex: {error}")))?,
        }),
        WireMutationKind::AppendFile { path, content_hex } => Ok(MutationKind::AppendFile {
            path: path.clone(),
            content: hex::decode(content_hex)
                .map_err(|error| malformed(format!("append content is not hex: {error}")))?,
        }),
        WireMutationKind::Unlink { path } => Ok(MutationKind::Unlink { path: path.clone() }),
        WireMutationKind::Rmdir { path } => Ok(MutationKind::Rmdir { path: path.clone() }),
        WireMutationKind::Rename {
            from,
            to,
            no_replace,
        } => Ok(MutationKind::Rename {
            from: from.clone(),
            to: to.clone(),
            no_replace: *no_replace,
        }),
        WireMutationKind::SetAttrs {
            path,
            size,
            executable,
            base,
        } => Ok(MutationKind::SetAttrs {
            path: path.clone(),
            size: *size,
            executable: *executable,
            base: base
                .as_ref()
                .map(file_identity_from_wire)
                .transpose()
                .map_err(|message| (WireErrorCode::Malformed, message))?,
        }),
        WireMutationKind::Fold { members } => {
            let mut kinds = Vec::with_capacity(members.len());
            for member in members {
                if matches!(*member.kind, WireMutationKind::Fold { .. }) {
                    return Err(malformed("a fold never nests: a member whose kind is itself a fold fails the whole submission".to_string()));
                }
                kinds.push(FoldMember {
                    kind: mutation_kind_from_wire(&member.kind, owner_instance)?,
                    forcer: member.forcer,
                });
            }
            Ok(MutationKind::Fold { members: kinds })
        }
    }
}
/// Map `MutationError` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_mutation_error(error: &MutationError) -> WireMutationError {
    use MutationError;
    let code = match error {
        MutationError::Saturated => WireErrorCode::Saturated,
        MutationError::Lock => WireErrorCode::Poisoned,
        MutationError::Conflicted { .. } => WireErrorCode::Conflicted,
        MutationError::Invalid(_) => WireErrorCode::Malformed,
        MutationError::NotFound(_) => WireErrorCode::NotFound,
        MutationError::NotADirectory(_) => WireErrorCode::NotADirectory,
        MutationError::IsDirectory(_) => WireErrorCode::IsDirectory,
        MutationError::AlreadyExists(_) => WireErrorCode::AlreadyExists,
        MutationError::DirectoryNotEmpty(_) => WireErrorCode::DirectoryNotEmpty,
        MutationError::InvalidRename(_) => WireErrorCode::InvalidRename,
        MutationError::Stale(_) => WireErrorCode::Stale,
        MutationError::StaleParent(_) => WireErrorCode::StaleParent,
        MutationError::TooLarge(_) => WireErrorCode::TooLarge,
        MutationError::TooMany { .. } => WireErrorCode::TooMany,
        MutationError::Store(_) => WireErrorCode::Store,
        MutationError::Engine => WireErrorCode::Engine,
        MutationError::NeedContent { .. } => WireErrorCode::NeedContent,
        MutationError::TimedOut => WireErrorCode::Timeout,
        MutationError::Shutdown => WireErrorCode::Shutdown,
    };
    WireMutationError {
        code,
        message: error.to_string(),
    }
}

/// Map `FoldForcerOutcome` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_fold_forcer_outcome(outcome: &FoldForcerOutcome) -> WireFoldForcerOutcome {
    use FoldForcerOutcome;
    match outcome {
        FoldForcerOutcome::Applied(inner) => {
            WireFoldForcerOutcome::Applied(Box::new(wire_mutation_outcome(inner)))
        }
        FoldForcerOutcome::Failed(error) => {
            WireFoldForcerOutcome::Failed(wire_mutation_error(error))
        }
    }
}

/// Map `FoldDisposition` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_fold_disposition(disposition: &FoldDisposition) -> WireFoldDisposition {
    use FoldDisposition;
    match disposition {
        FoldDisposition::Committed(identity) => {
            WireFoldDisposition::Committed(wire_file_identity(identity))
        }
        FoldDisposition::Done => WireFoldDisposition::Done,
        FoldDisposition::Created(identity) => {
            WireFoldDisposition::Created(wire_file_identity(identity))
        }
        FoldDisposition::Failed(error) => WireFoldDisposition::Failed(wire_mutation_error(error)),
        FoldDisposition::Restored => WireFoldDisposition::Restored,
    }
}

/// Map `MutationOutcome` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_mutation_outcome(outcome: &MutationOutcome) -> WireMutationOutcome {
    use MutationOutcome;
    match outcome {
        MutationOutcome::Done => WireMutationOutcome::Done,
        MutationOutcome::Created(identity) => {
            WireMutationOutcome::Created(wire_file_identity(identity))
        }
        MutationOutcome::Committed(identity) => {
            WireMutationOutcome::Committed(wire_file_identity(identity))
        }
        MutationOutcome::Fold { forcer, members } => WireMutationOutcome::Fold {
            forcer: wire_fold_forcer_outcome(forcer),
            members: members.iter().map(wire_fold_disposition).collect(),
        },
    }
}

/// Map `RetentionPolicy` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_retention_policy(policy: &RetentionPolicy) -> WireRetentionPolicy {
    match policy {
        RetentionPolicy::Pinned => WireRetentionPolicy::Pinned,
        RetentionPolicy::RemoteOnly => WireRetentionPolicy::RemoteOnly,
    }
}

/// Map `LocalPresence` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_presence(presence: &LocalPresence) -> WirePresence {
    match presence {
        LocalPresence::Present => WirePresence::Present,
        LocalPresence::Absent => WirePresence::Absent,
    }
}

/// Map `FileResidency` to its wire form. Exhaustive destructure:
/// a new source field fails the build here, not on the wire.
pub fn wire_policy_row(file: &FileResidency) -> WirePolicyRow {
    // Exhaustive destructure: a new source field fails here, and
    // the explicit omission below names the wire-size choice —
    // per-chunk vectors stay out of the answer, so a future
    // field cannot be silently ignored (nor silently added to
    // the wire). The same disclosure-map habit as every `From`
    // above.
    let FileResidency {
        path,
        chunks: _,
        pinned_chunks: _,
        policy,
        local,
    } = file;
    WirePolicyRow {
        path: path.clone(),
        policy: wire_retention_policy(policy),
        presence: wire_presence(local),
    }
}

/// Map a policy failure to a wire refusal. Every variant names its
/// own code — never collapse two refusals into one.
pub fn map_policy_error(error: PolicyError) -> (WireErrorCode, String) {
    use wyrd_core::view::ViewError;
    use PolicyError;
    let message = error.to_string();
    let code = match &error {
        PolicyError::View { source, .. } => match source {
            ViewError::NotFound => WireErrorCode::NotFound,
            ViewError::InvalidPath => WireErrorCode::Malformed,
            ViewError::NotADirectory => WireErrorCode::NotADirectory,
            ViewError::NotAFile => WireErrorCode::NotAFile,
            ViewError::Conflict => WireErrorCode::Conflicted,
            ViewError::NotMaterialized { .. } => WireErrorCode::NotMaterialized,
            ViewError::Unavailable { .. } => WireErrorCode::Unavailable,
            ViewError::Corrupt => WireErrorCode::Corrupt,
            ViewError::RejectedRepresentation { .. } => WireErrorCode::RejectedRepresentation,
            ViewError::LostRepresentation { .. } => WireErrorCode::LostRepresentation,
            ViewError::Store(..) => WireErrorCode::Store,
        },
        PolicyError::Conflict { .. } => WireErrorCode::Conflicted,
        PolicyError::PinnedPresent { .. } => WireErrorCode::PinnedPresent,
        PolicyError::TooDeep(_) => WireErrorCode::TooDeep,
        PolicyError::Engine(_) => WireErrorCode::Engine,
    };
    (code, message)
}
