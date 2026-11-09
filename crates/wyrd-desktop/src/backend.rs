//! The backend IPC surface: one method per interface operation the
//! shipped UI needs, forwarding through the shared client.
//!
//! Discipline, inherited from the interface contract:
//!
//! - Forward, never interpret. Refusals travel through as data with
//!   their codes intact; two refusals are never collapsed into one.
//! - The four-way answer taxonomy is preserved end to end:
//!   [`BackendError::Unavailable`] (no transport), [`BackendError::Unconfirmed`]
//!   (no terminal answer), [`BackendError::Refused`] (terminal owner
//!   refusal), [`BackendError::Incompatible`] (protocol mismatch).
//! - Poison on [`BackendError::Unconfirmed`]. The shared client
//!   already poisons the broken connection; the backend drops it and
//!   reconnects. Recovery is a new connection, never a resumed stream.
//! - Reconcile before retry. After `Unconfirmed`, re-read the touched
//!   paths through the owner ([`DesktopBackend::reconcile_lookup`])
//!   and decide from the re-read.
//! - No store opens. The backend never opens a drive directory, never
//!   links the durable side, never serves presentation. If a future
//!   contributor reaches for `wyrd-core`, `wyrd-sync`, `wyrd-view`, or
//!   `wyrd-format` from this crate, the gate refuses.
//! - `pause` and `resume` stay out of this surface until a UI
//!   requirement names them. The three first views contain no
//!   synchronization controls.
//! - `submit` is the only mutation path: the single mutation queue,
//!   never a second route.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use wyrd_owner_client::{
    ClientError, OwnerClient, WireAttr, WireDirEntry, WireError, WireFreshness, WireLiveness,
    WireMutationKind, WireMutationOutcome, WireNode, WireParentToken, WireSyncStatus,
};

/// The backend-side answer to one IPC request. The client's four-way
/// taxonomy, preserved exactly: a retry decision depends on which one
/// came back, so the mapping never collapses two outcomes into one.
/// [`BackendError::Refused`] carries the owner's [`WireError`]
/// through untouched — code and message, never re-meaned.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum BackendError {
    /// No transport: the owner is down, which says nothing about any
    /// drive. Never an empty drive, never evidence about any request.
    Unavailable { detail: String },
    /// No terminal answer: the outcome is unknown. Reconnect — the
    /// broken connection poisons itself — then reconcile (re-read
    /// through the owner) before deciding whether retry is safe. A
    /// transport break alone never authorizes retry and never
    /// establishes failure.
    Unconfirmed { op: String, detail: String },
    /// The owner answered with a defined refusal. Terminal: the
    /// request did not commit and will not commit on retry unless the
    /// caller changes what it asked for.
    Refused {
        #[serde(flatten)]
        error: WireError,
    },
    /// The peer does not speak this wire version or answered an
    /// operation with a result shape that operation never produces.
    Incompatible { detail: String },
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::Unavailable { detail } => write!(f, "owner unavailable: {detail}"),
            BackendError::Unconfirmed { op, detail } => write!(f, "unconfirmed {op}: {detail}"),
            BackendError::Refused { error } => {
                write!(f, "owner refused ({:?}): {}", error.code, error.message)
            }
            BackendError::Incompatible { detail } => write!(f, "incompatible owner: {detail}"),
        }
    }
}

impl std::error::Error for BackendError {}

impl From<ClientError> for BackendError {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::Unavailable(detail) => BackendError::Unavailable { detail },
            ClientError::Unconfirmed { op, detail } => BackendError::Unconfirmed { op, detail },
            ClientError::Refused(error) => BackendError::Refused { error },
            ClientError::Incompatible(detail) => BackendError::Incompatible { detail },
        }
    }
}

/// One drive connection: the backend's single client instance, held
/// under exclusive access. Concurrent Tauri commands serialize on
/// the mutex; when one request poisons the connection, every
/// operation after it resolves `Unconfirmed` without touching the
/// socket, and [`DesktopBackend::reconnect`] mints a new instance.
/// Outstanding operations at reconnect time resolve `Unconfirmed` —
/// reconnection never replays a mutation and never converts an
/// unconfirmed outcome into success.
#[derive(Debug)]
pub struct DesktopBackend {
    drive_dir: PathBuf,
    client: Mutex<OwnerClient>,
}

impl DesktopBackend {
    /// Connect to the interface socket derived from the drive
    /// directory — the same derivation the owner binds. A refused or
    /// absent socket is [`BackendError::Unavailable`]: the owner is
    /// unreachable, which says nothing about any drive's contents.
    pub fn connect(drive_dir: &Path) -> Result<Self, BackendError> {
        let client = OwnerClient::connect_drive(drive_dir)?;
        Ok(DesktopBackend {
            drive_dir: drive_dir.to_owned(),
            client: Mutex::new(client),
        })
    }

    /// Drop the current connection and mint a new instance against
    /// the same drive. Recovery is a new connection, never a resumed
    /// stream: outstanding operations resolve `Unconfirmed`, and the
    /// new instance replays nothing. A failed reconnect keeps the old
    /// (poisoned) instance in place and reports `Unavailable` — the
    /// backend never invents a connection it does not hold. A
    /// poisoned mutex recovers here too: the guard is reclaimed
    /// rather than brick the backend as `Incompatible`, because a
    /// panic while held must not defeat the new-connection recovery
    /// this method exists to provide.
    pub fn reconnect(&self) -> Result<(), BackendError> {
        let fresh = OwnerClient::connect_drive(&self.drive_dir)?;
        let mut held = self
            .client
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *held = fresh;
        Ok(())
    }

