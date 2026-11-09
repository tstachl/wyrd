//! Socket transport constants for the drive-owner interface: the
//! socket file name, its derivation from the drive directory, and the
//! frame bound both sides enforce. One definition, shared by the
//! client here and the daemon's server — the server re-exports these
//! rather than redefining them, so the two sides cannot drift.

use std::path::{Path, PathBuf};

/// The socket file name inside the drive directory. Beside the
/// store, never inside it: the store reads named files, and a
/// socket entry is not a store input. Removed on clean shutdown;
/// a stale entry from an unclean shutdown is unlinked at bind while
/// the store lock is held, so no competing owner can race the
/// reclaim.
pub const OWNER_SOCKET_NAME: &str = "owner.sock";

/// Derive the interface socket path from the drive directory
/// (OD-16-B): no discovery service, the path is the convention.
/// Removed on clean shutdown; a stale entry from an unclean shutdown
/// is unlinked at bind while the store lock is held.
pub fn socket_path_for(drive_dir: &Path) -> PathBuf {
    drive_dir.join(OWNER_SOCKET_NAME)
}

/// Largest single frame (request or response) in bytes: the read
/// bound plus JSON and hex overhead, rounded up. A frame past this
/// closes the connection; the client treats the break as unconfirmed.
pub const MAX_FRAME_BYTES: usize = 96 << 20;
