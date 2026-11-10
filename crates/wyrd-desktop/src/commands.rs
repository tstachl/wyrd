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

use tauri::{AppHandle, Manager};
use wyrd_owner_client::{
    WireAttr, WireDirEntry, WireFreshness, WireLiveness, WireMutationKind, WireMutationOutcome,
    WireNode, WireParentToken, WireSyncStatus,
};

use super::{AppState, BackendError, DesktopBackend};

/// Run one backend exchange on the blocking pool and report a dead
/// worker as [`BackendError::Unconfirmed`]: after dispatch the
/// outcome is unknown, so success and refusal are both dishonest.
async fn exchange<T, F>(app: AppHandle, op: &'static str, call: F) -> Result<T, BackendError>
where
    F: FnOnce(&DesktopBackend) -> Result<T, BackendError> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let backend = app.state::<AppState>();
        // Reclaim a poisoned guard like the backend itself does
        // (`DesktopBackend::with_client`): a panic while held must
        // not brick later calls.
        let guard = backend.lock().unwrap_or_else(|poison| poison.into_inner());
        call(&guard)
    })
    .await
    .unwrap_or_else(|join| {
        Err(BackendError::Unconfirmed {
            op: op.into(),
            detail: format!("ipc worker failed: {join}"),
        })
    })
}

/// Look up one path: the node plus the serving generation and
/// durable revision it was served from.
#[tauri::command]
pub async fn lookup(app: AppHandle, path: String) -> Result<(WireNode, u64, u64), BackendError> {
    exchange(app, "lookup", move |backend| backend.lookup(&path)).await
}

/// Stat one path: attributes plus serving generation and durable
/// revision.
#[tauri::command]
pub async fn stat(app: AppHandle, path: String) -> Result<(WireAttr, u64, u64), BackendError> {
    exchange(app, "stat", move |backend| backend.stat(&path)).await
}

/// List one directory: entries plus serving generation and durable
/// revision.
#[tauri::command]
pub async fn readdir(
    app: AppHandle,
    path: String,
) -> Result<(Vec<WireDirEntry>, u64, u64), BackendError> {
    exchange(app, "readdir", move |backend| backend.readdir(&path)).await
}

/// Read one whole file, plus serving generation and durable
/// revision.
#[tauri::command]
pub async fn read(app: AppHandle, path: String) -> Result<(Vec<u8>, u64, u64), BackendError> {
    exchange(app, "read", move |backend| backend.read(&path)).await
}

/// The owner's durable observation: what a member may observe,
/// never liveness.
#[tauri::command]
pub async fn observe(app: AppHandle) -> Result<WireSyncStatus, BackendError> {
    exchange(app, "observe", move |backend| backend.observe()).await
}

/// Transport health plus the last pass report, pinned to the pass
/// count. A separate object from the durable observation.
#[tauri::command]
pub async fn liveness(app: AppHandle) -> Result<WireLiveness, BackendError> {
    exchange(app, "liveness", move |backend| backend.liveness()).await
}

/// The projection freshness judgment.
#[tauri::command]
pub async fn freshness(app: AppHandle) -> Result<WireFreshness, BackendError> {
    exchange(app, "freshness", move |backend| backend.freshness()).await
}

/// Capture the parent-directory incarnation a create must send
/// against. `path` is the parent directory — `""` for the drive
/// root — not the file path itself.
#[tauri::command]
pub async fn capture_parent(app: AppHandle, path: String) -> Result<WireParentToken, BackendError> {
    exchange(app, "capture_parent", move |backend| {
        backend.capture_parent(&path)
    })
    .await
}

/// Forward one mutation to the owner's single queue and block for
/// its terminal answer. The only mutation path.
#[tauri::command]
pub async fn submit(
    app: AppHandle,
    kind: WireMutationKind,
) -> Result<WireMutationOutcome, BackendError> {
    exchange(app, "submit", move |backend| backend.submit(kind)).await
}

/// Backend-owned recovery: reconcile a path after an unconfirmed
/// outcome and receive the verdict. The frontend never reconciles
/// itself; absence is `Ok(None)`, never a refusal.
#[tauri::command]
pub async fn reconcile_lookup(
    app: AppHandle,
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
pub async fn reconnect(app: AppHandle) -> Result<(), BackendError> {
    exchange(app, "reconnect", move |backend| backend.reconnect()).await
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
}
