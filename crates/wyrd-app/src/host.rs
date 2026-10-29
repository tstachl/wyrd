//! The host's drive-ownership path: open the keystore through the
//! single-owner rule, compose the node over the daemon's view, and
//! refuse a held drive with an operator sentence that names the
//! holding process instead of retrying, deleting the lockfile, or
//! opening a second store handle.
//!
//! Transport construction (mailbox, bulk, serving) and the running
//! loop live in the run step, not here: this module names only the
//! durable side (`durable`, `runtime::Engine`, `keys`) beside
//! `wyrd-core` and `wyrd-daemon`. The host composition check pins
//! that boundary.

use std::path::{Path, PathBuf};

use wyrd_daemon::core::RuntimeMaterialization;
use wyrd_daemon::posture::{now_unix, read_posture, sidecar_path, ProcessPosture};
use wyrd_daemon::{DriveView, NodeError, WyrdNode};
use wyrd_format::FsObjectStore;
use wyrd_sync::durable::DurableError;
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::{Engine, EngineError};

/// The host's node: the same view the daemon builds its backends
/// from, so the host serves what the backends would serve.
pub type HostView = DriveView<FsObjectStore, RuntimeMaterialization>;

/// What opening a drive through the host can report. The lock
/// refusal carries the holding process, read from its posture
/// sidecar without touching the store — the operator sentence DG-1's
/// negative invariant requires, not a bare error kind.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// Another process holds the drive's exclusive lock. Never
    /// retried, never worked around: the holder sentence says which
    /// process, read from its sidecar (or notes its absence).
    #[error("drive {drive} is held by another process: {holder}")]
    DriveLocked {
        /// The drive directory that refused the open.
        drive: PathBuf,
        /// Which process holds it (instance id, serving id, and
        /// recency), or that no posture was recorded.
        holder: String,
    },
    /// The keystore open failed for a reason other than contention.
    #[error("cannot open drive: {0}")]
    Engine(#[from] EngineError),
    /// The object store beside the drive failed to open.
    #[error("object store failed: {0}")]
    Store(String),
    /// Node composition over the opened store failed.
    #[error(transparent)]
    Node(#[from] NodeError),
}

/// A drive the host owns: the lock held for the handle's lifetime,
/// the node composed and its live heads refreshed. Dropping this
/// without running releases the lock; running tears down in the
/// mount's order.
pub struct OpenHost {
    node: WyrdNode<HostView>,
    drive_dir: PathBuf,
}

impl OpenHost {
    /// The drive directory this host holds.
    pub fn drive_dir(&self) -> &Path {
        &self.drive_dir
    }

    /// The composed node, for reads before the run starts.
    pub fn node(&self) -> &WyrdNode<HostView> {
        &self.node
    }
}

/// Open a drive through the host: the keystore open takes the
/// exclusive lock (a second opener gets [`HostError::DriveLocked`],
/// never a wait), the object store opens beside it, and the node
/// composes over the daemon's view with live heads refreshed.
pub fn open_host(
    drive_dir: PathBuf,
    passphrase: &str,
    identity: DeviceIdentitySecret,
) -> Result<OpenHost, HostError> {
    let engine = match Engine::open_keystore(drive_dir.clone(), passphrase, identity) {
        Err(EngineError::Durable(DurableError::StoreLocked)) => {
            return Err(HostError::DriveLocked {
                drive: drive_dir.clone(),
                holder: holder_sentence(&drive_dir),
            });
        }
        result => result?,
    };
    let store = FsObjectStore::open(drive_dir.clone())
        .map_err(|error| HostError::Store(error.to_string()))?;
    let mut node = WyrdNode::new(engine, store)?;
    node.refresh_live_heads()?;
    Ok(OpenHost { node, drive_dir })
}

/// Which process holds the drive, read from its posture sidecar:
/// instance id, serving id, and recency — never aliveness, which a
/// sidecar cannot prove. No sidecar (or an unreadable one) still
/// refuses, saying so instead of naming a process.
fn holder_sentence(drive_dir: &Path) -> String {
    match read_posture(&sidecar_path(drive_dir)) {
        ProcessPosture::Fresh(file) => {
            let age = now_unix().saturating_sub(file.posture_unix());
            format!(
                "process {} (serving {}, posture {}s ago; liveness unverified)",
                file.instance(),
                file.serving_id(),
                age
            )
        }
        ProcessPosture::Stale(file, age) => format!(
            "process {} (serving {}, last recorded {}s ago; liveness unverified)",
            file.instance(),
            file.serving_id(),
            age
        ),
        ProcessPosture::Absent => {
            "no posture recorded (the holder never published; liveness unverified)".to_string()
        }
        ProcessPosture::Malformed => {
            "posture unreadable (sidecar present but not schema v1; liveness unverified)"
                .to_string()
        }
    }
}
