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
use wyrd_daemon::posture::{describe_holder, read_posture, sidecar_path};
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
    /// A configured retention quota the store already exceeds: refuse
    /// to start with both numbers instead of refusing every write.
    #[error("retention quota refused at startup: {0}")]
    Retention(#[from] wyrd_core::live::QuotaCheckError),
    /// Node composition over the opened store failed.
    #[error(transparent)]
    Node(#[from] NodeError),
    /// The supervised run or the transport tail failed at the io
    /// boundary.
    #[error("run failed: {0}")]
    Io(#[from] std::io::Error),
}

/// A drive the host owns: the lock held for the handle's lifetime,
/// the node composed and its live heads refreshed. Dropping this
/// without running releases the lock; running tears down in the
/// mount's order.
pub struct OpenHost {
    pub(crate) node: WyrdNode<HostView>,
    pub(crate) drive_dir: PathBuf,
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
    let config = wyrd_daemon::LiveConfig::for_local_sync();
    wyrd_daemon::process::check_startup_retention(&config, &store)?;
    let mut node = WyrdNode::new(engine, store)?;
    node.refresh_live_heads()?;
    Ok(OpenHost { node, drive_dir })
}

/// Which process holds the drive, read from its posture sidecar and
/// rendered by the daemon beside the reader, so every lock-refused
/// starter names the holder one way.
fn holder_sentence(drive_dir: &Path) -> String {
    describe_holder(&read_posture(&sidecar_path(drive_dir)))
}
