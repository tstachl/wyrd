//! Provisioning orchestration: create, reconfigure, and service
//! start in the fixed validate / open / stop / rewrite / verify
//! order. The parent module owns the authority contract; registry
//! I/O and service primitives come from its sibling modules.

use std::path::{Path, PathBuf};
use std::time::Duration;

use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::{Engine, EngineError};

use crate::owner::client::{ClientError, OwnerClient};
use crate::owner::wire::WireErrorCode;

use super::registry::{
    modify_registry, read_registry, valid_relays, validate_entry, validate_mountpoint,
    validate_mountpoint_live, BootstrapError, RegistryEntry, ServiceMode,
};
use super::service::{
    disable_service, enable_service, install_service, service_definition_path, start_service_only,
    ServiceCredential, SupervisedOwner,
};

/// How to initialize the drive before anything is registered.
pub enum Init {
    /// `Engine::create`: a new single-device drive.
    Create,
    /// `Engine::join`: join as an invited device from a sealed
    /// invitation file. The invitation exchange that produced the
    /// file stays out of band; joining consumes it.
    Join {
        /// Sealed invitation bytes consumed by the join.
        sealed_invitation: Vec<u8>,
    },
}

/// What provisioning this drive needs beyond initialization.
pub struct ProvisionConfig {
    /// Drive name registered in the registry.
    pub name: String,
    /// Absolute drive directory to initialize.
    pub drive_dir: PathBuf,
    /// Relay URLs peers dial to reach this drive.
    pub relays: Vec<String>,
    /// Target service mode: `vault` serves headless, `mount` serves
    /// the FUSE projection. Unknown modes refuse at validation.
    pub mode: ServiceMode,
    /// FUSE mountpoint. Required in `mount` mode, refused in `vault`
    /// mode — validated with the entry before anything is created.
    pub mountpoint: Option<PathBuf>,
    /// Device identity secret used to initialize the drive.
    pub identity: DeviceIdentitySecret,
    /// Passphrase protecting the drive keystore.
    pub passphrase: String,
    /// How to initialize the drive before registration.
    pub init: Init,
    /// Registry file path to write the entry into.
    pub registry_path: PathBuf,
    /// The `wyrd` binary whose `vault` or `mount` subcommand the
    /// service runs. Defaults to the `wyrd` beside the provisioning
    /// command; the installer ships both together.
    pub wyrd_bin: Option<PathBuf>,
    /// `wyrd-supervise` (`nix/drive/supervise.py`): the wait-status
    /// mapper the restart policy rests on. Defaults to the sibling
    /// of the `wyrd` binary. Managing a service without it is
    /// refused — a bare owner binary cannot express the terminal
    /// distinction — so pass an explicit path wherever the layout
    /// differs instead of running unmanaged.
    pub supervise_bin: Option<PathBuf>,
    /// How one provisioning run starts the supervised owner:
    /// exactly one credential backend, as a reference (paths or the
    /// store account) — never secrets. A managed file run names both
    /// files; a managed store run names the account; `no_service`
    /// file runs carry `None` for the passphrase (the per-session
    /// posture). A managed file run with `None` refuses at install
    /// time instead of writing a definition the manager could never
    /// start. The identity file is required in every file posture:
    /// the identity secret always comes from its file, never the
    /// pipe.
    pub credential: ServiceCredential,
    /// Install, enable, and start the per-drive service. Off skips
    /// all three: registration only, for operators who supervise
    /// the owner themselves.
    pub manage_service: bool,
    /// How long one socket verification waits for a terminal answer.
    /// An expiry is `Unconfirmed`, never success.
    pub verify_timeout: Duration,
}

/// Where one provisioning run ended. Ready is reported only as
/// `VerifiedAvailable` — a socket answer this session. Every other
/// variant names the stage that stopped, so partial completion is
/// explicit and resumable, never mistaken for ready. The post-write
/// variants carry the registry entries the read skipped, so the
/// caller can surface them instead of silently discarding rotten
/// data on rewrite.
#[derive(Debug)]
pub enum ProvisionState {
    /// Drive verified available; the socket answered this session.
    VerifiedAvailable {
        /// Registry entry that was written and verified.
        entry: RegistryEntry,
        /// Skip reasons carried forward from the written registry.
        skipped: Vec<String>,
    },
    /// Drive registered but the service or socket is not yet usable.
    RegisteredButUnavailable {
        /// Registry entry that was written.
        entry: RegistryEntry,
        /// Machine-readable failure kind.
        kind: ProvisionFailureKind,
        /// Human-readable failure reason.
        reason: String,
        /// Skip reasons carried forward from the written registry.
        skipped: Vec<String>,
    },
    /// Provisioning stopped before registration completed.
    ProvisioningIncomplete {
        /// Pipeline stage that stopped.
        stage: &'static str,
        /// Machine-readable failure kind.
        kind: ProvisionFailureKind,
        /// Human-readable failure reason.
        reason: String,
    },
}

/// The machine-readable failure kind for a provisioning run that
/// did not verify: one variant per terminal failure class (16
/// return sites share 12 kinds), carried alongside (never instead
/// of) the human-readable `reason`. Direct Rust callers match on
/// this instead of parsing text; the rendered strings are
/// unchanged. A new terminal `return` in `provision()` requires a
/// variant here — the compiler enforces the field, only review
/// enforces the right tag. Deliberately coarse — the desktop adapter
/// still branches on exit codes plus independent re-observation
/// alone and never parses CLI output, so this kind stops at the
/// `ProvisionState` boundary by design. The intended consumer is the
/// CLI/desktop adapter after a desktop-shell amendment; until then
/// it serves direct Rust callers and pins classification in tests.
/// `non_exhaustive` so adding a kind later is not a breaking change
/// for downstream exhaustive matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProvisionFailureKind {
    /// Entry validation refused before anything was created.
    Validate,
    /// The sealed invitation did not decode.
    DecodeInvitation,
    /// Engine creation or join failed.
    Initialize,
    /// The fresh open of the created drive failed.
    VerifyOpen,
    /// Another process holds the drive's store lock: stop the live
    /// owner first, the same rule every other offline writer states.
    /// Distinct from other open failures so the CLI can return the
    /// terminal held-drive exit instead of a usage error.
    StoreLocked,
    /// The registry could not be read.
    ReadRegistry,
    /// The registry entry could not be written.
    WriteRegistry,
    /// No `wyrd` binary for the service definition.
    NoWyrdBinary,
    /// No `wyrd-supervise` wrapper for the restart policy.
    NoSuperviseBinary,
    /// Service definition installation failed.
    InstallService,
    /// Service enablement failed.
    EnableService,
    /// Service start (or load verification) failed.
    StartService,
    /// Service stop (disable) before a reconfiguration failed.
    StopService,
    /// The owner socket never verified this session.
    VerifySocket,
}

