//! The Tauri command surface: one command per backend operation,
//! forwarding through one [`DesktopBackend`] per drive, keyed by
//! explicit drive identity.
//!
//! Discipline, inherited from the backend and the interface
//! contract:
//!
//! - Forward, never interpret. Every command calls exactly one
//!   [`DesktopBackend`] method with the caller's arguments and
//!   returns its answer untouched — taxonomy, refusal codes, and
//!   result shapes verbatim. No command exists that is not a
//!   backend method.
//! - Off-runtime exchanges. [`OwnerClient`] is synchronous with a
//!   long response wait, so every exchange runs on the blocking
//!   pool ([`tauri::async_runtime::spawn_blocking`]), never on a
//!   Tauri async executor thread. The state mutex is never held
//!   across an async await: the blocking task acquires an owned
//!   handle ([`tauri::AppHandle`]), locks inside, and returns.
//! - Owned arguments only (`String`, deserialized request types).
//!   Borrowed command arguments would force a `Result` return
//!   through the command macro; owned arguments keep every
//!   signature uniform instead.
//! - A failed worker is an unconfirmed outcome, never success and
//!   never a refusal: if the blocking task dies after dispatch,
//!   the outcome is unknown by definition.
//!
//! Command names are the backend method names verbatim (`lookup`,
//! `stat`, …): the frontend in `apps/desktop` invokes exactly
//! these, and the per-operation wrappers there land against this
//! list — never against invented names.

use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, Runtime};
use wyrd_owner_client::{
    OwnerClient, WireAttr, WireDirEntry, WireFreshness, WireLiveness, WireMutationKind,
    WireMutationOutcome, WireNode, WireParentToken, WireSyncStatus,
};
use wyrd_settings::SparseUpdate;

use super::registry::{self, RegistryState, RegistryStatus};
use super::settings_apply::{
    self, SettingsApplyRequest, SettingsApplyResult, SETTINGS_APPLY_TIMEOUT,
};
use super::settings_view::{self, DriveSettings};
use super::{AppState, BackendError, DesktopBackend, FlightTracker};

/// Run one backend exchange on the blocking pool and report a dead
/// worker as [`BackendError::Unconfirmed`]: after dispatch the
/// outcome is unknown, so success and refusal are both dishonest.
///
/// The drive's connection is cloned out of the map under a short
/// lock, then the exchange runs without holding the map: one
/// drive's poisoned connection or slow owner never blocks another
/// drive's commands.
async fn exchange<T, F, R>(
    app: AppHandle<R>,
    op: &'static str,
    drive_dir: String,
    call: F,
) -> Result<T, BackendError>
where
    F: FnOnce(&DesktopBackend) -> Result<T, BackendError> + Send + 'static,
    T: Send + 'static,
    R: Runtime,
{
    // Counted from dispatch so the quit drain waits for work the
    // runtime accepted but has not run yet. The guard owns its
    // count, so the app handle still moves into the blocking task.
    let _flight = app.state::<FlightTracker>().enter();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let backend = state.connection_for(&drive_dir)?;
        call(&backend)
    })
    .await
    .unwrap_or_else(|join| Err(worker_failed(op, join)))
}

/// A dead blocking worker means the exchange was dispatched but no
/// terminal answer was observed: [`BackendError::Unconfirmed`] by
/// definition, never success, never a refusal.
fn worker_failed(op: &'static str, join: impl std::fmt::Display) -> BackendError {
    BackendError::Unconfirmed {
        op: op.into(),
        detail: format!("ipc worker failed: {join}"),
    }
}

/// Run one local query on the blocking pool and count it in
/// flight. Unlike [`exchange`], the answer is a state struct, never
/// the owner taxonomy: these queries read local files and probe
/// sockets but forward no owner operation, so a dead worker is a
/// shell bug and surfaces as an explicit unreadable state inside the
/// payload — never success, never the taxonomy. Local queries hold
/// no owner connection and take no map lock: one slow registry
/// probe never wedges another drive's exchanges, bounded by the
/// probe timeout, and the count covers dispatch-queue time too.
async fn query<T, F, B, R>(app: AppHandle<R>, op: &'static str, build_unreadable: B, call: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    B: FnOnce(String) -> T + Send + 'static,
    T: Send + 'static,
    R: Runtime,
{
    let _flight = app.state::<FlightTracker>().enter();
    tauri::async_runtime::spawn_blocking(call)
        .await
        .unwrap_or_else(|join| build_unreadable(format!("ipc worker failed for {op}: {join}")))
}

/// Look up one path: the node plus the serving generation and
/// durable revision it was served from. `drive_dir` names the drive
/// explicitly — the backend keeps no implicit active drive.
#[tauri::command]
pub async fn lookup<R: Runtime>(
    app: AppHandle<R>,
    drive_dir: String,
    path: String,
) -> Result<(WireNode, u64, u64), BackendError> {
    exchange(app, "lookup", drive_dir, move |backend| {
        backend.lookup(&path)
    })
    .await
}

/// Stat one path: attributes plus serving generation and durable
/// revision.
#[tauri::command]
pub async fn stat<R: Runtime>(
    app: AppHandle<R>,
    drive_dir: String,
    path: String,
) -> Result<(WireAttr, u64, u64), BackendError> {
    exchange(app, "stat", drive_dir, move |backend| backend.stat(&path)).await
}

/// List one directory: entries plus serving generation and durable
/// revision.
#[tauri::command]
pub async fn readdir<R: Runtime>(
    app: AppHandle<R>,
    drive_dir: String,
    path: String,
) -> Result<(Vec<WireDirEntry>, u64, u64), BackendError> {
    exchange(app, "readdir", drive_dir, move |backend| {
        backend.readdir(&path)
    })
    .await
}

/// Read one whole file, plus serving generation and durable
/// revision.
#[tauri::command]
pub async fn read<R: Runtime>(
    app: AppHandle<R>,
    drive_dir: String,
    path: String,
) -> Result<(Vec<u8>, u64, u64), BackendError> {
    exchange(app, "read", drive_dir, move |backend| backend.read(&path)).await
}

/// The owner's durable observation: what a member may observe,
/// never liveness.
#[tauri::command]
pub async fn observe<R: Runtime>(
    app: AppHandle<R>,
    drive_dir: String,
) -> Result<WireSyncStatus, BackendError> {
    exchange(app, "observe", drive_dir, move |backend| backend.observe()).await
}

/// Transport health plus the last pass report, pinned to the pass
/// count. A separate object from the durable observation.
#[tauri::command]
pub async fn liveness<R: Runtime>(
    app: AppHandle<R>,
    drive_dir: String,
) -> Result<WireLiveness, BackendError> {
    exchange(app, "liveness", drive_dir, move |backend| {
        backend.liveness()
    })
    .await
}

/// The projection freshness judgment.
#[tauri::command]
pub async fn freshness<R: Runtime>(
    app: AppHandle<R>,
    drive_dir: String,
) -> Result<WireFreshness, BackendError> {
    exchange(app, "freshness", drive_dir, move |backend| {
        backend.freshness()
    })
    .await
}

/// Capture the parent-directory incarnation a create must send
/// against. `path` is the parent directory — `""` for the drive
/// root — not the file path itself.
#[tauri::command]
pub async fn capture_parent<R: Runtime>(
    app: AppHandle<R>,
    drive_dir: String,
    path: String,
) -> Result<WireParentToken, BackendError> {
    exchange(app, "capture_parent", drive_dir, move |backend| {
        backend.capture_parent(&path)
    })
    .await
}

/// Forward one mutation to the owner's single queue and block for
/// its terminal answer. The only mutation path.
#[tauri::command]
pub async fn submit<R: Runtime>(
    app: AppHandle<R>,
    drive_dir: String,
    kind: WireMutationKind,
) -> Result<WireMutationOutcome, BackendError> {
    exchange(app, "submit", drive_dir, move |backend| {
        backend.submit(kind)
    })
    .await
}

/// Backend-owned recovery: reconcile a path after an unconfirmed
/// outcome and receive the verdict. The frontend never reconciles
/// itself; absence is `Ok(None)`, never a refusal.
#[tauri::command]
pub async fn reconcile_lookup<R: Runtime>(
    app: AppHandle<R>,
    drive_dir: String,
    path: String,
) -> Result<Option<(WireNode, u64, u64)>, BackendError> {
    exchange(app, "reconcile_lookup", drive_dir, move |backend| {
        backend.reconcile_lookup(&path)
    })
    .await
}

/// Drop the connection for one drive and mint a new instance
/// against the same drive — that drive only. The frontend calls
/// this explicitly (the unavailable view's retry) after an
/// [`BackendError::Unconfirmed`] answer; nothing calls it
/// automatically, and recovery proves nothing about the prior
/// operation. Where no connection exists yet, this establishes one.
#[tauri::command]
pub async fn reconnect<R: Runtime>(
    app: AppHandle<R>,
    drive_dir: String,
) -> Result<(), BackendError> {
    let _flight = app.state::<FlightTracker>().enter();
    let drive = drive_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.reconnect_drive(&drive)
    })
    .await
    .unwrap_or_else(|join| Err(worker_failed("reconnect", join)))
}

