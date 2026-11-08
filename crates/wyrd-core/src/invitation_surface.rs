//! Invitation and join: the person-visible guided-flow vocabulary
//! (OD-13-A/B/C, all decided as recommended).
//!
//! The mechanism is three offline file exchanges the CLI already
//! performs (`pairing-request`, `member invite`, `device join`); what
//! was missing is the plain account the newcomer is owed at the
//! moment of joining. This module owns that copy, and only that
//! copy — no authorization, no transport, no membership reads.
//!
//! Three sentences, each pinned below:
//!
//! 1. History boundary (OD-13-B, option B): content written before
//!    this device was admitted may not be reachable. The admission
//!    walk covers the live chain only, and learning is
//!    statement-driven, not admission-driven — a late joiner's
//!    durable state names no superseded-epoch snapshot ids until
//!    later reconciliation delivers them (the v0.3 late-joiner
//!    tracker owns the gap itself; this surface states it, never
//!    fixes it, never promises completeness).
//! 2. Catch-up cadence: catch-up arrives on the next mounted sync,
//!    not at join. Join commits genesis plus the invitation grant;
//!    the admission transition and the capability's authorization
//!    arrive through the normal intake path afterwards.
//! 3. Recovery pointer: a lost invitation file recovers with
//!    `reissue`, never with a second admission — re-inviting an
//!    admitted device resumes its catch-up and reissues the file
//!    rather than authoring a duplicate.
//!
//! Out of band by construction (OD-13-A, option A): every step is a
//! file in, a file out, and no step takes a relay, a mailbox, or a
//! control message — an invitation bootstraps a device with no
//! engine yet, so it cannot arrive as one. Member-only sharing
//! (OD-13-C): the flow offers no role choice; the reader role stays
//! reachable only through the CLI flag, and the confirmation reads
//! the same either way.
//!
//! The single-use-identity consequence (a removed device returns
//! only under a fresh identity) is carried by the engine's own
//! refusal (`RetiredDevice` names the remedy) and pinned at the CLI
//! level; this module does not restate it.

use wyrd_format::{DeviceId, DriveId};

/// The history-boundary sentence: what the newcomer may not get.
/// Names the limit (pre-admission content may not be reachable) and
/// the mechanism (live-chain-only admission, statement-driven
/// learning) without promising that sync will ever complete it.
pub const HISTORY_BOUNDARY: &str = "history written before this device was admitted may not be reachable: the admission covers the live chain only, and pre-admission snapshots arrive through later reconciliation, never promised at join";

/// The catch-up sentence: when the newcomer becomes useful. Join
/// commits genesis plus the invitation grant; everything else
/// arrives on the next mounted sync.
pub const CATCH_UP_NEXT_SYNC: &str = "catch-up arrives on the next mounted sync, not here";

/// The recovery sentence: what to do when the invitation file is
/// lost. Reissue reseals the same admission; re-inviting resumes
/// the same admission — neither authors a second one.
pub const RECOVERY_IS_REISSUE: &str =
    "a lost invitation file recovers with reissue, never with a second admission";

/// Render the join confirmation: the joined line (byte-identical to
/// the legacy `device join` headline) followed by the boundary, the
/// catch-up cadence, and the recovery pointer. No completeness
/// claim appears: no "complete", no "up to date", no "in sync".
pub fn render_join_confirmation(device: &DeviceId, drive: &DriveId, epoch: u64) -> String {
    format!(
        "joined {device} drive {drive} at epoch {epoch}\n{HISTORY_BOUNDARY}\n{CATCH_UP_NEXT_SYNC}\n{RECOVERY_IS_REISSUE}\n"
    )
}

#[cfg(test)]
#[path = "tests_invitation_surface.rs"]
mod tests_invitation_surface;