impl ProvisionFailureKind {
    /// The `stage` string the `ProvisioningIncomplete` sites report
    /// alongside this kind. `None` for the kinds that only occur
    /// post-registration in `provision()` (`RegisteredButUnavailable`
    /// carries no stage). Construction keeps the literal CLI strings;
    /// the kind test asserts they agree, so a future edit that
    /// changes one and not the other fails a test instead of
    /// splitting display from machine kind. `reconfigure()` fails
    /// some service kinds pre-write (binary lookup at validation,
    /// install before the rewrite), so those map to their stages
    /// even though `provision()` never reports them there.
    #[cfg(test)]
    pub(super) fn stage(&self) -> Option<&'static str> {
        match self {
            ProvisionFailureKind::Validate
            | ProvisionFailureKind::NoWyrdBinary
            | ProvisionFailureKind::NoSuperviseBinary => Some("validate"),
            ProvisionFailureKind::DecodeInvitation => Some("initialize"),
            ProvisionFailureKind::Initialize => Some("initialize"),
            ProvisionFailureKind::VerifyOpen | ProvisionFailureKind::StoreLocked => {
                Some("verify-open")
            }
            ProvisionFailureKind::ReadRegistry => Some("read-registry"),
            ProvisionFailureKind::WriteRegistry => Some("write-registry"),
            ProvisionFailureKind::InstallService => Some("install-service"),
            ProvisionFailureKind::EnableService
            | ProvisionFailureKind::StartService
            | ProvisionFailureKind::VerifySocket => None,
            ProvisionFailureKind::StopService => Some("stop-service"),
        }
    }
}

/// The fixed handoff order: initialize → verify the open → write the
/// registry → install, enable, and start the service → verify the
/// socket. Registration and service steps happen only after a
/// successful initialization, never speculatively.
pub fn provision(config: ProvisionConfig) -> ProvisionState {
    let entry = RegistryEntry {
        service: format!("drive-{}", config.name),
        name: config.name.clone(),
        drive_dir: config.drive_dir.clone(),
        relays: config.relays.clone(),
        mode: config.mode.clone(),
        mountpoint: config.mountpoint.clone(),
    };
    if let Err(error) = validate_entry(&entry.name, &entry) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: error.to_string(),
        };
    }
    // Live mountpoint checks before anything is created: shape passed
    // above, but the path must also exist and stand apart from the
    // drive directory, or the installed definition could never work.
    if let Some(mountpoint) = &config.mountpoint {
        if let Err(error) = validate_mountpoint_live(&config.drive_dir, mountpoint) {
            return ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: error.to_string(),
            };
        }
    }
    // A managed service refuses a relay-less drive (it would serve
    // nothing peers can discover), so managing without relays is
    // refused before anything is created or registered — with that
    // pointer, not a definition that can never verify.
    if config.manage_service && config.relays.is_empty() {
        return ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: BootstrapError::ManagedWithoutRelays.to_string(),
        };
    }

    // 1. Initialize, or reconcile an existing drive. `DriveExists`
    // means a previous run (or another operator) already created
    // it: fall through and re-verify the open instead of re-creating.
    match &config.init {
        Init::Create => match Engine::create(
            config.drive_dir.clone(),
            &config.passphrase,
            config.identity.clone(),
        ) {
            Ok(_) | Err(EngineError::DriveExists) => {}
            Err(error) => {
                return ProvisionState::ProvisioningIncomplete {
                    stage: "initialize",
                    kind: ProvisionFailureKind::Initialize,
                    reason: error.to_string(),
                };
            }
        },
        Init::Join { sealed_invitation } => {
            let sealed = match wyrd_sync::control::SealedBootstrap::decode(sealed_invitation) {
                Ok(sealed) => sealed,
                Err(error) => {
                    return ProvisionState::ProvisioningIncomplete {
                        stage: "initialize",
                        kind: ProvisionFailureKind::DecodeInvitation,
                        reason: format!("decode invitation: {error}"),
                    };
                }
            };
            match Engine::join(
                config.drive_dir.clone(),
                &config.passphrase,
                config.identity.clone(),
                &sealed,
            ) {
                Ok(_) | Err(EngineError::DriveExists) => {}
                Err(error) => {
                    return ProvisionState::ProvisioningIncomplete {
                        stage: "initialize",
                        kind: ProvisionFailureKind::Initialize,
                        reason: error.to_string(),
                    };
                }
            }
        }
    };

    // 2. Verify the open before registering anything.
    if let Err(error) = Engine::open_keystore(
        config.drive_dir.clone(),
        &config.passphrase,
        config.identity.clone(),
    ) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "verify-open",
            kind: ProvisionFailureKind::VerifyOpen,
            reason: error.to_string(),
        };
    }

    // 3. Write the registry entry under the lock: the insert runs
    // against the fresh map, so a concurrent same-key update cannot
    // be clobbered by this caller's stale copy. The pre-write read
    // classifies an unreadable registry as `ReadRegistry` before the
    // write is attempted; the locked rewrite below is authoritative
    // for the merge. Skipped entries carry forward verbatim inside
    // the write, and the post-write reasons return — the caller
    // surfaces what the file it wrote actually skips.
    if let Err(error) = read_registry(&config.registry_path) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "read-registry",
            kind: ProvisionFailureKind::ReadRegistry,
            reason: error.to_string(),
        };
    }
    let skipped = match modify_registry(&config.registry_path, |drives| {
        drives.insert(entry.name.clone(), entry.clone());
        Ok(())
    }) {
        Ok(((), skipped)) => skipped,
        Err(error) => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "write-registry",
                kind: ProvisionFailureKind::WriteRegistry,
                reason: error.to_string(),
            };
        }
    };

    // 4. Install, enable, and start the per-drive service.
    if config.manage_service {
        let wyrd_bin = match &config.wyrd_bin {
            Some(path) => path.clone(),
            None => match sibling_bin("wyrd") {
                Some(path) => path,
                None => {
                    return ProvisionState::RegisteredButUnavailable {
                        entry,
                        skipped: skipped.clone(),
                        kind: ProvisionFailureKind::NoWyrdBinary,
                        reason: "no wyrd binary: pass --owner-bin (no wyrd beside this command)"
                            .to_string(),
                    };
                }
            },
        };
        let supervise_bin = match &config.supervise_bin {
            Some(path) => path.clone(),
            None => match sibling_bin("wyrd-supervise") {
                Some(path) => path,
                None => {
                    return ProvisionState::RegisteredButUnavailable {
                        entry,
                        skipped: skipped.clone(),
                        kind: ProvisionFailureKind::NoSuperviseBinary,
                        reason: "no wyrd-supervise: pass --supervise-bin \
                            (the restart policy needs the wrapper; \
                            a bare owner binary cannot express it)"
                            .to_string(),
                    };
                }
            },
        };
        let credential = match managed_credential(&config.credential) {
            Ok(credential) => credential,
            Err((kind, reason)) => {
                return ProvisionState::RegisteredButUnavailable {
                    entry,
                    skipped: skipped.clone(),
                    kind,
                    reason,
                };
            }
        };
        let owner = SupervisedOwner {
            supervise_bin,
            wyrd_bin,
            credential,
        };
        let definition = match install_service_definition(&entry, &owner, None) {
            Ok(definition) => definition,
            Err((kind, reason)) => {
                return ProvisionState::RegisteredButUnavailable {
                    entry,
                    skipped: skipped.clone(),
                    kind,
                    reason,
                };
            }
        };
        if let Err((kind, reason)) = enable_and_start(&entry, &definition) {
            return ProvisionState::RegisteredButUnavailable {
                entry,
                skipped: skipped.clone(),
                kind,
                reason,
            };
        }
    }

    // 5. Verify the socket: the owner must answer terminally. Ready
    // means `Ok` or the pre-first-pass `Unobserved` refusal — every
    // other refusal shape is unexpected from this operation and
    // stays unavailable, never verified. `Unconfirmed` and
    // `Unavailable` likewise never verify: the outcome is unknown
    // or the owner is down.
    match verify_owner_socket(&config.drive_dir, config.verify_timeout, false) {
        Ok(()) => ProvisionState::VerifiedAvailable { entry, skipped },
        Err((kind, reason)) => ProvisionState::RegisteredButUnavailable {
            entry,
            skipped,
            kind,
            reason,
        },
    }
}

