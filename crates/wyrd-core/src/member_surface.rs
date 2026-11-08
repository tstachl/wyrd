//! Member and owner management: the person-visible vocabulary for
//! the owner's screen (OD-14-A/B/C, all decided as recommended).
//!
//! The mechanism is five CLI operations the engine already
//! owner-gates (`list`, `log`, `status`, `remove`, `rotate`,
//! `set-owner`); what was missing is the account the owner is owed at
//! the point of action — the two sentences no document collected in
//! one place: what a removed device still holds and can still fetch,
//! and that history is never re-encrypted. This module owns that
//! copy, and only that copy — no authorization, no transport, no
//! membership reads. Every sentence below renders the normative
//! "What removal does and does not do" subsection of
//! `docs/contracts/trust.md` verbatim; no surface paraphrases it.
//!
//! Three settled rules shape every renderer:
//!
//! 1. Last-owner removal is a refusal, not an inline warning
//!    (OD-14-B, option A): the confirm control stays disabled until
//!    the terminal consequence has been read, which is today's CLI
//!    `--yes` shape generalized. One extra step on an operation that
//!    permanently ends a drive is the correct price.
//! 2. No co-ownership (OD-14-C, option A): v0 ownership is a
//!    singleton, so the `set-owner` target names a current member and
//!    nothing else, and `RemovingOwner` stays unreachable.
//! 3. Rotation mints a fresh epoch secret and changes nothing else:
//!    no member added or removed, and no device key touched —
//!    replacing a device is a removal plus an admission under a fresh
//!    identity, because identity is single-use within a membership
//!    chain.

use wyrd_format::{DeviceId, TransitionId};

/// The canonical removal sentence (OD-14-A): what the removed device
/// keeps and what it can still fetch. Rendered verbatim from the
/// normative trust.md subsection — never paraphrased.
pub const REMOVAL_BOUNDARY: &str = "Removal bounds acquisition, not knowledge: a removed device keeps its local plaintext, and can keep fetching any content whose transport root it learned before removal, from any serving member that still answers. It learns no new roots";

/// The canonical history sentence (OD-14-A): no silent
/// re-encryption, ever. Rendered verbatim from the normative trust.md
/// subsection — never paraphrased.
pub const HISTORY_NEVER_REENCRYPTED: &str = "History is never re-encrypted: excluding a removed device from old-epoch content is an explicit, separate migration, never an implicit side effect of removal";

/// The terminal-consequence sentence (OD-14-B, option A): what the
/// last-owner acknowledgement requires the person to have read.
/// Rendered verbatim from the normative trust.md subsection.
pub const SOLE_OWNER_TERMINAL: &str = "Removing the sole owner is valid but terminal: the owner set empties and no future transition can ever be authorized, so the drive freezes permanently";

/// Render the removal confirmation: the removed line (byte-identical
/// to the legacy `member remove` headline) followed by the canonical
/// boundary and history sentences. The terminal sentence appends only
/// when the owner set emptied with the author. No euphemism appears:
/// no "wiped", no "no longer has access", no "revoked all access", no
/// "can no longer read" — each would overstate a boundary that ends
/// acquisition, never possession.
pub fn render_removal_confirmation(
    device: &DeviceId,
    epoch: u64,
    carried: usize,
    terminal: bool,
) -> String {
    let mut out = format!("removed {device} at epoch {epoch} ({carried} carried)\n");
    out.push_str(REMOVAL_BOUNDARY);
    out.push('\n');
    out.push_str(HISTORY_NEVER_REENCRYPTED);
    out.push('\n');
    if terminal {
        out.push_str(SOLE_OWNER_TERMINAL);
        out.push('\n');
    }
    out
}

/// Render the rotation confirmation: the rotated line
/// (byte-identical to the legacy `member rotate` headline) followed
/// by what rotation did — a fresh epoch secret, membership
/// untouched. No device-key claim appears, because rotation cannot
/// express one: replacing a device still takes a removal plus an
/// admission under a fresh identity.
pub fn render_rotation_confirmation(epoch: u64, carried: usize) -> String {
    format!(
        "rotated to epoch {epoch} ({carried} carried)\nrotation mints a fresh epoch secret; membership unchanged — no member added or removed. Replacing a device still takes a removal plus an admission under a fresh identity\n"
    )
}

/// Render the handover confirmation: the new-owner line
/// (byte-identical to the legacy `member set-owner` headline)
/// attributed to the outgoing owner who signed it, with the carried
/// head count. v0 ownership is a singleton handed from member to
/// member; the engine refuses a non-member target, so the picker
/// offers current members and nothing else.
pub fn render_handover_confirmation(
    new_owner: &DeviceId,
    author: &DeviceId,
    epoch: u64,
    carried: usize,
) -> String {
    format!(
        "owner is now {new_owner} at epoch {epoch} ({carried} carried) — handed over by {author}\n"
    )
}

/// Render the frozen-drive notice for the management surface: the
/// freeze half names the epoch and its rival tips, and the remedy
/// half points at `member resolve` — never at authoring another
/// control, because nothing new is canonical while frozen. Only an
/// owner resolves.
pub fn render_frozen_notice(epoch: u64, rivals: &[TransitionId]) -> String {
    let rivals = rivals
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "frozen at epoch {epoch}\nrivals: {rivals}\nresolve with: member resolve <winner> --void <sibling>... (only an owner resolves; nothing else authors while frozen)"
    )
}

#[cfg(test)]
#[path = "tests_member_surface.rs"]
mod tests_member_surface;
