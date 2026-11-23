//! Local socket binding: the drive-directory socket and the
//! optional group-container socket plus its symlink. The parent
//! module owns the connection loop and re-exports these items.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use wyrd_format::DriveId;

use crate::settings::app_group_identifier;

/// Bind the drive-directory socket: reclaim a stale entry from an
/// unclean shutdown, then bind. The caller holds the store lock
/// (single owner by kernel enforcement), so no competing binder
/// races the reclaim. Failures refuse the run — an owner that
/// cannot serve its interface never runs deaf.
pub fn bind_listener(socket_path: &Path) -> std::io::Result<UnixListener> {
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Dev-only env bootstrap for the File Provider socket (harness and
/// manual development, never production): the absolute path of the
/// macOS App Group container. Set together with
/// [`FILEPROVIDER_DRIVE_ID_ENV`]. Absent (both unset) means the
/// production path below decides — the Linux behavior and the CI
/// behavior. One set without the other is a misconfiguration and
/// refuses the run; a malformed value refuses too, never a silent
/// primary-only fallback.
pub const FILEPROVIDER_GROUP_CONTAINER_ENV: &str = "WYRD_FILEPROVIDER_GROUP_CONTAINER";

/// The explicitly configured drive, as 64 lowercase hex chars, paired
/// with [`FILEPROVIDER_GROUP_CONTAINER_ENV`]. Dev-only: the
/// production bind derives the served drive from the opened drive
/// itself, so the daemon never invents it, never guesses a
/// single-drive default, and never reads it from the environment
/// outside this dev bootstrap.
pub const FILEPROVIDER_DRIVE_ID_ENV: &str = "WYRD_FILEPROVIDER_DRIVE_ID";

/// The production App Group container path for a home directory:
/// `~/Library/Group Containers/<group>`. The group id is the same
/// effective value the extension and mirror writer use; pure path
/// math so the derivation pins on every platform; only macOS binds
/// through it.
pub fn default_group_container_with_home(home: Option<PathBuf>) -> Option<PathBuf> {
    let home = home?;
    Some(
        home.join("Library/Group Containers")
            .join(app_group_identifier()),
    )
}

/// The production container when it exists on disk: the daemon
/// creates only `Sockets/` (and `by-drive/`) inside it, never the
/// container itself — a hand-created container is a decoy the
/// sandboxed extension cannot reach, so a missing directory means a
/// primary-only bind with a bounded late retry, never a created decoy.
/// The app is expected to initialize the container at launch; if the
/// owner starts first, the retry publishes the alias without a
/// restart. A present but unbindable container (a file at `Sockets/`,
/// permissions) still aborts a startup bind all-or-nothing; the late
/// retry is bounded and ends with a visible error rather than
/// leaving an owner that looks healthy while permanently deaf.
pub fn production_group_container() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        production_group_container_with_home(std::env::var_os("HOME").map(PathBuf::from))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Testable production-container gate: path math plus the `is_dir`
/// existence check, platform-independent so Linux CI pins the
/// decoy rule. Production calls this with `HOME` on macOS.
pub fn production_group_container_with_home(home: Option<PathBuf>) -> Option<PathBuf> {
    let container = default_group_container_with_home(home)?;
    if container.is_dir() {
        Some(container)
    } else {
        None
    }
}

/// The resolved dev bootstrap: where to bind, and for which drive.
#[derive(Debug)]
pub struct GroupContainerConfig {
    /// The existing App Group container directory. The daemon creates
    /// only `Sockets/` (and `by-drive/`) inside it, never the
    /// container itself — a hand-created container is a decoy the
    /// sandboxed extension cannot reach.
    pub group_container: PathBuf,
    /// The drive the group socket serves, explicitly configured.
    pub drive: DriveId,
}

/// Read the dev-only bootstrap from the environment: `None` when
/// neither variable is set, `Some(Err)` on any partial or malformed
/// configuration (fail closed, never a silent fallback). Production
/// never consults this: with both unset, [`super::service::OwnerService::bind_for_composer`]
/// derives the container from the platform convention and the drive
/// from the served drive itself.
pub fn group_container_from_env() -> Option<Result<GroupContainerConfig, String>> {
    let container = std::env::var_os(FILEPROVIDER_GROUP_CONTAINER_ENV).map(PathBuf::from);
    let drive_hex = std::env::var(FILEPROVIDER_DRIVE_ID_ENV).ok();
    match (container, drive_hex) {
        (None, None) => None,
        (Some(group_container), Some(drive_hex)) => Some(parse_group_config(group_container, &drive_hex)),
        (None, Some(_)) => Some(Err(format!(
            "{FILEPROVIDER_GROUP_CONTAINER_ENV} is unset while {FILEPROVIDER_DRIVE_ID_ENV} is set; set both or neither"
        ))),
        (Some(_), None) => Some(Err(format!(
            "{FILEPROVIDER_DRIVE_ID_ENV} is unset while {FILEPROVIDER_GROUP_CONTAINER_ENV} is set; set both or neither"
        ))),
    }
}

pub(super) fn parse_group_config(
    group_container: PathBuf,
    drive_hex: &str,
) -> Result<GroupContainerConfig, String> {
    if !group_container.is_absolute() {
        return Err(format!(
            "{FILEPROVIDER_GROUP_CONTAINER_ENV} must be absolute: {}",
            group_container.display()
        ));
    }
    if !group_container.is_dir() {
        return Err(format!(
            "{FILEPROVIDER_GROUP_CONTAINER_ENV} names no existing container directory (the daemon never creates it): {}",
            group_container.display()
        ));
    }
    let bytes = hex::decode(drive_hex)
        .map_err(|error| format!("{FILEPROVIDER_DRIVE_ID_ENV} is not hex: {error}"))?;
    let raw: [u8; 32] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        format!(
            "{FILEPROVIDER_DRIVE_ID_ENV} must be 32 bytes (64 hex chars), got {} bytes",
            bytes.len()
        )
    })?;
    if drive_hex.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(format!("{FILEPROVIDER_DRIVE_ID_ENV} must be lowercase hex"));
    }
    Ok(GroupContainerConfig {
        group_container,
        drive: DriveId::from_bytes(raw),
    })
}