/// What a mode reconfiguration changes about one registered
/// drive. The entry is looked up by its drive directory — the one
/// identity the `drive` command already carries — and must resolve
/// to exactly one registry key, never a guess across duplicates.
pub struct ReconfigureConfig {
    /// Absolute drive directory identifying the entry to reconfigure.
    pub drive_dir: PathBuf,
    /// Replacement relay set. `None` keeps the registered set; the
    /// CLI passes `Some` only when the operator named `--relay`
    /// flags, so a credential-path-only reconfigure never clears
    /// relays by accident.
    pub relays: Option<Vec<String>>,
    /// Target service mode, as the display string the registry
    /// carries. `vault` serves headless, `mount` serves the FUSE
    /// projection; any other value refuses with the unknown-mode
    /// pointer before anything stops, exactly like provisioning.
    /// The mountpoint travels alongside: required for `mount`,
    /// refused for `vault`.
    pub mode: ServiceMode,
    /// FUSE mountpoint for `mount` mode. `None` in `vault` mode.
    pub mountpoint: Option<PathBuf>,
    /// Device identity secret proving the keystore still opens.
    pub identity: DeviceIdentitySecret,
    /// Passphrase proving the keystore still opens.
    pub passphrase: String,
    /// Registry file path holding the entry to rewrite.
    pub registry_path: PathBuf,
    /// The `wyrd` binary whose `vault` or `mount` subcommand the
    /// service runs.
    /// Defaults to the `wyrd` beside the reconfiguring command; the
    /// installer ships both together.
    pub wyrd_bin: Option<PathBuf>,
    /// `wyrd-supervise`: the wait-status mapper the restart policy
    /// rests on. Managing a service without it is refused — a bare
    /// owner binary cannot express the terminal distinction — so an
    /// explicit path is required wherever the layout differs.
    pub supervise_bin: Option<PathBuf>,
    /// How one reconfiguration starts the supervised owner:
    /// exactly one credential backend, as a reference (paths or the
    /// store account) — never secrets. See `ProvisionConfig::credential`.
    pub credential: ServiceCredential,
    /// Explicit service definition path (tests); `None` resolves the
    /// platform path. Clearing and installing share it, so the two
    /// can never name different files — and hermetic tests name a
    /// scratch path instead of a developer's real services, mirroring
    /// the registry and binary overrides above.
    pub service_definition: Option<PathBuf>,
    /// Reinstall, enable, and restart the per-drive service after
    /// the rewrite. Off disables the old service, clears its
    /// definition, and rewrites the entry without installing a new
    /// one: registration only.
    pub manage_service: bool,
    /// How long one socket verification waits for a terminal answer.
    /// An expiry is `Unconfirmed`, never success.
    pub verify_timeout: Duration,
}

/// Apply the reconfigured relays, mode, and mountpoint to the
/// stored entry: the single place that builds the working copy
/// install and rewrite share, so the two can never diverge (a
/// managed definition can never render the old relay set beside the
/// new registry entry). Identity fields — name, service,
/// directory — stay put.
pub(super) fn updated_entry(
    mut stored: RegistryEntry,
    relays: Vec<String>,
    mode: ServiceMode,
    mountpoint: Option<PathBuf>,
) -> RegistryEntry {
    stored.relays = relays;
    stored.mode = mode;
    stored.mountpoint = mountpoint;
    stored
}

/// Whether the installed definition must be reinstalled from the
/// written entry: the registry lock may have resolved a fresher
/// relay set than the pre-stop working copy carried. Mode and
/// mountpoint are operator intent in both copies, so only the relay
/// set can differ — and when it does, the manager must serve the
/// written set, never the stale installed one.
pub(super) fn definition_needs_reinstall(working: &RegistryEntry, written: &RegistryEntry) -> bool {
    working.relays != written.relays
}

/// Install the per-drive service definition for a freshly
/// reconfigured (or newly provisioned) entry. Shared by both
/// handoffs so their install failures carry the same kind and
/// reason; the caller decides the state shape around it.
pub(super) fn install_service_definition(
    entry: &RegistryEntry,
    owner: &SupervisedOwner,
    definition_override: Option<&PathBuf>,
) -> Result<PathBuf, (ProvisionFailureKind, String)> {
    install_service(entry, owner, definition_override).map_err(|error| {
        (
            ProvisionFailureKind::InstallService,
            format!("install-service: {error}"),
        )
    })
}