/// Every registered drive plus per-drive reachability, verified
/// this session. A local query, not an owner operation: the answer
/// is a state struct (never the taxonomy), unreadable states ride
/// inside the payload, and any other rejection is a transport
/// failure. It takes no drive identity — it lists every drive — and
/// holds no owner connection: one slow probe never wedges another
/// drive's exchanges. The view carries no implicit active drive.
#[tauri::command]
pub async fn list_drives<R: Runtime>(app: AppHandle<R>) -> RegistryState {
    query(
        app,
        "list_drives",
        |reason| RegistryState {
            status: RegistryStatus::Unreadable { reason },
            drives: Vec::new(),
            skipped: Vec::new(),
        },
        registry::list_drives,
    )
    .await
}

/// One drive's configured bounds, as configured — never defaults,
/// never effective posture. A local query like [`list_drives`]:
/// unknown states ride inside the payload, and only an explicit
/// `Configured` carries values. `drive_dir` selects the section
/// without touching any owner connection. An invalid drive identity
/// answers the explicit unreadable state, never the taxonomy and
/// never a silent resolution against the process directory.
#[tauri::command]
pub async fn drive_settings<R: Runtime>(app: AppHandle<R>, drive_dir: String) -> DriveSettings {
    let dir = match super::validate_drive_path(&drive_dir, "drive_dir") {
        Ok(dir) => dir,
        Err(reason) => {
            return DriveSettings::Unreadable { reason };
        }
    };
    query(
        app,
        "drive_settings",
        |reason| DriveSettings::Unreadable { reason },
        move || settings_view::drive_settings(&dir),
    )
    .await
}

/// One completed settings edit plus the observed settings state
/// that decides the display. Infallible like the other local
/// queries: `result` carries the operation outcome (exit codes only
/// — CLI stderr travels as opaque display detail and is never parsed
/// for configured state), `observed` is the settings re-read taken
/// after the process completed, and only that observed section
/// decides what the display says.
///
/// The command shape is one drive identity plus one sparse update
/// (set-list with values, clear-list) — never a whole-document
/// replacement. Invalid identities and invalid updates refuse before
/// any process spawns; overlapping edits on one drive serialize
/// through the file lock (atomic commits, last writer wins per key),
/// so no per-drive busy state exists, unlike lifecycle operations.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SettingsWriteReport {
    /// Operation verdict: exit codes only, never configured state.
    pub result: SettingsApplyResult,
    /// The settings re-read taken after the run, deciding display.
    pub observed: DriveSettings,
}

/// Apply one sparse update to one drive's settings section through
/// the installed CLI, then re-read. The CLI runs the shared
/// transaction in-process, so the save works whether the drive's
/// owner runs or not; a saved edit applies at the next owner start
/// (restart-to-apply), never live.
#[tauri::command]
pub async fn drive_settings_write<R: Runtime>(
    app: AppHandle<R>,
    drive_dir: String,
    update: SparseUpdate,
) -> SettingsWriteReport {
    run_settings_write(app, "drive_settings_write", drive_dir, update).await
}

async fn run_settings_write<R>(
    app: AppHandle<R>,
    op: &'static str,
    drive_dir: String,
    update: SparseUpdate,
) -> SettingsWriteReport
where
    R: Runtime,
{
    let _flight = app.state::<FlightTracker>().enter();
    tauri::async_runtime::spawn_blocking(move || {
        // Validate the drive identity before anything spawns: a
        // non-canonical spelling must never key a second section
        // for one drive.
        let dir = match super::validate_drive_path(&drive_dir, "drive_dir") {
            Ok(dir) => dir,
            Err(reason) => {
                return SettingsWriteReport {
                    result: SettingsApplyResult::Refused { reason },
                    observed: DriveSettings::Unreadable {
                        reason: format!(
                            "refused before reading: invalid drive identity {}",
                            drive_dir
                        ),
                    },
                };
            }
        };
        let state = app.state::<super::AppState>();
        // No per-drive flight claim: lifecycle operations overlap
        // dangerously, but sparse edits serialize through the
        // settings file lock with atomic commits, so overlapping
        // saves converge per key instead of wedging.
        let outcome = match state.cli_bin() {
            Ok(bin) => settings_apply::run_settings_apply(
                &bin,
                &SettingsApplyRequest {
                    drive_dir: dir.clone(),
                    update,
                    cli_bin: None,
                    settings_path: None,
                },
                SETTINGS_APPLY_TIMEOUT,
            ),
            Err(reason) => settings_apply::SettingsApplyOutcome {
                result: SettingsApplyResult::Failed { reason },
                stderr: String::new(),
                code: None,
            },
        };
        // The paired re-read decides the display on every path: the
        // outcome carries the exit-code verdict, never configured
        // state.
        SettingsWriteReport {
            result: outcome.result,
            observed: settings_view::drive_settings(&dir),
        }
    })
    .await
    .unwrap_or_else(|join| SettingsWriteReport {
        result: SettingsApplyResult::Failed {
            reason: format!("ipc worker failed for {op}: {join}"),
        },
        // A dead worker is a shell bug, never a settings answer:
        // report the read as unreadable with the reason.
        observed: DriveSettings::Unreadable {
            reason: format!("ipc worker failed for {op}: {join}"),
        },
    })
}
/// One completed lifecycle operation plus the observed state that
/// decides the display. Infallible like the other local queries:
/// `result` carries the operation outcome (exit codes only — CLI
/// stderr travels as opaque display detail and is never parsed for
/// lifecycle facts), `observed` is the registry re-read plus
/// per-drive socket probe taken *after* the process completed, and
/// `membership` is the operated drive's membership tip re-observed
/// through the owner. Observed state determines the display; the
/// result indicates the operation outcome, never authoritative
/// lifecycle state.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProvisionReport {
    /// Operation outcome: exit codes only, never lifecycle state.
    pub result: super::provision::ProvisionResult,
    /// Registry re-read plus per-drive socket probe taken after the process completed.
    pub observed: RegistryState,
    /// The operated drive's membership as the owner reports it now:
    /// the tip epoch and member count from a terminal `observe`
    /// answer, or `None` where the owner did not answer. The leave
    /// contract's independent membership check — exit 0 alone never
    /// means "left", and CLI text never decides membership.
    pub membership: Option<MembershipObservation>,
}

/// The operated drive's membership tip as re-observed after the
/// operation: terminal owner answer only, never guessed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MembershipObservation {
    /// Membership tip epoch at re-observation, absent where the owner did not answer.
    pub epoch: Option<u64>,
    /// Member count in the terminal `observe` answer.
    pub known_members: usize,
}

/// Create one drive through the installed CLI (`bootstrap` without
/// an invitation file), then re-read and probe. All arguments are
/// owned strings; secret *paths* travel, never contents. `mode` is
/// `vault` or `mount` (`mount` needs `mountpoint`); any other value
/// refuses before any process spawns.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionCreateArgs {
    /// Registry entry name, 1–58 chars of `[a-z0-9-]`.
    pub name: String,
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
    /// Relays for the managed service; empty is refused before anything is created.
    pub relays: Vec<String>,
    /// Credential path, never contents.
    pub identity_file: String,
    /// Credential path, never contents.
    pub passphrase_file: String,
    /// Service mode display string: `vault` or `mount`.
    pub mode: String,
    /// FUSE mountpoint: required in `mount` mode, refused in `vault` mode.
    pub mountpoint: Option<String>,
    /// Register only: skip installing, enabling, and starting the service.
    pub no_service: bool,
}

/// Join one drive: create plus the sealed invitation file selected
/// out-of-band (file dialog) and travelling as a path.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionJoinArgs {
    /// Registry entry name, 1–58 chars of `[a-z0-9-]`.
    pub name: String,
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
    /// Relays for the managed service; empty is refused before anything is created.
    pub relays: Vec<String>,
    /// Credential path, never contents.
    pub identity_file: String,
    /// Credential path, never contents.
    pub passphrase_file: String,
    /// Sealed invitation file path, selected out-of-band.
    pub invitation_file: String,
    /// Service mode display string: `vault` or `mount`.
    pub mode: String,
    /// FUSE mountpoint: required in `mount` mode, refused in `vault` mode.
    pub mountpoint: Option<String>,
    /// Register only: skip installing, enabling, and starting the service.
    pub no_service: bool,
}

/// Leave one drive. `acknowledged` carries the sole-owner terminal
/// confirmation to the `--yes` boundary.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionLeaveArgs {
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
    /// Credential path, never contents.
    pub identity_file: String,
    /// Credential path, never contents.
    pub passphrase_file: String,
    /// Sole-owner terminal confirmation carried to the `--yes` boundary.
    pub acknowledged: bool,
}

