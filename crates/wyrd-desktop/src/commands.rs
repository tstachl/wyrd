//! The Tauri command surface: one command per backend operation,
//! forwarding through the single [`DesktopBackend`].
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
async fn exchange<T, F, R>(app: AppHandle<R>, op: &'static str, call: F) -> Result<T, BackendError>
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
        let backend = app.state::<AppState>();
        // Reclaim a poisoned guard like the backend itself does
        // (`DesktopBackend::with_client`): a panic while held must
        // not brick later calls.
        let guard = backend.lock().unwrap_or_else(|poison| poison.into_inner());
        call(&guard)
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
/// payload — never success, never the taxonomy. The backend lock is
/// held for the whole call, serializing with owner exchanges exactly
/// like every command: one slow probe delays others, bounded by the
/// probe timeout, and the count covers dispatch-queue time too.
async fn query<T, F, B, R>(app: AppHandle<R>, op: &'static str, build_unreadable: B, call: F) -> T
where
    F: FnOnce(&DesktopBackend) -> T + Send + 'static,
    B: FnOnce(String) -> T + Send + 'static,
    T: Send + 'static,
    R: Runtime,
{
    let _flight = app.state::<FlightTracker>().enter();
    tauri::async_runtime::spawn_blocking(move || {
        let backend = app.state::<AppState>();
        // Reclaim a poisoned guard like the backend itself does.
        let guard = backend.lock().unwrap_or_else(|poison| poison.into_inner());
        call(&guard)
    })
    .await
    .unwrap_or_else(|join| build_unreadable(format!("ipc worker failed for {op}: {join}")))
}

/// Look up one path: the node plus the serving generation and
/// durable revision it was served from.
#[tauri::command]
pub async fn lookup<R: Runtime>(
    app: AppHandle<R>,
    path: String,
) -> Result<(WireNode, u64, u64), BackendError> {
    exchange(app, "lookup", move |backend| backend.lookup(&path)).await
}

/// Stat one path: attributes plus serving generation and durable
/// revision.
#[tauri::command]
pub async fn stat<R: Runtime>(
    app: AppHandle<R>,
    path: String,
) -> Result<(WireAttr, u64, u64), BackendError> {
    exchange(app, "stat", move |backend| backend.stat(&path)).await
}

/// List one directory: entries plus serving generation and durable
/// revision.
#[tauri::command]
pub async fn readdir<R: Runtime>(
    app: AppHandle<R>,
    path: String,
) -> Result<(Vec<WireDirEntry>, u64, u64), BackendError> {
    exchange(app, "readdir", move |backend| backend.readdir(&path)).await
}

/// Read one whole file, plus serving generation and durable
/// revision.
#[tauri::command]
pub async fn read<R: Runtime>(
    app: AppHandle<R>,
    path: String,
) -> Result<(Vec<u8>, u64, u64), BackendError> {
    exchange(app, "read", move |backend| backend.read(&path)).await
}

/// The owner's durable observation: what a member may observe,
/// never liveness.
#[tauri::command]
pub async fn observe<R: Runtime>(app: AppHandle<R>) -> Result<WireSyncStatus, BackendError> {
    exchange(app, "observe", move |backend| backend.observe()).await
}

/// Transport health plus the last pass report, pinned to the pass
/// count. A separate object from the durable observation.
#[tauri::command]
pub async fn liveness<R: Runtime>(app: AppHandle<R>) -> Result<WireLiveness, BackendError> {
    exchange(app, "liveness", move |backend| backend.liveness()).await
}

/// The projection freshness judgment.
#[tauri::command]
pub async fn freshness<R: Runtime>(app: AppHandle<R>) -> Result<WireFreshness, BackendError> {
    exchange(app, "freshness", move |backend| backend.freshness()).await
}

/// Capture the parent-directory incarnation a create must send
/// against. `path` is the parent directory — `""` for the drive
/// root — not the file path itself.
#[tauri::command]
pub async fn capture_parent<R: Runtime>(
    app: AppHandle<R>,
    path: String,
) -> Result<WireParentToken, BackendError> {
    exchange(app, "capture_parent", move |backend| {
        backend.capture_parent(&path)
    })
    .await
}

/// Forward one mutation to the owner's single queue and block for
/// its terminal answer. The only mutation path.
#[tauri::command]
pub async fn submit<R: Runtime>(
    app: AppHandle<R>,
    kind: WireMutationKind,
) -> Result<WireMutationOutcome, BackendError> {
    exchange(app, "submit", move |backend| backend.submit(kind)).await
}

/// Backend-owned recovery: reconcile a path after an unconfirmed
/// outcome and receive the verdict. The frontend never reconciles
/// itself; absence is `Ok(None)`, never a refusal.
#[tauri::command]
pub async fn reconcile_lookup<R: Runtime>(
    app: AppHandle<R>,
    path: String,
) -> Result<Option<(WireNode, u64, u64)>, BackendError> {
    exchange(app, "reconcile_lookup", move |backend| {
        backend.reconcile_lookup(&path)
    })
    .await
}

/// Drop the current connection and mint a new instance against the
/// same drive. The frontend calls this explicitly (the unavailable
/// view's retry) after an [`BackendError::Unconfirmed`] answer;
/// nothing calls it automatically, and recovery proves nothing
/// about the prior operation.
#[tauri::command]
pub async fn reconnect<R: Runtime>(app: AppHandle<R>) -> Result<(), BackendError> {
    exchange(app, "reconnect", move |backend| backend.reconnect()).await
}

/// Every registered drive plus per-drive reachability, verified
/// this session. A local query, not an owner operation: the answer
/// is a state struct (never the taxonomy), unreadable states ride
/// inside the payload, and any other rejection is a transport
/// failure. The backend lock is held for the whole call, so a
/// registry with many entries serializes other commands behind the
/// per-drive probe bound.
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
        |backend| registry::list_drives_for(backend.drive_dir()),
    )
    .await
}

/// This drive's configured bounds, as configured — never defaults,
/// never effective posture. A local query like [`list_drives`]:
/// unknown states ride inside the payload, and only an explicit
/// `Configured` carries values.
#[tauri::command]
pub async fn drive_settings<R: Runtime>(app: AppHandle<R>) -> DriveSettings {
    query(
        app,
        "drive_settings",
        |reason| DriveSettings::Unreadable { reason },
        |backend| settings_view::drive_settings(backend.drive_dir()),
    )
    .await
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
            current: true,
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
                "current": true,
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
                    "current": true,
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
}