/// The credential a managed install renders: session-supplied
/// file credentials (`None` passphrase) refuse here, never render.
/// A definition embedding no readable credential would install
/// cleanly and then crash-loop under the manager — this fails that
/// run closed at install time instead. Every managed install site
/// routes through this for the same kind and reason. Store
/// credentials render as-is: the manager reads the item, no file
/// needed. The identity file is not checked: it is required in every
/// file posture, so its type carries the invariant and there is
/// nothing to refuse.
pub(super) fn managed_credential(
    credential: &ServiceCredential,
) -> Result<ServiceCredential, (ProvisionFailureKind, String)> {
    match credential {
        ServiceCredential::File {
            passphrase_file: None,
            ..
        } => Err((
            ProvisionFailureKind::InstallService,
            "managed service needs a passphrase file the manager can read: \
             session-supplied credentials (prompt or --passphrase-stdin) cannot render \
             a service definition — provision or reconfigure with --no-service and start \
             sessions with `drive unlock`"
                .to_string(),
        )),
        credential => Ok(credential.clone()),
    }
}

/// Classify a registry failure past the stop: resolve failures
/// (vanished entry, conflicting duplicates, a managed run that
/// resolved no relays) name the registry state to repair — the old
/// service is already stopped and cleared, so `validate` with the
/// stopped-service note describes what the operator must fix, not a
/// write that happened. Anything else is a write failure with the
/// entry untouched.
pub(super) fn classify_post_stop_failure(
    error: &BootstrapError,
) -> (&'static str, ProvisionFailureKind, String) {
    let stopped = "the old service is already stopped and cleared";
    match error {
        BootstrapError::Read { source, .. } if source.kind() == std::io::ErrorKind::NotFound => (
            "validate",
            ProvisionFailureKind::Validate,
            format!("{error} ({stopped}; re-register the drive, then re-run to converge)"),
        ),
        BootstrapError::ConflictingEntries { .. } => (
            "validate",
            ProvisionFailureKind::Validate,
            format!("{error} ({stopped}; resolve the duplicate by hand, then re-run to converge)"),
        ),
        BootstrapError::ManagedWithoutRelays => (
            "validate",
            ProvisionFailureKind::Validate,
            format!("{error} ({stopped}; register relays with --no-service first, then re-run to converge)"),
        ),
        _ => (
            "write-registry",
            ProvisionFailureKind::WriteRegistry,
            error.to_string(),
        ),
    }
}

/// Enable the installed definition and start it now. Shared by both
/// handoffs so their service failures carry the same kinds. The
/// start leg goes through `start_service_only` so the session bound
/// is established on the start path too, not just the enable that
/// precedes it in this same operation.
fn enable_and_start(
    entry: &RegistryEntry,
    definition: &Path,
) -> Result<(), (ProvisionFailureKind, String)> {
    if let Err(error) = enable_service(entry, definition) {
        return Err((
            ProvisionFailureKind::EnableService,
            format!("enable-service: {error}"),
        ));
    }
    if let Err(error) = start_service_only(entry, definition) {
        return Err((
            ProvisionFailureKind::StartService,
            format!("start-service: {error}"),
        ));
    }
    Ok(())
}

/// What one verify attempt concluded.
enum Attempt {
    /// `Ok` or the pre-first-pass `Unobserved` refusal: done.
    Verified,
    /// A terminal refusal from a live owner: retrying cannot change it.
    Answered(String),
    /// No live answer (connect failure, `Unconfirmed`,
    /// `Unavailable`): worth retrying while the deadline holds.
    Silent(String),
}

/// One connect-plus-observe attempt against the drive socket.
fn connect_observe(drive_dir: &Path, attempt_timeout: Duration) -> Attempt {
    let outcome = OwnerClient::connect_drive(drive_dir)
        .and_then(|client| client.with_timeout(attempt_timeout));
    let outcome = outcome.and_then(|mut client| client.observe());
    match outcome {
        Ok(_) => Attempt::Verified,
        Err(ClientError::Refused(error)) if error.code == WireErrorCode::Unobserved => {
            Attempt::Verified
        }
        Err(ClientError::Refused(error)) => Attempt::Answered(format!("{error:?}")),
        Err(error) => Attempt::Silent(error.to_string()),
    }
}

