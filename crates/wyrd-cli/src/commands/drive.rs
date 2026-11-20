//! The `drive` command: local drive lifecycle beyond membership —
//! deleting this drive's local copy after authoring your own removal,
//! reconfiguring a registered drive's supervision in place, and
//! starting an installed service without rewriting anything.
//!
//! Delete composes two operations that already exist: the owner
//! authors `Remove(self)` through the engine (which enforces
//! owner-only, the terminal acknowledgement, and the frozen-drive
//! refusal), then the runner removes the local directory. The
//! directory is removed only after the transition commits, so every
//! refusal — non-owner, unacknowledged terminal, frozen, locked store
//! — leaves it untouched. A mounted drive fails closed with the
//! store lock: stop the service first, the same rule `member` states.
//!
//! Service-start resolves the registry entry by drive directory
//! (exact match; zero or duplicate matches refuse without writing),
//! refuses where no definition file exists (reinstall is the recovery
//! there), then enables and starts the existing definition and
//! verifies the socket. No keystore opens: starting proves nothing
//! about membership and needs no credentials. Against a live drive
//! it restarts the owner by design, turning in-flight work
//! `Unconfirmed` — offer it only for stopped and wedged drives.
//!
//! One operator at a time: concurrent direct CLI invocations can
//! interleave past the store open (the desktop flight serializes
//! only its own calls), so serialize service-starts and
//! reconfigurations against any other lifecycle operation on the
//! drive — a start writes nothing, but its restart still races
//! another writer's stop the way any two managers would, and the
//! last writer wins the entry whole, never a torn mix.
//!
//! Reconfigure resolves the registry entry by drive directory (exact
//! match; zero or duplicate matches refuse without writing), proves
//! the keystore still opens (retrying once past a live managed
//! lock, while any other open failure refuses with everything
//! untouched), stops and clears the old service definition, installs
//! the new definition when managed from the updated entry, rewrites
//! the entry carrying skipped entries forward, and verifies the
//! socket.
//! One operator at a time: concurrent direct CLI invocations can
//! interleave past the store open (the desktop flight serializes
//! only its own calls), so serialize reconfigurations against any
//! other lifecycle operation on the drive — the last writer wins the
//! entry while each rewrite stays atomic.

use std::path::{Path, PathBuf};
use std::time::Duration;

use wyrd_core::lifecycle_surface::{render_deletion_confirmation, render_leave_refusal};
use wyrd_daemon::bootstrap::{
    default_registry_path, read_registry, reconfigure, service_definition_path_for_service,
    set_service_enabled, start_managed_service, ReconfigureConfig, RegistryEntry,
    StartServiceConfig,
};
use wyrd_format::{Change, DeviceId, MembershipTransition};
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::Engine;

use crate::args::{Credentials, DriveAction};
use crate::commands::bootstrap::{absolutize, canonicalize, warn_skipped};
use crate::commands::member::{carry_pending, require_last_owner_confirmation};
use crate::drive_config::LoadedConfig;
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
    credentials: &Credentials,
    config: Option<&LoadedConfig>,
) -> Result<(), CliError> {
    match action {
        DriveAction::Init | DriveAction::Bootstrap { .. } => {
            unreachable!("init/bootstrap resolve in the dispatcher before drive()")
        }
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
        DriveAction::Reconfigure {
            mode,
            mountpoint,
            relay,
            registry,
            owner_bin,
            supervise_bin,
            no_service,
        } => reconfigure_drive(
            drive_dir,
            mode,
            mountpoint,
            relay,
            registry,
            owner_bin,
            supervise_bin,
            no_service,
            credentials,
            config,
            passphrase,
            identity,
        ),
        DriveAction::ServiceStart { .. } => {
            unreachable!("service-start resolves in the dispatcher before drive()")
        }
        DriveAction::ServiceEnable { .. } => {
            unreachable!("service-enable resolves in the dispatcher before drive()")
        }
        DriveAction::ServiceDisable { .. } => {
            unreachable!("service-disable resolves in the dispatcher before drive()")
        }
        DriveAction::Unlock { .. } => {
            unreachable!("unlock runs the owner in the dispatcher before drive()")
        }
        DriveAction::MigrateCredentials { .. } => {
            unreachable!("migrate-credentials resolves in the dispatcher before drive()")
        }
    }
}

