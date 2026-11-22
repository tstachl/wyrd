//! The per-user drive registry: schema, validation, locked I/O.
//! Entries are untrusted configuration; every read validates and
//! skips what does not parse. The parent module owns the
//! authority contract and re-exports every public item here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Registry schema version. Bump only with a migration that reads
/// the previous version; readers refuse newer versions loudly
/// rather than guessing.
pub const REGISTRY_VERSION: u32 = 1;

/// Registry filename, fixed so installer, command, and desktop read
/// the same path derivation.
pub const REGISTRY_FILENAME: &str = "drives.json";

/// Supervision mode for one registered drive: the headless `vault`
/// owner, or the `mount` mode that serves the FUSE projection beside
/// the same owner interface. Unknown modes from newer writers are
/// skipped per entry with a reason — a future mode must never fail
/// the whole file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceMode {
    /// Headless owner service without a FUSE mount.
    Vault,
    /// Owner service plus the FUSE projection at the mountpoint.
    Mount,
    /// Forward-compatible mode from a newer writer; skipped with a reason.
    Unknown(String),
}

impl Serialize for ServiceMode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ServiceMode::Vault => serializer.serialize_str("vault"),
            ServiceMode::Mount => serializer.serialize_str("mount"),
            ServiceMode::Unknown(mode) => serializer.serialize_str(mode),
        }
    }
}

impl<'de> Deserialize<'de> for ServiceMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mode = String::deserialize(deserializer)?;
        match mode.as_str() {
            "vault" => Ok(ServiceMode::Vault),
            "mount" => Ok(ServiceMode::Mount),
            other => Ok(ServiceMode::Unknown(other.to_string())),
        }
    }
}

/// One registered drive: facts for discovery and supervision, never
/// secrets. `drive_dir` must be absolute; `name` and `service` are
/// `[a-z0-9-]` so service labels and unit names derive without
/// quoting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Drive name; `[a-z0-9-]` token used for the service name.
    pub name: String,
    /// Absolute drive directory holding the store and socket.
    pub drive_dir: PathBuf,
    /// Relay URLs peers dial to reach this drive.
    pub relays: Vec<String>,
    /// Service token deriving the per-drive service definition name.
    pub service: String,
    /// Supervision mode selecting vault or mount argv.
    pub mode: ServiceMode,
    /// FUSE mountpoint. `Some` in `mount` mode, `None` in `vault`
    /// mode — a vault entry carrying one is invalid, like a mount
    /// entry missing one. Optional with a default so pre-mount
    /// registry files decode with `None`.
    #[serde(default)]
    pub mountpoint: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RegistryFile {
    version: u32,
    /// Decoded per entry by the reader: structurally corrupt values
    /// fail the whole file (as before), while entries that decode
    /// but fail validation skip per entry with a reason — and are
    /// carried forward verbatim by the next write instead of being
    /// deleted (see `write_registry`).
    drives: BTreeMap<String, serde_json::Value>,
}

/// What reading the registry reports: the valid entries plus one
/// reason per skipped entry. Skips keep discovery working when one
/// entry rots; the reasons surface so a rotten entry is repaired,
/// not silently forgotten.
#[derive(Debug, Default)]
pub struct Registry {
    /// Valid entries keyed by registry name.
    pub drives: BTreeMap<String, RegistryEntry>,
    /// Per-entry skip reasons for entries that failed validation.
    pub skipped: Vec<String>,
}