/// Verify the owner socket answers terminally this session: `Ok` or
/// the pre-first-pass `Unobserved` refusal verifies; any other
/// terminal refusal reports unavailable without retrying (a live
/// owner answered). Silence — connect failures, `Unconfirmed`,
/// `Unavailable` — retries until the deadline when `retry_connect`
/// is set (managed reconfigure: a freshly restarted owner still
/// binding never reads as unavailable). A pre-existing socket whose
/// connect fails is unlinked first — it normally belongs to the
/// stopped owner, and the wait below then observes only the
/// restarted owner binding. Without retry (provisioning and
/// `--no-service`) the first attempt decides, exactly as before:
/// wait for appearance, then one terminal attempt.
fn verify_owner_socket(
    drive_dir: &Path,
    verify_timeout: Duration,
    retry_connect: bool,
) -> Result<(), (ProvisionFailureKind, String)> {
    let missing = |reason: String| {
        (
            ProvisionFailureKind::VerifySocket,
            format!("verify-socket: {reason}"),
        )
    };
    if !retry_connect {
        if !wait_for_socket(drive_dir, verify_timeout) {
            return Err(missing("owner.sock never appeared".to_string()));
        }
        return match connect_observe(drive_dir, verify_timeout) {
            Attempt::Verified => Ok(()),
            Attempt::Answered(reason) | Attempt::Silent(reason) => Err(missing(reason)),
        };
    }
    let sock = crate::owner::socket_path_for(drive_dir);
    if sock.exists() {
        match connect_observe(drive_dir, verify_timeout) {
            Attempt::Verified => return Ok(()),
            Attempt::Answered(reason) => return Err(missing(reason)),
            Attempt::Silent(_) => {
                let _ = std::fs::remove_file(&sock);
            }
        }
    }
    let deadline = std::time::Instant::now() + verify_timeout;
    loop {
        // Bound every wait and attempt by the remaining budget so a
        // wedged owner cannot stretch one step past the deadline.
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if !wait_for_socket(drive_dir, remaining) {
            return Err(missing("owner.sock never appeared".to_string()));
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match connect_observe(drive_dir, remaining.min(verify_timeout)) {
            Attempt::Verified => return Ok(()),
            Attempt::Answered(reason) => return Err(missing(reason)),
            Attempt::Silent(reason) => {
                if std::time::Instant::now() >= deadline {
                    return Err(missing(reason));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// Whether an open failure is store-lock contention (as opposed
/// to a credential or state failure): only the lock carries the
/// terminal held-drive kind through the reconfigure handoff.
fn is_store_locked(error: &EngineError) -> bool {
    matches!(
        error,
        EngineError::Durable(wyrd_sync::durable::DurableError::StoreLocked)
    )
}

/// Reconfigure one registered drive in place: validate the target,
/// read the registry and resolve the entry by directory, prove the
/// keystore opens (a live managed lock stops the definition and
/// retries once; any other open failure refuses with everything
/// untouched), stop and clear the old service definition, install
/// the new definition from the updated entry when managed, rewrite
/// the entry, and verify the socket. The fixed order keeps every
/// refusal honest: cheap checks precede the keystore open (no KDF on
/// a refusal) and credential failures precede the stop (no outage on
/// a typo); install and rewrite share one working copy, so the
/// definition can never render the old relay set beside the new
/// entry; the drive is reported ready only when its socket verifies
/// this session — never because the entry was rewritten. Every
/// failure past the stop is retried by re-running, which converges
/// from the stopped-and-cleared state (the operation is idempotent
/// there).
pub fn reconfigure(config: ReconfigureConfig) -> ProvisionState {
    match reconfigure_phases(&config) {
        Ok(state) => state,
        Err(state) => *state,
    }
}

/// Run the phases in the order the contract fixes. A terminal state
/// travels out as `Err` so `?` short-circuits the rest; boxing keeps
/// the error small without changing the public `ProvisionState`
/// shape the caller matches on.
fn reconfigure_phases(config: &ReconfigureConfig) -> Result<ProvisionState, Box<ProvisionState>> {
    validate_reconfigure(config)?;
    let stored = resolve_reconfigure_entry(config)?;
    let (relays, working) = reconfigure_relays_and_working(config, stored)?;
    let (wyrd_bin, supervise_bin) = resolve_reconfigure_bins(config)?;
    prove_keystore_unlockable(config, &working)?;
    stop_reconfigure_service(config, &working)?;
    let definition = install_reconfigure_definition(config, &working, &wyrd_bin, &supervise_bin)?;
    let (written, skipped) = rewrite_reconfigure_entry(config, relays)?;
    let (definition, skipped, effective) = reconcile_reconfigure_definition(
        config,
        &working,
        wyrd_bin,
        supervise_bin,
        definition,
        written,
        skipped,
    )?;
    Ok(finish_reconfigure(config, effective, skipped, definition))
}

fn validate_reconfigure(config: &ReconfigureConfig) -> Result<(), Box<ProvisionState>> {
    // 0. Validate the mode and mountpoint before anything moves: no
    // I/O, no side effects, so a refusal touches nothing. Unknown
    // modes refuse here; mount without a mountpoint (or vault with
    // one) refuses through the entry validation below.
    if matches!(config.mode, ServiceMode::Unknown(_)) {
        return Err(Box::new(ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: BootstrapError::UnknownServiceMode.to_string(),
        }));
    }
    if let Some(mountpoint) = &config.mountpoint {
        if matches!(config.mode, ServiceMode::Vault) {
            return Err(Box::new(ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: BootstrapError::UnexpectedMountpoint.to_string(),
            }));
        }
        if let Err(error) = validate_mountpoint(&config.drive_dir, mountpoint) {
            return Err(Box::new(ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: error.to_string(),
            }));
        }
    } else if matches!(config.mode, ServiceMode::Mount) {
        return Err(Box::new(ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: BootstrapError::MissingMountpoint.to_string(),
        }));
    }
    // Live mountpoint checks before anything moves: shape passed
    // above, but the path must also exist and stand apart from the
    // drive directory, or the installed definition could never work.
    if let Some(mountpoint) = &config.mountpoint {
        if let Err(error) = validate_mountpoint_live(&config.drive_dir, mountpoint) {
            return Err(Box::new(ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: error.to_string(),
            }));
        }
    }

    Ok(())
}

fn resolve_reconfigure_entry(
    config: &ReconfigureConfig,
) -> Result<RegistryEntry, Box<ProvisionState>> {
    // 1. Read the registry and resolve the entry by directory. Zero
    // matches means the drive was never registered (or was
    // registered under another spelling — directories match by
    // exact path only); several means conflicting registrations
    // that only the operator can untangle, never this command.
    let registry = match read_registry(&config.registry_path) {
        Ok(registry) => registry,
        Err(error) => {
            return Err(Box::new(ProvisionState::ProvisioningIncomplete {
                stage: "read-registry",
                kind: ProvisionFailureKind::ReadRegistry,
                reason: error.to_string(),
            }));
        }
    };
    let mut matches: Vec<String> = registry
        .drives
        .iter()
        .filter(|(_, entry)| entry.drive_dir == config.drive_dir)
        .map(|(key, _)| key.clone())
        .collect();
    matches.sort();
    let key = match matches.len() {
        0 => {
            return Err(Box::new(ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: format!(
                    "no registry entry for {}: register with `wyrd bootstrap` first",
                    config.drive_dir.display()
                ),
            }));
        }
        1 => matches.pop().expect("one match"),
        _ => {
            return Err(Box::new(ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: format!(
                    "conflicting registry entries for {}: {} — resolve the duplicate by hand first",
                    config.drive_dir.display(),
                    matches.join(", ")
                ),
            }));
        }
    };
    let stored = registry.drives.get(&key).expect("key resolved").clone();

    Ok(stored)
}