/// Create one drive through the installed CLI (`bootstrap` without
/// an invitation file), then re-read and probe. All arguments are
/// owned strings; secret *paths* travel, never contents. `mode` is
/// `vault` or `mount` (`mount` needs `mountpoint`); any other value
/// refuses before any process spawns.
#[tauri::command]
pub async fn provision_create<R: Runtime>(
    app: AppHandle<R>,
    args: ProvisionCreateArgs,
) -> ProvisionReport {
    run_provision(
        app,
        "provision_create",
        args.drive_dir.clone(),
        true,
        move |state, dir| {
            let request = super::provision::ProvisionRequest {
                name: args.name,
                drive_dir: dir,
                relays: args.relays,
                identity_file: PathBuf::from(args.identity_file),
                passphrase_file: PathBuf::from(args.passphrase_file),
                invitation_file: None,
                registry_path: None,
                owner_bin: None,
                supervise_bin: None,
                no_service: args.no_service,
                mode: args.mode,
                mountpoint: args.mountpoint.map(PathBuf::from),
            };
            let cli = state.cli_bin();
            match cli {
                Ok(bin) => super::provision::run_bootstrap(
                    &bin,
                    &request,
                    super::provision::PROVISION_TIMEOUT,
                ),
                Err(reason) => super::provision::ProvisionOutcome {
                    result: super::provision::ProvisionResult::VersionMismatch { reason },
                    stderr: String::new(),
                    code: None,
                },
            }
        },
    )
    .await
}

/// Join one drive through the installed CLI (`bootstrap` with the
/// sealed invitation file at `invitation_file`), then re-read and
/// probe. The invitation file is selected out-of-band (file dialog)
/// and travels as a path — the adapter reads at most its bounded
/// bytes through the CLI, never its interpretation.
#[tauri::command]
pub async fn provision_join<R: Runtime>(
    app: AppHandle<R>,
    args: ProvisionJoinArgs,
) -> ProvisionReport {
    run_provision(
        app,
        "provision_join",
        args.drive_dir.clone(),
        true,
        move |state, dir| {
            let request = super::provision::ProvisionRequest {
                name: args.name,
                drive_dir: dir,
                relays: args.relays,
                identity_file: PathBuf::from(args.identity_file),
                passphrase_file: PathBuf::from(args.passphrase_file),
                invitation_file: Some(PathBuf::from(args.invitation_file)),
                registry_path: None,
                owner_bin: None,
                supervise_bin: None,
                no_service: args.no_service,
                mode: args.mode,
                mountpoint: args.mountpoint.map(PathBuf::from),
            };
            let cli = state.cli_bin();
            match cli {
                Ok(bin) => super::provision::run_bootstrap(
                    &bin,
                    &request,
                    super::provision::PROVISION_TIMEOUT,
                ),
                Err(reason) => super::provision::ProvisionOutcome {
                    result: super::provision::ProvisionResult::VersionMismatch { reason },
                    stderr: String::new(),
                    code: None,
                },
            }
        },
    )
    .await
}

/// Leave the drive through the installed CLI (`member leave`), then
/// re-observe membership and lifecycle. `acknowledged` carries the
/// sole-owner terminal confirmation to the `--yes` boundary — a
/// frontend omitting the dialog cannot bypass it. A non-owner leave
/// authors nothing (exit 0 refusal); the re-observation proves the
/// membership is unchanged.
#[tauri::command]
pub async fn provision_leave<R: Runtime>(
    app: AppHandle<R>,
    args: ProvisionLeaveArgs,
) -> ProvisionReport {
    run_provision(
        app,
        "provision_leave",
        args.drive_dir.clone(),
        false,
        move |state, dir| {
            let request = super::provision::LeaveRequest {
                drive_dir: dir,
                identity_file: PathBuf::from(args.identity_file),
                passphrase_file: PathBuf::from(args.passphrase_file),
                acknowledged: args.acknowledged,
            };
            let cli = state.cli_bin();
            match cli {
                Ok(bin) => {
                    super::provision::run_leave(&bin, &request, super::provision::PROVISION_TIMEOUT)
                }
                Err(reason) => super::provision::ProvisionOutcome {
                    result: super::provision::ProvisionResult::VersionMismatch { reason },
                    stderr: String::new(),
                    code: None,
                },
            }
        },
    )
    .await
}

/// Reconfigure one registered drive through the installed CLI
/// (`drive reconfigure`), then re-read and probe. All arguments are
/// owned strings; secret *paths* travel, never contents. `mode` is
/// `vault` or `mount` (`mount` needs `mountpoint`); any other value
/// refuses before any process spawns. `relays`
/// replaces the registered set when non-empty and keeps it when
/// empty, so a credential-path-only reconfigure never clears relays
/// by accident. Serialized against the per-drive flight like every
/// other lifecycle operation, with registry, socket, and membership
/// re-observation after the process completes.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionReconfigureArgs {
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
    /// Replacement relays when non-empty; empty keeps the registered set.
    pub relays: Vec<String>,
    /// Credential path, never contents.
    pub identity_file: String,
    /// Credential path, never contents.
    pub passphrase_file: String,
    /// Service mode display string: `vault` or `mount`.
    pub mode: String,
    /// FUSE mountpoint: required in `mount` mode, refused in `vault` mode.
    pub mountpoint: Option<String>,
    /// Rewrite the entry without installing any OS service.
    pub no_service: bool,
}

/// Reconfigure one registered drive through the installed CLI
/// (`drive reconfigure`), then re-read and probe.
#[tauri::command]
pub async fn provision_reconfigure<R: Runtime>(
    app: AppHandle<R>,
    args: ProvisionReconfigureArgs,
) -> ProvisionReport {
    run_provision(
        app,
        "provision_reconfigure",
        args.drive_dir.clone(),
        true,
        move |state, dir| {
            let request = super::provision::ReconfigureRequest {
                drive_dir: dir,
                relays: args.relays,
                identity_file: PathBuf::from(args.identity_file),
                passphrase_file: PathBuf::from(args.passphrase_file),
                registry_path: None,
                owner_bin: None,
                supervise_bin: None,
                no_service: args.no_service,
                mode: args.mode,
                mountpoint: args.mountpoint.map(PathBuf::from),
            };
            let cli = state.cli_bin();
            match cli {
                Ok(bin) => super::provision::run_reconfigure(
                    &bin,
                    &request,
                    super::provision::PROVISION_TIMEOUT,
                ),
                Err(reason) => super::provision::ProvisionOutcome {
                    result: super::provision::ProvisionResult::VersionMismatch { reason },
                    stderr: String::new(),
                    code: None,
                },
            }
        },
    )
    .await
}

/// Start one registered drive's installed service through the
/// installed CLI (`drive service-start`), then re-read and probe.
/// No credentials travel — starting opens no keystore. The recovery
/// for "installed but stopped": explicit, never automatic, and a
/// missing definition refuses with the reinstall pointer (the
/// reconfigure recovery below).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionServiceStartArgs {
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
}

/// Start one registered drive's installed service through the
/// installed CLI (`drive service-start`), then re-read and probe.
#[tauri::command]
pub async fn provision_service_start<R: Runtime>(
    app: AppHandle<R>,
    args: ProvisionServiceStartArgs,
) -> ProvisionReport {
    run_provision(
        app,
        "provision_service_start",
        args.drive_dir.clone(),
        true,
        move |state, dir| {
            let request = super::provision::ServiceStartRequest {
                drive_dir: dir,
                registry_path: None,
            };
            let cli = state.cli_bin();
            match cli {
                Ok(bin) => super::provision::run_service_start(
                    &bin,
                    &request,
                    super::provision::PROVISION_TIMEOUT,
                ),
                Err(reason) => super::provision::ProvisionOutcome {
                    result: super::provision::ProvisionResult::VersionMismatch { reason },
                    stderr: String::new(),
                    code: None,
                },
            }
        },
    )
    .await
}

/// Unlock arguments: the drive identity, the device identity file
/// path, and the passphrase text. The text travels backend -> CLI
/// on the stdin pipe (never argv); the frontend holds it only for
/// the unlock entry, never persisted there. `Debug` is redacted
/// so no log line carries the secret.
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveUnlockArgs {
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
    /// Credential path, never contents: the device identity file
    /// the start-up gate verified.
    pub identity_file: String,
    /// Passphrase text piped to the CLI on stdin, never argv.
    pub passphrase: String,
}

impl std::fmt::Debug for DriveUnlockArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriveUnlockArgs")
            .field("drive_dir", &self.drive_dir)
            .field("identity_file", &self.identity_file)
            .field("passphrase", &"REDACTED")
            .finish()
    }
}

/// Lock arguments: the drive identity only. Locking kills only
/// the app-held unlock child and scrubs the session secret —
/// never a reattached owner the app did not start.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveLockArgs {
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
}

