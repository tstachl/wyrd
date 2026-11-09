//! Drive lifecycle: the person-visible vocabulary for leaving a
//! drive and closing it (OD-11-A/B, both decided as recommended).
//!
//! The mechanism is one CLI operation the engine already gates
//! (`member remove`, owner-only) plus the host and supervisor paths
//! that already shut down in the mount's teardown order. What was
//! missing is the honest account the person asking to leave is owed
//! at the point of asking — whether they may leave at all, and what
//! stays behind either way. This module owns that copy, and only
//! that copy — no authorization, no transport, no membership reads,
//! no sequencing, no state inspection, no confirmation, no error
//! propagation. The runner owns all of those; the host and the
//! supervisor own the actual shutdown.
//!
//! Two settled rules shape every renderer:
//!
//! 1. Authority stays with the owner (OD-11-A, option A): a device
//!    that is not an owner cannot author its own removal
//!    (`docs/contracts/epochs.md` rule 3), so the leave surface for
//!    a non-owner says the owner must remove it, shows the local
//!    device id in copyable form, and offers nothing else. It never
//!    authors a membership event and never reports success. The
//!    sole-owner case reuses the existing `Remove(self)`
//!    transition with its terminal warning. No leave-request message
//!    exists in this surface.
//! 2. Close is quit only (OD-11-B, option A): the quit path
//!    reproduces the mount's teardown order and promises nothing
//!    about the directory. Local forget is a separate destructive
//!    operation with its own authority and confirmation, never an
//!    implicit half of close.

use wyrd_format::DeviceId;

use super::member_surface::{HISTORY_NEVER_REENCRYPTED, REMOVAL_BOUNDARY};

/// What a non-owner asking to leave must read: leaving takes an
/// owner-authored removal, this device authored nothing, and the
/// shared membership is unchanged. Names the local device id so the
/// person can copy it to the owner. States the boundary so the
/// acknowledgement no removal erases held bytes travels with the
/// refusal itself. Never a success: no "left", no "removed", no
/// "complete".
pub const LEAVE_NEEDS_OWNER: &str = "only an owner can remove a device: leaving takes the owner authoring your removal; this device authored nothing and the shared membership is unchanged";

/// What a device with no membership must read: it holds no member
/// or reader seat, so there is nothing to leave. Still authors
/// nothing, still names the device for the owner conversation.
pub const LEAVE_NOT_A_MEMBER: &str =
    "this device holds no member or reader seat on this drive: there is nothing to leave";

/// What close promises: the mount's teardown order, and nothing
/// about the directory. Close keeps local content, membership
/// records, credentials, and drive metadata exactly as they stand —
/// it is an orderly process shutdown, never a forget.
pub const QUIT_ORDER: &str = "close quits in the mount teardown order: unmount and session join first, then admission close and loop join, then transport under bounded deadlines; close keeps the directory, membership, credentials, and metadata, and promises nothing about their contents";

/// What export needs: the drive un-mounted. Export is offline by
/// construction and opens the keystore, so while the application
/// holds the store it fails closed with the lock held — no retry,
/// no lock removal.
pub const EXPORT_NEEDS_UNMOUNTED: &str =
    "export needs the drive un-mounted and fails closed while the application holds it";

/// Render the non-owner leave refusal: the owner requirement first,
/// then the local device id in copyable form, then the canonical
/// boundary and history sentences verbatim. The refusal states the
/// person is still in the shared membership — local departure is
/// not membership removal.
pub fn render_leave_refusal(device: &DeviceId) -> String {
    format!(
        "{LEAVE_NEEDS_OWNER}\ndevice: {device}\nask the owner to run: member remove {device}\nyou are still in the shared membership: local departure is not membership removal\n{REMOVAL_BOUNDARY}\n{HISTORY_NEVER_REENCRYPTED}\n"
    )
}

/// Render the no-seat refusal: the device id plus the no-membership
/// sentence. Authors nothing by construction — a pure string.
pub fn render_not_a_member(device: &DeviceId) -> String {
    format!("{LEAVE_NOT_A_MEMBER}\ndevice: {device}\n")
}

/// Render the quit note: the teardown order plus the no-forget
/// promise. A pure string — the supervisor owns the shutdown.
pub fn render_quit_note() -> String {
    format!("{QUIT_ORDER}\n")
}

/// Render the export requirement: offline plus un-mounted. A pure
/// string — the opener owns the lock behavior.
pub fn render_export_requirement() -> String {
    format!("{EXPORT_NEEDS_UNMOUNTED}\n")
}

#[cfg(test)]
#[path = "tests_lifecycle_surface.rs"]
mod tests_lifecycle_surface;
