//! The `drive` command: local drive lifecycle beyond membership —
//! deleting this drive's local copy after authoring your own removal.
//!
//! Delete composes two operations that already exist: the owner
//! authors `Remove(self)` through the engine (which enforces
//! owner-only, the terminal acknowledgement, and the frozen-drive
//! refusal), then the runner removes the local directory. The
//! directory is removed only after the transition commits, so every
//! refusal — non-owner, unacknowledged terminal, frozen, locked store
//! — leaves it untouched. A mounted drive fails closed with the
//! store lock: stop the service first, the same rule `member` states.

use std::path::PathBuf;

use wyrd_core::lifecycle_surface::{render_deletion_confirmation, render_leave_refusal};
use wyrd_format::{Change, DeviceId, MembershipTransition};
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::Engine;

use crate::args::DriveAction;
use crate::commands::member::{carry_pending, require_last_owner_confirmation};
use crate::CliError;

/// What `drive delete` did: the authored self-removal plus the
/// carried count and the terminal flag, returned so tests assert the
/// transition shape without reopening a directory that is gone.
pub(crate) struct DeleteReport {
    pub(crate) device: DeviceId,
    pub(crate) transition: MembershipTransition,
    pub(crate) carried: usize,
    pub(crate) terminal: bool,
}

/// Run one `drive` action over the keystore.
pub(crate) fn drive(
    drive_dir: PathBuf,
    action: DriveAction,
    passphrase: &str,
    identity: DeviceIdentitySecret,
) -> Result<(), CliError> {
    match action {
        DriveAction::Delete { yes } => {
            let report = delete_drive(drive_dir.clone(), passphrase, identity, yes)?;
            print!(
                "{}",
                render_deletion_confirmation(
                    &report.device,
                    report.transition.epoch,
                    report.carried,
                    &drive_dir,
                    report.terminal
                )
            );
            Ok(())
        }
    }
}

/// Delete this drive locally: refuse a non-owner before anything is
/// authored or touched, require acknowledgement of the terminal
/// consequence, author `Remove(self)` through the owner, then remove
/// the local directory. The engine owns the frozen-drive refusal
/// (nothing commits while frozen, so the directory survives); the
/// directory removal runs only after the commit, and the store is
/// released first.
pub(crate) fn delete_drive(
    drive_dir: PathBuf,
    passphrase: &str,
    identity: DeviceIdentitySecret,
    yes: bool,
) -> Result<DeleteReport, CliError> {
    let mut engine = Engine::open_keystore(drive_dir.clone(), passphrase, identity)?;
    let this = engine.device();
    // OD-12-C, option A: a non-owner authors nothing and loses
    // nothing — the refusal names the owner-run command, like the
    // `member leave` refusal, and the directory is never touched.
    let owners = engine
        .membership_log()
        .known_state()
        .and_then(|tip| engine.membership_log().owners_of(&tip.transition_id))
        .unwrap_or_default();
    if !owners.contains(&this) {
        return Err(CliError::Usage(render_leave_refusal(&this)));
    }
    // Terminal acknowledgement first: without `--yes` the refusal
    // carries the terminal consequence and authors nothing.
    require_last_owner_confirmation(&engine, &this, yes)?;
    // Stage the carry, then commit the self-removal. From here the
    // membership half is durable: every error below leaves a
    // committed terminal transition behind, so the forget half must
    // not depend on the drain. The carry is a no-op for a removed
    // author (it holds no new-epoch material and drains nothing), so
    // a drain failure is reported loudly and forgotten anyway —
    // stranding a terminal drive with its directory present and no
    // retry would be worse than a warning on stderr.
    engine.stage_carry_heads()?;
    let transition = engine.remove_device(this)?;
    let carried = match carry_pending(&mut engine, &drive_dir) {
        Ok(carried) => carried,
        Err(error) => {
            eprintln!("warning: carry drain failed after the removal committed: {error}");
            0
        }
    };
    // The deletion transition names exactly one change: this device's
    // own removal. The engine constructs it literally, and the named
    // contract test pins the shape; anything else would mean the
    // surface authored work the person never asked for.
    debug_assert!(
        transition.changes() == [Change::Remove(this)],
        "deletion authors only its own removal"
    );
    let terminal = engine
        .membership_log()
        .owners_of(&transition.transition_id())
        .map(|owners| owners.is_empty())
        .unwrap_or(false);
    let report = DeleteReport {
        device: this,
        transition,
        carried,
        terminal,
    };
    // Release the store before removing its directory: the lock dies
    // with the engine, and the removal is a plain filesystem walk
    // afterwards.
    drop(engine);
    if let Err(source) = std::fs::remove_dir_all(&drive_dir) {
        // The removal already committed: the membership half stands
        // and the directory half is the operator's manual step now.
        // Report loudly rather than printing a confirmation that
        // would be a lie.
        return Err(CliError::Io {
            path: drive_dir,
            source,
        });
    }
    Ok(report)
}