/// Unlock one drive for the session: spawn the persistent owner
/// through the version-matched CLI with the passphrase on the
/// stdin pipe, verify the socket answers, then hold the child
/// under the session monitor and re-read. Infallible like the
/// other lifecycle commands: every path — including timeouts and
/// spawn failures — returns a report the paired observation
/// decides, never a throw.
#[tauri::command]
pub async fn drive_unlock<R: Runtime>(app: AppHandle<R>, args: DriveUnlockArgs) -> ProvisionReport {
    run_unlock(app, "drive_unlock", args.drive_dir.clone(), args).await
}

/// Lock one drive for the session: kill the app-held unlock child
/// and scrub the session secret, then re-read. Refuses where this
/// app holds no unlock session — a reattached owner the app did
/// not start is never killed.
#[tauri::command]
pub async fn drive_lock<R: Runtime>(app: AppHandle<R>, args: DriveLockArgs) -> ProvisionReport {
    run_lock(app, "drive_lock", args.drive_dir.clone()).await
}

/// Platform key defaults plus existence (no secret reads). A local
/// query: infallible, counted in flight. Pre-commit the UI treats the
/// paths as intended locations; post-commit the same paths with
/// existence set are the actual files. Trivial cost (two metadata
/// probes), so no blocking pool — the flight count still covers the
/// dispatch for the quit drain.
#[tauri::command]
pub async fn onboarding_defaults<R: Runtime>(app: AppHandle<R>) -> super::onboarding::KeyDefaults {
    let _flight = app.state::<super::FlightTracker>().enter();
    super::onboarding::default_key_paths()
}

/// One generated passphrase for write-down: writes nothing. Infallible
/// like the other local queries — the report carries the outcome plus
/// fresh existence, and a dead worker resolves `Unknown` inside the
/// payload, never a throw.
#[tauri::command]
pub async fn onboarding_new_passphrase<R: Runtime>(
    app: AppHandle<R>,
) -> super::onboarding::OnboardingReport {
    run_onboarding(app, "onboarding_new_passphrase", |state| {
        let cli = state.cli_bin();
        match cli {
            Ok(bin) => {
                super::onboarding::run_new_passphrase(&bin, super::onboarding::onboarding_timeout())
            }
            Err(reason) => super::onboarding::OnboardingReport {
                result: super::onboarding::OnboardingResult::VersionMismatch { reason },
                defaults: super::onboarding::default_key_paths(),
            },
        }
    })
    .await
}

/// Commit arguments: credential paths plus the passphrase text. The
/// text travels backend -> CLI on the stdin pipe (never argv); the
/// frontend holds it only from write-down to Finish. `Debug` is
/// redacted so no log line carries the secret.
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingCommitArgs {
    /// Credential path, never contents.
    pub identity_file: String,
    /// Credential path, never contents.
    pub passphrase_file: String,
    /// Passphrase text piped to the CLI on stdin, never argv.
    pub passphrase: String,
}

impl std::fmt::Debug for OnboardingCommitArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnboardingCommitArgs")
            .field("identity_file", &self.identity_file)
            .field("passphrase_file", &self.passphrase_file)
            .field("passphrase", &"REDACTED")
            .finish()
    }
}

/// Commit both credential files (generate + atomic write + read-back
/// signal via fresh existence). Infallible; dead workers resolve
/// `Unknown`.
#[tauri::command]
pub async fn onboarding_commit<R: Runtime>(
    app: AppHandle<R>,
    args: OnboardingCommitArgs,
) -> super::onboarding::OnboardingReport {
    run_onboarding(app, "onboarding_commit", move |state| {
        let cli = state.cli_bin();
        match cli {
            Ok(bin) => super::onboarding::run_commit(
                &bin,
                Path::new(&args.identity_file),
                Path::new(&args.passphrase_file),
                &args.passphrase,
                super::onboarding::onboarding_timeout(),
            ),
            Err(reason) => super::onboarding::OnboardingReport {
                result: super::onboarding::OnboardingResult::VersionMismatch { reason },
                defaults: super::onboarding::default_key_paths(),
            },
        }
    })
    .await
}

/// Verify arguments: credential paths only, never contents.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingVerifyArgs {
    /// Credential path, never contents.
    pub identity_file: String,
    /// Credential path, never contents.
    pub passphrase_file: String,
}

/// Verify both files through the hardened reader (exit codes only).
/// Infallible; the report's fresh existence is the read-back signal.
#[tauri::command]
pub async fn onboarding_verify<R: Runtime>(
    app: AppHandle<R>,
    args: OnboardingVerifyArgs,
) -> super::onboarding::OnboardingReport {
    run_onboarding(app, "onboarding_verify", move |state| {
        let cli = state.cli_bin();
        match cli {
            Ok(bin) => super::onboarding::run_verify(
                &bin,
                Path::new(&args.identity_file),
                Path::new(&args.passphrase_file),
                super::onboarding::onboarding_timeout(),
            ),
            Err(reason) => super::onboarding::OnboardingReport {
                result: super::onboarding::OnboardingResult::VersionMismatch { reason },
                defaults: super::onboarding::default_key_paths(),
            },
        }
    })
    .await
}

/// Adopt arguments: credential paths only, never contents. The
/// backend verifies the pair first, then records it as the device's
/// credential pair for this and later launches.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingAdoptArgs {
    /// Credential path, never contents.
    pub identity_file: String,
    /// Credential path, never contents.
    pub passphrase_file: String,
}

/// Verify the pair, then record it as adopted. Infallible; without
/// the record a restore or repair choice would be orphaned after the
/// session — the gate resolves the adopted pair, not just the
/// platform defaults.
#[tauri::command]
pub async fn onboarding_adopt<R: Runtime>(
    app: AppHandle<R>,
    args: OnboardingAdoptArgs,
) -> super::onboarding::OnboardingReport {
    run_onboarding(app, "onboarding_adopt", move |state| {
        let cli = state.cli_bin();
        match cli {
            Ok(bin) => super::onboarding::run_adopt(
                &bin,
                Path::new(&args.identity_file),
                Path::new(&args.passphrase_file),
                super::onboarding::onboarding_timeout(),
            ),
            Err(reason) => super::onboarding::OnboardingReport {
                result: super::onboarding::OnboardingResult::VersionMismatch { reason },
                defaults: super::onboarding::default_key_paths(),
            },
        }
    })
    .await
}

/// Reveal arguments: one path to show in the platform file manager.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevealArgs {
    /// One path to show in the platform file manager.
    pub path: String,
}

/// Best-effort reveal of one path in the platform file manager
/// (`open -R` on macOS, `xdg-open` on the directory elsewhere).
/// Returns `true` where the opener spawned; never throws — a missing
/// opener is not an onboarding failure.
#[tauri::command]
pub async fn reveal_in_folder<R: Runtime>(app: AppHandle<R>, args: RevealArgs) -> bool {
    let _flight = app.state::<super::FlightTracker>().enter();
    let path = args.path;
    tauri::async_runtime::spawn_blocking(move || reveal_path(&path))
        .await
        .unwrap_or(false)
}

fn reveal_path(path: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| true)
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let dir = std::path::Path::new(path)
            .parent()
            .map(|p| p.as_os_str().to_owned())
            .unwrap_or_else(|| std::ffi::OsString::from(path));
        std::process::Command::new("xdg-open")
            .arg(dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| true)
            .unwrap_or(false)
    }
}

/// Run one onboarding CLI exchange on the blocking pool and count it
/// in flight. Infallible: a dead worker resolves `Unknown` with fresh
/// existence, never success and never a throw.
async fn run_onboarding<R, F>(
    app: AppHandle<R>,
    op: &'static str,
    call: F,
) -> super::onboarding::OnboardingReport
where
    F: FnOnce(&super::AppState) -> super::onboarding::OnboardingReport + Send + 'static,
    R: Runtime,
{
    let _flight = app.state::<super::FlightTracker>().enter();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<super::AppState>();
        call(&state)
    })
    .await
    .unwrap_or_else(|join| super::onboarding::OnboardingReport {
        result: super::onboarding::OnboardingResult::Unknown {
            detail: format!("ipc worker failed for {op}: {join}"),
        },
        defaults: super::onboarding::default_key_paths(),
    })
}