    /// The drive this backend connects to.
    pub fn drive_dir(&self) -> &Path {
        &self.drive_dir
    }

    fn with_client<T>(
        &self,
        call: impl FnOnce(&mut OwnerClient) -> Result<T, ClientError>,
    ) -> Result<T, BackendError> {
        // A poisoned mutex reclaims its guard instead of bricking
        // the backend: the call path holds no panic sites (socket IO
        // reports errors, never panics), so poisoning means a bug
        // elsewhere, and every later call degrading to
        // `Incompatible` would defeat `reconnect`'s recovery. The
        // poisoned instance itself is untouched — still poisoned if
        // the client broke — only the lock recovers.
        let mut held = self
            .client
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        call(&mut held).map_err(BackendError::from)
    }

    /// Look up one path: the node plus the serving generation and
    /// durable revision it was served from.
    pub fn lookup(&self, path: &str) -> Result<(WireNode, u64, u64), BackendError> {
        self.with_client(|client| client.lookup(path))
    }

    /// Stat one path: attributes plus serving generation and durable
    /// revision.
    pub fn stat(&self, path: &str) -> Result<(WireAttr, u64, u64), BackendError> {
        self.with_client(|client| client.stat(path))
    }

    /// List one directory: entries plus serving generation and
    /// durable revision.
    pub fn readdir(&self, path: &str) -> Result<(Vec<WireDirEntry>, u64, u64), BackendError> {
        self.with_client(|client| client.readdir(path))
    }

    /// Read one whole file, plus serving generation and durable
    /// revision. Files past the interface read bound refuse with
    /// `TooLarge` — never truncated, never streamed.
    pub fn read(&self, path: &str) -> Result<(Vec<u8>, u64, u64), BackendError> {
        self.with_client(|client| client.read(path))
    }

    /// The owner's durable observation: what a member may observe,
    /// never liveness.
    pub fn observe(&self) -> Result<WireSyncStatus, BackendError> {
        self.with_client(|client| client.observe())
    }

    /// Transport health plus the last pass report, pinned to the pass
    /// count. A separate object from the durable observation: a live
    /// socket is not a durable fact.
    pub fn liveness(&self) -> Result<WireLiveness, BackendError> {
        self.with_client(|client| client.liveness())
    }

    /// The projection freshness judgment: fresh empty, fresh
    /// non-empty, retained last-good with its refresh failure, or no
    /// trustworthy projection.
    pub fn freshness(&self) -> Result<WireFreshness, BackendError> {
        self.with_client(|client| client.freshness())
    }

    /// Capture the parent-directory incarnation a create must send
    /// against. `path` is the parent directory that will contain the
    /// create — `""` for the drive root — not the file path itself.
    pub fn capture_parent(&self, path: &str) -> Result<WireParentToken, BackendError> {
        self.with_client(|client| client.capture_parent(path))
    }

    /// Forward one mutation to the owner's single queue and block for
    /// its terminal answer. The only mutation path: there is no
    /// second route through this backend. A sent request is not a
    /// committed write — only the terminal answer says what happened.
    pub fn submit(&self, kind: WireMutationKind) -> Result<WireMutationOutcome, BackendError> {
        self.with_client(|client| client.submit(kind))
    }

    /// Backend-owned recovery, not an ordinary frontend command: the
    /// frontend asks the backend to reconcile a path and receives
    /// the verdict; it never reconciles itself. A missing path is
    /// `Ok(None)` — absence as data, not as refusal — any other
    /// refusal propagates, and a transport break stays `Unconfirmed`.
    /// A failed re-read never resolves the original request: it stays
    /// unconfirmed.
    pub fn reconcile_lookup(
        &self,
        path: &str,
    ) -> Result<Option<(WireNode, u64, u64)>, BackendError> {
        self.with_client(|client| client.reconcile_lookup(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wyrd_owner_client::{WireErrorCode, WIRE_VERSION};

    /// The IPC error taxonomy serializes typed: the frontend can
    /// tell `Unavailable` from `Unconfirmed` from the JSON alone,
    /// and a refusal keeps its code — mapping never collapses two
    /// outcomes in transit.
    #[test]
    fn error_taxonomy_serializes_typed() {
        let unavailable = serde_json::to_value(BackendError::Unavailable {
            detail: "no owner".to_string(),
        })
        .unwrap();
        let unconfirmed = serde_json::to_value(BackendError::Unconfirmed {
            op: "lookup".to_string(),
            detail: "torn frame".to_string(),
        })
        .unwrap();
        let refused = serde_json::to_value(BackendError::Refused {
            error: WireError {
                code: WireErrorCode::NotFound,
                message: "no such path".to_string(),
            },
        })
        .unwrap();
        assert_eq!(unavailable["kind"], "unavailable");
        assert_eq!(unconfirmed["kind"], "unconfirmed");
        assert_eq!(unconfirmed["op"], "lookup");
        assert_eq!(refused["kind"], "refused");
        assert_eq!(refused["code"], "NotFound");
        assert_ne!(
            unavailable["kind"], unconfirmed["kind"],
            "the frontend tells them apart from the JSON alone"
        );
        // The wire version the refusal decodes under is the pinned
        // one: the taxonomy never drifts off the interface version.
        assert_eq!(WIRE_VERSION, 1);
    }
}
