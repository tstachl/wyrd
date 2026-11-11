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

use std::path::PathBuf;
use tauri::{AppHandle, Manager, Runtime};
use wyrd_owner_client::{
    WireAttr, WireDirEntry, WireFreshness, WireLiveness, WireMutationKind, WireMutationOutcome,
    WireNode, WireParentToken, WireSyncStatus,
};

use super::registry::{self, RegistryState, RegistryStatus};
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
    pub result: super::provision::ProvisionResult,
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
    pub epoch: Option<u64>,
    pub known_members: usize,
}

/// Create one drive through the installed CLI (`bootstrap` without
/// an invitation file), then re-read and probe. All arguments are
/// owned strings; secret *paths* travel, never contents. `mode` is
/// the platform default `vault` — any other value refuses with the
/// installer-follow-up pointer before any process spawns.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionCreateArgs {
    pub name: String,
    pub drive_dir: String,
    pub relays: Vec<String>,
    pub identity_file: String,
    pub passphrase_file: String,
    pub mode: String,
    pub no_service: bool,
}

/// Join one drive: create plus the sealed invitation file selected
/// out-of-band (file dialog) and travelling as a path.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionJoinArgs {
    pub name: String,
    pub drive_dir: String,
    pub relays: Vec<String>,
    pub identity_file: String,
    pub passphrase_file: String,
    pub invitation_file: String,
    pub mode: String,
    pub no_service: bool,
}

/// Leave one drive. `acknowledged` carries the sole-owner terminal
/// confirmation to the `--yes` boundary.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionLeaveArgs {
    pub drive_dir: String,
    pub identity_file: String,
    pub passphrase_file: String,
    pub acknowledged: bool,
}

/// Create one drive through the installed CLI (`bootstrap` without
/// an invitation file), then re-read and probe. All arguments are
/// owned strings; secret *paths* travel, never contents. `mode` is
/// the platform default `vault` — any other value refuses with the
/// installer-follow-up pointer before any process spawns.
#[tauri::command]
pub async fn provision_create<R: Runtime>(
    app: AppHandle<R>,
    args: ProvisionCreateArgs,
) -> ProvisionReport {
    run_provision(
        app,
        "provision_create",
        args.drive_dir.clone(),
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
                membership: observe_membership(&state, &dir),
            };
        }
        let _release = FlightRelease::claim(&state, dir.clone());
        let outcome = call(&state, dir.clone());
        // Release before observing: the process is gone (the adapter
        // kills and reaps past the timeout), so the next operation
        // can never overlap this one — but the observation still
        // happens before any report is returned.
        drop(_release);
        ProvisionReport {
            result: outcome.result,
            observed: registry::list_drives(),
            membership: observe_membership(&state, &dir),
        }
    })
    .await
    .unwrap_or_else(|join| ProvisionReport {
        result: super::provision::ProvisionResult::Unknown {
            detail: format!("ipc worker failed for {op}: {join}"),
        },
        observed: registry::list_drives(),
        membership: None,
    })
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
fn observe_membership(
    state: &super::AppState,
    drive_dir: &std::path::Path,
) -> Option<MembershipObservation> {
    let backend = state.connection_for(&drive_dir.to_string_lossy()).ok()?;
    let status = backend.observe().ok()?;
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
        };
        let served = serde_json::to_value((&node, 3u64, 7u64)).unwrap();
        assert_eq!(
            served,
            serde_json::json!([{"File": {"size": 11, "executable": false, "chunks": ["9be2"]}}, 3, 7])
        );

        let attr = WireAttr {
            kind: WireKind::File,
            size: 11,
            executable: false,
        };
        assert_eq!(
            serde_json::to_value((&attr, 3u64, 7u64)).unwrap(),
            serde_json::json!([{"kind": "File", "size": 11, "executable": false}, 3, 7])
        );

        let entries = vec![WireDirEntry {
            name: "notes.txt".to_string(),
            node: node.clone(),
        }];
        assert_eq!(
            serde_json::to_value((&entries, 3u64, 7u64)).unwrap()[0],
            serde_json::json!([{"name": "notes.txt", "node": {"File": {"size": 11, "executable": false, "chunks": ["9be2"]}}}])
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
        // `null`, never dropped keys.
        let kind = WireMutationKind::SetAttrs {
            path: "notes.txt".to_string(),
            size: None,
            executable: Some(true),
            base: None,
        };
        assert_eq!(
            serde_json::to_value(&kind).unwrap(),
            serde_json::json!({"SetAttrs": {"path": "notes.txt", "size": null, "executable": true, "base": null}})
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

        let drive = DriveView {
            name: "alpha".to_string(),
            drive_dir: "/drives/alpha".to_string(),
            relays: vec!["wss://relay.example".to_string()],
            service: "drive-alpha".to_string(),
            mode: "vault".to_string(),
            availability: DriveAvailability::Available,
        };
        assert_eq!(
            serde_json::to_value(&drive).unwrap(),
            serde_json::json!({
                "name": "alpha",
                "drive_dir": "/drives/alpha",
                "relays": ["wss://relay.example"],
                "service": "drive-alpha",
                "mode": "vault",
                "availability": {"state": "available"},
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
                    "availability": {"state": "available"},
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
            serde_json::to_value(DriveSettings::Nosection).unwrap(),
            serde_json::json!({"state": "nosection"})
        );
        assert_eq!(
            serde_json::to_value(DriveSettings::Nofile).unwrap(),
            serde_json::json!({"state": "nofile"})
        );
        assert_eq!(
            serde_json::to_value(DriveSettings::Unreadable {
                reason: "settings /x/settings.json is version 2, this shell reads 1".to_string()
            })
            .unwrap(),
            serde_json::json!({"state": "unreadable", "reason": "settings /x/settings.json is version 2, this shell reads 1"})
        );
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
    }
}
