//! The host's drive-ownership path: open the keystore through the
//! single-owner rule and refuse a held drive with an operator
//! sentence that names the holding process instead of retrying,
//! deleting the lockfile, or opening a second store handle.
//!
//! Composition (store, node, serving, live split) and the running
//! loop live in the daemon's shared owner implementation, not here:
//! this module names only the lock and the holder sentence (`durable`
//! for the lock error, `keys` for the opener, the posture sidecar for
//! the sentence) beside `wyrd-core` and `wyrd-daemon`. The host
//! composition check pins that boundary.

use std::path::{Path, PathBuf};

use wyrd_daemon::core::RuntimeMaterialization;
use wyrd_daemon::posture::describe_drive_holder;
use wyrd_daemon::DriveView;
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
    /// The shared owner composition refused the drive. Composition
    /// is one copy in the daemon; this variant carries its verdict
    /// without rephrasing it.
    #[error(transparent)]
    Owner(#[from] wyrd_daemon::OwnerError),
}

/// A drive the host owns: the lock held for the handle's lifetime,
/// with the opened engine ready for the shared composition. Dropping
/// this without running releases the lock; composing and running
/// tears down in the mount's order.
pub struct OpenHost {
    pub(crate) engine: Engine,
    pub(crate) drive_dir: PathBuf,
}

impl OpenHost {
    /// The drive directory this host holds.
    pub fn drive_dir(&self) -> &Path {
        &self.drive_dir
    }
}

/// Open a drive through the host: the keystore open takes the
/// exclusive lock (a second opener gets [`HostError::DriveLocked`],
/// never a wait). Everything downstream of the lock — store,
/// retention, node, serving, live split — is the daemon's shared
/// owner composition, reached through [`crate::run::compose`].
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
    Ok(OpenHost { engine, drive_dir })
}

/// Which process holds the drive, read from either posture sidecar
/// (vault or mount — a mount holder publishes only its own file)
/// and rendered by the daemon beside the reader, so every
/// lock-refused starter names the holder one way.
fn holder_sentence(drive_dir: &Path) -> String {
    describe_drive_holder(drive_dir)
}