fn reconfigure_relays_and_working(
    config: &ReconfigureConfig,
    stored: RegistryEntry,
) -> Result<(Option<Vec<String>>, RegistryEntry), Box<ProvisionState>> {
    // 2. Candidate relays: flags replace the registered set, absence
    // keeps it — so a credential-path-only reconfigure never clears
    // relays by accident. Explicit values validate before anything
    // stops: a bad relay refuses with the service still up. A kept
    // set is intent, not a snapshot: it resolves from the fresh entry
    // under the lock below, so a concurrent relay update landing
    // between this read and the rewrite is kept, not reverted.
    let relays = match config.relays.clone() {
        Some(candidate) => {
            if !valid_relays(&candidate) {
                return Err(Box::new(ProvisionState::ProvisioningIncomplete {
                    stage: "validate",
                    kind: ProvisionFailureKind::Validate,
                    reason: BootstrapError::InvalidRelays.to_string(),
                }));
            }
            Some(candidate)
        }
        None => None,
    };
    // Pre-stop working values for the definition install: explicit
    // relays, or the pre-stop stored set as a stand-in the lock
    // re-resolves below. A managed drive with no relays to serve is
    // refused before anything stops, like provisioning.
    let install_relays = match &relays {
        Some(explicit) => explicit.clone(),
        None => stored.relays.clone(),
    };
    if config.manage_service && install_relays.is_empty() {
        return Err(Box::new(ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: BootstrapError::ManagedWithoutRelays.to_string(),
        }));
    }

    // The working copy from here on: install and rewrite share the
    // one entry `updated_entry` builds from the pre-stop read, so a
    // managed definition can never render the old relay set beside
    // the new registry entry. The authoritative rewrite below
    // re-resolves under the lock and applies the same intent —
    // explicit relays, or the fresh kept set — to the fresh entry,
    // so a concurrent same-key update between this read and the
    // write is preserved underneath the transformation instead of
    // being clobbered by the stale copy. When the lock resolves a
    // fresher relay set than this copy carries, the definition is
    // reinstalled from the written entry before enabling, so the
    // two never disagree.
    let working = updated_entry(
        stored,
        install_relays,
        config.mode.clone(),
        config.mountpoint.clone(),
    );

    Ok((relays, working))
}

fn resolve_reconfigure_bins(
    config: &ReconfigureConfig,
) -> Result<(Option<PathBuf>, Option<PathBuf>), Box<ProvisionState>> {
    // 3. Resolve the service binaries before stopping: a missing
    // binary refuses with the service still up, never with the drive
    // down. Same reason text as provisioning; the kind marks the
    // pre-stop validation site.
    let (wyrd_bin, supervise_bin) = if config.manage_service {
        let wyrd_bin = match &config.wyrd_bin {
            Some(path) => path.clone(),
            None => match sibling_bin("wyrd") {
                Some(path) => path,
                None => {
                    return Err(Box::new(ProvisionState::ProvisioningIncomplete {
                        stage: "validate",
                        kind: ProvisionFailureKind::NoWyrdBinary,
                        reason: "no wyrd binary: pass --owner-bin (no wyrd beside this command)"
                            .to_string(),
                    }));
                }
            },
        };
        let supervise_bin = match &config.supervise_bin {
            Some(path) => path.clone(),
            None => match sibling_bin("wyrd-supervise") {
                Some(path) => path,
                None => {
                    return Err(Box::new(ProvisionState::ProvisioningIncomplete {
                        stage: "validate",
                        kind: ProvisionFailureKind::NoSuperviseBinary,
                        reason: "no wyrd-supervise: pass --supervise-bin \
                            (the restart policy needs the wrapper; \
                            a bare owner binary cannot express it)"
                            .to_string(),
                    }));
                }
            },
        };
        (Some(wyrd_bin), Some(supervise_bin))
    } else {
        (None, None)
    };

    Ok((wyrd_bin, supervise_bin))
}

fn prove_keystore_unlockable(
    config: &ReconfigureConfig,
    working: &RegistryEntry,
) -> Result<(), Box<ProvisionState>> {
    // 4. Prove the keystore opens before anything stops: credential
    // failures (wrong passphrase, unreadable identity, unopenable
    // store) refuse with the service still running and everything
    // untouched. Only a live managed lock takes the stop path: stop
    // and clear the definition, then retry the open once. A second
    // failure stops here with the service already down — re-running
    // converges. A live lock without management refuses outright
    // (nothing to stop); only the lock refusal carries the terminal
    // kind, every other open failure stays a usage error.
    let open = || {
        Engine::open_keystore(
            config.drive_dir.clone(),
            &config.passphrase,
            config.identity.clone(),
        )
    };
    match open() {
        Ok(_) => {}
        Err(error) if is_store_locked(&error) && config.manage_service => {
            if let Err(error) = disable_service(working, config.service_definition.as_ref()) {
                return Err(Box::new(ProvisionState::ProvisioningIncomplete {
                    stage: "stop-service",
                    kind: ProvisionFailureKind::StopService,
                    reason: format!("stop-service: {error}"),
                }));
            }
            if let Err(error) = open() {
                let kind = if is_store_locked(&error) {
                    ProvisionFailureKind::StoreLocked
                } else {
                    ProvisionFailureKind::VerifyOpen
                };
                return Err(Box::new(ProvisionState::ProvisioningIncomplete {
                    stage: "verify-open",
                    kind,
                    reason: error.to_string(),
                }));
            }
        }
        Err(error) => {
            let kind = if is_store_locked(&error) {
                ProvisionFailureKind::StoreLocked
            } else {
                ProvisionFailureKind::VerifyOpen
            };
            return Err(Box::new(ProvisionState::ProvisioningIncomplete {
                stage: "verify-open",
                kind,
                reason: error.to_string(),
            }));
        }
    }

    Ok(())
}

fn stop_reconfigure_service(
    config: &ReconfigureConfig,
    working: &RegistryEntry,
) -> Result<(), Box<ProvisionState>> {
    // 5. Stop and clear the old service definition. Idempotent: a
    // no-op when step 4 already stopped (the file is gone), and a
    // missing definition file means the drive was never managed —
    // nothing to clear.
    if let Err(error) = disable_service(working, config.service_definition.as_ref()) {
        return Err(Box::new(ProvisionState::ProvisioningIncomplete {
            stage: "stop-service",
            kind: ProvisionFailureKind::StopService,
            reason: format!("stop-service: {error}"),
        }));
    }

    Ok(())
}

