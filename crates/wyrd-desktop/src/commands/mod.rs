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
//! - Off-runtime exchanges. [`wyrd_owner_client::OwnerClient`] is synchronous with a
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
use wyrd_settings::SparseUpdate;

use crate::credential_ref::CredentialReference;
use crate::registry::{self, RegistryState, RegistryStatus};
use crate::settings_apply::{
    self, SettingsApplyRequest, SettingsApplyResult, SETTINGS_APPLY_TIMEOUT,
};
use crate::settings_view::{self, DriveSettings};
use crate::{AppState, BackendError, DesktopBackend, FlightTracker};

mod delete;
mod membership;
mod onboarding;
mod provision;
mod unlock;

pub use delete::{provision_delete, ProvisionDeleteArgs};
pub use membership::{provision_leave, MembershipObservation, ProvisionLeaveArgs};
pub use onboarding::{
    onboarding_adopt, onboarding_commit, onboarding_defaults, onboarding_new_passphrase,
    onboarding_verify, reveal_in_folder, OnboardingAdoptArgs, OnboardingCommitArgs,
    OnboardingVerifyArgs, RevealArgs,
};
#[cfg(test)]
pub(crate) use provision::credential_source;
pub use provision::{
    provision_create, provision_join, provision_reconfigure, provision_service_disable,
    provision_service_enable, provision_service_start, ProvisionCreateArgs, ProvisionJoinArgs,
    ProvisionReconfigureArgs, ProvisionServiceDisableArgs, ProvisionServiceEnableArgs,
    ProvisionServiceStartArgs,
};
pub use unlock::{drive_lock, drive_unlock, DriveLockArgs, DriveUnlockArgs};

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

/// One drive's credential backend as observed from its deployment
/// config through the installed CLI: a reference — backend kind,
/// store account, or credential paths — never secret contents. A
/// read-only query: no keystore opens, no owner connection is
/// claimed, and every failure is an explicit `Unknown` with a
/// reason so the shell keeps its operator choice. The CLI is the
/// one parser of `default.toml`; this command invents no shape.
#[tauri::command]
pub async fn drive_credential_reference<R: Runtime>(
    app: AppHandle<R>,
    drive_dir: String,
) -> CredentialReference {
    let dir = match super::validate_drive_path(&drive_dir, "drive_dir") {
        Ok(dir) => dir,
        Err(reason) => return CredentialReference::Unknown { reason },
    };
    let _flight = app.state::<FlightTracker>().enter();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<crate::AppState>();
        let cli_bin = match state.cli_bin() {
            Ok(bin) => bin,
            Err(reason) => return CredentialReference::Unknown { reason },
        };
        crate::credential_ref::run_credential_reference(
            &cli_bin,
            &dir,
            crate::credential_ref::CREDENTIAL_REF_TIMEOUT,
        )
    })
    .await
    .unwrap_or_else(|join| CredentialReference::Unknown {
        reason: format!("ipc worker failed: {join}"),
    })
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