/// Start this registered drive's installed service without rewriting
/// anything and without touching its login-start preference: start
/// the existing definition now, then verify the socket. A disabled
/// drive started manually stays disabled at the next login. No
/// keystore opens and no credentials load — the dispatcher calls
/// this before credential loading, so a stopped service starts even
/// where the operator's key files are momentarily unavailable.
/// Ready is reported only when the socket verifies this session.
pub(crate) fn service_start_drive(
    drive_dir: PathBuf,
    registry: Option<PathBuf>,
) -> Result<(), CliError> {
    let report = start_managed_service(StartServiceConfig {
        drive_dir: drive_dir.clone(),
        registry_path: registry.unwrap_or_else(default_registry_path),
        service_definition: None,
        verify_timeout: Duration::from_secs(30),
    });
    match report {
        wyrd_daemon::bootstrap::ProvisionState::VerifiedAvailable { entry, skipped } => {
            warn_skipped(&skipped);
            println!(
                "drive {} service started and verified available (socket {})",
                entry.name,
                entry.drive_dir.join("owner.sock").display()
            );
            Ok(())
        }
        wyrd_daemon::bootstrap::ProvisionState::RegisteredButUnavailable {
            entry,
            reason,
            skipped,
            ..
        } => {
            warn_skipped(&skipped);
            Err(CliError::Usage(format!(
                "drive {} service started but unavailable: {reason}",
                entry.name
            )))
        }
        // No store ever opens on this path, so the terminal
        // held-drive kind cannot occur here: every incomplete stage
        // is a usage error with its stage.
        wyrd_daemon::bootstrap::ProvisionState::ProvisioningIncomplete {
            stage, reason, ..
        } => Err(CliError::Usage(format!(
            "service start incomplete at {stage}: {reason}"
        ))),
    }
}

/// Resolve the registry entry and definition file a service-toggle
/// operates on: managed drives only. A missing definition refuses
/// with the reinstall pointer — session-posture (`--no-service`)
/// drives have no unit to enable, disable, or start, and a removed
/// definition is the reinstall recovery, never a toggle.
fn resolve_service_toggle_target(
    drive_dir: &Path,
    registry: Option<PathBuf>,
) -> Result<(RegistryEntry, PathBuf), CliError> {
    let registry = read_registry(&registry.unwrap_or_else(default_registry_path)).map_err(|error| {
        CliError::Usage(format!(
            "service toggle cannot read the registry: {error} (register with `wyrd bootstrap` first)"
        ))
    })?;
    let mut matches: Vec<RegistryEntry> = registry
        .drives
        .values()
        .filter(|entry| entry.drive_dir == drive_dir)
        .cloned()
        .collect();
    matches.sort_by(|left, right| left.name.cmp(&right.name));
    let entry = match matches.len() {
        0 => {
            return Err(CliError::Usage(format!(
                "no registry entry for {}: register with `wyrd bootstrap` first",
                drive_dir.display()
            )));
        }
        1 => matches.pop().expect("one match"),
        _ => {
            let names: Vec<&str> = matches.iter().map(|entry| entry.name.as_str()).collect();
            return Err(CliError::Usage(format!(
                "conflicting registry entries for {}: {} — resolve the duplicate by hand first",
                drive_dir.display(),
                names.join(", ")
            )));
        }
    };
    let definition = service_definition_path_for_service(&entry.service);
    if !definition.exists() {
        return Err(CliError::Usage(format!(
            "no service definition installed for {} ({} missing): reinstall with `wyrd drive {} reconfigure --mode <vault|mount>`, or start a session directly with `wyrd drive {} unlock`",
            entry.name,
            definition.display(),
            drive_dir.display(),
            drive_dir.display()
        )));
    }
    Ok((entry, definition))
}

/// Enable this registered drive's login start without starting it:
/// record the preference with the service manager, then verify the
/// record — the running state is unchanged either way. Managed
/// drives only (see [`resolve_service_toggle_target`]).
pub(crate) fn service_enable_drive(
    drive_dir: PathBuf,
    registry: Option<PathBuf>,
) -> Result<(), CliError> {
    let (entry, definition) = resolve_service_toggle_target(&drive_dir, registry)?;
    set_service_enabled(&entry, &definition, true).map_err(|error| {
        CliError::Usage(format!(
            "drive {} login start could not be enabled: {error}",
            entry.name
        ))
    })?;
    println!(
        "drive {} login start enabled (starts at next login; running state unchanged)",
        entry.name
    );
    Ok(())
}