/// The App Group socket for a drive:
/// `<container>/Sockets/<fingerprint>.sock`. The filename derives
/// from [`DriveId::fileprovider_socket_fingerprint`]; the daemon is
/// the only computer of it, the Swift extension resolves the
/// [`group_symlink_path`] instead, so the derivation cannot drift
/// between the two sides.
pub fn group_socket_path(group_container: &Path, drive: &DriveId) -> PathBuf {
    group_container
        .join("Sockets")
        .join(format!("{}.sock", drive.fileprovider_socket_fingerprint()))
}

/// The stable alias the extension resolves:
/// `<container>/Sockets/by-drive/<drive-hex>.sock`, a symlink to the
/// fingerprint filename beside it. The domain carries the drive id,
/// so the extension needs no hash — and no second derivation that
/// could disagree with the daemon's.
pub fn group_symlink_path(group_container: &Path, drive: &DriveId) -> PathBuf {
    group_container
        .join("Sockets")
        .join("by-drive")
        .join(format!("{drive}.sock"))
}

/// Control metadata beside the alias:
/// `<container>/Sockets/by-drive/<drive-hex>.label`, holding the
/// registry name of the served drive. The registrar uses it for the
/// Finder domain display name; the domain identifier stays the full
/// drive id. No drive bytes, keys, or manifests go in this file.
pub fn group_label_path(group_container: &Path, drive: &DriveId) -> PathBuf {
    group_container
        .join("Sockets")
        .join("by-drive")
        .join(format!("{drive}.label"))
}

/// Bind the group socket with a live-endpoint check: an existing
/// entry that still connects is a live owner — never unlink it, fail
/// closed (`AddrInUse`) instead. A dead entry unlinks like the
/// primary's stale reclaim. The caller holds the store lock, so the
/// only competing binder is a cross-drive fingerprint collision,
/// which also fails here rather than stealing the path.
pub(super) fn bind_group_listener(socket_path: &Path) -> std::io::Result<UnixListener> {
    if socket_path.exists() {
        match UnixStream::connect(socket_path) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!(
                        "fileprovider socket already served: {}",
                        socket_path.display()
                    ),
                ));
            }
            Err(_) => {
                let _ = std::fs::remove_file(socket_path);
            }
        }
    }
    bind_listener(socket_path).map_err(|error| {
        // The team-prefixed group id leaves little headroom under the
        // platform's sun_path ceiling. Name the two things an operator
        // can change instead of surfacing a bare InvalidInput.
        std::io::Error::new(
            error.kind(),
            format!(
                "cannot bind group socket at {}: {error} (shorten WYRD_APP_GROUP or use a shorter account name)",
                socket_path.display()
            ),
        )
    })
}

/// Publish (or republish) the `by-drive` symlink beside the group
/// socket. The target is the bare fingerprint filename, relative so
/// the alias survives container moves.
pub(super) fn publish_group_symlink(
    link: &Path,
    fingerprint_file_name: &str,
) -> std::io::Result<()> {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(link);
    std::os::unix::fs::symlink(format!("../{fingerprint_file_name}"), link)?;
    Ok(())
}

/// Largest whole-file read the interface serves in one request.
/// Bounds one request's memory and the response frame together: the
/// hex encoding doubles the bytes on the wire, so 32 MiB reads stay
/// under the frame ceiling with JSON overhead to spare. Files larger
/// than this are refused with [`crate::owner::wire::WireErrorCode::TooLarge`], never
/// truncated, never streamed past the cap. Paged reads are a future
/// additive operation, not a silent behavior of this one.
pub const DEFAULT_MAX_READ_BYTES: u64 = 32 << 20;

/// Largest file content one submit may carry, in decoded bytes: the
/// commit and append bodies of the submit, summed across fold
/// members. The hex encoding doubles the bytes on the wire, so 32
/// MiB of content stays under the frame ceiling with JSON overhead
/// to spare. A submit past this bound is refused with
/// [`crate::owner::wire::WireErrorCode::TooLarge`] before any decode or queue admission
/// — with retry guidance in the message, never silence. A window
/// saves a larger file through the mount, or by chunked submission
/// (several appends or commits, each within the bound), never by a
/// single over-bound submit.
pub const MAX_SUBMIT_CONTENT_BYTES: usize = 32 << 20;

/// Maximum simultaneous owner-interface connections, shared by
/// every accept loop on one server: a second listener is a second
/// accept path, never a second budget. One thread serves each
/// connection, so the accept loops share one fixed pool: a
/// connection past the bound is closed immediately — the client
/// treats the break as unconfirmed — and an accept flood cannot
/// grow threads without bound. Idle connections hold their slot;
/// consumers keep few, short-lived connections.
pub const MAX_OWNER_CONNECTIONS: usize = 16;

/// How long the read side blocks registering demand before failing
/// a remote-only read. Matches the backend's bounded open: demand
/// waits, it never hangs the consumer past this.
pub const READ_DEMAND_TIMEOUT: Duration = Duration::from_secs(30);