fn install_reconfigure_definition(
    config: &ReconfigureConfig,
    working: &RegistryEntry,
    wyrd_bin: &Option<PathBuf>,
    supervise_bin: &Option<PathBuf>,
) -> Result<Option<PathBuf>, Box<ProvisionState>> {
    // 6. Install the new definition when managed — from the working
    // entry, before the registry rewrite, so a crash between them
    // leaves the new definition beside the old entry (the next run
    // reconciles forward) rather than the old definition over the
    // new entry. Without management the definition is already cleared
    // above.
    let definition = if config.manage_service {
        let credential = match managed_credential(&config.credential) {
            Ok(credential) => credential,
            Err((kind, reason)) => {
                return Err(Box::new(ProvisionState::ProvisioningIncomplete {
                    stage: "install-service",
                    kind,
                    reason,
                }));
            }
        };
        let owner = SupervisedOwner {
            supervise_bin: supervise_bin.clone().expect("resolved when managed"),
            wyrd_bin: wyrd_bin.clone().expect("resolved when managed"),
            credential,
        };
        match install_service_definition(working, &owner, config.service_definition.as_ref()) {
            Ok(definition) => Some(definition),
            Err((kind, reason)) => {
                return Err(Box::new(ProvisionState::ProvisioningIncomplete {
                    stage: "install-service",
                    kind,
                    reason,
                }));
            }
        }
    } else {
        None
    };

    Ok(definition)
}

fn rewrite_reconfigure_entry(
    config: &ReconfigureConfig,
    relays: Option<Vec<String>>,
) -> Result<(RegistryEntry, Vec<String>), Box<ProvisionState>> {
    // 7. Rewrite the entry in place under the lock. The updater
    // re-resolves by directory against the fresh map and applies the
    // operator's intent — explicit relays, or the fresh kept set —
    // plus the intended mode and mountpoint. A concurrent same-key
    // update between the pre-stop read and this write is preserved
    // underneath the transformation instead of being clobbered by
    // the stale working copy — including a relay update landing
    // while this run kept the set. Skipped entries carry forward
    // verbatim inside the write, so a future mode or a rotten entry
    // survives the rewrite and is surfaced by the caller instead of
    // silently discarded.
    let intended_relays = relays;
    let intended_mode = config.mode.clone();
    let intended_mountpoint = config.mountpoint.clone();
    let drive_dir = config.drive_dir.clone();
    let registry_path = config.registry_path.clone();
    let manage_service = config.manage_service;
    let (written, skipped) = match modify_registry(&config.registry_path, |drives| {
        let mut matches: Vec<String> = drives
            .iter()
            .filter(|(_, entry)| entry.drive_dir == drive_dir)
            .map(|(key, _)| key.clone())
            .collect();
        matches.sort();
        match matches.len() {
            0 => Err(BootstrapError::Read {
                path: registry_path.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "no registry entry for {}: register with `wyrd bootstrap` first",
                        drive_dir.display()
                    ),
                ),
            }),
            1 => {
                let key = matches.pop().expect("one match");
                let fresh = drives.get(&key).expect("key resolved").clone();
                // Intent, resolved fresh: explicit flags win, absence
                // keeps whatever the fresh entry carries — never the
                // pre-stop snapshot. Values re-validate here, and a
                // managed run with nothing to serve refuses like the
                // pre-stop check: a concurrent no-service writer may
                // have emptied the set after this run read it (nothing
                // else can empty a set — reconfigure keeps or
                // replaces non-empty, provision validates — but the
                // lock is where the kept set is authoritative, so the
                // rule is enforced here too).
                let resolved = match &intended_relays {
                    Some(explicit) => explicit.clone(),
                    None => fresh.relays.clone(),
                };
                if !valid_relays(&resolved) {
                    return Err(BootstrapError::RefusedWrite {
                        source: Box::new(BootstrapError::InvalidRelays),
                    });
                }
                if manage_service && resolved.is_empty() {
                    return Err(BootstrapError::ManagedWithoutRelays);
                }
                let updated = updated_entry(
                    fresh,
                    resolved,
                    intended_mode.clone(),
                    intended_mountpoint.clone(),
                );
                drives.insert(key, updated.clone());
                Ok(updated)
            }
            _ => Err(BootstrapError::ConflictingEntries {
                dir: drive_dir.clone(),
                keys: matches.join(", "),
            }),
        }
    }) {
        Ok((written, skipped)) => (written, skipped),
        Err(error) => {
            let (stage, kind, reason) = classify_post_stop_failure(&error);
            return Err(Box::new(ProvisionState::ProvisioningIncomplete {
                stage,
                kind,
                reason,
            }));
        }
    };

    Ok((written, skipped))
}

fn reconcile_reconfigure_definition(
    config: &ReconfigureConfig,
    working: &RegistryEntry,
    wyrd_bin: Option<PathBuf>,
    supervise_bin: Option<PathBuf>,
    definition: Option<PathBuf>,
    written: RegistryEntry,
    skipped: Vec<String>,
) -> Result<(Option<PathBuf>, Vec<String>, RegistryEntry), Box<ProvisionState>> {
    // The installed definition carries the pre-stop working copy;
    // the lock may have resolved a fresher relay set. Reinstall from
    // the written entry when they differ, so the manager never serves
    // a stale relay set beside the new registry entry. A crash
    // between the write and this reinstall leaves the written entry
    // beside the stopped old definition — the next run reads the
    // fresh entry, installs it, and converges.
    let effective = written;
    let mut definition = definition;
    if definition.is_some() && definition_needs_reinstall(working, &effective) {
        let credential = match managed_credential(&config.credential) {
            Ok(credential) => credential,
            Err((kind, reason)) => {
                return Err(Box::new(ProvisionState::RegisteredButUnavailable {
                    entry: effective,
                    skipped,
                    kind,
                    reason,
                }));
            }
        };
        let owner = SupervisedOwner {
            supervise_bin: supervise_bin.expect("resolved when managed"),
            wyrd_bin: wyrd_bin.expect("resolved when managed"),
            credential,
        };
        match install_service_definition(&effective, &owner, config.service_definition.as_ref()) {
            Ok(definition_path) => definition = Some(definition_path),
            Err((kind, reason)) => {
                return Err(Box::new(ProvisionState::RegisteredButUnavailable {
                    entry: effective,
                    skipped,
                    kind,
                    reason,
                }));
            }
        }
    }

    Ok((definition, skipped, effective))
}