/// Every way the registry and service-definition I/O in this
/// module can fail. Provisioning also fails at engine
/// initialization and socket verification with other error text;
/// those paths are outside this type. Variants carry
/// the failing identity — paths, names, labels — so the desktop
/// backend can branch on failure instead of parsing text.
/// `Display` reproduces the historical message text exactly: the CLI
/// renders `reason` verbatim and the desktop shell reads the same
/// text. Batch discipline: [`read_registry`] counts validation
/// failures as per-entry skip reasons and keeps going, while
/// [`write_registry`] raises them and writes nothing.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// The registry file could not be read.
    #[error("read {}: {source}", path.display())]
    Read {
        /// Registry path that could not be read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The registry file is not valid JSON.
    #[error("parse {}: {source}", path.display())]
    Parse {
        /// Registry path that failed to parse.
        path: PathBuf,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// The registry schema version is newer than this build reads.
    #[error("registry {} is version {found}, this build reads {REGISTRY_VERSION}", path.display())]
    UnsupportedVersion {
        /// Registry path with an unsupported schema version.
        path: PathBuf,
        /// Version found in the file.
        found: u32,
    },
    /// A registry key does not match its entry's name.
    #[error("key {key:?} does not match entry name {name:?}")]
    KeyMismatch {
        /// Registry key whose entry mismatched.
        key: String,
        /// Entry name that did not match the key.
        name: String,
    },
    /// A drive name breaks the `[a-z0-9-]` token rule.
    #[error("name {name:?} must be 1-64 chars of [a-z0-9-]")]
    InvalidName {
        /// Drive name that broke the token rule.
        name: String,
    },
    /// A drive name leaves no room for the `drive-` service prefix.
    #[error("name {name:?} leaves no room for the drive- service prefix (max 58 chars)")]
    NameTooLongForServicePrefix {
        /// Drive name leaving no room for the service prefix.
        name: String,
    },
    /// A service name breaks the `[a-z0-9-]` token rule.
    #[error("service {service:?} must be 1-64 chars of [a-z0-9-]")]
    InvalidService {
        /// Service token that broke the token rule.
        service: String,
    },
    /// A drive directory is not absolute.
    #[error("drive_dir {} must be absolute", dir.display())]
    RelativeDriveDir {
        /// Drive directory that was not absolute.
        dir: PathBuf,
    },
    /// A drive directory escapes through `..`.
    #[error("drive_dir {} must not contain ..", dir.display())]
    DriveDirHasDotDot {
        /// Drive directory escaping through `..`.
        dir: PathBuf,
    },
    /// The entry names a service mode this build does not manage.
    #[error("unknown service mode: this build manages `vault` and `mount`")]
    UnknownServiceMode,
    /// A `vault` entry carries a mountpoint it never uses.
    #[error("vault mode carries no mountpoint: remove the mountpoint for a vault entry")]
    UnexpectedMountpoint,
    /// A `mount` entry names no mountpoint.
    #[error("mount mode needs a mountpoint: pass --mountpoint with an absolute path")]
    MissingMountpoint,
    /// A mountpoint breaks the path rules.
    #[error("invalid mountpoint: {reason}")]
    InvalidMountpoint {
        /// Why the mountpoint was refused.
        reason: String,
    },
    /// A managed service with no relays to serve: a relay-less drive
    /// serves nothing peers can discover.
    #[error("managing a service needs at least one --relay: a relay-less drive serves nothing peers can discover; register with --no-service instead")]
    ManagedWithoutRelays,
    /// The drive directory resolves to several registry keys under
    /// the write lock: a concurrent registration duplicated the
    /// entry between the pre-stop resolve and the rewrite.
    #[error(
        "conflicting registry entries for {dir}: {keys} — resolve the duplicate by hand first"
    )]
    ConflictingEntries {
        /// Drive directory resolving to several registry keys.
        dir: PathBuf,
        /// Conflicting registry keys.
        keys: String,
    },
    /// A relay URL is empty or carries whitespace/control bytes.
    #[error("relays must be non-empty URLs without whitespace or control bytes")]
    InvalidRelays,
    /// A drive directory carries control characters.
    #[error("drive_dir {} must not contain control characters", dir.display())]
    DriveDirHasControlChars {
        /// Drive directory carrying control characters.
        dir: PathBuf,
    },
    /// The candidate set fails validation: nothing was written.
    #[error("refusing to write: {source}")]
    RefusedWrite {
        /// Underlying validation failure; nothing was written.
        #[source]
        source: Box<BootstrapError>,
    },
    /// The registry could not be encoded.
    #[error("encode registry: {source}")]
    Encode {
        /// Underlying JSON encoding error.
        #[source]
        source: serde_json::Error,
    },
    /// A directory for the registry or service file could not be created.
    #[error("create {}: {source}", path.display())]
    CreateDir {
        /// Directory that could not be created.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The registry lockfile could not be opened or locked.
    #[error("lock {}: {source}", path.display())]
    LockFile {
        /// Lockfile that could not be opened or locked.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A registry or service file could not be written.
    #[error("write {}: {source}", path.display())]
    WriteFile {
        /// File that could not be written.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The atomic replace of a registry or service file failed.
    #[error("replace {}: {source}", path.display())]
    ReplaceFile {
        /// Destination that the atomic replace failed on.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Owner-only permissions could not be applied.
    #[error("chmod {}: {source}", path.display())]
    Chmod {
        /// File whose owner-only permissions could not be applied.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A written file could not be synced to disk.
    #[error("sync {}: {source}", path.display())]
    SyncFile {
        /// File that could not be synced to disk.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A service definition file could not be removed when clearing
    /// a reconfigured drive.
    #[error("remove {}: {source}", path.display())]
    RemoveFile {
        /// Service definition file that could not be removed.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A service manager command could not be spawned.
    #[error("spawn {program}: {source}")]
    Spawn {
        /// Service manager program that could not be spawned.
        program: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A service manager command ran and failed.
    #[error("{program} {args} failed: {detail}")]
    CommandFailed {
        /// Service manager program that ran and failed.
        program: String,
        /// Arguments it ran with.
        args: String,
        /// Captured failure detail.
        detail: String,
    },
    /// Enabling the service through its manager failed.
    #[error("enable {name}: {source}")]
    EnableService {
        /// Service name that failed to enable.
        name: String,
        /// Underlying bootstrap failure.
        #[source]
        source: Box<BootstrapError>,
    },
    /// Lingering is enabled for this user: the user manager (and a
    /// managed unit) would survive logout, violating the desktop
    /// session bound. Refusal only — Wyrd never changes lingering
    /// configuration; disable it by hand and retry, or run this
    /// drive outside a login session via `--no-service`
    /// registration with independent supervision of
    /// `wyrd owner start`.
    #[error("lingering is enabled for UID {uid}: a managed unit would survive logout, violating the desktop session bound — run `loginctl disable-linger {uid}` (requires privilege) and retry; Wyrd never changes lingering configuration. For headless operation without a login session, register with `--no-service` and supervise `wyrd owner start` independently: managed user-scope services are desktop lifecycle infrastructure")]
    LingeringEnabled {
        /// UID whose user manager lingers past logout.
        uid: u32,
    },
    /// Lingering state could not be determined on a systemd host.
    /// Refusal, never an assumed safe default: enabling login-start
    /// without knowing whether the manager lingers could silently
    /// violate the session bound.
    #[error("cannot determine lingering state for UID {uid}: {detail} — refusing rather than assuming the session bound holds. Managed user-scope services are desktop lifecycle infrastructure; for headless operation without a login session, register with `--no-service` and supervise `wyrd owner start` independently")]
    LingerUnknown {
        /// UID whose lingering state was probed.
        uid: u32,
        /// What the probe ran into.
        detail: String,
    },
    /// Reloading the user service manager failed.
    #[error("reload user units: {source}")]
    ReloadUnits {
        /// Underlying failure reloading the user service manager.
        #[source]
        source: Box<BootstrapError>,
    },
    /// The managed service is not loaded after enablement.
    #[error("start {label}: agent not loaded: {source}")]
    AgentNotLoaded {
        /// Launchd agent label that was not loaded.
        label: String,
        /// Underlying bootstrap failure.
        #[source]
        source: Box<BootstrapError>,
    },
    /// Restarting the managed service failed.
    #[error("restart {unit}: {source}")]
    RestartService {
        /// Systemd unit that failed to restart.
        unit: String,
        /// Underlying bootstrap failure.
        #[source]
        source: Box<BootstrapError>,
    },
    /// Stopping (disabling) or clearing the managed service before
    /// a reconfiguration failed.
    #[error("stop {name}: {source}")]
    StopService {
        /// Service name that failed to stop before reconfiguration.
        name: String,
        /// Underlying bootstrap failure.
        #[source]
        source: Box<BootstrapError>,
    },
    /// Per-drive services exist only on Unix managers.
    #[error("per-drive services are implemented for Unix (launchd agent, systemd user unit)")]
    ServicesUnsupported,
}

/// Where the registry lives when no explicit path is given:
/// per-user state, resolved through platform conventions. The
/// installer creates it with owner-only permissions; the
/// registration flow below is its normal writer.
pub fn default_registry_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library/Application Support/Wyrd")
                .join(REGISTRY_FILENAME);
        }
    }
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("wyrd").join(REGISTRY_FILENAME);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local/state/wyrd")
            .join(REGISTRY_FILENAME);
    }
    PathBuf::from(REGISTRY_FILENAME)
}

/// Read and validate the registry. A missing file is an empty
/// registry, not an error — a fresh installation has registered
/// nothing yet. A newer schema version refuses loudly. Invalid
/// entries are skipped with reasons; valid ones are unaffected.
pub fn read_registry(path: &Path) -> Result<Registry, BootstrapError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Registry::default());
        }
        Err(source) => {
            return Err(BootstrapError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let file: RegistryFile =
        serde_json::from_slice(&bytes).map_err(|source| BootstrapError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    if file.version != REGISTRY_VERSION {
        return Err(BootstrapError::UnsupportedVersion {
            path: path.to_path_buf(),
            found: file.version,
        });
    }
    let mut registry = Registry::default();
    for (key, value) in file.drives {
        let entry: RegistryEntry =
            serde_json::from_value(value).map_err(|source| BootstrapError::Parse {
                path: path.to_path_buf(),
                source,
            })?;
        match validate_entry(&key, &entry) {
            Ok(()) => {
                registry.drives.insert(key, entry);
            }
            Err(error) => registry.skipped.push(format!("entry {key:?}: {error}")),
        }
    }
    Ok(registry)
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Relay URLs are non-empty and carry no whitespace or control
/// bytes. The managed path additionally requires a non-empty set;
/// keeping the predicate separate lets reconfiguration validate the
/// candidate values before anything stops.
pub(super) fn valid_relays(relays: &[String]) -> bool {
    !relays.iter().any(|relay| {
        relay.is_empty()
            || relay
                .bytes()
                .any(|byte| byte < 0x20 || byte == 0x7f || byte.is_ascii_whitespace())
    })
}

pub(super) fn validate_entry(key: &str, entry: &RegistryEntry) -> Result<(), BootstrapError> {
    if key != entry.name {
        return Err(BootstrapError::KeyMismatch {
            key: key.to_string(),
            name: entry.name.clone(),
        });
    }
    if !valid_token(&entry.name) {
        return Err(BootstrapError::InvalidName {
            name: entry.name.clone(),
        });
    }
    if entry.name.len() + "drive-".len() > 64 {
        return Err(BootstrapError::NameTooLongForServicePrefix {
            name: entry.name.clone(),
        });
    }
    if !valid_token(&entry.service) {
        return Err(BootstrapError::InvalidService {
            service: entry.service.clone(),
        });
    }
    if !entry.drive_dir.is_absolute() {
        return Err(BootstrapError::RelativeDriveDir {
            dir: entry.drive_dir.clone(),
        });
    }
    if has_dotdot(&entry.drive_dir) {
        return Err(BootstrapError::DriveDirHasDotDot {
            dir: entry.drive_dir.clone(),
        });
    }
    match (&entry.mode, &entry.mountpoint) {
        (ServiceMode::Vault, Some(_)) => {
            return Err(BootstrapError::UnexpectedMountpoint);
        }
        (ServiceMode::Vault, None) => {}
        (ServiceMode::Mount, None) => {
            return Err(BootstrapError::MissingMountpoint);
        }
        (ServiceMode::Mount, Some(mountpoint)) => {
            validate_mountpoint(&entry.drive_dir, mountpoint)?;
        }
        (ServiceMode::Unknown(_), _) => {
            return Err(BootstrapError::UnknownServiceMode);
        }
    }
    if !valid_relays(&entry.relays) {
        return Err(BootstrapError::InvalidRelays);
    }
    if entry
        .drive_dir
        .to_string_lossy()
        .bytes()
        .any(|byte| byte < 0x20 || byte == 0x7f)
    {
        return Err(BootstrapError::DriveDirHasControlChars {
            dir: entry.drive_dir.clone(),
        });
    }
    Ok(())
}

fn has_dotdot(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}

/// Live mountpoint checks for the provision/reconfigure validate
/// stage — never for `validate_entry`: the mountpoint must exist as
/// a directory (a typo would install a definition that exits
/// immediately and restart-loops until the socket timeout), and must
/// neither contain the drive directory nor sit inside it (the
/// projection would hide the store or shadow part of it). Shape
/// problems stay in `validate_entry` so the read path keeps
/// reporting a missing mountpoint as a skip — a loud verdict on
/// what is registered — instead of hiding an otherwise valid entry.
pub(super) fn validate_mountpoint_live(
    drive_dir: &Path,
    mountpoint: &Path,
) -> Result<(), BootstrapError> {
    if !mountpoint.is_dir() {
        return Err(BootstrapError::InvalidMountpoint {
            reason: format!(
                "mountpoint {} does not exist or is not a directory",
                mountpoint.display()
            ),
        });
    }
    if drive_dir.starts_with(mountpoint) {
        return Err(BootstrapError::InvalidMountpoint {
            reason: format!(
                "mountpoint {} must not contain the drive directory {}",
                mountpoint.display(),
                drive_dir.display()
            ),
        });
    }
    if mountpoint.starts_with(drive_dir) {
        return Err(BootstrapError::InvalidMountpoint {
            reason: format!(
                "mountpoint {} must not sit inside the drive directory {}",
                mountpoint.display(),
                drive_dir.display()
            ),
        });
    }
    Ok(())
}

/// Mountpoint path rules, mirroring the drive-directory rules: the
/// service template renders the mountpoint into the unit, so a
/// relative, escaping, or control-carrying path is refused before
/// anything is written. A mountpoint equal to the drive directory
/// would mount the projection over the store itself, so it refuses
/// too.
pub(super) fn validate_mountpoint(
    drive_dir: &Path,
    mountpoint: &Path,
) -> Result<(), BootstrapError> {
    if !mountpoint.is_absolute() {
        return Err(BootstrapError::InvalidMountpoint {
            reason: format!("mountpoint {} must be absolute", mountpoint.display()),
        });
    }
    if has_dotdot(mountpoint) {
        return Err(BootstrapError::InvalidMountpoint {
            reason: format!("mountpoint {} must not contain ..", mountpoint.display()),
        });
    }
    if mountpoint
        .to_string_lossy()
        .bytes()
        .any(|byte| byte < 0x20 || byte == 0x7f)
    {
        return Err(BootstrapError::InvalidMountpoint {
            reason: format!(
                "mountpoint {} must not contain control characters",
                mountpoint.display()
            ),
        });
    }
    if mountpoint == drive_dir {
        return Err(BootstrapError::InvalidMountpoint {
            reason: format!(
                "mountpoint {} must not equal the drive directory",
                mountpoint.display()
            ),
        });
    }
    Ok(())
}

/// The inter-writer serialization for the registry: the registration
/// flow (this module) and the desktop shell both hold an exclusive
/// advisory lock on this sibling for the whole read-modify-write, so
/// concurrent rewrites serialize instead of losing entries. Atomic
/// rename alone does not stop lost updates — two writers reading one
/// base and renaming in turn leave only the second write's entries —
/// so every writer must hold this lock. The lockfile is never
/// renamed, so the lock always names a stable inode; the kernel
/// releases it on crash, so no stale lock can wedge either writer.
/// Blocking is deliberate: registry writes are rare, and waiting
/// beats losing. Both writers derive this path the same way, so no
/// configuration can point them at different locks.
pub fn registry_lock_path(path: &Path) -> PathBuf {
    path.with_extension("lock")
}

/// Hold the registry's exclusive lock, creating the lockfile
/// owner-only on first use. The returned handle keeps the lock;
/// dropping it releases.
fn lock_registry(path: &Path) -> Result<std::fs::File, BootstrapError> {
    let lock_path = registry_lock_path(path);
    if let Some(parent) = lock_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| BootstrapError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|source| BootstrapError::LockFile {
            path: lock_path.clone(),
            source,
        })?;
    restrict_owner_only(&lock_path)?;
    file.lock().map_err(|source| BootstrapError::LockFile {
        path: lock_path,
        source,
    })?;
    Ok(file)
}

/// Write the registry crash-safely: temporary file in the same
/// directory, sync it, atomic replace, sync the directory. An
/// interrupted write can never leave a truncated registry that
/// makes every drive undiscoverable.
///
/// Entries the reader skipped (unknown modes, hand edits that fail
/// validation) are carried forward verbatim from the existing file:
/// a rewrite must never delete what this build does not understand.
/// Keys the new map carries win; only absent keys merge back, so a
/// re-registration that repairs an entry takes effect. A missing
/// file contributes nothing; a present-but-unreadable one (malformed
/// JSON, newer schema, structurally corrupt entry) refuses the
/// write — the reader reports such files loudly and the writer
/// matches. The candidate map itself is fully validated and refuses
/// the write on any invalid entry.
///
/// Replacement through the same locked path `modify_registry`
/// provides: candidates validate before the lock (so a refusal
/// touches nothing), then the replacement, the carry-forward merge,
/// and the crash-safe rename run under one lock hold. Keys the
/// candidate map drops come back from the carried file verbatim —
/// this replaces the valid set, it does not delete carried keys.
pub fn write_registry(
    path: &Path,
    drives: &BTreeMap<String, RegistryEntry>,
) -> Result<(), BootstrapError> {
    // Serialize against the desktop shell's writes (and concurrent
    // registration flows): validation precedes the lock, and the
    // lock covers the read below, the replacement, the merge, and
    // the rename, so two writers never interleave on one base. Held
    // to end of scope — through the directory sync.
    for (key, entry) in drives {
        validate_entry(key, entry).map_err(|source| BootstrapError::RefusedWrite {
            source: Box::new(source),
        })?;
    }
    modify_registry(path, |valid| {
        *valid = drives.clone();
        Ok(())
    })
    .map(|_| ())
}

/// Locked read-modify-write for the registry call sites (create,
/// reconfigure): the updater closure runs against the fresh valid
/// map while the registry lock is held, and the merge plus the
/// crash-safe rewrite happen under the same lock hold before it
/// releases. Callers that read outside the lock and pass whole maps
/// into `write_registry` can clobber a concurrent same-key update
/// with their stale copy — keys still converge (the under-lock
/// re-read carries absent keys), but the concurrent update to an
/// already-present key is lost. Running the transformation under
/// the lock closes that window: concurrent same-key updates
/// serialize, and the last writer wins cleanly with no torn mix.
///
/// The closure contract, by necessity of the lock it runs under:
///
/// - it runs while the cross-process registry lock is held, so it
///   must stay non-blocking and do no I/O — file reads, socket
///   probes, and service waits all happen outside;
/// - it sees only the validated entries (skipped raws are invisible
///   to it) and returns its result; the whole resulting map is then
///   validated like `write_registry` validates its candidates;
/// - it cannot delete: keys the resulting map drops come back from
///   the carried raws verbatim (a future `forget` needs an explicit
///   deletion path, not this primitive).
///
/// Carried entries merge verbatim, and the post-write skipped reasons
/// return alongside — so the caller surfaces rotten data from the
/// file it actually wrote, not from a pre-lock read. Crate-private:
/// the CLI drives this through `provision` and `reconfigure`, and no
/// other crate may take the lock.
pub(crate) fn modify_registry<T>(
    path: &Path,
    modify: impl FnOnce(&mut BTreeMap<String, RegistryEntry>) -> Result<T, BootstrapError>,
) -> Result<(T, Vec<String>), BootstrapError> {
    // One lock hold for the read, the transformation, the merge,
    // and the rename — the same serialization `write_registry`
    // provides, extended over the caller's transformation so no
    // stale copy can interleave between its read and its write.
    let _lock = lock_registry(path)?;
    let mut raws: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut valid: BTreeMap<String, RegistryEntry> = BTreeMap::new();
    match std::fs::read(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(BootstrapError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
        Ok(existing) => {
            let file: RegistryFile =
                serde_json::from_slice(&existing).map_err(|source| BootstrapError::Parse {
                    path: path.to_path_buf(),
                    source,
                })?;
            if file.version != REGISTRY_VERSION {
                return Err(BootstrapError::UnsupportedVersion {
                    path: path.to_path_buf(),
                    found: file.version,
                });
            }
            for (key, value) in file.drives {
                match serde_json::from_value::<RegistryEntry>(value.clone()) {
                    Err(source) => {
                        return Err(BootstrapError::Parse {
                            path: path.to_path_buf(),
                            source,
                        });
                    }
                    Ok(entry) => match validate_entry(&key, &entry) {
                        Ok(()) => {
                            valid.insert(key.clone(), entry);
                            raws.insert(key, value);
                        }
                        Err(_) => {
                            raws.insert(key, value);
                        }
                    },
                }
            }
        }
    }
    let result = modify(&mut valid)?;
    for (key, entry) in &valid {
        validate_entry(key, entry).map_err(|source| BootstrapError::RefusedWrite {
            source: Box::new(source),
        })?;
    }
    let mut merged: BTreeMap<String, serde_json::Value> = valid
        .iter()
        .map(|(key, entry)| {
            serde_json::to_value(entry)
                .map(|value| (key.clone(), value))
                .map_err(|source| BootstrapError::Encode { source })
        })
        .collect::<Result<_, _>>()?;
    for (key, value) in raws {
        merged.entry(key).or_insert(value);
    }
    let file = RegistryFile {
        version: REGISTRY_VERSION,
        drives: merged.clone(),
    };
    let bytes =
        serde_json::to_vec_pretty(&file).map_err(|source| BootstrapError::Encode { source })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| BootstrapError::CreateDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, &bytes).map_err(|source| BootstrapError::WriteFile {
        path: tmp.clone(),
        source,
    })?;
    restrict_owner_only(&tmp)?;
    sync_file(&tmp)?;
    std::fs::rename(&tmp, path).map_err(|source| BootstrapError::ReplaceFile {
        path: path.to_path_buf(),
        source,
    })?;
    restrict_owner_only(path)?;
    sync_dir(path);
    // Fresh skipped reasons from the file just written: every merged
    // value that validates is valid, the rest is skipped with its
    // reason — the same verdict `read_registry` would report.
    let mut skipped = Vec::new();
    for (key, value) in &merged {
        let entry: Result<RegistryEntry, _> = serde_json::from_value(value.clone());
        match entry {
            Ok(entry) => {
                if let Err(error) = validate_entry(key, &entry) {
                    skipped.push(format!("entry {key:?}: {error}"));
                }
            }
            Err(source) => {
                skipped.push(format!(
                    "entry {key:?}: {}",
                    BootstrapError::Parse {
                        path: path.to_path_buf(),
                        source,
                    }
                ));
            }
        }
    }
    Ok((result, skipped))
}

/// Owner-only file permissions (Unix): the registry holds drive
/// facts, not secrets, but it is still per-user state — no group or
/// other access, ever.
#[cfg(unix)]
pub(super) fn restrict_owner_only(path: &Path) -> Result<(), BootstrapError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
        BootstrapError::Chmod {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
pub(super) fn restrict_owner_only(_path: &Path) -> Result<(), BootstrapError> {
    Ok(())
}

pub(super) fn sync_file(path: &Path) -> Result<(), BootstrapError> {
    let file = std::fs::File::open(path).map_err(|source| BootstrapError::SyncFile {
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| BootstrapError::SyncFile {
        path: path.to_path_buf(),
        source,
    })
}

fn sync_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}