/// The one registered IPC handler: every `#[tauri::command]` in this
/// module and its four feature submodules, listed once. The paths name
/// the defining modules because Tauri's macro resolves its generated
/// helper macros relative to each command's path.
///
/// Generic over the runtime so the mock runtime drives the same surface
/// as the shipped app. The crate root's [`crate::invoke_handler`] is the
/// thin public entry point.
pub fn handler<R: Runtime>() -> impl Fn(tauri::ipc::Invoke<R>) -> bool {
    tauri::generate_handler![
        lookup,
        stat,
        readdir,
        read,
        observe,
        liveness,
        freshness,
        capture_parent,
        submit,
        reconcile_lookup,
        reconnect,
        list_drives,
        drive_settings,
        drive_credential_reference,
        drive_settings_write,
        provision::provision_create,
        provision::provision_join,
        delete::provision_delete,
        membership::provision_leave,
        provision::provision_reconfigure,
        provision::provision_service_start,
        provision::provision_service_enable,
        provision::provision_service_disable,
        unlock::drive_unlock,
        unlock::drive_lock,
        onboarding::onboarding_defaults,
        onboarding::onboarding_new_passphrase,
        onboarding::onboarding_commit,
        onboarding::onboarding_verify,
        onboarding::onboarding_adopt,
        onboarding::reveal_in_folder,
    ]
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
            published_unix_secs: 1_700_000_000,
        };
        assert_eq!(
            serde_json::to_value(&freshness).unwrap(),
            serde_json::json!({"state": {"RetainedLastGood": {"detail": "relay quiet"}}, "revision": 9, "generation": 4, "published_unix_secs": 1700000000})
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
        use super::super::service::EnablementState;
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

        // The enablement taxonomy beside the lifecycle state: the
        // frontend tells the toggle position apart from the JSON
        // alone. Independent of the lifecycle observation — a
        // manually started service may run while disabled.
        assert_eq!(
            serde_json::to_value(EnablementState::Enabled {
                detail: "login start recorded".to_string()
            })
            .unwrap(),
            serde_json::json!({"enablement": "enabled", "detail": "login start recorded"})
        );
        assert_eq!(
            serde_json::to_value(EnablementState::Disabled {
                detail: "no login start".to_string()
            })
            .unwrap(),
            serde_json::json!({"enablement": "disabled", "detail": "no login start"})
        );
        assert_eq!(
            serde_json::to_value(EnablementState::Unknown {
                detail: "the manager did not answer".to_string()
            })
            .unwrap(),
            serde_json::json!({"enablement": "unknown", "detail": "the manager did not answer"})
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
            enablement: EnablementState::Enabled {
                detail: "login start recorded".to_string(),
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
                "enablement": {"enablement": "enabled", "detail": "login start recorded"},
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
                    "enablement": {"enablement": "enabled", "detail": "login start recorded"},
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
            identity_file: Some("/keys/identity".to_string()),
            credential_backend: None,
            credential_account: None,
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

    /// IPC credential mapping: absent backend is the file backend
    /// (paths pass through for the argv builders to consume);
    /// `os-store` needs the account, with seeds only where the
    /// operation takes them; anything else refuses with its name —
    /// all before any process spawns.
    #[test]
    fn credential_source_mapping_pins_both_backends() {
        use super::super::provision::{CredentialSource, SeedFiles};
        let file = |backend: Option<String>| {
            credential_source(
                backend,
                None,
                Some("/keys/identity".to_string()),
                Some("/keys/passphrase".to_string()),
                true,
            )
        };
        assert_eq!(
            file(None).unwrap(),
            CredentialSource::File {
                identity_file: Some(PathBuf::from("/keys/identity")),
                passphrase_file: Some(PathBuf::from("/keys/passphrase")),
            }
        );
        assert_eq!(
            file(Some("file".to_string())).unwrap(),
            file(None).unwrap(),
            "an explicit file backend matches the absent default"
        );
        // A store create seeds from the input files; a converged
        // re-run names only the item.
        let seeded = credential_source(
            Some("os-store".to_string()),
            Some("wyrd-drive-alpha".to_string()),
            Some("/keys/identity".to_string()),
            Some("/keys/passphrase".to_string()),
            true,
        )
        .unwrap();
        assert_eq!(
            seeded,
            CredentialSource::OsStore {
                account: "wyrd-drive-alpha".to_string(),
                seed: Some(SeedFiles {
                    identity_file: PathBuf::from("/keys/identity"),
                    passphrase_file: PathBuf::from("/keys/passphrase"),
                }),
            }
        );
        let converged = credential_source(
            Some("os-store".to_string()),
            Some("wyrd-drive-alpha".to_string()),
            None,
            None,
            true,
        )
        .unwrap();
        assert_eq!(
            converged,
            CredentialSource::OsStore {
                account: "wyrd-drive-alpha".to_string(),
                seed: None,
            }
        );
        // Partial seeds refuse (one path without the other is
        // never a drive credential), and seeds refuse where the
        // operation never takes them.
        let partial = credential_source(
            Some("os-store".to_string()),
            Some("wyrd-drive-alpha".to_string()),
            Some("/keys/identity".to_string()),
            None,
            true,
        )
        .unwrap_err();
        assert!(partial.contains("partial seed"), "{partial}");
        let misplaced = credential_source(
            Some("os-store".to_string()),
            Some("wyrd-drive-alpha".to_string()),
            Some("/keys/identity".to_string()),
            Some("/keys/passphrase".to_string()),
            false,
        )
        .unwrap_err();
        assert!(misplaced.contains("bootstrap input only"), "{misplaced}");
        // A store backend without an account, an account without
        // the store backend, and an unknown backend all refuse
        // with their names.
        let no_account =
            credential_source(Some("os-store".to_string()), None, None, None, true).unwrap_err();
        assert!(no_account.contains("no credential account"), "{no_account}");
        let stray_account = credential_source(
            None,
            Some("wyrd-drive-alpha".to_string()),
            Some("/keys/identity".to_string()),
            Some("/keys/passphrase".to_string()),
            true,
        )
        .unwrap_err();
        assert!(
            stray_account.contains("one backend per drive"),
            "{stray_account}"
        );
        let unknown = credential_source(
            Some("vault".to_string()),
            None,
            Some("/keys/identity".to_string()),
            Some("/keys/passphrase".to_string()),
            true,
        )
        .unwrap_err();
        assert!(unknown.contains("unknown credential backend"), "{unknown}");
    }

    /// IPC wire agreement: the credential fields deserialize from
    /// the frontend's camelCase keys, and absent keys default to
    /// `None` — an older frontend sending paths only keeps working.
    /// This pins the Rust half of the boundary the `mockIPC` store
    /// test pins from the TS half (`operations.test.ts`).
    #[test]
    fn credential_ipc_fields_deserialize_from_camel_case() {
        let create: ProvisionCreateArgs = serde_json::from_value(serde_json::json!({
            "name": "alpha",
            "driveDir": "/drives/alpha",
            "relays": [],
            "identityFile": "/keys/identity",
            "passphraseFile": "/keys/passphrase",
            "credentialBackend": "os-store",
            "credentialAccount": "wyrd-drive-alpha",
            "mode": "vault",
            "noService": false,
        }))
        .expect("store-backed create deserializes");
        assert_eq!(
            create.credential_backend.as_deref(),
            Some("os-store"),
            "backend rides credentialBackend"
        );
        assert_eq!(
            create.credential_account.as_deref(),
            Some("wyrd-drive-alpha"),
            "account rides credentialAccount"
        );
        // Absent keys are the file-backend default, never an error.
        let leave: ProvisionLeaveArgs = serde_json::from_value(serde_json::json!({
            "driveDir": "/drives/alpha",
            "identityFile": "/keys/identity",
            "passphraseFile": "/keys/passphrase",
            "acknowledged": true,
        }))
        .expect("a paths-only leave deserializes");
        assert_eq!(leave.credential_backend, None);
        assert_eq!(leave.credential_account, None);
        assert_eq!(leave.identity_file.as_deref(), Some("/keys/identity"));
        let unlock: DriveUnlockArgs = serde_json::from_value(serde_json::json!({
            "driveDir": "/drives/alpha",
            "credentialBackend": "os-store",
            "credentialAccount": "wyrd-drive-alpha",
            "passphrase": "hunter2-hunter2",
        }))
        .expect("a store unlock deserializes with no identity file");
        assert_eq!(unlock.identity_file, None);
        assert_eq!(
            unlock.credential_account.as_deref(),
            Some("wyrd-drive-alpha")
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
