//! Offline egress: materialize the namespace to a plain directory tree.
//!
//! The export walk is generic over [`NamespaceView`](crate::view::NamespaceView), so the guarantee
//! holds for every provider, not just the FUSE one: files stream
//! through `open_file`/`read`, symlinks recreate from the node's
//! target, the executable bit is preserved, and empty directories
//! survive. The output needs no wyrd software to read afterward —
//! that is the point. Export is offline by construction: it never
//! touches relays, the mailbox, or the bulk source, so
//! [`ViewError::NotMaterialized`]
//! content fails the export closed instead of fetching.
//!
//! Failure is atomic: the walk lands in a payload subdirectory of a
//! uniquely named staging container and only the payload is renamed
//! into place after the whole tree succeeds, so a failed export
//! leaves no partial tree behind — neither a retry-blocking
//! `DestinationNotEmpty` nor a tree that looks complete but silently
//! dropped files. The heartbeat marker lives in the container next
//! to the payload, never inside it, so control state shares no
//! namespace with user content. Staging names are
//! unique per run, never shared: no export can ever delete or
//! observe another export's live tree.
//!
//! Crashed runs leave their staging behind, so every run heartbeats
//! its staging directory (a timestamp rewritten per read window) and
//! sweeps siblings whose heartbeat is older than `STALE_AFTER`
//! before starting. Fresh staging is always left alone — the safe
//! direction under clock skew is treating a directory as live — so
//! concurrent same-destination exports race to publish instead of
//! sharing state: the first rename wins, later publishers fail with
//! `DestinationNotEmpty`, and `dest` always holds one complete tree,
//! never a mix.
//!
//! Symlinks are validated against the one immutable view used by this
//! export ([`confine_symlink_target`](crate::view::confine_symlink_target)):
//! absolute, root-escaping, compositionally escaping, conflicting,
//! cyclic, and over-budget targets are refused, because the plain copy
//! must stay self-contained. Mounted FUSE views do not follow symlinks
//! in v0. Multi-head conflicts materialize as `name@N` siblings,
//! numbered in SnapshotId byte order exactly like the version-selection
//! grammar, so `doc@1` on disk is `doc@1` in the mount. Export never
//! picks a winner silently; a stored name colliding with a versioned
//! sibling fails closed as [`ExportError::NameCollision`].
//!
//! Walk depth is bounded by the format's [`MAX_PATH_DEPTH`](wyrd_format::MAX_PATH_DEPTH):
//! the mutation layer never authors deeper trees, so a deeper walk
//! means a provider outside the format contract, and export refuses
//! rather than recursing unbounded.

use std::path::PathBuf;

use crate::view::{ConfinementError, ViewError};

/// What one export produced, for logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExportReport {
    /// Files written to the export tree.
    pub files: u64,
    /// Directories written to the export tree.
    pub dirs: u64,
    /// Symlinks recreated in the export tree.
    pub symlinks: u64,
    /// Multi-head conflicts materialized as versioned siblings.
    pub conflicts: u64,
    /// Total file bytes written.
    pub bytes: u64,
}

/// Egress failures. View failures carry the drive path that caused
/// them; I/O failures carry the filesystem path. Both name the exact
/// obstacle so a failed export is actionable, never a bare errno.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// Destination exists and is not empty; nothing was written.
    #[error("destination {0} exists and is not empty")]
    DestinationNotEmpty(PathBuf),
    /// A view read failed at the named drive path.
    #[error("view failed at {path}: {source}")]
    View {
        /// Drive path the view failed on.
        path: String,
        /// Underlying view failure.
        source: ViewError,
    },
    /// A filesystem write failed.
    #[error("{path}: {source}")]
    Io {
        /// Filesystem path the write failed on.
        path: PathBuf,
        /// Underlying I/O failure.
        source: std::io::Error,
    },
    /// A stored name collides with a versioned sibling.
    #[error("export name collision at {0}: a stored name meets a versioned sibling")]
    NameCollision(PathBuf),
    /// A symlink cannot be proven confined and is refused.
    #[error("symlink at {path} cannot be proven confined and is refused: {source}")]
    Symlink {
        /// Drive path of the refused symlink.
        path: String,
        /// Confinement failure.
        source: ConfinementError,
    },
    /// Served bytes disagree with the declared file size.
    #[error("{path}: served {actual} bytes for a {expected}-byte file")]
    SizeMismatch {
        /// Drive path of the mismatched file.
        path: String,
        /// Declared file size.
        expected: u64,
        /// Bytes actually served.
        actual: u64,
    },
    /// Namespace deeper than the format path-depth limit.
    #[error("namespace deeper than the 256-component limit at {0}")]
    TooDeep(String),
    /// Symlinks are not supported on this platform.
    #[error("symlinks are not supported on this platform")]
    SymlinkUnsupported,
}

mod staging;
mod walk;

pub use walk::export_tree;

#[cfg(test)]
mod tests_harness;
#[cfg(test)]
mod tests_staging;
#[cfg(test)]
mod tests_walk;