/// Run one lifecycle operation serialized per drive: validate the
/// identity, claim the flight (refuse `Busy` when held), run the
/// blocking CLI exchange, release the flight, then re-read the
/// registry, probe every drive, and re-observe the operated drive's
/// membership — verification-via-reread on every path, including
/// timeouts and spawn failures. Everything runs on the blocking
/// pool (the registry probe fans out over sockets with a per-drive
/// bound and must never park an async runtime worker). A dead
/// blocking worker resolves `Unknown`, never success: the outcome
/// is unknown by definition, and only the observed state decides
/// the display. The flight releases through an RAII guard, so a
/// panicking exchange can never wedge the drive as busy — and the
/// CLI process itself is killed and reaped past the timeout before
/// the release, so no two operations can overlap.
async fn run_provision<R, F>(
    app: AppHandle<R>,
    op: &'static str,
    drive_dir: String,
    evict_after: bool,
    call: F,
) -> ProvisionReport
where
    F: FnOnce(&super::AppState, PathBuf) -> super::provision::ProvisionOutcome + Send + 'static,
    R: Runtime,
{
    let _flight = app.state::<FlightTracker>().enter();
    tauri::async_runtime::spawn_blocking(move || {
        // Validate the drive identity before touching the flight map:
        // a non-canonical spelling must never key a second flight
        // for one drive.
        let dir = match super::validate_drive_path(&drive_dir, "drive_dir") {
            Ok(dir) => dir,
            Err(reason) => {
                return ProvisionReport {
                    result: super::provision::ProvisionResult::ExpectedFailure { detail: reason },
                    observed: registry::list_drives(),
                    membership: None,
                };
            }
        };
        let state = app.state::<super::AppState>();
        if !state.claim_flight(&dir) {
            return ProvisionReport {
                result: super::provision::ProvisionResult::Busy { drive_dir },
                observed: registry::list_drives(),
                membership: observe_membership(&dir),
            };
        }
        let _release = FlightRelease::claim(&state, dir.clone());
        let outcome = call(&state, dir.clone());
        // Owner-restarting operations evict the cached connection:
        // the next exchange mints a fresh one instead of breaking
        // on the pre-restart stream (see `evict_drive`). Leave
        // authors membership without replacing the server, so it
        // keeps the connection it had.
        if evict_after {
            state.evict_drive(&dir);
        }
        // The flight releases only after the observation below: the
        // observation is part of the operation, so a second request
        // on the same drive waits for the full verified picture
        // instead of interleaving with it. (The process itself is
        // already gone — the adapter kills and reaps past the
        // timeout — so holding here serializes observation, never
        // process overlap.)
        let report = ProvisionReport {
            result: outcome.result,
            observed: registry::list_drives(),
            membership: observe_membership(&dir),
        };
        drop(_release);
        report
    })
    .await
    .unwrap_or_else(|join| ProvisionReport {
        result: super::provision::ProvisionResult::Unknown {
            detail: format!("ipc worker failed for {op}: {join}"),
        },
        // A dead worker is a shell bug, never an owner answer: report
        // the registry as unreadable with the reason instead of
        // probing sockets from the fallback path.
        observed: RegistryState {
            status: RegistryStatus::Unreadable {
                reason: format!("ipc worker failed for {op}: {join}"),
            },
            drives: Vec::new(),
            skipped: Vec::new(),
        },
        membership: None,
    })
}

/// Run one unlock exchange serialized per drive: validate the
/// identity, claim the flight (refuse `Busy` when held), spawn the
/// persistent owner, verify the socket answers within the unlock
/// bound, store the session and start its monitor, then re-read
/// the registry, probe every drive, and re-observe the operated
/// drive's membership — verification-via-reread on every path.
/// Everything runs on the blocking pool like `run_provision`. A
/// dead blocking worker resolves `Unknown`, never success: the
/// outcome is unknown by definition, and only the observed state
/// decides the display.
async fn run_unlock<R>(
    app: AppHandle<R>,
    op: &'static str,
    drive_dir: String,
    args: DriveUnlockArgs,
) -> ProvisionReport
where
    R: Runtime,
{
    let _flight = app.state::<FlightTracker>().enter();
    tauri::async_runtime::spawn_blocking(move || {
        // Validate the drive identity before touching the flight map:
        // a non-canonical spelling must never key a second flight
        // or a second session for one drive.
        let dir = match super::validate_drive_path(&drive_dir, "drive_dir") {
            Ok(dir) => dir,
            Err(reason) => {
                return ProvisionReport {
                    result: super::provision::ProvisionResult::ExpectedFailure { detail: reason },
                    observed: registry::list_drives(),
                    membership: None,
                };
            }
        };
        let state = app.state::<super::AppState>();
        if !state.claim_flight(&dir) {
            return ProvisionReport {
                result: super::provision::ProvisionResult::Busy { drive_dir },
                observed: registry::list_drives(),
                membership: observe_membership(&dir),
            };
        }
        let _release = FlightRelease::claim(&state, dir.clone());
        let outcome = unlock_exchange(&state, &app, &dir, &args);
        // The flight releases only after the observation below: the
        // observation is part of the operation, so a second request
        // on the same drive waits for the full verified picture
        // instead of interleaving with it. (The spawned owner is
        // held by the session monitor past this point, never
        // reaped here — holding here serializes observation, never
        // process overlap.)
        let report = ProvisionReport {
            result: outcome.result,
            observed: registry::list_drives(),
            membership: observe_membership(&dir),
        };
        drop(_release);
        report
    })
    .await
    .unwrap_or_else(|join| ProvisionReport {
        result: super::provision::ProvisionResult::Unknown {
            detail: format!("ipc worker failed for {op}: {join}"),
        },
        observed: RegistryState {
            status: RegistryStatus::Unreadable {
                reason: format!("ipc worker failed for {op}: {join}"),
            },
            drives: Vec::new(),
            skipped: Vec::new(),
        },
        membership: None,
    })
}