fn finish_reconfigure(
    config: &ReconfigureConfig,
    effective: RegistryEntry,
    skipped: Vec<String>,
    definition: Option<PathBuf>,
) -> ProvisionState {
    // 8. Enable, start, and verify when managed — the same handoff
    // provisioning runs. With `--no-service` and no manually
    // supervised owner, the socket never appears and readiness is
    // withheld — registered, never verified.
    if let Some(definition) = definition {
        if let Err((kind, reason)) = enable_and_start(&effective, &definition) {
            return ProvisionState::RegisteredButUnavailable {
                entry: effective,
                skipped: skipped.clone(),
                kind,
                reason,
            };
        }
    }
    match verify_owner_socket(
        &config.drive_dir,
        config.verify_timeout,
        config.manage_service,
    ) {
        Ok(()) => ProvisionState::VerifiedAvailable {
            entry: effective,
            skipped,
        },
        Err((kind, reason)) => ProvisionState::RegisteredButUnavailable {
            entry: effective,
            skipped,
            kind,
            reason,
        },
    }
}

/// Start one registered drive's installed service without rewriting
/// anything: resolve the entry by directory, refuse where no
/// definition file exists (reinstall is the recovery there, not
/// start), then enable and start the existing definition and verify
/// the socket. No keystore opens, no registry writes, no credential
/// use — starting proves nothing about membership and grants none.
/// Never auto-runs: the caller (the desktop recovery button) runs
/// this only on the person's explicit decision, and a deliberately
/// stopped drive stays stopped until they do.
///
/// Against a live drive this restarts the owner: the start
/// primitives are the reconfigure ones (macOS unloads then loads,
/// systemd restarts), so in-flight work turns `Unconfirmed`. The
/// desktop offers the verb only for stopped and wedged drives; a
/// direct CLI run against a healthy drive restarts it by design, and
/// the socket verify afterwards proves the new owner answers.
///
/// No binary preflight: the definition names its own programs, so
/// sibling checks would validate the wrong files and refuse starts
/// that would work. A missing program surfaces through
/// `start_service_only` and the socket verify instead.
pub struct StartServiceConfig {
    /// Absolute drive directory identifying the entry to start.
    pub drive_dir: PathBuf,
    /// Registry file path holding the entry.
    pub registry_path: PathBuf,
    /// Explicit service definition path (tests); `None` resolves the
    /// platform path, exactly like installation.
    pub service_definition: Option<PathBuf>,
    /// How long one socket verification waits for a terminal answer.
    pub verify_timeout: Duration,
}

/// Start the installed per-drive service for one registered drive.
/// See [`StartServiceConfig`] for the no-rewrite contract.
#[cfg(unix)]
pub fn start_managed_service(config: StartServiceConfig) -> ProvisionState {
    // 1. Read the registry and resolve the entry by directory. Zero
    // matches means the drive was never registered; several means
    // conflicting registrations only the operator can untangle.
    let registry = match read_registry(&config.registry_path) {
        Ok(registry) => registry,
        Err(error) => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "read-registry",
                kind: ProvisionFailureKind::ReadRegistry,
                reason: error.to_string(),
            };
        }
    };
    let mut matches: Vec<String> = registry
        .drives
        .iter()
        .filter(|(_, entry)| entry.drive_dir == config.drive_dir)
        .map(|(key, _)| key.clone())
        .collect();
    matches.sort();
    let entry = match matches.len() {
        0 => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: format!(
                    "no registry entry for {}: register with `wyrd bootstrap` first",
                    config.drive_dir.display()
                ),
            };
        }
        1 => {
            let key = matches.pop().expect("one match");
            registry.drives.get(&key).expect("key resolved").clone()
        }
        _ => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: format!(
                    "conflicting registry entries for {}: {} — resolve the duplicate by hand first",
                    config.drive_dir.display(),
                    matches.join(", ")
                ),
            };
        }
    };
    // 2. The definition must already exist. A missing file is
    // usually the reinstall recovery — but a deliberately
    // unmanaged (`--no-service`, the per-session posture) drive
    // never has one, and reinstalling a managed definition it
    // cannot start would be the wrong repair. Name both
    // recoveries: reinstall the managed service, or start a
    // session directly. Refuse before touching the manager, with
    // that pointer.
    let owned;
    let definition = match config.service_definition.as_ref() {
        Some(path) => path.as_path(),
        None => {
            owned = service_definition_path(&entry);
            &owned
        }
    };
    if !definition.exists() {
        return ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: format!(
                "no service definition installed for {} ({} missing): reinstall with `wyrd drive {} reconfigure --mode <vault|mount>`, or start a session directly with `wyrd drive {} unlock`",
                entry.name,
                definition.display(),
                config.drive_dir.display(),
                config.drive_dir.display()
            ),
        };
    }
    // 3. Start the existing definition without touching its
    // login-start preference, then verify the socket with the
    // reconfigure retry (a freshly restarted owner still binding
    // never reads as unavailable). No binary preflight: the
    // definition names its own programs; a missing program fails
    // here and in the verify below, which is where the recovery
    // actually runs. Starting never enables: a disabled drive
    // started manually stays disabled at the next login.
    if let Err(error) = start_service_only(&entry, definition) {
        return ProvisionState::RegisteredButUnavailable {
            entry,
            skipped: registry.skipped.clone(),
            kind: ProvisionFailureKind::StartService,
            reason: format!("start-service: {error}"),
        };
    }
    match verify_owner_socket(&config.drive_dir, config.verify_timeout, true) {
        Ok(()) => ProvisionState::VerifiedAvailable {
            entry,
            skipped: registry.skipped,
        },
        Err((kind, reason)) => ProvisionState::RegisteredButUnavailable {
            entry,
            skipped: registry.skipped,
            kind,
            reason,
        },
    }
}

#[cfg(not(unix))]
pub fn start_managed_service(_config: StartServiceConfig) -> ProvisionState {
    ProvisionState::ProvisioningIncomplete {
        stage: "validate",
        kind: ProvisionFailureKind::Validate,
        reason: BootstrapError::ServicesUnsupported.to_string(),
    }
}

/// Wait for the owner socket to appear: the service was just
/// enabled and the owner may still be starting. Polls; never
/// invents an answer.
fn wait_for_socket(drive_dir: &Path, timeout: Duration) -> bool {
    let sock = crate::owner::socket_path_for(drive_dir);
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if sock.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    sock.exists()
}

/// Locate a sibling binary beside the running executable: the
/// installer ships the set together, so the sibling is the default.
/// Overridable with an explicit path wherever the layout differs.
fn sibling_bin(name: &str) -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let dir = current.parent()?;
    let candidate = dir.join(name);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(&candidate) {
            if metadata.permissions().mode() & 0o111 != 0 {
                return Some(candidate);
            }
        }
        None
    }
    #[cfg(not(unix))]
    {
        if candidate.exists() {
            return Some(candidate);
        }
        None
    }
}