/// Disable this registered drive's login start and stop it now,
/// keeping the definition file: record the preference with the
/// service manager, then verify the record and the stopped state.
/// Managed drives only (see [`resolve_service_toggle_target`]).
pub(crate) fn service_disable_drive(
    drive_dir: PathBuf,
    registry: Option<PathBuf>,
) -> Result<(), CliError> {
    let (entry, definition) = resolve_service_toggle_target(&drive_dir, registry)?;
    set_service_enabled(&entry, &definition, false).map_err(|error| {
        CliError::Usage(format!(
            "drive {} login start could not be disabled: {error}",
            entry.name
        ))
    })?;
    println!(
        "drive {} login start disabled (service stopped; stays stopped at login)",
        entry.name
    );
    Ok(())
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

/// Reconfigure one registered drive in place: resolve the
/// credential paths the service definition names, then run the
/// daemon's stop-clear-reconfigure-restart-verify handoff and
/// report exactly which stage a partial run reached. Ready is
/// reported only when the socket verifies this session — a
/// rewritten entry alone never reports ready.
#[allow(clippy::too_many_arguments)]
fn reconfigure_drive(
    drive_dir: PathBuf,
    mode: String,
    mountpoint: Option<PathBuf>,
    relay: Vec<String>,
    registry: Option<PathBuf>,
    owner_bin: Option<PathBuf>,
    supervise_bin: Option<PathBuf>,
    no_service: bool,
    credentials: &Credentials,
    config: Option<&LoadedConfig>,
    passphrase: &str,
    identity: DeviceIdentitySecret,
) -> Result<(), CliError> {
    // Credential paths for the definition: the identity file
    // always, the passphrase file only when a definition will
    // actually render one. Session posture (`--passphrase-stdin`
    // with `--no-service`) skips the file entirely.
    let (identity_file, passphrase_file) =
        crate::credentials::resolve_service_credential_files(credentials, config, !no_service)?;
    let identity_file = canonicalize(&identity_file)?;
    let passphrase_file = passphrase_file
        .map(|path| canonicalize(&path))
        .transpose()?;
    let owner_bin = owner_bin.map(absolutize);
    let supervise_bin = supervise_bin.map(absolutize);
    // Flags replace the registered set; no flags keep it, so a
    // credential-path-only reconfigure never clears relays by
    // accident.
    let relays = if relay.is_empty() { None } else { Some(relay) };
    let registry_path = registry.clone().unwrap_or_else(default_registry_path);
    let (mode, mountpoint) = super::bootstrap::parse_service_mode(&mode, mountpoint)?;
    // The daemon reports the lock refusal as data (the state shape
    // carries kinds, never typed errors); the CLI restores the
    // terminal held-drive exit the supervisor keys on, so keep the
    // directory for that arm.
    let report = reconfigure(ReconfigureConfig {
        drive_dir: drive_dir.clone(),
        relays,
        mode,
        mountpoint,
        identity,
        passphrase: passphrase.to_string(),
        registry_path,
        wyrd_bin: owner_bin,
        supervise_bin,
        identity_file,
        passphrase_file,
        service_definition: None,
        manage_service: !no_service,
        verify_timeout: Duration::from_secs(30),
    });
    match report {
        wyrd_daemon::bootstrap::ProvisionState::VerifiedAvailable { entry, skipped } => {
            warn_skipped(&skipped);
            println!(
                "drive {} reconfigured and verified available (socket {})",
                entry.name,
                entry.drive_dir.join("owner.sock").display()
            );
            Ok(())
        }
        wyrd_daemon::bootstrap::ProvisionState::RegisteredButUnavailable {
            entry,
            reason,
            skipped,
            ..
        } => {
            warn_skipped(&skipped);
            Err(CliError::Usage(format!(
                "drive {} reconfigured but unavailable: {reason}",
                entry.name
            )))
        }
        wyrd_daemon::bootstrap::ProvisionState::ProvisioningIncomplete {
            stage,
            kind,
            reason,
        } => {
            if matches!(
                kind,
                wyrd_daemon::bootstrap::ProvisionFailureKind::StoreLocked
            ) {
                // A live owner holding the drive is the terminal
                // held-drive case (exit 78, never retried by the
                // supervisor): stop the service first, the same rule
                // every other offline writer states.
                return Err(CliError::OwnerLocked { drive: drive_dir });
            }
            Err(CliError::Usage(format!(
                "reconfiguration incomplete at {stage}: {reason}"
            )))
        }
    }
}