/// Run one lock exchange serialized per drive: validate the
/// identity, claim the flight (refuse `Busy` when an unlock
/// verify is in flight), take the app-held session, kill its
/// owner, scrub its secret, then re-read and re-observe. A dead
/// blocking worker resolves `Unknown`, never success.
async fn run_lock<R>(app: AppHandle<R>, op: &'static str, drive_dir: String) -> ProvisionReport
where
    R: Runtime,
{
    let _flight = app.state::<FlightTracker>().enter();
    tauri::async_runtime::spawn_blocking(move || {
        let dir = match super::validate_drive_path(&drive_dir, "drive_dir") {
            Ok(dir) => dir,
            Err(reason) => {
                return ProvisionReport {
                    result: super::provision::ProvisionResult::ExpectedFailure { detail: reason },
                    observed: registry::list_drives(),
                    membership: None,
                };
            }
        };
        let state = app.state::<super::AppState>();
        if !state.claim_flight(&dir) {
            return ProvisionReport {
                result: super::provision::ProvisionResult::Busy { drive_dir },
                observed: registry::list_drives(),
                membership: observe_membership(&dir),
            };
        }
        let _release = FlightRelease::claim(&state, dir.clone());
        // Take the session out of the map before touching the
        // child: the kill runs lock-free, so a monitor poll can
        // never respawn a drive being locked — it sees no entry
        // and exits.
        let taken = state.sessions().lock().unwrap_or_else(|poison| poison.into_inner()).remove(&dir);
        let Some(entry) = taken else {
            return ProvisionReport {
                result: super::provision::ProvisionResult::ExpectedFailure {
                    detail: "no unlock session held by this app for this drive: nothing to lock (a reattached owner the app did not start is never killed — stop it out-of-band)".to_string(),
                },
                observed: registry::list_drives(),
                membership: observe_membership(&dir),
            };
        };
        // Destructure to drop the secret (scrubbed) and kill the
        // child: best-effort, then reaped. An already-exited owner
        // still resolves completed — the session is gone either
        // way, and the observation says what serves now. Evict only
        // once the child is reaped: a probe landing between evict
        // and kill could otherwise cache a connection to the
        // still-alive old owner and poison on it later.
        let super::session::UnlockSession { mut child, .. } = entry;
        if let Some(mut child) = child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        state.evict_drive(&dir);
        let report = ProvisionReport {
            result: super::provision::ProvisionResult::Completed {
                detail: "drive locked for this session: the owner is stopped and the session secret scrubbed"
                    .to_string(),
            },
            observed: registry::list_drives(),
            membership: observe_membership(&dir),
        };
        drop(_release);
        report
    })
    .await
    .unwrap_or_else(|join| ProvisionReport {
        result: super::provision::ProvisionResult::Unknown {
            detail: format!("ipc worker failed for {op}: {join}"),
        },
        observed: RegistryState {
            status: RegistryStatus::Unreadable {
                reason: format!("ipc worker failed for {op}: {join}"),
            },
            drives: Vec::new(),
            skipped: Vec::new(),
        },
        membership: None,
    })
}

/// Spawn the unlock owner and verify it serves: an early exit maps
/// by code (78 is the terminal held-drive, anything else an
/// expected failure with the bounded CLI detail), a socket answer
/// within the bound completes, and expiry is unknown — the owner
/// may still come up under the session monitor, so the session is
/// stored on both the completed and the unknown paths, never on
/// the early-exit paths. Storing means moving the child and the
/// secret into the session map and starting its monitor; the
/// entry's generation lets a stale watcher exit instead of
/// respawning a locked drive.
fn unlock_exchange<R>(
    state: &super::AppState,
    app: &AppHandle<R>,
    dir: &Path,
    args: &DriveUnlockArgs,
) -> super::provision::ProvisionOutcome
where
    R: Runtime,
{
    use super::provision::{ProvisionResult, UNLOCK_VERIFY_TIMEOUT};
    use zeroize::Zeroizing;
    // Already unlocked in this session: refuse before any spawn —
    // retyping means lock-then-unlock, never a second owner on one
    // store (the CLI would refuse it with the held-drive lock
    // anyway, but failing fast names the repair).
    if state
        .sessions()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .contains_key(dir)
    {
        return super::provision::ProvisionOutcome {
            result: ProvisionResult::ExpectedFailure {
                detail: "already unlocked in this session: lock first to re-unlock with a different passphrase"
                    .to_string(),
            },
            stderr: String::new(),
            code: None,
        };
    }
    // Managed drives start through the manager, never beside it:
    // spawning an app-supervised owner over an installed definition
    // would create a second supervisor path. The store lock would
    // refuse a live manager anyway, but failing fast names the
    // repair — and a definition-less drive (the per-session
    // posture, or a removed definition) proceeds.
    if registry::managed_definition_exists(dir) {
        return super::provision::ProvisionOutcome {
            result: ProvisionResult::ExpectedFailure {
                detail: "this drive has a managed service definition: start it with `drive service-start` instead of spawning a second supervisor (or reconfigure with `--no-service` for the per-session posture)".to_string(),
            },
            stderr: String::new(),
            code: None,
        };
    }
    let secret = Zeroizing::new(args.passphrase.clone().into_bytes());
    let cli = match state.cli_bin() {
        Ok(bin) => bin,
        Err(reason) => {
            return super::provision::ProvisionOutcome {
                result: ProvisionResult::VersionMismatch { reason },
                stderr: String::new(),
                code: None,
            };
        }
    };
    let request = super::provision::UnlockRequest {
        drive_dir: dir.to_path_buf(),
        identity_file: PathBuf::from(&args.identity_file),
        // The registered relay set: managed starts bake it into the
        // service definition, but unlock spawns the owner directly —
        // without the registry relays a drive with no `default.toml`
        // refuses relay-less. Empty where the drive is unregistered
        // or the registry is unreadable, so the CLI falls back to
        // the drive config file exactly as before.
        relays: registry::registry_relays_for(dir),
        retained_quota: None,
    };
    let mut spawned = match super::provision::spawn_unlock(&cli, &request, &secret) {
        Ok(spawned) => spawned,
        Err(result) => {
            return super::provision::ProvisionOutcome {
                result,
                stderr: String::new(),
                code: None,
            };
        }
    };
    // The spawn replaced whatever served this socket before: forget
    // the cached connection so the first probe past this point mints
    // a fresh one instead of breaking on the pre-restart stream
    // (which would poison the client and force a manual reconnect).
    state.evict_drive(dir);
    let deadline = std::time::Instant::now() + UNLOCK_VERIFY_TIMEOUT;
    loop {
        match spawned.child.try_wait() {
            Ok(Some(_)) => {
                let (code, detail) = super::provision::collect_unlock_exit_detail(spawned);
                let result = match code {
                    Some(78) => ProvisionResult::HeldDrive { detail },
                    _ => ProvisionResult::ExpectedFailure { detail },
                };
                return super::provision::ProvisionOutcome {
                    result,
                    stderr: String::new(),
                    code,
                };
            }
            Ok(None) => {}
            Err(error) => {
                let _ = spawned.child.kill();
                let _ = spawned.child.wait();
                return super::provision::ProvisionOutcome {
                    result: ProvisionResult::Unknown {
                        detail: format!(
                            "cannot poll the unlock owner: {error}; outcome unknown — re-reading observed state"
                        ),
                    },
                    stderr: String::new(),
                    code: None,
                };
            }
        }
        if unlock_socket_answers(dir) {
            let detail =
                "owner answering this session (socket verified) — the drive is open".to_string();
            store_unlock_session(state, app, dir, secret, spawned, &cli, request);
            return super::provision::ProvisionOutcome {
                result: ProvisionResult::Completed { detail },
                stderr: String::new(),
                code: None,
            };
        }
        if std::time::Instant::now() >= deadline {
            store_unlock_session(state, app, dir, secret, spawned, &cli, request);
            return super::provision::ProvisionOutcome {
                result: ProvisionResult::Unknown {
                    detail: "the owner did not answer within 30s; it may still come up under the session monitor — re-reading observed state".to_string(),
                },
                stderr: String::new(),
                code: None,
            };
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Move the verified child and its secret into the session map and
/// start its monitor: the app now owns this drive's session. The
/// drain threads detach with the live owner (consume and discard
/// until the last pipe writer closes).
fn store_unlock_session<R>(
    state: &super::AppState,
    app: &AppHandle<R>,
    dir: &Path,
    secret: zeroize::Zeroizing<Vec<u8>>,
    spawned: super::provision::SpawnedUnlock,
    cli: &std::path::Path,
    request: super::provision::UnlockRequest,
) where
    R: Runtime,
{
    let super::provision::SpawnedUnlock {
        child,
        stdout_thread,
        stderr_thread,
    } = spawned;
    drop(stdout_thread);
    drop(stderr_thread);
    let generation = state.next_generation();
    state
        .sessions()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(
            dir.to_path_buf(),
            super::session::UnlockSession::new(
                secret,
                child,
                cli.to_path_buf(),
                request,
                generation,
            ),
        );
    super::session::watch_session(app.clone(), dir.to_path_buf(), generation);
}

/// Whether the drive's owner answers now: connect plus one
/// terminal `liveness` answer under a short bound. A terminal
/// refusal still proves the owner answers (transport exists), so
/// it reads ready — availability is transport-level, never a
/// judgment on the drive. Anything else is not-an-answer-yet.
fn unlock_socket_answers(drive_dir: &std::path::Path) -> bool {
    const UNLOCK_SOCKET_BOUND: std::time::Duration = std::time::Duration::from_secs(2);
    let client = match OwnerClient::connect_drive(drive_dir) {
        Ok(client) => client,
        Err(_) => return false,
    };
    let mut client = match client.with_timeout(UNLOCK_SOCKET_BOUND) {
        Ok(bounded) => bounded,
        Err(_) => return false,
    };
    matches!(
        client.liveness(),
        Ok(_) | Err(wyrd_owner_client::ClientError::Refused(_))
    )
}

/// Releases one drive's lifecycle flight when dropped: the flight
/// frees on every path through the exchange — normal return and
/// panic alike — so a failed exchange can never wedge the drive as
/// `Busy` until restart. Created only after a successful claim, and
/// dropped explicitly before the post-run observation (double release
/// is a harmless no-op).
struct FlightRelease<'a> {
    state: &'a super::AppState,
    drive: PathBuf,
}
impl<'a> FlightRelease<'a> {
    fn claim(state: &'a super::AppState, drive: PathBuf) -> Self {
        FlightRelease { state, drive }
    }
}

impl Drop for FlightRelease<'_> {
    fn drop(&mut self) {
        self.state.release_flight(&self.drive);
    }
}

/// Independently re-observe the operated drive's membership: a
/// terminal `observe` answer carries the tip epoch and member count;
/// no transport, no terminal answer, or any refusal yields `None` —
/// membership unknown, never guessed from CLI text.
///
/// Bounded like the registry probe (5 s): a wedged owner must cost
/// seconds on a finished operation, never the client's default
/// response wait. Connects directly with the probe timeout instead
/// of going through the connection map — the same shape as the
/// registry's per-drive probe.
fn observe_membership(drive_dir: &std::path::Path) -> Option<MembershipObservation> {
    const MEMBERSHIP_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    let client = OwnerClient::connect_drive(drive_dir).ok()?;
    let mut client = client.with_timeout(MEMBERSHIP_PROBE_TIMEOUT).ok()?;
    let status = client.observe().ok()?;
    Some(MembershipObservation {
        epoch: status.tip.as_ref().map(|tip| tip.epoch),
        known_members: status.known_members,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wyrd_owner_client::{
        WireError, WireErrorCode, WireFileIdentity, WireFreshnessState, WireKind,
    };

    /// Every result shape the commands return, serialized once: the
    /// frontend mirrors in `apps/desktop/src/lib/ipc/results.ts`
    /// must read exactly this. Tuples become arrays, enums stay
    /// externally tagged, `Option` becomes `null` — any wrapper,
    /// rename, or flattening here breaks the mirror, and this test
    /// is where that breakage shows. The JSON below is copied into
    /// `results.test.ts` verbatim.
    #[test]
    fn result_shapes_serialize_verbatim() {
        let node = WireNode::File {
            size: 11,
            executable: false,
            chunks: vec!["9be2".to_string()],
            identity: Some("ab".repeat(32)),
        };
        let served = serde_json::to_value((&node, 3u64, 7u64)).unwrap();
        assert_eq!(
            served,
            serde_json::json!([{"File": {"size": 11, "executable": false, "chunks": ["9be2"], "identity": "ab".repeat(32)}}, 3, 7])
        );

        let attr = WireAttr {
            kind: WireKind::File,
            size: 11,
            executable: false,
            identity: None,
        };
        assert_eq!(
            serde_json::to_value((&attr, 3u64, 7u64)).unwrap(),
            serde_json::json!([{"kind": "File", "size": 11, "executable": false, "identity": null}, 3, 7])
        );

        let entries = vec![WireDirEntry {
            name: "notes.txt".to_string(),
            node: node.clone(),
        }];
        assert_eq!(
            serde_json::to_value((&entries, 3u64, 7u64)).unwrap()[0],
            serde_json::json!([{"name": "notes.txt", "node": {"File": {"size": 11, "executable": false, "chunks": ["9be2"], "identity": "ab".repeat(32)}}}])
        );

        let bytes = vec![104u8, 105u8];
        assert_eq!(
            serde_json::to_value((&bytes, 3u64, 7u64)).unwrap(),
            serde_json::json!([[104, 105], 3, 7])
        );

        let freshness = WireFreshness {
            state: WireFreshnessState::RetainedLastGood {
                detail: "relay quiet".to_string(),
            },
            revision: 9,
            generation: 4,
        };
        assert_eq!(
            serde_json::to_value(&freshness).unwrap(),
            serde_json::json!({"state": {"RetainedLastGood": {"detail": "relay quiet"}}, "revision": 9, "generation": 4})
        );

        let token = WireParentToken {
            instance: 2,
            counter: 5,
        };
        assert_eq!(
            serde_json::to_value(token).unwrap(),
            serde_json::json!({"instance": 2, "counter": 5})
        );

        // `SetAttrs` pins `Option` handling: absent fields are
        // `null`, never dropped keys. The identity precondition is
        // required since v2: absent fails parse, never a silent
        // path-only mutation.
        let kind = WireMutationKind::SetAttrs {
            path: "notes.txt".to_string(),
            size: None,
            executable: Some(true),
            base: None,
            expected_identity: "ab".repeat(32),
        };
        assert_eq!(
            serde_json::to_value(&kind).unwrap(),
            serde_json::json!({"SetAttrs": {"path": "notes.txt", "size": null, "executable": true, "base": null, "expected_identity": "abababababababababababababababababababababababababababababababab"}})
        );

        let outcome = WireMutationOutcome::Created(WireFileIdentity {
            size: 11,
            executable: false,
            chunks: vec!["9be2".to_string()],
        });
        assert_eq!(
            serde_json::to_value(&outcome).unwrap(),
            serde_json::json!({"Created": {"size": 11, "executable": false, "chunks": ["9be2"]}})
        );
        assert_eq!(
            serde_json::to_value(WireMutationOutcome::Done).unwrap(),
            serde_json::json!("Done")
        );

        // Absence is `null` data, never a refusal shape.
        let absent: Option<(WireNode, u64, u64)> = None;
        assert_eq!(
            serde_json::to_value(&absent).unwrap(),
            serde_json::Value::Null
        );
        let present: Option<(WireNode, u64, u64)> = Some((node, 3, 7));
        assert!(serde_json::to_value(&present).unwrap().is_array());

        // The unit answer serializes as `null`.
        assert_eq!(serde_json::to_value(()).unwrap(), serde_json::Value::Null);

        // A refusal-shaped error keeps its code even here, so the
        // mirror's `WireMutationError` stays honest.
        let refusal = WireError {
            code: WireErrorCode::NotFound,
            message: "no such path".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&refusal).unwrap(),
            serde_json::json!({"code": "NotFound", "message": "no such path"})
        );
    }

    /// Every local-query shape the commands return, serialized once:
    /// the frontend mirrors in `apps/desktop/src/lib/ipc/results.ts`
    /// must read exactly this. Internally tagged enums carry their
    /// tag alongside flattened content — any wrapper, rename, or
    /// flattening here breaks the mirror, and this test is where
    /// that breakage shows. The JSON below is copied into
    /// `results.test.ts` verbatim.
    #[test]
    fn local_query_shapes_serialize_verbatim() {
        use super::super::service::ServiceState;
        use super::super::{DriveAvailability, DriveSettings, DriveSettingsView};
        use super::super::{DriveView, RegistryState, RegistryStatus};

        assert_eq!(
            serde_json::to_value(DriveAvailability::Available).unwrap(),
            serde_json::json!({"state": "available"})
        );
        assert_eq!(
            serde_json::to_value(DriveAvailability::Unavailable {
                detail: "no owner".to_string()
            })
            .unwrap(),
            serde_json::json!({"state": "unavailable", "detail": "no owner"})
        );
        assert_eq!(
            serde_json::to_value(DriveAvailability::Unknown {
                detail: "no terminal answer".to_string()
            })
            .unwrap(),
            serde_json::json!({"state": "unknown", "detail": "no terminal answer"})
        );
        // The service-state taxonomy beside availability: the frontend
        // tells every branching state apart from the JSON alone, and
        // the recovery buttons branch on it. Every state the
        // unavailable screen branches on is pinned here.
        assert_eq!(
            serde_json::to_value(ServiceState::Stopped {
                detail: "installed but stopped".to_string()
            })
            .unwrap(),
            serde_json::json!({"state": "stopped", "detail": "installed but stopped"})
        );
        assert_eq!(
            serde_json::to_value(ServiceState::Wedged {
                detail: "running but socket-absent".to_string()
            })
            .unwrap(),
            serde_json::json!({"state": "wedged", "detail": "running but socket-absent"})
        );
        assert_eq!(
            serde_json::to_value(ServiceState::Unknown {
                detail: "the manager did not answer".to_string()
            })
            .unwrap(),
            serde_json::json!({"state": "unknown", "detail": "the manager did not answer"})
        );
        assert_eq!(
            serde_json::to_value(ServiceState::NotInstalled {
                detail: "no definition".to_string()
            })
            .unwrap(),
            serde_json::json!({"state": "notinstalled", "detail": "no definition"})
        );
        assert_eq!(
            serde_json::to_value(ServiceState::Unsupported {
                detail: "no managed service in this build".to_string()
            })
            .unwrap(),
            serde_json::json!({"state": "unsupported", "detail": "no managed service in this build"})
        );

        let drive = DriveView {
            name: "alpha".to_string(),
            drive_dir: "/drives/alpha".to_string(),
            relays: vec!["wss://relay.example".to_string()],
            service: "drive-alpha".to_string(),
            mode: "vault".to_string(),
            mountpoint: None,
            availability: DriveAvailability::Available,
            service_state: ServiceState::Running {
                detail: "the service manager reports the owner running".to_string(),
            },
            session: Some(super::super::SessionState::Unlocked {
                detail: "the owner answered this session — the drive is open".to_string(),
            }),
        };
        assert_eq!(
            serde_json::to_value(&drive).unwrap(),
            serde_json::json!({
                "name": "alpha",
                "drive_dir": "/drives/alpha",
                "relays": ["wss://relay.example"],
                "service": "drive-alpha",
                "mode": "vault",
                "mountpoint": null,
                "availability": {"state": "available"},
                "service_state": {"state": "running", "detail": "the service manager reports the owner running"},
                "session": {"state": "unlocked", "detail": "the owner answered this session — the drive is open"},
            })
        );

        let ready = RegistryState {
            status: RegistryStatus::Ready,
            drives: vec![drive],
            skipped: vec![
                "entry \"bad\": key \"bad\" does not match entry name \"other\"".to_string(),
            ],
        };
        assert_eq!(
            serde_json::to_value(&ready).unwrap(),
            serde_json::json!({
                "status": "ready",
                "drives": [{
                    "name": "alpha",
                    "drive_dir": "/drives/alpha",
                    "relays": ["wss://relay.example"],
                    "service": "drive-alpha",
                    "mode": "vault",
                    "mountpoint": null,
                    "availability": {"state": "available"},
                    "service_state": {"state": "running", "detail": "the service manager reports the owner running"},
                    "session": {"state": "unlocked", "detail": "the owner answered this session — the drive is open"},
                }],
                "skipped": ["entry \"bad\": key \"bad\" does not match entry name \"other\""],
            })
        );
        assert_eq!(
            serde_json::to_value(RegistryState {
                status: RegistryStatus::Nofile,
                drives: Vec::new(),
                skipped: Vec::new(),
            })
            .unwrap(),
            serde_json::json!({"status": "nofile", "drives": [], "skipped": []})
        );
        assert_eq!(
            serde_json::to_value(RegistryStatus::Unreadable {
                reason: "registry /x/drives.json is version 2, this shell reads 1".to_string()
            })
            .unwrap(),
            serde_json::json!({"status": "unreadable", "reason": "registry /x/drives.json is version 2, this shell reads 1"})
        );

        assert_eq!(
            serde_json::to_value(DriveSettings::Configured(DriveSettingsView {
                retained_bytes_quota: Some(0),
                max_admit_per_pass: Some(50),
                fetch_pass_budget_secs: None,
                max_mutation_wait_secs: None,
                open_timeout_secs: Some(45),
            }))
            .unwrap(),
            serde_json::json!({"state": "configured", "retained_bytes_quota": 0, "max_admit_per_pass": 50, "fetch_pass_budget_secs": null, "max_mutation_wait_secs": null, "open_timeout_secs": 45})
        );
        assert_eq!(
            serde_json::to_value(DriveSettings::Nosection {
                path: "/x/settings.json".to_string(),
            })
            .unwrap(),
            serde_json::json!({"state": "nosection", "path": "/x/settings.json"})
        );
        assert_eq!(
            serde_json::to_value(DriveSettings::Nofile {
                path: "/x/settings.json".to_string(),
            })
            .unwrap(),
            serde_json::json!({"state": "nofile", "path": "/x/settings.json"})
        );
        assert_eq!(
            serde_json::to_value(DriveSettings::Unreadable {
                reason: "settings /x/settings.json is version 2, this shell reads 1".to_string()
            })
            .unwrap(),
            serde_json::json!({"state": "unreadable", "reason": "settings /x/settings.json is version 2, this shell reads 1"})
        );
    }

    /// The session gate serializes verbatim: locked and unlocked
    /// stay tagged and distinct, and a managed drive (definition
    /// installed) carries an explicit `null` — never an absent
    /// key the frontend could misread. The JSON below is copied
    /// into `results.test.ts` verbatim.
    #[test]
    fn session_state_serializes_verbatim() {
        use super::super::SessionState;
        assert_eq!(
            serde_json::to_value(SessionState::Unlocked {
                detail: "the owner answered this session — the drive is open".to_string(),
            })
            .unwrap(),
            serde_json::json!({"state": "unlocked", "detail": "the owner answered this session — the drive is open"})
        );
        assert_eq!(
            serde_json::to_value(SessionState::Locked {
                detail: "no owner serves and no service definition exists: unlock the drive for this session with its passphrase, or reinstall the managed definition".to_string(),
            })
            .unwrap(),
            serde_json::json!({"state": "locked", "detail": "no owner serves and no service definition exists: unlock the drive for this session with its passphrase, or reinstall the managed definition"})
        );
        let managed: Option<SessionState> = None;
        assert_eq!(
            serde_json::to_value(&managed).unwrap(),
            serde_json::Value::Null
        );
    }

    /// Unlock args never leak the passphrase through `Debug`: the
    /// redacted marker shows, the bytes do not.
    #[test]
    fn unlock_args_debug_redacts_the_passphrase() {
        let args = DriveUnlockArgs {
            drive_dir: "/drives/alpha".to_string(),
            identity_file: "/keys/identity".to_string(),
            passphrase: "hunter2-hunter2".to_string(),
        };
        let rendered = format!("{args:?}");
        assert!(rendered.contains("REDACTED"), "rendered: {rendered}");
        assert!(!rendered.contains("hunter2"), "rendered: {rendered}");
        assert!(
            rendered.contains("/drives/alpha") && rendered.contains("/keys/identity"),
            "identities stay visible: {rendered}"
        );
    }

    /// The settings-write report serializes verbatim for the shell:
    /// the tagged result plus the observed read state, so the
    /// frontend mirrors this shape without invention.
    #[test]
    fn settings_write_report_serializes_verbatim() {
        use super::super::{DriveSettings, DriveSettingsView, SettingsApplyResult};
        use super::SettingsWriteReport;

        let applied = SettingsWriteReport {
            result: SettingsApplyResult::Applied {
                detail: "settings applied".to_string(),
            },
            observed: DriveSettings::Configured(DriveSettingsView {
                retained_bytes_quota: None,
                max_admit_per_pass: Some(64),
                fetch_pass_budget_secs: None,
                max_mutation_wait_secs: None,
                open_timeout_secs: Some(30),
            }),
        };
        assert_eq!(
            serde_json::to_value(&applied).unwrap(),
            serde_json::json!({
                "result": {"result": "applied", "detail": "settings applied"},
                "observed": {"state": "configured", "retained_bytes_quota": null, "max_admit_per_pass": 64, "fetch_pass_budget_secs": null, "max_mutation_wait_secs": null, "open_timeout_secs": 30},
            })
        );
        for (result, name) in [
            (
                SettingsApplyResult::Refused {
                    reason: "unknown settings key".to_string(),
                },
                "refused",
            ),
            (
                SettingsApplyResult::Conflict {
                    reason: "changed under".to_string(),
                },
                "conflict",
            ),
            (
                SettingsApplyResult::Unverified {
                    reason: "verification read failed".to_string(),
                },
                "unverified",
            ),
            (
                SettingsApplyResult::Failed {
                    reason: "no cli".to_string(),
                },
                "failed",
            ),
        ] {
            let report = SettingsWriteReport {
                result,
                observed: DriveSettings::Nosection {
                    path: "/x/settings.json".to_string(),
                },
            };
            assert_eq!(
                serde_json::to_value(&report).unwrap()["result"]["result"],
                serde_json::json!(name),
                "the result tag names the outcome"
            );
        }
    }

    /// A dead blocking worker is unconfirmed by definition: the
    /// exchange was dispatched, no terminal answer was observed, so
    /// success and refusal are both dishonest. The `JoinError` here
    /// is real — a blocking task that actually panicked — not a
    /// fabricated error value.
    #[test]
    fn dead_worker_is_unconfirmed() {
        let join = tauri::async_runtime::block_on(async {
            tauri::async_runtime::spawn_blocking(|| panic!("worker died"))
                .await
                .unwrap_err()
        });
        match worker_failed("lookup", join) {
            BackendError::Unconfirmed { op, detail } => {
                assert_eq!(op, "lookup");
                assert!(detail.contains("ipc worker failed"), "detail: {detail}");
            }
            other => panic!("a dead worker is unconfirmed: {other:?}"),
        }
    }

    /// The provisioning report serializes verbatim: the frontend
    /// mirror in `apps/desktop/src/lib/ipc/results.ts` must read
    /// exactly this. Internally tagged result kinds stay lowercase
    /// (matching the `BackendError` kind vocabulary); the observed
    /// registry nests under `observed` and the membership tip under
    /// `membership`. The JSON below is copied into `results.test.ts`
    /// verbatim.
    #[test]
    fn provision_report_serializes_verbatim() {
        use super::super::provision::ProvisionResult;
        use super::super::{RegistryState, RegistryStatus};
        let report = super::ProvisionReport {
            result: ProvisionResult::Completed {
                detail: "drive alpha verified available (socket /drives/alpha/owner.sock)"
                    .to_string(),
            },
            observed: RegistryState {
                status: RegistryStatus::Nofile,
                drives: Vec::new(),
                skipped: Vec::new(),
            },
            membership: Some(super::MembershipObservation {
                epoch: Some(2),
                known_members: 1,
            }),
        };
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            serde_json::json!({
                "result": {"result": "completed", "detail": "drive alpha verified available (socket /drives/alpha/owner.sock)"},
                "observed": {"status": "nofile", "drives": [], "skipped": []},
                "membership": {"epoch": 2, "known_members": 1},
            })
        );
        let busy = super::ProvisionReport {
            result: ProvisionResult::Busy {
                drive_dir: "/drives/alpha".to_string(),
            },
            observed: RegistryState {
                status: RegistryStatus::Nofile,
                drives: Vec::new(),
                skipped: Vec::new(),
            },
            membership: None,
        };
        assert_eq!(
            serde_json::to_value(&busy).unwrap(),
            serde_json::json!({
                "result": {"result": "busy", "drive_dir": "/drives/alpha"},
                "observed": {"status": "nofile", "drives": [], "skipped": []},
                "membership": null,
            })
        );
        // Args arrive camelCase on the wire (Tauri maps them) and the
        // struct renames accordingly: a frontend-shaped payload must
        // deserialize.
        let args: super::ProvisionCreateArgs = serde_json::from_value(serde_json::json!({
            "name": "alpha",
            "driveDir": "/drives/alpha",
            "relays": ["wss://relay.example"],
            "identityFile": "/keys/identity",
            "passphraseFile": "/keys/passphrase",
            "mode": "vault",
            "noService": true,
        }))
        .expect("camelCase args deserialize");
        assert_eq!(args.drive_dir, "/drives/alpha");
        assert!(args.no_service);
        // Onboarding args arrive the same way: credential paths
        // camelCase, the commit passphrase as a plain field (it
        // rides the stdin pipe from here, never argv).
        let commit: super::OnboardingCommitArgs = serde_json::from_value(serde_json::json!({
            "identityFile": "/keys/identity",
            "passphraseFile": "/keys/passphrase",
            "passphrase": "abcd6e-789abc-d6e789-abcd6e",
        }))
        .expect("camelCase commit args deserialize");
        assert_eq!(commit.identity_file, "/keys/identity");
        assert_eq!(commit.passphrase, "abcd6e-789abc-d6e789-abcd6e");
        let adopt: super::OnboardingAdoptArgs = serde_json::from_value(serde_json::json!({
            "identityFile": "/keys/identity",
            "passphraseFile": "/keys/passphrase",
        }))
        .expect("camelCase adopt args deserialize");
        assert_eq!(adopt.passphrase_file, "/keys/passphrase");
    }
}
