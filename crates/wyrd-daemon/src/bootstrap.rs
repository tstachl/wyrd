//! First-drive provisioning: the per-user drive registry, the
//! OS service handoff, and the fixed initialize-then-register order.
//!
//! The contract lives in `docs/design/desktop-shell.md` (bootstrap
//! section): a registry file the installer creates and two writers
//! share — the registration flow and the desktop shell. Never a
//! manager daemon, never a discovery protocol. This module is the
//! registry as code plus the provisioning orchestration the `wyrd
//! bootstrap` command drives. The desktop backend writes the same
//! file through its own backend (which cannot link this crate); both
//! writers serialize on the registry lockfile, so concurrent rewrites
//! converge instead of losing entries.
//!
//! Three rules hold throughout:
//!
//! - The registry holds drive facts, never secrets. Entries name a
//!   directory, a relay set, and a service; key material stays in
//!   the OS keychain and credential files (settings contract).
//! - Registration and service enablement happen only after a
//!   successful initialization. A drive is reported ready only when
//!   its socket verifies this session — never because its registry
//!   entry exists.
//! - Entries are untrusted configuration. Every read validates and
//!   skips what does not parse; every connection still
//!   authenticates through the owner interface's own checks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::{Engine, EngineError};

use crate::owner::client::{ClientError, OwnerClient};
use crate::owner::wire::WireErrorCode;

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
    Vault,
    Mount,
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
    pub name: String,
    pub drive_dir: PathBuf,
    pub relays: Vec<String>,
    pub service: String,
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
    pub drives: BTreeMap<String, RegistryEntry>,
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
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The registry file is not valid JSON.
    #[error("parse {}: {source}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// The registry schema version is newer than this build reads.
    #[error("registry {} is version {found}, this build reads {REGISTRY_VERSION}", path.display())]
    UnsupportedVersion { path: PathBuf, found: u32 },
    /// A registry key does not match its entry's name.
    #[error("key {key:?} does not match entry name {name:?}")]
    KeyMismatch { key: String, name: String },
    /// A drive name breaks the `[a-z0-9-]` token rule.
    #[error("name {name:?} must be 1-64 chars of [a-z0-9-]")]
    InvalidName { name: String },
    /// A drive name leaves no room for the `drive-` service prefix.
    #[error("name {name:?} leaves no room for the drive- service prefix (max 58 chars)")]
    NameTooLongForServicePrefix { name: String },
    /// A service name breaks the `[a-z0-9-]` token rule.
    #[error("service {service:?} must be 1-64 chars of [a-z0-9-]")]
    InvalidService { service: String },
    /// A drive directory is not absolute.
    #[error("drive_dir {} must be absolute", dir.display())]
    RelativeDriveDir { dir: PathBuf },
    /// A drive directory escapes through `..`.
    #[error("drive_dir {} must not contain ..", dir.display())]
    DriveDirHasDotDot { dir: PathBuf },
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
    InvalidMountpoint { reason: String },
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
    ConflictingEntries { dir: PathBuf, keys: String },
    /// A relay URL is empty or carries whitespace/control bytes.
    #[error("relays must be non-empty URLs without whitespace or control bytes")]
    InvalidRelays,
    /// A drive directory carries control characters.
    #[error("drive_dir {} must not contain control characters", dir.display())]
    DriveDirHasControlChars { dir: PathBuf },
    /// The candidate set fails validation: nothing was written.
    #[error("refusing to write: {source}")]
    RefusedWrite {
        #[source]
        source: Box<BootstrapError>,
    },
    /// The registry could not be encoded.
    #[error("encode registry: {source}")]
    Encode {
        #[source]
        source: serde_json::Error,
    },
    /// A directory for the registry or service file could not be created.
    #[error("create {}: {source}", path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The registry lockfile could not be opened or locked.
    #[error("lock {}: {source}", path.display())]
    LockFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A registry or service file could not be written.
    #[error("write {}: {source}", path.display())]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The atomic replace of a registry or service file failed.
    #[error("replace {}: {source}", path.display())]
    ReplaceFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Owner-only permissions could not be applied.
    #[error("chmod {}: {source}", path.display())]
    Chmod {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A written file could not be synced to disk.
    #[error("sync {}: {source}", path.display())]
    SyncFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A service definition file could not be removed when clearing
    /// a reconfigured drive.
    #[error("remove {}: {source}", path.display())]
    RemoveFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A service manager command could not be spawned.
    #[error("spawn {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    /// A service manager command ran and failed.
    #[error("{program} {args} failed: {detail}")]
    CommandFailed {
        program: String,
        args: String,
        detail: String,
    },
    /// Enabling the service through its manager failed.
    #[error("enable {name}: {source}")]
    EnableService {
        name: String,
        #[source]
        source: Box<BootstrapError>,
    },
    /// Reloading the user service manager failed.
    #[error("reload user units: {source}")]
    ReloadUnits {
        #[source]
        source: Box<BootstrapError>,
    },
    /// The managed service is not loaded after enablement.
    #[error("start {label}: agent not loaded: {source}")]
    AgentNotLoaded {
        label: String,
        #[source]
        source: Box<BootstrapError>,
    },
    /// Restarting the managed service failed.
    #[error("restart {unit}: {source}")]
    RestartService {
        unit: String,
        #[source]
        source: Box<BootstrapError>,
    },
    /// Stopping (disabling) or clearing the managed service before
    /// a reconfiguration failed.
    #[error("stop {name}: {source}")]
    StopService {
        name: String,
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
fn valid_relays(relays: &[String]) -> bool {
    !relays.iter().any(|relay| {
        relay.is_empty()
            || relay
                .bytes()
                .any(|byte| byte < 0x20 || byte == 0x7f || byte.is_ascii_whitespace())
    })
}

fn validate_entry(key: &str, entry: &RegistryEntry) -> Result<(), BootstrapError> {
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
fn validate_mountpoint_live(drive_dir: &Path, mountpoint: &Path) -> Result<(), BootstrapError> {
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
fn validate_mountpoint(drive_dir: &Path, mountpoint: &Path) -> Result<(), BootstrapError> {
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
/// Replacement through the same locked path [`modify_registry`]
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
fn restrict_owner_only(path: &Path) -> Result<(), BootstrapError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
        BootstrapError::Chmod {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn restrict_owner_only(_path: &Path) -> Result<(), BootstrapError> {
    Ok(())
}

fn sync_file(path: &Path) -> Result<(), BootstrapError> {
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

/// XML-escape one plist string value. Paths, relays, and binary
/// locations are operator input; the template must not break or
/// reinterpret them.
fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Shell-quote one systemd `ExecStart` argument. systemd splits the
/// line itself, so arguments with spaces, quotes, or backslashes are
/// double-quoted with backslash escapes. `%` is doubled first:
/// systemd expands bare specifiers (`%i`, `%n`, …) even inside
/// quotes, and a relay or path must never become a specifier.
fn systemd_quote(arg: &str) -> String {
    let escaped = arg.replace('%', "%%");
    if escaped
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_./:=,".contains(&byte))
    {
        return escaped;
    }
    let mut quoted = String::with_capacity(escaped.len() + 2);
    quoted.push('"');
    for ch in escaped.chars() {
        if ch == '"' || ch == '\\' {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

/// The supervised owner command: the `wyrd-supervise` wrapper from
/// `nix/drive` (OD-34-C) around `wyrd owner start` or `wyrd mount` with
/// file credentials. Managers cannot distinguish the binary's
/// collapsed exit codes on their own — every CLI failure is 2 except
/// the terminal held-drive 78 — so the wrapper maps the wait status
/// onto the exit the supervisor's policy wants (restart-worthy per
/// manager, 78 never). Routing through the wrapper is what makes the
/// restart policy below truthful; a bare owner binary cannot express
/// it. The mount argv mirrors the owner argv with the mount
/// subcommand, its mountpoint, and the mount posture sidecar — the
/// restart contract is identical, since a held mount is terminal the
/// same way a held vault is.
#[derive(Debug, Clone)]
pub struct SupervisedOwner {
    /// `wyrd-supervise` (`nix/drive/supervise.py`): same installer
    /// layout as the `wyrd` binary, overridable per command.
    pub supervise_bin: PathBuf,
    /// The `wyrd` binary whose `vault` or `mount` subcommand serves
    /// the drive and binds the interface socket.
    pub wyrd_bin: PathBuf,
    /// Hardened credential files, resolved the way `load_credentials`
    /// resolves them. Paths, not secrets, travel here — the service
    /// definition names the same files the operator already keeps.
    pub identity_file: PathBuf,
    pub passphrase_file: PathBuf,
}

impl SupervisedOwner {
    /// The full supervised argv: wrapper, per-manager restart
    /// status, posture path, `--` separator, then the owner or mount
    /// command. Owner and mount share the credential and relay flags;
    /// mount adds its mountpoint and serves the mount posture sidecar.
    pub fn argv(&self, entry: &RegistryEntry, restart_status: &str) -> Vec<String> {
        let posture = match entry.mode {
            ServiceMode::Mount => "mount.posture",
            _ => "vault.posture",
        };
        let mut argv = vec![
            self.supervise_bin.to_string_lossy().into_owned(),
            "--restart-status".to_string(),
            restart_status.to_string(),
            "--posture-path".to_string(),
            entry.drive_dir.join(posture).to_string_lossy().into_owned(),
            "--".to_string(),
            self.wyrd_bin.to_string_lossy().into_owned(),
        ];
        match entry.mode {
            ServiceMode::Mount => {
                argv.push("mount".to_string());
                argv.push(entry.drive_dir.to_string_lossy().into_owned());
                argv.push(
                    entry
                        .mountpoint
                        .as_ref()
                        .expect("mount entry carries a mountpoint: validate before argv")
                        .to_string_lossy()
                        .into_owned(),
                );
                argv.push("--identity-file".to_string());
                argv.push(self.identity_file.to_string_lossy().into_owned());
                argv.push("--passphrase-file".to_string());
                argv.push(self.passphrase_file.to_string_lossy().into_owned());
                for relay in &entry.relays {
                    argv.push("--relay".to_string());
                    argv.push(relay.clone());
                }
            }
            _ => {
                // Parent flags precede the subcommand: clap parses
                // `owner <dir> [flags] start`, never flags after `start`.
                argv.push("owner".to_string());
                argv.push(entry.drive_dir.to_string_lossy().into_owned());
                argv.push("--identity-file".to_string());
                argv.push(self.identity_file.to_string_lossy().into_owned());
                argv.push("--passphrase-file".to_string());
                argv.push(self.passphrase_file.to_string_lossy().into_owned());
                for relay in &entry.relays {
                    argv.push("--relay".to_string());
                    argv.push(relay.clone());
                }
                argv.push("start".to_string());
            }
        }
        argv
    }
}

/// Render the per-drive launchd agent property list. `KeepAlive`
/// with `SuccessfulExit` restarts the wrapper's exit 0 (unexpected
/// death mapped restart-worthy) and never its 78 (terminal held
/// drive) — the `nix/drive` semantics, which only hold because the
/// wrapper normalizes the child's collapsed codes first.
pub fn render_launchd_plist(label: &str, owner: &SupervisedOwner, entry: &RegistryEntry) -> String {
    let mut argv = String::new();
    for arg in owner.argv(entry, "0") {
        argv.push_str(&format!("    <string>{}</string>\n", xml_escape(&arg)));
    }
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
            "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
            "<plist version=\"1.0\">\n",
            "<dict>\n",
            "    <key>Label</key>\n",
            "    <string>{label}</string>\n",
            "    <key>ProgramArguments</key>\n",
            "    <array>\n{argv}",
            "    </array>\n",
            "    <key>WorkingDirectory</key>\n",
            "    <string>/</string>\n",
            "    <key>RunAtLoad</key>\n",
            "    <true/>\n",
            "    <key>KeepAlive</key>\n",
            "    <dict>\n",
            "        <key>SuccessfulExit</key>\n",
            "        <true/>\n",
            "    </dict>\n",
            "    <key>ThrottleInterval</key>\n",
            "    <integer>30</integer>\n",
            "    <key>StandardOutPath</key>\n",
            "    <string>{log}</string>\n",
            "    <key>StandardErrorPath</key>\n",
            "    <string>{log}</string>\n",
            "</dict>\n",
            "</plist>\n"
        ),
        label = xml_escape(label),
        argv = argv,
        log = xml_escape(&entry.drive_dir.join("service.log").to_string_lossy()),
    )
}

/// Render the per-drive systemd user unit. `Restart=on-failure`
/// with `RestartPreventExitStatus=78` mirrors the `nix/drive`
/// contract against the wrapper's normalized statuses: unexpected
/// death restarts rate-limited, the terminal held drive stands down.
pub fn render_systemd_unit(
    description: &str,
    owner: &SupervisedOwner,
    entry: &RegistryEntry,
) -> String {
    let exec = owner
        .argv(entry, "1")
        .iter()
        .map(|arg| systemd_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        concat!(
            "[Unit]\n",
            "Description={description}\n",
            "After=network-online.target\n",
            "Wants=network-online.target\n\n",
            "[Service]\n",
            "Type=simple\n",
            "WorkingDirectory=/\n",
            "ExecStart={exec}\n",
            "Restart=on-failure\n",
            "RestartPreventExitStatus=78\n",
            "RestartSec=5\n",
            "StartLimitIntervalSec=300\n",
            "StartLimitBurst=3\n\n",
            "[Install]\n",
            "WantedBy=default.target\n"
        ),
        description = description,
        exec = exec,
    )
}

/// Where the per-drive service definition lives on this
/// platform: the launchd agent property list on macOS, the systemd
/// user unit elsewhere. Shared by installation (write it) and
/// reconfiguration (stop and clear it): both must name the same
/// file, or a reconfigure would disable one definition while the
/// manager loads another.
#[cfg(unix)]
fn service_definition_path(entry: &RegistryEntry) -> PathBuf {
    service_definition_path_for_service(&entry.service)
}

/// Platform definition path for one service token. `pub` (hidden)
/// for the desktop mirror test: `wyrd-desktop` duplicates this
/// derivation for observation-only probes and cannot depend on this
/// crate, so the integration test pins both sides to the same path
/// instead of letting them drift silently.
#[doc(hidden)]
pub fn service_definition_path_for_service(service: &str) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("Library/LaunchAgents");
        let label = format!("md.enesis.wyrd.{service}");
        dir.join(format!("{label}.plist"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join(".config")
            })
            .join("systemd/user");
        dir.join(format!("wyrd-{service}.service"))
    }
}

/// Install the per-drive service definition for this platform and
/// return its path. Writes atomically with owner-only permissions:
/// a half-written unit must never be what the manager loads.
/// Installation writes the definition; enabling and starting are
/// separate steps so registration can report exactly which one
/// failed.
///
/// `definition_override` names the file directly (tests); `None`
/// resolves the platform path. The reconfigure override covers
/// clearing and installing together, so the two can never name
/// different files.
#[cfg(unix)]
pub fn install_service(
    entry: &RegistryEntry,
    owner: &SupervisedOwner,
    definition_override: Option<&PathBuf>,
) -> Result<PathBuf, BootstrapError> {
    // Managed modes only: the template renders `vault` and `mount`
    // argv, so an unknown future mode or a mismatched mountpoint
    // refuses here instead of rendering a definition the manager
    // would run deaf. Provisioning and reconfiguration validate
    // before installing, so this is the backstop for direct callers.
    validate_entry(&entry.name, entry)?;
    let path = match definition_override {
        Some(path) => path.clone(),
        None => service_definition_path(entry),
    };
    let dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    std::fs::create_dir_all(&dir).map_err(|source| BootstrapError::CreateDir {
        path: dir.clone(),
        source,
    })?;
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{}", entry.service);
        write_service_file(&path, &render_launchd_plist(&label, owner, entry))?;
        Ok(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        write_service_file(
            &path,
            &render_systemd_unit(&format!("Wyrd drive {}", entry.name), owner, entry),
        )?;
        Ok(path)
    }
}

/// Atomic owner-only service write: temporary sibling, sync,
/// replace — the manager never loads a half-written definition.
#[cfg(unix)]
fn write_service_file(path: &Path, contents: &str) -> Result<(), BootstrapError> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, contents).map_err(|source| BootstrapError::WriteFile {
        path: tmp.clone(),
        source,
    })?;
    restrict_owner_only(&tmp)?;
    sync_file(&tmp)?;
    std::fs::rename(&tmp, path).map_err(|source| BootstrapError::ReplaceFile {
        path: path.to_path_buf(),
        source,
    })?;
    restrict_owner_only(path)
}

#[cfg(not(unix))]
pub fn install_service(
    _entry: &RegistryEntry,
    _owner: &SupervisedOwner,
    _definition_override: Option<&PathBuf>,
) -> Result<PathBuf, BootstrapError> {
    Err(BootstrapError::ServicesUnsupported)
}

/// Enable the installed service for startup-at-login. Separate
/// from installation and from starting, so each failure reports its
/// own stage. On macOS `load -w` loads now and starts the agent, so
/// enablement is the start; re-enabling unloads first, which
/// restarts a running service rather than duplicating it. Disabling
/// later is the reconfigure stop path (`launchctl unload -w` plus
/// definition removal / `systemctl --user disable --now` plus
/// removal) — the documented stop, distinct from a crash the manager
/// retries.
#[cfg(unix)]
pub fn enable_service(
    entry: &RegistryEntry,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] definition: &Path,
) -> Result<(), BootstrapError> {
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{}", entry.service);
        // Unload-then-load: a disabled leftover re-enables rather
        // than duplicating, and a running service restarts on the
        // new definition. Errors from the unload are ignored — the
        // service may never have loaded — but the load must succeed.
        let _ = run("launchctl", &["unload", &definition.to_string_lossy()]);
        run("launchctl", &["load", "-w", &definition.to_string_lossy()]).map_err(|source| {
            BootstrapError::EnableService {
                name: label,
                source: Box::new(source),
            }
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = format!("wyrd-{}.service", entry.service);
        run("systemctl", &["--user", "daemon-reload"]).map_err(|source| {
            BootstrapError::ReloadUnits {
                source: Box::new(source),
            }
        })?;
        run("systemctl", &["--user", "enable", &unit]).map_err(|source| {
            BootstrapError::EnableService {
                name: unit,
                source: Box::new(source),
            }
        })
    }
}

/// Start the enabled service now. On macOS enablement already
/// started the agent, so this only verifies it is loaded — a second
/// start path would double-start. On systemd enablement persists
/// without starting, so this starts explicitly.
#[cfg(unix)]
pub fn start_service(entry: &RegistryEntry) -> Result<(), BootstrapError> {
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{}", entry.service);
        run("launchctl", &["list", &label]).map_err(|source| BootstrapError::AgentNotLoaded {
            label,
            source: Box::new(source),
        })?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = format!("wyrd-{}.service", entry.service);
        // Restart, not start: enablement leaves a running unit
        // alone, and a re-run that changed relays or credential
        // paths must bring the new definition live instead of
        // verifying the still-running old owner.
        run("systemctl", &["--user", "restart", &unit]).map_err(|source| {
            BootstrapError::RestartService {
                unit,
                source: Box::new(source),
            }
        })
    }
}

/// Stop and clear the per-drive service definition before a
/// reconfiguration: disable the manager unit (or unload the agent)
/// so a restart cannot bring the old definition back over the new
/// registry entry, then remove the definition file so no login or
/// reload resurrects it. A missing definition file means the drive
/// was never managed on this machine — nothing to clear, not an
/// error. Runs before the registry rewrite, so a failure stops with
/// the old entry still standing; every later failure is retried by
/// re-running, which converges from the stopped-and-cleared state.
///
/// `definition_override` names the definition file directly
/// (tests); `None` resolves the platform path. It mirrors the
/// registry/binary overrides the provisioning config already
/// carries, so hermetic tests never touch a developer's real
/// service definitions.
#[cfg(unix)]
pub fn disable_service(
    entry: &RegistryEntry,
    definition_override: Option<&PathBuf>,
) -> Result<(), BootstrapError> {
    let owned;
    let definition = match definition_override {
        Some(path) => path.as_path(),
        None => {
            owned = service_definition_path(entry);
            &owned
        }
    };
    if !definition.exists() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{}", entry.service);
        // Not loaded means nothing to stop: the unload below would
        // fail on the absent job, and that absence is expected (a
        // never-managed drive, or this operation's own earlier
        // pass), never a stop failure.
        if run("launchctl", &["list", &label]).is_ok() {
            // `-w` persists the disable like the `load -w` the
            // enable path uses; without it the `RunAtLoad` plist
            // would load again at the next login.
            run(
                "launchctl",
                &["unload", "-w", &definition.to_string_lossy()],
            )
            .map_err(|source| BootstrapError::StopService {
                name: label.clone(),
                source: Box::new(source),
            })?;
        }
        clear_definition(definition, &label)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = format!("wyrd-{}.service", entry.service);
        // Only an enabled unit needs the manager's disable, and only
        // an active unit needs its stop: on a never-enabled,
        // never-active unit both would fail on the unknown name, and
        // that absence is expected (a never-managed drive, or this
        // operation's own earlier pass), never a stop failure. An
        // active-but-disabled unit is stopped but not disabled. The
        // definition file below is removed either way.
        let enabled = run("systemctl", &["--user", "is-enabled", "--quiet", &unit]).is_ok();
        let active = run("systemctl", &["--user", "is-active", "--quiet", &unit]).is_ok();
        if active {
            // Stop the running unit; the enablement symlink is
            // handled separately below.
            run("systemctl", &["--user", "stop", &unit]).map_err(|source| {
                BootstrapError::StopService {
                    name: unit.clone(),
                    source: Box::new(source),
                }
            })?;
        }
        if enabled {
            run("systemctl", &["--user", "disable", &unit]).map_err(|source| {
                BootstrapError::StopService {
                    name: unit.clone(),
                    source: Box::new(source),
                }
            })?;
        }
        clear_definition(definition, &unit)?;
        if enabled || active {
            // The manager drops the removed unit on its next reload;
            // reload after the removal (not before) so a stale
            // in-memory unit cannot outlive the registry rewrite below.
            run("systemctl", &["--user", "daemon-reload"]).map_err(|source| {
                BootstrapError::StopService {
                    name: unit,
                    source: Box::new(source),
                }
            })?;
        }
        Ok(())
    }
}

/// Remove the service definition file after a successful stop. A
/// missing file is not an error (already cleared, or a concurrent
/// pass got there first — the re-run converges either way).
#[cfg(unix)]
fn clear_definition(definition: &Path, name: &str) -> Result<(), BootstrapError> {
    match std::fs::remove_file(definition) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(BootstrapError::StopService {
            name: name.to_string(),
            source: Box::new(BootstrapError::RemoveFile {
                path: definition.to_path_buf(),
                source,
            }),
        }),
    }
}

#[cfg(not(unix))]
pub fn disable_service(
    _entry: &RegistryEntry,
    definition_override: Option<&PathBuf>,
) -> Result<(), BootstrapError> {
    // Mirror the Unix early-out so a never-managed drive
    // reconfigures anywhere; a present definition needs a manager
    // this platform does not have.
    if let Some(path) = definition_override {
        if !path.exists() {
            return Ok(());
        }
    }
    Err(BootstrapError::ServicesUnsupported)
}

#[cfg(unix)]
fn run(program: &str, args: &[&str]) -> Result<(), BootstrapError> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|source| BootstrapError::Spawn {
            program: program.to_string(),
            source,
        })?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    Err(BootstrapError::CommandFailed {
        program: program.to_string(),
        args: args.join(" "),
        detail: detail.trim().to_string(),
    })
}

/// How to initialize the drive before anything is registered.
pub enum Init {
    /// `Engine::create`: a new single-device drive.
    Create,
    /// `Engine::join`: join as an invited device from a sealed
    /// invitation file. The invitation exchange that produced the
    /// file stays out of band; joining consumes it.
    Join { sealed_invitation: Vec<u8> },
}

/// What provisioning this drive needs beyond initialization.
pub struct ProvisionConfig {
    pub name: String,
    pub drive_dir: PathBuf,
    pub relays: Vec<String>,
    /// Target service mode: `vault` serves headless, `mount` serves
    /// the FUSE projection. Unknown modes refuse at validation.
    pub mode: ServiceMode,
    /// FUSE mountpoint. Required in `mount` mode, refused in `vault`
    /// mode — validated with the entry before anything is created.
    pub mountpoint: Option<PathBuf>,
    pub identity: DeviceIdentitySecret,
    pub passphrase: String,
    pub init: Init,
    pub registry_path: PathBuf,
    /// The `wyrd` binary whose `vault` or `mount` subcommand the
    /// service runs. Defaults to the `wyrd` beside the provisioning
    /// command; the installer ships both together.
    pub wyrd_bin: Option<PathBuf>,
    /// `wyrd-supervise` (`nix/drive/supervise.py`): the wait-status
    /// mapper the restart policy rests on. Defaults to the sibling
    /// of the `wyrd` binary. Managing a service without it is
    /// refused — a bare owner binary cannot express the terminal
    /// distinction — so pass an explicit path wherever the layout
    /// differs instead of running unmanaged.
    pub supervise_bin: Option<PathBuf>,
    /// Hardened credential files for the service definition,
    /// resolved the way `load_credentials` resolves them. Paths,
    /// never secrets, travel here.
    pub identity_file: PathBuf,
    pub passphrase_file: PathBuf,
    /// Install, enable, and start the per-drive service. Off skips
    /// all three: registration only, for operators who supervise
    /// the owner themselves.
    pub manage_service: bool,
    /// How long one socket verification waits for a terminal answer.
    /// An expiry is `Unconfirmed`, never success.
    pub verify_timeout: Duration,
}

/// Where one provisioning run ended. Ready is reported only as
/// `VerifiedAvailable` — a socket answer this session. Every other
/// variant names the stage that stopped, so partial completion is
/// explicit and resumable, never mistaken for ready. The post-write
/// variants carry the registry entries the read skipped, so the
/// caller can surface them instead of silently discarding rotten
/// data on rewrite.
#[derive(Debug)]
pub enum ProvisionState {
    VerifiedAvailable {
        entry: RegistryEntry,
        skipped: Vec<String>,
    },
    RegisteredButUnavailable {
        entry: RegistryEntry,
        kind: ProvisionFailureKind,
        reason: String,
        skipped: Vec<String>,
    },
    ProvisioningIncomplete {
        stage: &'static str,
        kind: ProvisionFailureKind,
        reason: String,
    },
}

/// The machine-readable failure kind for a provisioning run that
/// did not verify: one variant per terminal failure class (16
/// return sites share 12 kinds), carried alongside (never instead
/// of) the human-readable `reason`. Direct Rust callers match on
/// this instead of parsing text; the rendered strings are
/// unchanged. A new terminal `return` in `provision()` requires a
/// variant here — the compiler enforces the field, only review
/// enforces the right tag. Deliberately coarse — the desktop adapter
/// still branches on exit codes plus independent re-observation
/// alone and never parses CLI output, so this kind stops at the
/// `ProvisionState` boundary by design. The intended consumer is the
/// CLI/desktop adapter after a desktop-shell amendment; until then
/// it serves direct Rust callers and pins classification in tests.
/// `non_exhaustive` so adding a kind later is not a breaking change
/// for downstream exhaustive matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProvisionFailureKind {
    /// Entry validation refused before anything was created.
    Validate,
    /// The sealed invitation did not decode.
    DecodeInvitation,
    /// Engine creation or join failed.
    Initialize,
    /// The fresh open of the created drive failed.
    VerifyOpen,
    /// Another process holds the drive's store lock: stop the live
    /// owner first, the same rule every other offline writer states.
    /// Distinct from other open failures so the CLI can return the
    /// terminal held-drive exit instead of a usage error.
    StoreLocked,
    /// The registry could not be read.
    ReadRegistry,
    /// The registry entry could not be written.
    WriteRegistry,
    /// No `wyrd` binary for the service definition.
    NoWyrdBinary,
    /// No `wyrd-supervise` wrapper for the restart policy.
    NoSuperviseBinary,
    /// Service definition installation failed.
    InstallService,
    /// Service enablement failed.
    EnableService,
    /// Service start (or load verification) failed.
    StartService,
    /// Service stop (disable) before a reconfiguration failed.
    StopService,
    /// The owner socket never verified this session.
    VerifySocket,
}

impl ProvisionFailureKind {
    /// The `stage` string the `ProvisioningIncomplete` sites report
    /// alongside this kind. `None` for the kinds that only occur
    /// post-registration in `provision()` (`RegisteredButUnavailable`
    /// carries no stage). Construction keeps the literal CLI strings;
    /// the kind test asserts they agree, so a future edit that
    /// changes one and not the other fails a test instead of
    /// splitting display from machine kind. `reconfigure()` fails
    /// some service kinds pre-write (binary lookup at validation,
    /// install before the rewrite), so those map to their stages
    /// even though `provision()` never reports them there.
    #[cfg(test)]
    fn stage(&self) -> Option<&'static str> {
        match self {
            ProvisionFailureKind::Validate
            | ProvisionFailureKind::NoWyrdBinary
            | ProvisionFailureKind::NoSuperviseBinary => Some("validate"),
            ProvisionFailureKind::DecodeInvitation => Some("initialize"),
            ProvisionFailureKind::Initialize => Some("initialize"),
            ProvisionFailureKind::VerifyOpen | ProvisionFailureKind::StoreLocked => {
                Some("verify-open")
            }
            ProvisionFailureKind::ReadRegistry => Some("read-registry"),
            ProvisionFailureKind::WriteRegistry => Some("write-registry"),
            ProvisionFailureKind::InstallService => Some("install-service"),
            ProvisionFailureKind::EnableService
            | ProvisionFailureKind::StartService
            | ProvisionFailureKind::VerifySocket => None,
            ProvisionFailureKind::StopService => Some("stop-service"),
        }
    }
}

/// The fixed handoff order: initialize → verify the open → write the
/// registry → install, enable, and start the service → verify the
/// socket. Registration and service steps happen only after a
/// successful initialization, never speculatively.
pub fn provision(config: ProvisionConfig) -> ProvisionState {
    let entry = RegistryEntry {
        service: format!("drive-{}", config.name),
        name: config.name.clone(),
        drive_dir: config.drive_dir.clone(),
        relays: config.relays.clone(),
        mode: config.mode.clone(),
        mountpoint: config.mountpoint.clone(),
    };
    if let Err(error) = validate_entry(&entry.name, &entry) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: error.to_string(),
        };
    }
    // Live mountpoint checks before anything is created: shape passed
    // above, but the path must also exist and stand apart from the
    // drive directory, or the installed definition could never work.
    if let Some(mountpoint) = &config.mountpoint {
        if let Err(error) = validate_mountpoint_live(&config.drive_dir, mountpoint) {
            return ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: error.to_string(),
            };
        }
    }
    // A managed service refuses a relay-less drive (it would serve
    // nothing peers can discover), so managing without relays is
    // refused before anything is created or registered — with that
    // pointer, not a definition that can never verify.
    if config.manage_service && config.relays.is_empty() {
        return ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: BootstrapError::ManagedWithoutRelays.to_string(),
        };
    }

    // 1. Initialize, or reconcile an existing drive. `DriveExists`
    // means a previous run (or another operator) already created
    // it: fall through and re-verify the open instead of re-creating.
    match &config.init {
        Init::Create => match Engine::create(
            config.drive_dir.clone(),
            &config.passphrase,
            config.identity.clone(),
        ) {
            Ok(_) | Err(EngineError::DriveExists) => {}
            Err(error) => {
                return ProvisionState::ProvisioningIncomplete {
                    stage: "initialize",
                    kind: ProvisionFailureKind::Initialize,
                    reason: error.to_string(),
                };
            }
        },
        Init::Join { sealed_invitation } => {
            let sealed = match wyrd_sync::control::SealedBootstrap::decode(sealed_invitation) {
                Ok(sealed) => sealed,
                Err(error) => {
                    return ProvisionState::ProvisioningIncomplete {
                        stage: "initialize",
                        kind: ProvisionFailureKind::DecodeInvitation,
                        reason: format!("decode invitation: {error}"),
                    };
                }
            };
            match Engine::join(
                config.drive_dir.clone(),
                &config.passphrase,
                config.identity.clone(),
                &sealed,
            ) {
                Ok(_) | Err(EngineError::DriveExists) => {}
                Err(error) => {
                    return ProvisionState::ProvisioningIncomplete {
                        stage: "initialize",
                        kind: ProvisionFailureKind::Initialize,
                        reason: error.to_string(),
                    };
                }
            }
        }
    };

    // 2. Verify the open before registering anything.
    if let Err(error) = Engine::open_keystore(
        config.drive_dir.clone(),
        &config.passphrase,
        config.identity.clone(),
    ) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "verify-open",
            kind: ProvisionFailureKind::VerifyOpen,
            reason: error.to_string(),
        };
    }

    // 3. Write the registry entry under the lock: the insert runs
    // against the fresh map, so a concurrent same-key update cannot
    // be clobbered by this caller's stale copy. The pre-write read
    // classifies an unreadable registry as `ReadRegistry` before the
    // write is attempted; the locked rewrite below is authoritative
    // for the merge. Skipped entries carry forward verbatim inside
    // the write, and the post-write reasons return — the caller
    // surfaces what the file it wrote actually skips.
    if let Err(error) = read_registry(&config.registry_path) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "read-registry",
            kind: ProvisionFailureKind::ReadRegistry,
            reason: error.to_string(),
        };
    }
    let skipped = match modify_registry(&config.registry_path, |drives| {
        drives.insert(entry.name.clone(), entry.clone());
        Ok(())
    }) {
        Ok(((), skipped)) => skipped,
        Err(error) => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "write-registry",
                kind: ProvisionFailureKind::WriteRegistry,
                reason: error.to_string(),
            };
        }
    };

    // 4. Install, enable, and start the per-drive service.
    if config.manage_service {
        let wyrd_bin = match &config.wyrd_bin {
            Some(path) => path.clone(),
            None => match sibling_bin("wyrd") {
                Some(path) => path,
                None => {
                    return ProvisionState::RegisteredButUnavailable {
                        entry,
                        skipped: skipped.clone(),
                        kind: ProvisionFailureKind::NoWyrdBinary,
                        reason: "no wyrd binary: pass --owner-bin (no wyrd beside this command)"
                            .to_string(),
                    };
                }
            },
        };
        let supervise_bin = match &config.supervise_bin {
            Some(path) => path.clone(),
            None => match sibling_bin("wyrd-supervise") {
                Some(path) => path,
                None => {
                    return ProvisionState::RegisteredButUnavailable {
                        entry,
                        skipped: skipped.clone(),
                        kind: ProvisionFailureKind::NoSuperviseBinary,
                        reason: "no wyrd-supervise: pass --supervise-bin \
                            (the restart policy needs the wrapper; \
                            a bare owner binary cannot express it)"
                            .to_string(),
                    };
                }
            },
        };
        let owner = SupervisedOwner {
            supervise_bin,
            wyrd_bin,
            identity_file: config.identity_file.clone(),
            passphrase_file: config.passphrase_file.clone(),
        };
        let definition = match install_service_definition(&entry, &owner, None) {
            Ok(definition) => definition,
            Err((kind, reason)) => {
                return ProvisionState::RegisteredButUnavailable {
                    entry,
                    skipped: skipped.clone(),
                    kind,
                    reason,
                };
            }
        };
        if let Err((kind, reason)) = enable_and_start(&entry, &definition) {
            return ProvisionState::RegisteredButUnavailable {
                entry,
                skipped: skipped.clone(),
                kind,
                reason,
            };
        }
    }

    // 5. Verify the socket: the owner must answer terminally. Ready
    // means `Ok` or the pre-first-pass `Unobserved` refusal — every
    // other refusal shape is unexpected from this operation and
    // stays unavailable, never verified. `Unconfirmed` and
    // `Unavailable` likewise never verify: the outcome is unknown
    // or the owner is down.
    match verify_owner_socket(&config.drive_dir, config.verify_timeout, false) {
        Ok(()) => ProvisionState::VerifiedAvailable { entry, skipped },
        Err((kind, reason)) => ProvisionState::RegisteredButUnavailable {
            entry,
            skipped,
            kind,
            reason,
        },
    }
}

/// What a mode reconfiguration changes about one registered
/// drive. The entry is looked up by its drive directory — the one
/// identity the `drive` command already carries — and must resolve
/// to exactly one registry key, never a guess across duplicates.
pub struct ReconfigureConfig {
    pub drive_dir: PathBuf,
    /// Replacement relay set. `None` keeps the registered set; the
    /// CLI passes `Some` only when the operator named `--relay`
    /// flags, so a credential-path-only reconfigure never clears
    /// relays by accident.
    pub relays: Option<Vec<String>>,
    /// Target service mode, as the display string the registry
    /// carries. `vault` serves headless, `mount` serves the FUSE
    /// projection; any other value refuses with the unknown-mode
    /// pointer before anything stops, exactly like provisioning.
    /// The mountpoint travels alongside: required for `mount`,
    /// refused for `vault`.
    pub mode: ServiceMode,
    /// FUSE mountpoint for `mount` mode. `None` in `vault` mode.
    pub mountpoint: Option<PathBuf>,
    pub identity: DeviceIdentitySecret,
    pub passphrase: String,
    pub registry_path: PathBuf,
    /// The `wyrd` binary whose `vault` or `mount` subcommand the
    /// service runs.
    /// Defaults to the `wyrd` beside the reconfiguring command; the
    /// installer ships both together.
    pub wyrd_bin: Option<PathBuf>,
    /// `wyrd-supervise`: the wait-status mapper the restart policy
    /// rests on. Managing a service without it is refused — a bare
    /// owner binary cannot express the terminal distinction — so an
    /// explicit path is required wherever the layout differs.
    pub supervise_bin: Option<PathBuf>,
    /// Hardened credential files for the service definition,
    /// resolved the way `load_credentials` resolves them. Paths,
    /// never secrets, travel here.
    pub identity_file: PathBuf,
    pub passphrase_file: PathBuf,
    /// Explicit service definition path (tests); `None` resolves the
    /// platform path. Clearing and installing share it, so the two
    /// can never name different files — and hermetic tests name a
    /// scratch path instead of a developer's real services, mirroring
    /// the registry and binary overrides above.
    pub service_definition: Option<PathBuf>,
    /// Reinstall, enable, and restart the per-drive service after
    /// the rewrite. Off disables the old service, clears its
    /// definition, and rewrites the entry without installing a new
    /// one: registration only.
    pub manage_service: bool,
    /// How long one socket verification waits for a terminal answer.
    /// An expiry is `Unconfirmed`, never success.
    pub verify_timeout: Duration,
}

/// Apply the reconfigured relays, mode, and mountpoint to the
/// stored entry: the single place that builds the working copy
/// install and rewrite share, so the two can never diverge (a
/// managed definition can never render the old relay set beside the
/// new registry entry). Identity fields — name, service,
/// directory — stay put.
fn updated_entry(
    mut stored: RegistryEntry,
    relays: Vec<String>,
    mode: ServiceMode,
    mountpoint: Option<PathBuf>,
) -> RegistryEntry {
    stored.relays = relays;
    stored.mode = mode;
    stored.mountpoint = mountpoint;
    stored
}

/// Whether the installed definition must be reinstalled from the
/// written entry: the registry lock may have resolved a fresher
/// relay set than the pre-stop working copy carried. Mode and
/// mountpoint are operator intent in both copies, so only the relay
/// set can differ — and when it does, the manager must serve the
/// written set, never the stale installed one.
fn definition_needs_reinstall(working: &RegistryEntry, written: &RegistryEntry) -> bool {
    working.relays != written.relays
}

/// Install the per-drive service definition for a freshly
/// reconfigured (or newly provisioned) entry. Shared by both
/// handoffs so their install failures carry the same kind and
/// reason; the caller decides the state shape around it.
fn install_service_definition(
    entry: &RegistryEntry,
    owner: &SupervisedOwner,
    definition_override: Option<&PathBuf>,
) -> Result<PathBuf, (ProvisionFailureKind, String)> {
    install_service(entry, owner, definition_override).map_err(|error| {
        (
            ProvisionFailureKind::InstallService,
            format!("install-service: {error}"),
        )
    })
}

/// Classify a registry failure past the stop: resolve failures
/// (vanished entry, conflicting duplicates, a managed run that
/// resolved no relays) name the registry state to repair — the old
/// service is already stopped and cleared, so `validate` with the
/// stopped-service note describes what the operator must fix, not a
/// write that happened. Anything else is a write failure with the
/// entry untouched.
fn classify_post_stop_failure(
    error: &BootstrapError,
) -> (&'static str, ProvisionFailureKind, String) {
    let stopped = "the old service is already stopped and cleared";
    match error {
        BootstrapError::Read { source, .. } if source.kind() == std::io::ErrorKind::NotFound => (
            "validate",
            ProvisionFailureKind::Validate,
            format!("{error} ({stopped}; re-register the drive, then re-run to converge)"),
        ),
        BootstrapError::ConflictingEntries { .. } => (
            "validate",
            ProvisionFailureKind::Validate,
            format!("{error} ({stopped}; resolve the duplicate by hand, then re-run to converge)"),
        ),
        BootstrapError::ManagedWithoutRelays => (
            "validate",
            ProvisionFailureKind::Validate,
            format!("{error} ({stopped}; register relays with --no-service first, then re-run to converge)"),
        ),
        _ => (
            "write-registry",
            ProvisionFailureKind::WriteRegistry,
            error.to_string(),
        ),
    }
}

/// Enable the installed definition and start it now. Shared by both
/// handoffs so their service failures carry the same kinds.
fn enable_and_start(
    entry: &RegistryEntry,
    definition: &Path,
) -> Result<(), (ProvisionFailureKind, String)> {
    if let Err(error) = enable_service(entry, definition) {
        return Err((
            ProvisionFailureKind::EnableService,
            format!("enable-service: {error}"),
        ));
    }
    if let Err(error) = start_service(entry) {
        return Err((
            ProvisionFailureKind::StartService,
            format!("start-service: {error}"),
        ));
    }
    Ok(())
}

/// What one verify attempt concluded.
enum Attempt {
    /// `Ok` or the pre-first-pass `Unobserved` refusal: done.
    Verified,
    /// A terminal refusal from a live owner: retrying cannot change it.
    Answered(String),
    /// No live answer (connect failure, `Unconfirmed`,
    /// `Unavailable`): worth retrying while the deadline holds.
    Silent(String),
}

/// One connect-plus-observe attempt against the drive socket.
fn connect_observe(drive_dir: &Path, attempt_timeout: Duration) -> Attempt {
    let outcome = OwnerClient::connect_drive(drive_dir)
        .and_then(|client| client.with_timeout(attempt_timeout));
    let outcome = outcome.and_then(|mut client| client.observe());
    match outcome {
        Ok(_) => Attempt::Verified,
        Err(ClientError::Refused(error)) if error.code == WireErrorCode::Unobserved => {
            Attempt::Verified
        }
        Err(ClientError::Refused(error)) => Attempt::Answered(format!("{error:?}")),
        Err(error) => Attempt::Silent(error.to_string()),
    }
}

/// Verify the owner socket answers terminally this session: `Ok` or
/// the pre-first-pass `Unobserved` refusal verifies; any other
/// terminal refusal reports unavailable without retrying (a live
/// owner answered). Silence — connect failures, `Unconfirmed`,
/// `Unavailable` — retries until the deadline when `retry_connect`
/// is set (managed reconfigure: a freshly restarted owner still
/// binding never reads as unavailable). A pre-existing socket whose
/// connect fails is unlinked first — it normally belongs to the
/// stopped owner, and the wait below then observes only the
/// restarted owner binding. Without retry (provisioning and
/// `--no-service`) the first attempt decides, exactly as before:
/// wait for appearance, then one terminal attempt.
fn verify_owner_socket(
    drive_dir: &Path,
    verify_timeout: Duration,
    retry_connect: bool,
) -> Result<(), (ProvisionFailureKind, String)> {
    let missing = |reason: String| {
        (
            ProvisionFailureKind::VerifySocket,
            format!("verify-socket: {reason}"),
        )
    };
    if !retry_connect {
        if !wait_for_socket(drive_dir, verify_timeout) {
            return Err(missing("owner.sock never appeared".to_string()));
        }
        return match connect_observe(drive_dir, verify_timeout) {
            Attempt::Verified => Ok(()),
            Attempt::Answered(reason) | Attempt::Silent(reason) => Err(missing(reason)),
        };
    }
    let sock = crate::owner::socket_path_for(drive_dir);
    if sock.exists() {
        match connect_observe(drive_dir, verify_timeout) {
            Attempt::Verified => return Ok(()),
            Attempt::Answered(reason) => return Err(missing(reason)),
            Attempt::Silent(_) => {
                let _ = std::fs::remove_file(&sock);
            }
        }
    }
    let deadline = std::time::Instant::now() + verify_timeout;
    loop {
        // Bound every wait and attempt by the remaining budget so a
        // wedged owner cannot stretch one step past the deadline.
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if !wait_for_socket(drive_dir, remaining) {
            return Err(missing("owner.sock never appeared".to_string()));
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match connect_observe(drive_dir, remaining.min(verify_timeout)) {
            Attempt::Verified => return Ok(()),
            Attempt::Answered(reason) => return Err(missing(reason)),
            Attempt::Silent(reason) => {
                if std::time::Instant::now() >= deadline {
                    return Err(missing(reason));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// Whether an open failure is store-lock contention (as opposed
/// to a credential or state failure): only the lock carries the
/// terminal held-drive kind through the reconfigure handoff.
fn is_store_locked(error: &EngineError) -> bool {
    matches!(
        error,
        EngineError::Durable(wyrd_sync::durable::DurableError::StoreLocked)
    )
}

/// Reconfigure one registered drive in place: validate the target,
/// read the registry and resolve the entry by directory, prove the
/// keystore opens (a live managed lock stops the definition and
/// retries once; any other open failure refuses with everything
/// untouched), stop and clear the old service definition, install
/// the new definition from the updated entry when managed, rewrite
/// the entry, and verify the socket. The fixed order keeps every
/// refusal honest: cheap checks precede the keystore open (no KDF on
/// a refusal) and credential failures precede the stop (no outage on
/// a typo); install and rewrite share one working copy, so the
/// definition can never render the old relay set beside the new
/// entry; the drive is reported ready only when its socket verifies
/// this session — never because the entry was rewritten. Every
/// failure past the stop is retried by re-running, which converges
/// from the stopped-and-cleared state (the operation is idempotent
/// there).
pub fn reconfigure(config: ReconfigureConfig) -> ProvisionState {
    // 0. Validate the mode and mountpoint before anything moves: no
    // I/O, no side effects, so a refusal touches nothing. Unknown
    // modes refuse here; mount without a mountpoint (or vault with
    // one) refuses through the entry validation below.
    if matches!(config.mode, ServiceMode::Unknown(_)) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: BootstrapError::UnknownServiceMode.to_string(),
        };
    }
    if let Some(mountpoint) = &config.mountpoint {
        if matches!(config.mode, ServiceMode::Vault) {
            return ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: BootstrapError::UnexpectedMountpoint.to_string(),
            };
        }
        if let Err(error) = validate_mountpoint(&config.drive_dir, mountpoint) {
            return ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: error.to_string(),
            };
        }
    } else if matches!(config.mode, ServiceMode::Mount) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: BootstrapError::MissingMountpoint.to_string(),
        };
    }
    // Live mountpoint checks before anything moves: shape passed
    // above, but the path must also exist and stand apart from the
    // drive directory, or the installed definition could never work.
    if let Some(mountpoint) = &config.mountpoint {
        if let Err(error) = validate_mountpoint_live(&config.drive_dir, mountpoint) {
            return ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: error.to_string(),
            };
        }
    }

    // 1. Read the registry and resolve the entry by directory. Zero
    // matches means the drive was never registered (or was
    // registered under another spelling — directories match by
    // exact path only); several means conflicting registrations
    // that only the operator can untangle, never this command.
    let registry = match read_registry(&config.registry_path) {
        Ok(registry) => registry,
        Err(error) => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "read-registry",
                kind: ProvisionFailureKind::ReadRegistry,
                reason: error.to_string(),
            };
        }
    };
    let mut matches: Vec<String> = registry
        .drives
        .iter()
        .filter(|(_, entry)| entry.drive_dir == config.drive_dir)
        .map(|(key, _)| key.clone())
        .collect();
    matches.sort();
    let key = match matches.len() {
        0 => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: format!(
                    "no registry entry for {}: register with `wyrd bootstrap` first",
                    config.drive_dir.display()
                ),
            };
        }
        1 => matches.pop().expect("one match"),
        _ => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: format!(
                    "conflicting registry entries for {}: {} — resolve the duplicate by hand first",
                    config.drive_dir.display(),
                    matches.join(", ")
                ),
            };
        }
    };
    let stored = registry.drives.get(&key).expect("key resolved").clone();

    // 2. Candidate relays: flags replace the registered set, absence
    // keeps it — so a credential-path-only reconfigure never clears
    // relays by accident. Explicit values validate before anything
    // stops: a bad relay refuses with the service still up. A kept
    // set is intent, not a snapshot: it resolves from the fresh entry
    // under the lock below, so a concurrent relay update landing
    // between this read and the rewrite is kept, not reverted.
    let relays = match config.relays.clone() {
        Some(candidate) => {
            if !valid_relays(&candidate) {
                return ProvisionState::ProvisioningIncomplete {
                    stage: "validate",
                    kind: ProvisionFailureKind::Validate,
                    reason: BootstrapError::InvalidRelays.to_string(),
                };
            }
            Some(candidate)
        }
        None => None,
    };
    // Pre-stop working values for the definition install: explicit
    // relays, or the pre-stop stored set as a stand-in the lock
    // re-resolves below. A managed drive with no relays to serve is
    // refused before anything stops, like provisioning.
    let install_relays = match &relays {
        Some(explicit) => explicit.clone(),
        None => stored.relays.clone(),
    };
    if config.manage_service && install_relays.is_empty() {
        return ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: BootstrapError::ManagedWithoutRelays.to_string(),
        };
    }

    // The working copy from here on: install and rewrite share the
    // one entry `updated_entry` builds from the pre-stop read, so a
    // managed definition can never render the old relay set beside
    // the new registry entry. The authoritative rewrite below
    // re-resolves under the lock and applies the same intent —
    // explicit relays, or the fresh kept set — to the fresh entry,
    // so a concurrent same-key update between this read and the
    // write is preserved underneath the transformation instead of
    // being clobbered by the stale copy. When the lock resolves a
    // fresher relay set than this copy carries, the definition is
    // reinstalled from the written entry before enabling, so the
    // two never disagree.
    let working = updated_entry(
        stored,
        install_relays,
        config.mode.clone(),
        config.mountpoint.clone(),
    );

    // 3. Resolve the service binaries before stopping: a missing
    // binary refuses with the service still up, never with the drive
    // down. Same reason text as provisioning; the kind marks the
    // pre-stop validation site.
    let (wyrd_bin, supervise_bin) = if config.manage_service {
        let wyrd_bin = match &config.wyrd_bin {
            Some(path) => path.clone(),
            None => match sibling_bin("wyrd") {
                Some(path) => path,
                None => {
                    return ProvisionState::ProvisioningIncomplete {
                        stage: "validate",
                        kind: ProvisionFailureKind::NoWyrdBinary,
                        reason: "no wyrd binary: pass --owner-bin (no wyrd beside this command)"
                            .to_string(),
                    };
                }
            },
        };
        let supervise_bin = match &config.supervise_bin {
            Some(path) => path.clone(),
            None => match sibling_bin("wyrd-supervise") {
                Some(path) => path,
                None => {
                    return ProvisionState::ProvisioningIncomplete {
                        stage: "validate",
                        kind: ProvisionFailureKind::NoSuperviseBinary,
                        reason: "no wyrd-supervise: pass --supervise-bin \
                            (the restart policy needs the wrapper; \
                            a bare owner binary cannot express it)"
                            .to_string(),
                    };
                }
            },
        };
        (Some(wyrd_bin), Some(supervise_bin))
    } else {
        (None, None)
    };

    // 4. Prove the keystore opens before anything stops: credential
    // failures (wrong passphrase, unreadable identity, unopenable
    // store) refuse with the service still running and everything
    // untouched. Only a live managed lock takes the stop path: stop
    // and clear the definition, then retry the open once. A second
    // failure stops here with the service already down — re-running
    // converges. A live lock without management refuses outright
    // (nothing to stop); only the lock refusal carries the terminal
    // kind, every other open failure stays a usage error.
    let open = || {
        Engine::open_keystore(
            config.drive_dir.clone(),
            &config.passphrase,
            config.identity.clone(),
        )
    };
    match open() {
        Ok(_) => {}
        Err(error) if is_store_locked(&error) && config.manage_service => {
            if let Err(error) = disable_service(&working, config.service_definition.as_ref()) {
                return ProvisionState::ProvisioningIncomplete {
                    stage: "stop-service",
                    kind: ProvisionFailureKind::StopService,
                    reason: format!("stop-service: {error}"),
                };
            }
            if let Err(error) = open() {
                let kind = if is_store_locked(&error) {
                    ProvisionFailureKind::StoreLocked
                } else {
                    ProvisionFailureKind::VerifyOpen
                };
                return ProvisionState::ProvisioningIncomplete {
                    stage: "verify-open",
                    kind,
                    reason: error.to_string(),
                };
            }
        }
        Err(error) => {
            let kind = if is_store_locked(&error) {
                ProvisionFailureKind::StoreLocked
            } else {
                ProvisionFailureKind::VerifyOpen
            };
            return ProvisionState::ProvisioningIncomplete {
                stage: "verify-open",
                kind,
                reason: error.to_string(),
            };
        }
    }

    // 5. Stop and clear the old service definition. Idempotent: a
    // no-op when step 4 already stopped (the file is gone), and a
    // missing definition file means the drive was never managed —
    // nothing to clear.
    if let Err(error) = disable_service(&working, config.service_definition.as_ref()) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "stop-service",
            kind: ProvisionFailureKind::StopService,
            reason: format!("stop-service: {error}"),
        };
    }

    // 6. Install the new definition when managed — from the working
    // entry, before the registry rewrite, so a crash between them
    // leaves the new definition beside the old entry (the next run
    // reconciles forward) rather than the old definition over the
    // new entry. Without management the definition is already cleared
    // above.
    let definition = if config.manage_service {
        let owner = SupervisedOwner {
            supervise_bin: supervise_bin.clone().expect("resolved when managed"),
            wyrd_bin: wyrd_bin.clone().expect("resolved when managed"),
            identity_file: config.identity_file.clone(),
            passphrase_file: config.passphrase_file.clone(),
        };
        match install_service_definition(&working, &owner, config.service_definition.as_ref()) {
            Ok(definition) => Some(definition),
            Err((kind, reason)) => {
                return ProvisionState::ProvisioningIncomplete {
                    stage: "install-service",
                    kind,
                    reason,
                };
            }
        }
    } else {
        None
    };

    // 7. Rewrite the entry in place under the lock. The updater
    // re-resolves by directory against the fresh map and applies the
    // operator's intent — explicit relays, or the fresh kept set —
    // plus the intended mode and mountpoint. A concurrent same-key
    // update between the pre-stop read and this write is preserved
    // underneath the transformation instead of being clobbered by
    // the stale working copy — including a relay update landing
    // while this run kept the set. Skipped entries carry forward
    // verbatim inside the write, so a future mode or a rotten entry
    // survives the rewrite and is surfaced by the caller instead of
    // silently discarded.
    let intended_relays = relays;
    let intended_mode = config.mode.clone();
    let intended_mountpoint = config.mountpoint.clone();
    let drive_dir = config.drive_dir.clone();
    let registry_path = config.registry_path.clone();
    let manage_service = config.manage_service;
    let (written, skipped) = match modify_registry(&config.registry_path, |drives| {
        let mut matches: Vec<String> = drives
            .iter()
            .filter(|(_, entry)| entry.drive_dir == drive_dir)
            .map(|(key, _)| key.clone())
            .collect();
        matches.sort();
        match matches.len() {
            0 => Err(BootstrapError::Read {
                path: registry_path.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "no registry entry for {}: register with `wyrd bootstrap` first",
                        drive_dir.display()
                    ),
                ),
            }),
            1 => {
                let key = matches.pop().expect("one match");
                let fresh = drives.get(&key).expect("key resolved").clone();
                // Intent, resolved fresh: explicit flags win, absence
                // keeps whatever the fresh entry carries — never the
                // pre-stop snapshot. Values re-validate here, and a
                // managed run with nothing to serve refuses like the
                // pre-stop check: a concurrent no-service writer may
                // have emptied the set after this run read it (nothing
                // else can empty a set — reconfigure keeps or
                // replaces non-empty, provision validates — but the
                // lock is where the kept set is authoritative, so the
                // rule is enforced here too).
                let resolved = match &intended_relays {
                    Some(explicit) => explicit.clone(),
                    None => fresh.relays.clone(),
                };
                if !valid_relays(&resolved) {
                    return Err(BootstrapError::RefusedWrite {
                        source: Box::new(BootstrapError::InvalidRelays),
                    });
                }
                if manage_service && resolved.is_empty() {
                    return Err(BootstrapError::ManagedWithoutRelays);
                }
                let updated = updated_entry(
                    fresh,
                    resolved,
                    intended_mode.clone(),
                    intended_mountpoint.clone(),
                );
                drives.insert(key, updated.clone());
                Ok(updated)
            }
            _ => Err(BootstrapError::ConflictingEntries {
                dir: drive_dir.clone(),
                keys: matches.join(", "),
            }),
        }
    }) {
        Ok((written, skipped)) => (written, skipped),
        Err(error) => {
            let (stage, kind, reason) = classify_post_stop_failure(&error);
            return ProvisionState::ProvisioningIncomplete {
                stage,
                kind,
                reason,
            };
        }
    };
    // The installed definition carries the pre-stop working copy;
    // the lock may have resolved a fresher relay set. Reinstall from
    // the written entry when they differ, so the manager never serves
    // a stale relay set beside the new registry entry. A crash
    // between the write and this reinstall leaves the written entry
    // beside the stopped old definition — the next run reads the
    // fresh entry, installs it, and converges.
    let effective = written;
    let mut definition = definition;
    if definition.is_some() && definition_needs_reinstall(&working, &effective) {
        let owner = SupervisedOwner {
            supervise_bin: supervise_bin.expect("resolved when managed"),
            wyrd_bin: wyrd_bin.expect("resolved when managed"),
            identity_file: config.identity_file.clone(),
            passphrase_file: config.passphrase_file.clone(),
        };
        match install_service_definition(&effective, &owner, config.service_definition.as_ref()) {
            Ok(definition_path) => definition = Some(definition_path),
            Err((kind, reason)) => {
                return ProvisionState::RegisteredButUnavailable {
                    entry: effective,
                    skipped,
                    kind,
                    reason,
                };
            }
        }
    }

    // 8. Enable, start, and verify when managed — the same handoff
    // provisioning runs. With `--no-service` and no manually
    // supervised owner, the socket never appears and readiness is
    // withheld — registered, never verified.
    if let Some(definition) = definition {
        if let Err((kind, reason)) = enable_and_start(&effective, &definition) {
            return ProvisionState::RegisteredButUnavailable {
                entry: effective,
                skipped: skipped.clone(),
                kind,
                reason,
            };
        }
    }
    match verify_owner_socket(
        &config.drive_dir,
        config.verify_timeout,
        config.manage_service,
    ) {
        Ok(()) => ProvisionState::VerifiedAvailable {
            entry: effective,
            skipped,
        },
        Err((kind, reason)) => ProvisionState::RegisteredButUnavailable {
            entry: effective,
            skipped,
            kind,
            reason,
        },
    }
}

/// Start one registered drive's installed service without rewriting
/// anything: resolve the entry by directory, refuse where no
/// definition file exists (reinstall is the recovery there, not
/// start), then enable and start the existing definition and verify
/// the socket. No keystore opens, no registry writes, no credential
/// use — starting proves nothing about membership and grants none.
/// Never auto-runs: the caller (the desktop recovery button) runs
/// this only on the person's explicit decision, and a deliberately
/// stopped drive stays stopped until they do.
///
/// Against a live drive this restarts the owner: the enable/start
/// primitives are the reconfigure ones (macOS unloads then loads,
/// systemd restarts), so in-flight work turns `Unconfirmed`. The
/// desktop offers the verb only for stopped and wedged drives; a
/// direct CLI run against a healthy drive restarts it by design, and
/// the socket verify afterwards proves the new owner answers.
///
/// No binary preflight: the definition names its own programs, so
/// sibling checks would validate the wrong files and refuse starts
/// that would work. A missing program surfaces through
/// `enable_and_start` and the socket verify instead.
pub struct StartServiceConfig {
    pub drive_dir: PathBuf,
    pub registry_path: PathBuf,
    /// Explicit service definition path (tests); `None` resolves the
    /// platform path, exactly like installation.
    pub service_definition: Option<PathBuf>,
    /// How long one socket verification waits for a terminal answer.
    pub verify_timeout: Duration,
}

/// Start the installed per-drive service for one registered drive.
/// See [`StartServiceConfig`] for the no-rewrite contract.
#[cfg(unix)]
pub fn start_managed_service(config: StartServiceConfig) -> ProvisionState {
    // 1. Read the registry and resolve the entry by directory. Zero
    // matches means the drive was never registered; several means
    // conflicting registrations only the operator can untangle.
    let registry = match read_registry(&config.registry_path) {
        Ok(registry) => registry,
        Err(error) => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "read-registry",
                kind: ProvisionFailureKind::ReadRegistry,
                reason: error.to_string(),
            };
        }
    };
    let mut matches: Vec<String> = registry
        .drives
        .iter()
        .filter(|(_, entry)| entry.drive_dir == config.drive_dir)
        .map(|(key, _)| key.clone())
        .collect();
    matches.sort();
    let entry = match matches.len() {
        0 => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: format!(
                    "no registry entry for {}: register with `wyrd bootstrap` first",
                    config.drive_dir.display()
                ),
            };
        }
        1 => {
            let key = matches.pop().expect("one match");
            registry.drives.get(&key).expect("key resolved").clone()
        }
        _ => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: format!(
                    "conflicting registry entries for {}: {} — resolve the duplicate by hand first",
                    config.drive_dir.display(),
                    matches.join(", ")
                ),
            };
        }
    };
    // 2. The definition must already exist: a missing file is the
    // reinstall recovery, never this one. Refuse before touching the
    // manager, with that pointer.
    let owned;
    let definition = match config.service_definition.as_ref() {
        Some(path) => path.as_path(),
        None => {
            owned = service_definition_path(&entry);
            &owned
        }
    };
    if !definition.exists() {
        return ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: format!(
                "no service definition installed for {} ({} missing): reinstall with `wyrd drive {} reconfigure --mode <vault|mount>` first",
                entry.name,
                definition.display(),
                config.drive_dir.display()
            ),
        };
    }
    // 3. Enable and start the existing definition, then verify the
    // socket with the reconfigure retry (a freshly restarted owner
    // still binding never reads as unavailable). No binary preflight:
    // the definition names its own programs; a missing program fails
    // here and in the verify below, which is where the recovery
    // actually runs.
    if let Err((kind, reason)) = enable_and_start(&entry, definition) {
        return ProvisionState::RegisteredButUnavailable {
            entry,
            skipped: registry.skipped.clone(),
            kind,
            reason,
        };
    }
    match verify_owner_socket(&config.drive_dir, config.verify_timeout, true) {
        Ok(()) => ProvisionState::VerifiedAvailable {
            entry,
            skipped: registry.skipped,
        },
        Err((kind, reason)) => ProvisionState::RegisteredButUnavailable {
            entry,
            skipped: registry.skipped,
            kind,
            reason,
        },
    }
}

#[cfg(not(unix))]
pub fn start_managed_service(_config: StartServiceConfig) -> ProvisionState {
    ProvisionState::ProvisioningIncomplete {
        stage: "validate",
        kind: ProvisionFailureKind::Validate,
        reason: BootstrapError::ServicesUnsupported.to_string(),
    }
}

/// Wait for the owner socket to appear: the service was just
/// enabled and the owner may still be starting. Polls; never
/// invents an answer.
fn wait_for_socket(drive_dir: &Path, timeout: Duration) -> bool {
    let sock = crate::owner::socket_path_for(drive_dir);
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if sock.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    sock.exists()
}

/// Locate a sibling binary beside the running executable: the
/// installer ships the set together, so the sibling is the default.
/// Overridable with an explicit path wherever the layout differs.
fn sibling_bin(name: &str) -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let dir = current.parent()?;
    let candidate = dir.join(name);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(&candidate) {
            if metadata.permissions().mode() & 0o111 != 0 {
                return Some(candidate);
            }
        }
        None
    }
    #[cfg(not(unix))]
    {
        if candidate.exists() {
            return Some(candidate);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    pub(super) fn scratch(case: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("wyrd-bootstrap-{case}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(super) fn entry(name: &str, dir: &Path) -> RegistryEntry {
        RegistryEntry {
            name: name.to_string(),
            drive_dir: dir.join("drive"),
            relays: vec!["relay://example".to_string()],
            service: format!("drive-{name}"),
            mode: ServiceMode::Vault,
            mountpoint: None,
        }
    }

    pub(super) fn mount_entry(name: &str, dir: &Path, mountpoint: &Path) -> RegistryEntry {
        RegistryEntry {
            name: name.to_string(),
            drive_dir: dir.join("drive"),
            relays: vec!["relay://example".to_string()],
            service: format!("drive-{name}"),
            mode: ServiceMode::Mount,
            mountpoint: Some(mountpoint.to_path_buf()),
        }
    }

    #[test]
    fn registry_round_trips_and_rejects_a_newer_schema() {
        let dir = scratch("schema");
        let path = dir.join("drives.json");
        let mut drives = BTreeMap::new();
        drives.insert("demo".to_string(), entry("demo", &dir));
        write_registry(&path, &drives).unwrap();

        let read = read_registry(&path).unwrap();
        assert!(read.skipped.is_empty());
        assert_eq!(read.drives["demo"], drives["demo"]);

        std::fs::write(&path, r#"{"version": 999, "drives": {}}"#).unwrap();
        let refused = read_registry(&path).unwrap_err();
        assert!(refused.to_string().contains("version 999"), "{refused}");
    }

    #[test]
    fn registry_skips_invalid_entries_with_reasons_and_keeps_valid_ones() {
        let dir = scratch("invalid");
        let path = dir.join("drives.json");
        let good = entry("good", &dir);
        let mut bad_relative = entry("bad", &dir);
        bad_relative.drive_dir = PathBuf::from("relative/path");
        let mut bad_name = entry("BAD NAME!", &dir);
        bad_name.name = "BAD NAME!".to_string();
        let mut drives = BTreeMap::new();
        drives.insert("good".to_string(), good);
        // Written by hand, bypassing the validating writer.
        let raw = serde_json::json!({
            "version": 1,
            "drives": {
                "good": drives["good"],
                "bad": bad_relative,
                "BAD NAME!": bad_name,
            }
        });
        std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();

        let read = read_registry(&path).unwrap();
        assert_eq!(read.drives.len(), 1);
        assert!(read.drives.contains_key("good"));
        assert_eq!(read.skipped.len(), 2, "{:?}", read.skipped);
    }

    #[test]
    fn registry_write_refuses_invalid_entries_and_never_writes_partial() {
        let dir = scratch("nowrite");
        let path = dir.join("drives.json");
        let mut drives = BTreeMap::new();
        let mut bad = entry("bad", &dir);
        bad.drive_dir = PathBuf::from("relative/path");
        drives.insert("bad".to_string(), bad);
        let refused = write_registry(&path, &drives).unwrap_err();
        assert!(refused.to_string().contains("absolute"), "{refused}");
        assert!(!path.exists(), "no partial registry may remain");
    }

    /// A present-but-unreadable registry refuses the write instead
    /// of being replaced: malformed JSON fails loud, and a newer
    /// schema is never downgraded to version 1 with only the
    /// candidates. Either way nothing is written.
    #[test]
    fn registry_write_refuses_unreadable_and_newer_files() {
        let dir = scratch("refuse-unreadable");
        let malformed = dir.join("drives.json");
        std::fs::write(&malformed, r#"{"version": 1, "drives": {"#).unwrap();
        let mut drives = BTreeMap::new();
        drives.insert("new".to_string(), entry("new", &dir));
        let refused = write_registry(&malformed, &drives).unwrap_err();
        assert!(refused.to_string().contains("parse"), "{refused}");
        assert_eq!(
            std::fs::read(&malformed).unwrap(),
            b"{\"version\": 1, \"drives\": {",
            "the malformed file is untouched"
        );

        let newer = dir.join("newer.json");
        std::fs::write(&newer, r#"{"version": 999, "drives": {}}"#).unwrap();
        let refused = write_registry(&newer, &drives).unwrap_err();
        assert!(refused.to_string().contains("999"), "{refused}");
        assert_eq!(
            std::fs::read(&newer).unwrap(),
            br#"{"version": 999, "drives": {}}"#,
            "the newer file is never downgraded"
        );
    }

    /// A structurally corrupt entry refuses the write: it decodes
    /// as a raw value but not as a registry entry, exactly like the
    /// reader, so carrying it and reporting success would contradict
    /// the reader's loud verdict on the same file. Nothing is written.
    #[test]
    fn registry_write_refuses_structurally_corrupt_entries() {
        let dir = scratch("refuse-corrupt");
        let path = dir.join("drives.json");
        let corrupt = r#"{"version": 1, "drives": {"x": {"drive_dir": "/d"}}}"#;
        std::fs::write(&path, corrupt).unwrap();
        let mut drives = BTreeMap::new();
        drives.insert("new".to_string(), entry("new", &dir));
        let refused = write_registry(&path, &drives).unwrap_err();
        assert!(refused.to_string().contains("parse"), "{refused}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            corrupt.as_bytes(),
            "the corrupt file is untouched"
        );
    }

    /// Concurrent writers converge without lost entries: every thread
    /// writes one distinct key through the same read-modify-write,
    /// and the registry lock serializes them, so the final read holds
    /// every key. Without the lock, two writers racing on one base
    /// would leave only the second rename's entries (atomic rename
    /// does not stop lost updates). Threads share the process but
    /// open separate lock descriptions, so the kernel lock contends
    /// exactly like two processes.
    #[test]
    fn concurrent_registration_writes_converge_without_lost_entries() {
        let dir = scratch("concurrent");
        let path = dir.join("drives.json");
        let count = 8usize;
        std::thread::scope(|scope| {
            for i in 0..count {
                let path = &path;
                let dir = &dir;
                scope.spawn(move || {
                    let name = format!("w{i:02}");
                    let mut drives = BTreeMap::new();
                    drives.insert(name.clone(), entry(&name, dir));
                    write_registry(path, &drives).unwrap();
                });
            }
        });
        let read = read_registry(&path).unwrap();
        assert!(read.skipped.is_empty(), "{:?}", read.skipped);
        assert_eq!(read.drives.len(), count, "{:?}", read.drives.keys());
        for i in 0..count {
            assert!(
                read.drives.contains_key(&format!("w{i:02}")),
                "writer w{i:02} survived: {:?}",
                read.drives.keys()
            );
        }
    }

    /// Every reachable terminal provision site carries its
    /// machine-readable kind: direct callers match on `kind`, never
    /// on `reason` text. Rendered text is pinned where the CLI owns
    /// it (`crates/wyrd-cli/tests/bootstrap.rs`, contains-pins on
    /// the state lines); this test pins the mapping from site to
    /// kind, plus the `stage` agreement for the incomplete sites.
    #[test]
    fn provision_failures_carry_their_machine_readable_kind() {
        use ProvisionFailureKind as Kind;
        // Validation refuses before anything is created.
        let dir = scratch("kind-validate");
        let registry = dir.join("drives.json");
        let mut config = provision_no_service(&dir, &registry, "BAD NAME!");
        config.name = "BAD NAME!".to_string();
        match provision(config) {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, Kind::Validate);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("validation must stop first: {other:?}"),
        }
        // An undecodable invitation stops at initialization.
        let dir = scratch("kind-invitation");
        let registry = dir.join("drives.json");
        let mut config = provision_no_service(&dir, &registry, "demo");
        config.init = Init::Join {
            sealed_invitation: b"not a sealed invitation".to_vec(),
        };
        match provision(config) {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, Kind::DecodeInvitation);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("a bad invitation must stop at initialize: {other:?}"),
        }
        // A drive directory that is a regular file cannot be
        // initialized as a store.
        let dir = scratch("kind-initialize");
        let blocker = dir.join("file");
        std::fs::write(&blocker, b"x").unwrap();
        let registry = dir.join("drives.json");
        let mut config = provision_no_service(&dir, &registry, "demo");
        config.drive_dir = blocker;
        match provision(config) {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, Kind::Initialize);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("a file cannot be initialized: {other:?}"),
        }
        // A registry path that is a directory cannot be read.
        let dir = scratch("kind-read-registry");
        let config = provision_no_service(&dir, &dir, "demo");
        match provision(config) {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, Kind::ReadRegistry);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("an unreadable registry must stop: {other:?}"),
        }
        // Managed-service binary lookup stops before any service
        // operation: this is pinned by construction, not by test —
        // exercising it end to end would install a real service
        // definition whenever the test binary has version-matched
        // siblings, so `NoWyrdBinary` and `NoSuperviseBinary` stay
        // literal-adjacent to their reason strings like
        // `WriteRegistry` and `VerifyOpen` below. (`WriteRegistry`
        // needs a write failure after a successful read of the same
        // path, and `VerifyOpen` needs a drive that creates but does
        // not open; neither is reachable hermetically.)
        // No owner socket verifies nothing, with the socket kind.
        let dir = scratch("kind-socket");
        let registry = dir.join("drives.json");
        match provision(provision_no_service(&dir, &registry, "demo")) {
            ProvisionState::RegisteredButUnavailable { kind, reason, .. } => {
                assert_eq!(kind, Kind::VerifySocket);
                assert!(reason.contains("verify-socket"), "{reason}");
            }
            other => panic!("a missing owner must never verify: {other:?}"),
        }
    }

    /// `run` reports spawn and command failures with their program
    /// and detail: missing programs never spawn, failing programs
    /// report their own text. The service-stage `ProvisionFailureKind`
    /// variants (`NoWyrdBinary`, `NoSuperviseBinary`,
    /// `InstallService`, `EnableService`, `StartService`,
    /// `StopService`) and the service-manager `BootstrapError`
    /// variants beneath them (`Spawn`, `CommandFailed`,
    /// `ReloadUnits`, `AgentNotLoaded`, `RestartService`,
    /// `StopService`) stay literal-adjacent to their reason
    /// strings — reaching them end to end would touch a real
    /// manager, as the provision kind test documents.
    #[test]
    fn service_commands_report_spawn_and_command_failures() {
        // `run` itself: missing program spawns nothing, a failing
        // program reports its own text.
        match run("wyrd-definitely-missing-binary-xyz", &[]) {
            Err(BootstrapError::Spawn { program, .. }) => {
                assert_eq!(program, "wyrd-definitely-missing-binary-xyz");
            }
            other => panic!("a missing program must fail spawn: {other:?}"),
        }
        match run("sh", &["-c", "exit 3"]) {
            Err(BootstrapError::CommandFailed {
                program, detail, ..
            }) => {
                assert_eq!(program, "sh");
                assert!(detail.is_empty(), "exit 3 carries no stderr: {detail:?}");
            }
            other => panic!("a failing program must report: {other:?}"),
        }
    }

    #[test]
    fn registry_missing_file_is_an_empty_registry() {
        let dir = scratch("missing");
        let read = read_registry(&dir.join("absent.json")).unwrap();
        assert!(read.drives.is_empty() && read.skipped.is_empty());
    }

    /// Every `Display` string pinned exactly: a future edit that
    /// changes user-visible text fails here. Variants over foreign
    /// errors (`Parse`, `Encode`) pin the format around the live
    /// source error instead of the dependency's wording.
    #[test]
    fn bootstrap_display_reproduces_the_historical_messages_exactly() {
        use std::io::{Error as IoError, ErrorKind};
        let io = |kind: ErrorKind, message: &str| IoError::new(kind, message.to_owned());
        let parse_source = serde_json::from_slice::<serde_json::Value>(b"xx").unwrap_err();
        struct AlwaysFails;
        impl Serialize for AlwaysFails {
            fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("boom"))
            }
        }
        let encode_source = serde_json::to_vec(&AlwaysFails).unwrap_err();
        let command = BootstrapError::CommandFailed {
            program: "launchctl".to_string(),
            args: "list missing".to_string(),
            detail: "No such process".to_string(),
        };
        let cases: Vec<(BootstrapError, String)> = vec![
            (
                BootstrapError::Read {
                    path: "/reg/drives.json".into(),
                    source: io(ErrorKind::NotFound, "missing"),
                },
                "read /reg/drives.json: missing".to_string(),
            ),
            (
                BootstrapError::Parse {
                    path: "/reg/drives.json".into(),
                    source: parse_source,
                },
                format!(
                    "parse /reg/drives.json: {}",
                    serde_json::from_slice::<serde_json::Value>(b"xx").unwrap_err()
                ),
            ),
            (
                BootstrapError::UnsupportedVersion {
                    path: "/reg/drives.json".into(),
                    found: 999,
                },
                format!("registry /reg/drives.json is version 999, this build reads {REGISTRY_VERSION}"),
            ),
            (
                BootstrapError::KeyMismatch {
                    key: "a".to_string(),
                    name: "b".to_string(),
                },
                "key \"a\" does not match entry name \"b\"".to_string(),
            ),
            (
                BootstrapError::InvalidName {
                    name: "BAD NAME!".to_string(),
                },
                "name \"BAD NAME!\" must be 1-64 chars of [a-z0-9-]".to_string(),
            ),
            (
                BootstrapError::NameTooLongForServicePrefix {
                    name: "a-very-long-drive-name-that-leaves-no-room-at-all-xyz".to_string(),
                },
                "name \"a-very-long-drive-name-that-leaves-no-room-at-all-xyz\" leaves no room for the drive- service prefix (max 58 chars)".to_string(),
            ),
            (
                BootstrapError::InvalidService {
                    service: "BAD!".to_string(),
                },
                "service \"BAD!\" must be 1-64 chars of [a-z0-9-]".to_string(),
            ),
            (
                BootstrapError::RelativeDriveDir {
                    dir: "relative/path".into(),
                },
                "drive_dir relative/path must be absolute".to_string(),
            ),
            (
                BootstrapError::DriveDirHasDotDot {
                    dir: "/reg/../drive".into(),
                },
                "drive_dir /reg/../drive must not contain ..".to_string(),
            ),
            (BootstrapError::UnknownServiceMode, "unknown service mode: this build manages `vault` and `mount`".to_string()),
            (
                BootstrapError::InvalidRelays,
                "relays must be non-empty URLs without whitespace or control bytes".to_string(),
            ),
            (
                BootstrapError::DriveDirHasControlChars {
                    dir: "/reg/drive".into(),
                },
                "drive_dir /reg/drive must not contain control characters".to_string(),
            ),
            (
                BootstrapError::RefusedWrite {
                    source: Box::new(BootstrapError::RelativeDriveDir {
                        dir: "relative/path".into(),
                    }),
                },
                "refusing to write: drive_dir relative/path must be absolute".to_string(),
            ),
            (
                BootstrapError::Encode {
                    source: encode_source,
                },
                "encode registry: boom".to_string(),
            ),
            (
                BootstrapError::CreateDir {
                    path: "/reg".into(),
                    source: io(ErrorKind::PermissionDenied, "denied"),
                },
                "create /reg: denied".to_string(),
            ),
            (
                BootstrapError::WriteFile {
                    path: "/reg/drives.json".into(),
                    source: io(ErrorKind::StorageFull, "full"),
                },
                "write /reg/drives.json: full".to_string(),
            ),
            (
                BootstrapError::ReplaceFile {
                    path: "/reg/drives.json".into(),
                    source: io(ErrorKind::PermissionDenied, "denied"),
                },
                "replace /reg/drives.json: denied".to_string(),
            ),
            (
                BootstrapError::Chmod {
                    path: "/reg/drives.json".into(),
                    source: io(ErrorKind::PermissionDenied, "denied"),
                },
                "chmod /reg/drives.json: denied".to_string(),
            ),
            (
                BootstrapError::SyncFile {
                    path: "/reg/drives.json".into(),
                    source: io(ErrorKind::Other, "sync lost"),
                },
                "sync /reg/drives.json: sync lost".to_string(),
            ),
            (
                BootstrapError::RemoveFile {
                    path: "/reg/drive-demo.plist".into(),
                    source: io(ErrorKind::PermissionDenied, "denied"),
                },
                "remove /reg/drive-demo.plist: denied".to_string(),
            ),
            (
                BootstrapError::Spawn {
                    program: "launchctl".to_string(),
                    source: io(ErrorKind::NotFound, "missing"),
                },
                "spawn launchctl: missing".to_string(),
            ),
            (
                BootstrapError::CommandFailed {
                    program: "launchctl".to_string(),
                    args: "list missing".to_string(),
                    detail: "No such process".to_string(),
                },
                "launchctl list missing failed: No such process".to_string(),
            ),
            (
                BootstrapError::EnableService {
                    name: "md.enesis.wyrd.drive-demo".to_string(),
                    source: Box::new(command),
                },
                "enable md.enesis.wyrd.drive-demo: launchctl list missing failed: No such process"
                    .to_string(),
            ),
            (
                BootstrapError::ReloadUnits {
                    source: Box::new(BootstrapError::Spawn {
                        program: "systemctl".to_string(),
                        source: io(ErrorKind::NotFound, "missing"),
                    }),
                },
                "reload user units: spawn systemctl: missing".to_string(),
            ),
            (
                BootstrapError::AgentNotLoaded {
                    label: "md.enesis.wyrd.drive-demo".to_string(),
                    source: Box::new(BootstrapError::CommandFailed {
                        program: "launchctl".to_string(),
                        args: "list md.enesis.wyrd.drive-demo".to_string(),
                        detail: "No such process".to_string(),
                    }),
                },
                "start md.enesis.wyrd.drive-demo: agent not loaded: launchctl list md.enesis.wyrd.drive-demo failed: No such process".to_string(),
            ),
            (
                BootstrapError::RestartService {
                    unit: "wyrd-drive-demo.service".to_string(),
                    source: Box::new(BootstrapError::Spawn {
                        program: "systemctl".to_string(),
                        source: io(ErrorKind::NotFound, "missing"),
                    }),
                },
                "restart wyrd-drive-demo.service: spawn systemctl: missing".to_string(),
            ),
            (
                BootstrapError::StopService {
                    name: "wyrd-drive-demo.service".to_string(),
                    source: Box::new(BootstrapError::Spawn {
                        program: "systemctl".to_string(),
                        source: io(ErrorKind::NotFound, "missing"),
                    }),
                },
                "stop wyrd-drive-demo.service: spawn systemctl: missing".to_string(),
            ),
            (
                BootstrapError::ServicesUnsupported,
                "per-drive services are implemented for Unix (launchd agent, systemd user unit)"
                    .to_string(),
            ),
        ];
        assert_eq!(cases.len(), 28, "every variant pins its message");
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected, "Display changed");
        }
    }

    #[test]
    fn bootstrap_failures_match_by_variant_not_by_text() {
        let dir = scratch("typed");
        let mut bad = entry("bad", &dir);
        bad.name = "BAD NAME!".to_string();
        let error = validate_entry("BAD NAME!", &bad).unwrap_err();
        assert!(
            matches!(error, BootstrapError::InvalidName { .. }),
            "callers branch on the variant, never parse the message: {error}"
        );
        // The text readers keep working: Display is unchanged.
        assert!(
            error
                .to_string()
                .contains("must be 1-64 chars of [a-z0-9-]"),
            "{error}"
        );
        let nested = BootstrapError::RefusedWrite {
            source: Box::new(error),
        };
        assert!(
            nested.to_string().starts_with("refusing to write: "),
            "{nested}"
        );
    }

    #[test]
    fn service_templates_name_the_service_paths_and_relays() {
        let dir = scratch("templates");
        let owner = SupervisedOwner {
            supervise_bin: PathBuf::from("/opt/wyrd/bin/wyrd-supervise"),
            wyrd_bin: PathBuf::from("/opt/wyrd/bin/wyrd"),
            identity_file: dir.join("identity"),
            passphrase_file: dir.join("passphrase"),
        };
        let owned = entry("demo", &dir);
        // Whole-argv pin, order-sensitive: clap parses
        // `owner <dir> [parent flags] start` and rejects flags after
        // `start`, so a substring check would pass on an unparseable
        // definition. The supervised tail after `--` must match
        // exactly.
        let argv = owner.argv(&owned, "0");
        let tail = argv
            .iter()
            .skip_while(|arg| *arg != "--")
            .skip(1)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            tail,
            vec![
                "/opt/wyrd/bin/wyrd",
                "owner",
                &owned.drive_dir.to_string_lossy(),
                "--identity-file",
                &dir.join("identity").to_string_lossy(),
                "--passphrase-file",
                &dir.join("passphrase").to_string_lossy(),
                "--relay",
                "relay://example",
                "start",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>(),
            "{argv:?}"
        );
        let plist = render_launchd_plist("md.enesis.wyrd.drive-demo", &owner, &owned);
        assert!(plist.contains("<string>md.enesis.wyrd.drive-demo</string>"));
        assert!(plist.contains("wyrd-supervise"));
        assert!(plist.contains("--restart-status"));
        assert!(plist.contains("<string>0</string>"), "{plist}");
        assert!(plist.contains("vault.posture"));
        assert!(plist.contains("<string>owner</string>"), "{plist}");
        assert!(plist.contains("<string>start</string>"), "{plist}");
        assert!(plist.contains(&*owned.drive_dir.display().to_string()));
        assert!(plist.contains("--identity-file"));
        assert!(plist.contains("--relay"));
        assert!(plist.contains("<key>SuccessfulExit</key>"));
        // Owner-first ordering: the agent starts at login, before any
        // UI opens — the UI reads Unavailable until this socket appears.
        assert!(plist.contains("<key>RunAtLoad</key>"), "{plist}");
        let unit = render_systemd_unit("Wyrd drive demo", &owner, &owned);
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("RestartPreventExitStatus=78"));
        assert!(unit.contains("wyrd-supervise"));
        assert!(unit.contains("--restart-status=1") || unit.contains("--restart-status 1"));
        assert!(unit.contains(&*owned.drive_dir.display().to_string()));
    }

    #[test]
    fn mount_templates_name_the_mountpoint_and_posture() {
        let dir = scratch("templates-mount");
        let owner = SupervisedOwner {
            supervise_bin: PathBuf::from("/opt/wyrd/bin/wyrd-supervise"),
            wyrd_bin: PathBuf::from("/opt/wyrd/bin/wyrd"),
            identity_file: dir.join("identity"),
            passphrase_file: dir.join("passphrase"),
        };
        // A mountpoint with spaces and an ampersand pins the
        // operator-input escaping on both managers: the argv must
        // carry it whole, never split or reinterpreted.
        let mountpoint = dir.join("my mounts & more");
        let owned = mount_entry("demo", &dir, &mountpoint);
        let argv = owner.argv(&owned, "1");
        assert!(argv.contains(&"mount".to_string()), "{argv:?}");
        assert!(
            argv.contains(&mountpoint.to_string_lossy().into_owned()),
            "{argv:?}"
        );
        assert!(!argv.contains(&"vault".to_string()), "{argv:?}");
        assert!(
            argv.iter().any(|arg| arg.ends_with("mount.posture")),
            "{argv:?}"
        );
        let plist = render_launchd_plist("md.enesis.wyrd.drive-demo", &owner, &owned);
        assert!(plist.contains("my mounts &amp; more"), "{plist}");
        assert!(plist.contains("mount.posture"));
        assert!(plist.contains("<string>mount</string>"), "{plist}");
        assert!(plist.contains("<key>SuccessfulExit</key>"), "{plist}");
        let unit = render_systemd_unit("Wyrd drive demo", &owner, &owned);
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("RestartPreventExitStatus=78"));
        assert!(unit.contains("mount.posture"), "{unit}");
        assert!(unit.contains(" mount "), "{unit}");
    }

    #[test]
    fn mount_validation_pins_the_mountpoint_rules() {
        let dir = scratch("mount-validate");
        // Mount without a mountpoint refuses.
        let mut missing = entry("demo", &dir);
        missing.mode = ServiceMode::Mount;
        assert!(matches!(
            validate_entry("demo", &missing),
            Err(BootstrapError::MissingMountpoint)
        ));
        // Vault with a mountpoint refuses.
        let mut stray = entry("demo", &dir);
        stray.mountpoint = Some(dir.join("mnt"));
        assert!(matches!(
            validate_entry("demo", &stray),
            Err(BootstrapError::UnexpectedMountpoint)
        ));
        // Relative, escaping, and control-carrying mountpoints refuse.
        let mut relative = mount_entry("demo", &dir, PathBuf::from("relative/mnt").as_path());
        let error = validate_entry("demo", &relative).unwrap_err();
        assert!(error.to_string().contains("must be absolute"), "{error}");
        relative.mountpoint = Some(PathBuf::from("/ok/../evil"));
        let error = validate_entry("demo", &relative).unwrap_err();
        assert!(error.to_string().contains("must not contain .."), "{error}");
        // A mountpoint equal to the drive directory would mount the
        // projection over the store itself, so it refuses.
        let drive = dir.join("drive");
        let mut same = mount_entry("demo", &dir, drive.as_path());
        same.drive_dir = drive.clone();
        let error = validate_entry("demo", &same).unwrap_err();
        assert!(error.to_string().contains("must not equal"), "{error}");
        // Unknown future modes still skip with the generic reason.
        let mut future = entry("demo", &dir);
        future.mode = ServiceMode::Unknown("fuse".to_string());
        let error = validate_entry("demo", &future).unwrap_err();
        assert!(
            error.to_string().contains("unknown service mode"),
            "{error}"
        );
        assert!(
            !error.to_string().contains("installer follow-up"),
            "the follow-up pointer is retired: {error}"
        );
    }

    #[test]
    fn provision_mount_registers_and_verifies_the_socket() {
        let dir = scratch("prov-mnt");
        let registry = dir.join("drives.json");
        let mountpoint = dir.join("mnt");
        std::fs::create_dir_all(&mountpoint).unwrap();
        let sock = dir.join("drive").join("owner.sock");
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let server = stub_owner(sock, 1, "Unobserved");
        let report = provision(provision_mount_no_service(
            &dir,
            &registry,
            "demo",
            &mountpoint,
        ));
        let ProvisionState::VerifiedAvailable { entry, .. } = report else {
            panic!("a stubbed owner must verify a mount provision: {report:?}");
        };
        assert!(matches!(entry.mode, ServiceMode::Mount));
        assert_eq!(entry.mountpoint, Some(mountpoint.clone()));
        let read = read_registry(&registry).unwrap();
        assert!(read.skipped.is_empty());
        assert_eq!(read.drives["demo"].mountpoint, Some(mountpoint));
        server.join().unwrap();
    }

    #[test]
    fn provision_mount_without_a_mountpoint_refuses_before_creating() {
        let dir = scratch("provision-mount-nomp");
        let registry = dir.join("drives.json");
        let mut config = provision_no_service(&dir, &registry, "demo");
        config.mode = ServiceMode::Mount;
        config.mountpoint = None;
        let report = provision(config);
        assert!(
            matches!(
                report,
                ProvisionState::ProvisioningIncomplete {
                    stage: "validate",
                    ..
                }
            ),
            "validation precedes initialization: {report:?}"
        );
        assert!(!dir.join("drive").exists());
        assert!(!registry.exists());
    }

    /// Concurrent same-key updates through the locked path converge
    /// on one update with no torn mix: two writers racing on one key
    /// serialize under the lock, so the final entry is one writer's
    /// whole update — never one writer's relays beside the other's
    /// mode. Without the lock (a stale read plus a whole-map write)
    /// the second rename would silently clobber the first update.
    #[test]
    fn concurrent_same_key_updates_converge_without_a_torn_mix() {
        let dir = scratch("samekey");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let mountpoint = dir.join("mnt");
        std::fs::create_dir_all(&mountpoint).unwrap();
        let registry_clone = registry.clone();
        let mountpoint_clone = mountpoint.clone();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                modify_registry(&registry, |drives| {
                    let stored = drives.get("demo").expect("demo registered").clone();
                    let updated = updated_entry(
                        stored,
                        vec!["relay://a".to_string()],
                        ServiceMode::Vault,
                        None,
                    );
                    drives.insert("demo".to_string(), updated);
                    Ok(())
                })
                .unwrap();
            });
            scope.spawn(|| {
                modify_registry(&registry_clone, |drives| {
                    let stored = drives.get("demo").expect("demo registered").clone();
                    let updated = updated_entry(
                        stored,
                        vec!["relay://b".to_string()],
                        ServiceMode::Mount,
                        Some(mountpoint_clone.clone()),
                    );
                    drives.insert("demo".to_string(), updated);
                    Ok(())
                })
                .unwrap();
            });
        });
        let read = read_registry(&registry).unwrap();
        let entry = &read.drives["demo"];
        // One winner, whole: relays and mode/mountpoint agree with
        // each other, never a mix of both writers.
        match entry.relays.as_slice() {
            [relay] if relay == "relay://a" => {
                assert!(matches!(entry.mode, ServiceMode::Vault));
                assert_eq!(entry.mountpoint, None);
            }
            [relay] if relay == "relay://b" => {
                assert!(matches!(entry.mode, ServiceMode::Mount));
                assert_eq!(entry.mountpoint, Some(mountpoint));
            }
            other => panic!("one writer must win whole: {other:?}"),
        }
    }

    /// Per-field writers through the locked path both survive: writer
    /// A changes only the relays (taking mode and mountpoint from the
    /// entry it reads under the lock), writer B changes only the
    /// mode and mountpoint (keeping the fresh relays). Either commit
    /// order leaves A's relays beside B's mode — a stale whole-map
    /// write would lose one side whichever committed last.
    #[test]
    fn concurrent_disjoint_updates_both_survive() {
        let dir = scratch("disjoint");
        let registry = dir.join("drives.json");
        let mountpoint = dir.join("mnt");
        std::fs::create_dir_all(&mountpoint).unwrap();
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let registry_clone = registry.clone();
        let mountpoint_clone = mountpoint.clone();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                modify_registry(&registry, |drives| {
                    let fresh = drives.get("demo").expect("demo registered").clone();
                    let updated = updated_entry(
                        fresh.clone(),
                        vec!["relay://a".to_string()],
                        fresh.mode,
                        fresh.mountpoint,
                    );
                    drives.insert("demo".to_string(), updated);
                    Ok(())
                })
                .unwrap();
            });
            scope.spawn(|| {
                modify_registry(&registry_clone, |drives| {
                    let fresh = drives.get("demo").expect("demo registered").clone();
                    let updated = updated_entry(
                        fresh.clone(),
                        fresh.relays,
                        ServiceMode::Mount,
                        Some(mountpoint_clone.clone()),
                    );
                    drives.insert("demo".to_string(), updated);
                    Ok(())
                })
                .unwrap();
            });
        });
        let read = read_registry(&registry).unwrap();
        let entry = &read.drives["demo"];
        assert_eq!(entry.relays, vec!["relay://a".to_string()]);
        assert!(matches!(entry.mode, ServiceMode::Mount));
        assert_eq!(entry.mountpoint, Some(mountpoint));
    }

    /// A kept relay set never reverts a concurrent update: writer B
    /// sets an explicit relay set while writer A reconfigures with no
    /// `--relay` flags (keep intent) at the `reconfigure` call sites
    /// themselves. Whatever order the two rewrites commit, the final
    /// entry carries B's set — A's keep resolves fresh, never stale.
    /// Retries on the terminal store lock keep the race hermetic: two
    /// reconfigures in one process briefly contend on the keystore
    /// open, and a refused attempt simply re-runs.
    #[test]
    fn reconfigure_keep_never_reverts_a_concurrent_relay_update() {
        let dir = scratch("keep-race");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let sock = dir.join("drive").join("owner.sock");
        // Exactly two connections: each thread verifies once, on the
        // attempt its keystore open succeeds — attempts refused with
        // the store lock never reach the socket.
        let server = stub_owner(sock, 2, "Unobserved");
        let registry_a = registry.clone();
        let registry_b = registry.clone();
        let dir_a = dir.clone();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                for _ in 0..60 {
                    let report = reconfigure(reconfigure_no_service(
                        &dir_a,
                        &registry_a,
                        None,
                        ServiceMode::Vault,
                        None,
                    ));
                    match report {
                        ProvisionState::VerifiedAvailable { .. } => return,
                        ProvisionState::ProvisioningIncomplete {
                            kind: ProvisionFailureKind::StoreLocked,
                            ..
                        } => {
                            // The keystore open holds through the
                            // KDF, so a colliding attempt backs
                            // off instead of spinning against it.
                            std::thread::sleep(std::time::Duration::from_millis(200));
                            continue;
                        }
                        other => panic!("keep must verify or contend: {other:?}"),
                    }
                }
                panic!("keep never verified past contention");
            });
            scope.spawn(move || {
                for _ in 0..60 {
                    let report = reconfigure(reconfigure_no_service(
                        &dir,
                        &registry_b,
                        Some(vec!["relay://new".to_string()]),
                        ServiceMode::Vault,
                        None,
                    ));
                    match report {
                        ProvisionState::VerifiedAvailable { .. } => return,
                        ProvisionState::ProvisioningIncomplete {
                            kind: ProvisionFailureKind::StoreLocked,
                            ..
                        } => {
                            std::thread::sleep(std::time::Duration::from_millis(200));
                            continue;
                        }
                        other => panic!("explicit update must verify or contend: {other:?}"),
                    }
                }
                panic!("explicit update never verified past contention");
            });
        });
        let read = read_registry(&registry).unwrap();
        assert_eq!(
            read.drives["demo"].relays,
            vec!["relay://new".to_string()],
            "the explicit update survives the keep"
        );
        server.join().unwrap();
    }

    /// Post-stop failures classify honestly: resolve failures name
    /// the registry state to repair with the stopped-service note,
    /// genuine write failures stay `write-registry`.
    #[test]
    fn post_stop_failures_classify_the_repair() {
        let (stage, kind, reason) = classify_post_stop_failure(&BootstrapError::Read {
            path: "/reg/drives.json".into(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no registry entry for /d: register with `wyrd bootstrap` first",
            ),
        });
        assert_eq!(stage, "validate");
        assert_eq!(kind, ProvisionFailureKind::Validate);
        assert!(reason.contains("already stopped and cleared"), "{reason}");
        assert!(reason.contains("re-register"), "{reason}");

        let (stage, kind, reason) =
            classify_post_stop_failure(&BootstrapError::ConflictingEntries {
                dir: "/d".into(),
                keys: "a, b".to_string(),
            });
        assert_eq!(stage, "validate");
        assert_eq!(kind, ProvisionFailureKind::Validate);
        assert!(reason.contains("resolve the duplicate"), "{reason}");

        let (stage, kind, reason) =
            classify_post_stop_failure(&BootstrapError::ManagedWithoutRelays);
        assert_eq!(stage, "validate");
        assert_eq!(kind, ProvisionFailureKind::Validate);
        assert!(reason.contains("--no-service"), "{reason}");

        let (stage, kind, _) = classify_post_stop_failure(&BootstrapError::RefusedWrite {
            source: Box::new(BootstrapError::InvalidRelays),
        });
        assert_eq!(stage, "write-registry");
        assert_eq!(kind, ProvisionFailureKind::WriteRegistry);
    }

    /// The reinstall decision compares relay sets only: equal sets
    /// need no reinstall (order counts — a rewrite is harmless but
    /// pointless), differing sets do, and a mode or mountpoint
    /// difference alone never triggers one (both copies carry
    /// operator intent there).
    #[test]
    fn reinstall_decision_compares_relay_sets_only() {
        let dir = scratch("reinstall-decide");
        let base = entry("demo", &dir);
        assert!(!definition_needs_reinstall(&base, &base));
        let mut ordered = base.clone();
        ordered.relays = vec!["relay://a".to_string(), "relay://b".to_string()];
        let mut reordered = base.clone();
        reordered.relays = vec!["relay://b".to_string(), "relay://a".to_string()];
        assert!(definition_needs_reinstall(&ordered, &reordered));
        let mut mode_only = base.clone();
        mode_only.mode = ServiceMode::Mount;
        mode_only.mountpoint = Some(dir.join("mnt"));
        assert!(!definition_needs_reinstall(&base, &mode_only));
        let mut differing = base.clone();
        differing.relays = vec!["relay://fresh".to_string()];
        assert!(definition_needs_reinstall(&base, &differing));
    }

    /// A definition install failure carries the install-service kind:
    /// the caller reports it post-registration instead of mistaking
    /// it for a write or start failure. Hermetic: the override names
    /// a path whose parent is a file, so the atomic write fails
    /// before any manager is touched.
    #[test]
    fn install_failure_carries_the_install_service_kind() {
        let dir = scratch("reinstall-kind");
        let owner = SupervisedOwner {
            supervise_bin: dir.join("wyrd-supervise"),
            wyrd_bin: dir.join("wyrd"),
            identity_file: dir.join("identity"),
            passphrase_file: dir.join("passphrase"),
        };
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let definition = blocker.join("wyrd-test-definition");
        match install_service_definition(&entry("demo", &dir), &owner, Some(&definition)) {
            Err((kind, reason)) => {
                assert_eq!(kind, ProvisionFailureKind::InstallService);
                assert!(reason.contains("install-service"), "{reason}");
            }
            Ok(_) => panic!("an unwritable definition path must fail install"),
        }
    }

    /// Starting an unregistered drive refuses before anything is
    /// touched: no keystore opens, no manager is contacted, nothing
    /// is written. The pointer names registration, not supervision.
    #[test]
    fn start_unregistered_drive_refuses_with_the_registration_pointer() {
        let dir = scratch("start-unregistered");
        let registry = dir.join("drives.json");
        let report = start_managed_service(StartServiceConfig {
            drive_dir: dir.join("drive"),
            registry_path: registry.clone(),
            service_definition: Some(dir.join("wyrd-test-definition")),
            verify_timeout: Duration::from_secs(1),
        });
        match report {
            ProvisionState::ProvisioningIncomplete { stage, reason, .. } => {
                assert_eq!(stage, "validate");
                assert!(reason.contains("register with"), "{reason}");
            }
            other => panic!("an unregistered drive refuses before anything moves: {other:?}"),
        }
        assert!(!registry.exists(), "a refusal writes nothing");
    }

    /// Starting without an installed definition refuses with the
    /// reinstall pointer — never the manager: a missing file is the
    /// reinstall recovery, not this one. Hermetic: the override
    /// names a path that never exists, so no manager is touched.
    #[test]
    fn start_without_a_definition_refuses_with_the_reinstall_pointer() {
        let dir = scratch("start-nodef");
        let registry = dir.join("drives.json");
        let mut drives = BTreeMap::new();
        drives.insert("demo".to_string(), entry("demo", &dir));
        write_registry(&registry, &drives).unwrap();
        let missing = dir.join("wyrd-test-definition-missing");
        assert!(!missing.exists());
        let report = start_managed_service(StartServiceConfig {
            drive_dir: dir.join("drive"),
            registry_path: registry,
            service_definition: Some(missing),
            verify_timeout: Duration::from_secs(1),
        });
        match report {
            ProvisionState::ProvisioningIncomplete { stage, reason, .. } => {
                assert_eq!(stage, "validate");
                assert!(reason.contains("reinstall"), "{reason}");
                assert!(reason.contains("reconfigure"), "{reason}");
            }
            other => panic!("a missing definition refuses with reinstall: {other:?}"),
        }
    }

    /// Starting against a definition the manager cannot load reports
    /// unavailable, never success: the enable/start failure maps to
    /// its stage kind like every other handoff. Hermetic: the
    /// override names a scratch file with garbage content, so the
    /// load fails before anything is registered — no live unit is
    /// touched and no residue can remain. (On macOS `load` fails on
    /// the garbage plist; on Linux the unit name is unknown to the
    /// user manager, so enable fails first.)
    #[test]
    fn start_with_an_unloadable_definition_reports_unavailable() {
        let dir = scratch("start-baddef");
        let registry = dir.join("drives.json");
        let mut drives = BTreeMap::new();
        drives.insert("demo".to_string(), entry("demo", &dir));
        write_registry(&registry, &drives).unwrap();
        let definition = dir.join("wyrd-test-definition");
        std::fs::write(&definition, b"not a service definition").unwrap();
        let report = start_managed_service(StartServiceConfig {
            drive_dir: dir.join("drive"),
            registry_path: registry,
            service_definition: Some(definition),
            verify_timeout: Duration::from_secs(1),
        });
        match report {
            ProvisionState::RegisteredButUnavailable { reason, .. } => {
                assert!(
                    reason.contains("enable-service")
                        || reason.contains("start-service")
                        || reason.contains("verify-socket"),
                    "a refused start names its stage: {reason}"
                );
            }
            other => panic!("an unloadable definition reports unavailable: {other:?}"),
        }
    }

    /// The installed definition follows the written entry: when the
    /// lock resolves a fresher relay set than the pre-stop working
    /// copy carried, the definition is reinstalled from the written
    /// entry at the same override path — the manager never serves a
    /// stale relay set beside the new registry entry.
    #[test]
    fn reinstall_follows_the_written_relay_set() {
        let dir = scratch("reinstall");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        // Stage a concurrent update between the pre-stop read and the
        // rewrite by writing it directly: the working copy below
        // carries the stale pre-stop set, the file carries the fresh
        // set the lock will resolve.
        let mut read = read_registry(&registry).unwrap();
        read.drives.get_mut("demo").unwrap().relays = vec!["relay://fresh".to_string()];
        write_registry(&registry, &read.drives).unwrap();
        let stale = RegistryEntry {
            name: "demo".to_string(),
            drive_dir: dir.join("drive"),
            relays: vec![],
            service: "drive-demo".to_string(),
            mode: ServiceMode::Vault,
            mountpoint: None,
        };
        let written = RegistryEntry {
            relays: vec!["relay://fresh".to_string()],
            ..stale.clone()
        };
        let owner = SupervisedOwner {
            supervise_bin: dir.join("wyrd-supervise"),
            wyrd_bin: dir.join("wyrd"),
            identity_file: dir.join("identity"),
            passphrase_file: dir.join("passphrase"),
        };
        let definition = dir.join("wyrd-test-definition");
        install_service_definition(&stale, &owner, Some(&definition)).unwrap();
        assert!(
            !std::fs::read_to_string(&definition)
                .unwrap()
                .contains("relay://fresh"),
            "the stale install carries the stale set"
        );
        install_service_definition(&written, &owner, Some(&definition)).unwrap();
        let content = std::fs::read_to_string(&definition).unwrap();
        assert!(
            content.contains("relay://fresh"),
            "the reinstall carries the written set: {content}"
        );
    }

    /// Mount provisioning refuses a missing, file, containing, or
    /// contained mountpoint before anything is created: no drive
    /// directory and no registry appear, and the reason names the
    /// rule. Shape problems stay in the entry validator; these live
    /// checks run after it.
    #[test]
    fn provision_mount_refuses_unusable_mountpoints_before_creating() {
        // Missing mountpoint.
        let dir = scratch("prov-mnt-missing");
        let registry = dir.join("drives.json");
        let missing = dir.join("no-such-mnt");
        let report = provision(provision_mount_no_service(
            &dir, &registry, "demo", &missing,
        ));
        match report {
            ProvisionState::ProvisioningIncomplete { stage, reason, .. } => {
                assert_eq!(stage, "validate");
                assert!(reason.contains("does not exist"), "{reason}");
            }
            other => panic!("a missing mountpoint must refuse: {other:?}"),
        }
        assert!(!dir.join("drive").exists());
        assert!(!registry.exists());

        // Mountpoint that is a file, not a directory.
        let dir = scratch("prov-mnt-file");
        let registry = dir.join("drives.json");
        let file = dir.join("file-mnt");
        std::fs::write(&file, b"x").unwrap();
        let report = provision(provision_mount_no_service(&dir, &registry, "demo", &file));
        match report {
            ProvisionState::ProvisioningIncomplete { stage, reason, .. } => {
                assert_eq!(stage, "validate");
                assert!(reason.contains("not a directory"), "{reason}");
            }
            other => panic!("a file mountpoint must refuse: {other:?}"),
        }
        assert!(!registry.exists());

        // Mountpoint containing the drive directory.
        let dir = scratch("prov-mnt-contains");
        let registry = dir.join("drives.json");
        let report = provision(provision_mount_no_service(
            &dir,
            &registry,
            "demo",
            dir.as_path(),
        ));
        match report {
            ProvisionState::ProvisioningIncomplete { stage, reason, .. } => {
                assert_eq!(stage, "validate");
                assert!(reason.contains("must not contain"), "{reason}");
            }
            other => panic!("a containing mountpoint must refuse: {other:?}"),
        }
        assert!(!registry.exists());

        // Mountpoint inside the drive directory.
        let dir = scratch("prov-mnt-inside");
        let registry = dir.join("drives.json");
        let drive = dir.join("drive");
        std::fs::create_dir_all(&drive).unwrap();
        let inside = drive.join("mnt");
        std::fs::create_dir_all(&inside).unwrap();
        let report = provision(provision_mount_no_service(&dir, &registry, "demo", &inside));
        match report {
            ProvisionState::ProvisioningIncomplete { stage, reason, .. } => {
                assert_eq!(stage, "validate");
                assert!(reason.contains("must not sit inside"), "{reason}");
            }
            other => panic!("an inside mountpoint must refuse: {other:?}"),
        }
        assert!(!registry.exists());
    }

    /// A stub owner socket: answers every request with the given
    /// refusal code. It speaks the real framed protocol, so the
    /// handoff test exercises the real client, the real framing, and
    /// the real refusal mapping — only the owner is fake. The
    /// accept loop is deadline-bounded so the thread always returns
    /// for `join`, even when the caller makes fewer connections than
    /// the cap.
    pub(super) fn stub_owner(
        sock: PathBuf,
        max_requests: usize,
        code: &'static str,
    ) -> std::thread::JoinHandle<()> {
        if sock.exists() {
            std::fs::remove_file(&sock).unwrap();
        }
        std::thread::spawn(move || {
            let listener = UnixListener::bind(&sock).unwrap();
            listener.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            let mut served = 0;
            while served < max_requests && std::time::Instant::now() < deadline {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                served += 1;
                stream
                    .set_read_timeout(Some(Duration::from_secs(30)))
                    .unwrap();
                let mut len = [0u8; 4];
                if stream.read_exact(&mut len).is_err() {
                    continue;
                }
                let len = u32::from_be_bytes(len) as usize;
                if len > 96 * 1024 * 1024 {
                    continue;
                }
                let mut body = vec![0u8; len];
                if stream.read_exact(&mut body).is_err() {
                    continue;
                }
                let id: u64 = serde_json::from_slice::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|value| value.get("id")?.as_u64())
                    .unwrap_or(0);
                let reply = serde_json::json!({
                    "v": crate::owner::WIRE_VERSION,
                    "id": id,
                    "result": {"err": {"code": code, "message": "stub refusal"}},
                });
                let body = serde_json::to_vec(&reply).unwrap();
                let _ = stream.write_all(&(body.len() as u32).to_be_bytes());
                let _ = stream.write_all(&body);
            }
        })
    }

    pub(super) fn identity() -> DeviceIdentitySecret {
        DeviceIdentitySecret::from_bytes([7u8; 32]).unwrap()
    }

    pub(super) fn provision_no_service(dir: &Path, registry: &Path, name: &str) -> ProvisionConfig {
        ProvisionConfig {
            name: name.to_string(),
            drive_dir: dir.join("drive"),
            relays: vec![],
            mode: ServiceMode::Vault,
            mountpoint: None,
            identity: identity(),
            passphrase: "bootstrap-test-pass".to_string(),
            init: Init::Create,
            registry_path: registry.to_path_buf(),
            wyrd_bin: Some(dir.join("wyrd")),
            supervise_bin: Some(dir.join("wyrd-supervise")),
            identity_file: dir.join("identity"),
            passphrase_file: dir.join("passphrase"),
            manage_service: false,
            verify_timeout: Duration::from_secs(10),
        }
    }

    pub(super) fn provision_mount_no_service(
        dir: &Path,
        registry: &Path,
        name: &str,
        mountpoint: &Path,
    ) -> ProvisionConfig {
        let mut config = provision_no_service(dir, registry, name);
        config.mode = ServiceMode::Mount;
        config.mountpoint = Some(mountpoint.to_path_buf());
        config
    }

    #[test]
    fn provision_verifies_the_socket_and_registers_the_drive() {
        let dir = scratch("provision");
        let registry = dir.join("drives.json");
        let sock = dir.join("drive").join("owner.sock");
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let server = stub_owner(sock, 1, "Unobserved");

        let report = provision(provision_no_service(&dir, &registry, "demo"));
        let ProvisionState::VerifiedAvailable { entry, .. } = report else {
            panic!("expected verified available, got terminal state without success");
        };
        assert_eq!(entry.name, "demo");

        let read = read_registry(&registry).unwrap();
        assert!(read.skipped.is_empty());
        assert_eq!(read.drives["demo"].drive_dir, dir.join("drive"));
        server.join().unwrap();
    }

    #[test]
    fn provision_without_an_owner_registers_but_withholds_ready() {
        let dir = scratch("noowner");
        let registry = dir.join("drives.json");
        let report = provision(provision_no_service(&dir, &registry, "demo"));
        let ProvisionState::RegisteredButUnavailable { entry, reason, .. } = report else {
            panic!("a missing owner must never verify");
        };
        assert!(reason.contains("verify-socket"), "{reason}");
        // Registered all the same: the next run reconciles instead of
        // starting over.
        let read = read_registry(&registry).unwrap();
        assert!(read.drives.contains_key(&entry.name));

        // Re-run reconciles the existing drive (create refuses,
        // the flow re-verifies) instead of failing on it.
        let again = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(
            matches!(again, ProvisionState::RegisteredButUnavailable { .. }),
            "re-run must reconcile, not fail on the existing drive"
        );
    }

    #[test]
    fn provision_rejects_bad_names_before_touching_anything() {
        let dir = scratch("badname");
        let registry = dir.join("drives.json");
        let mut config = provision_no_service(&dir, &registry, "BAD NAME!");
        config.name = "BAD NAME!".to_string();
        let report = provision(config);
        assert!(
            matches!(
                report,
                ProvisionState::ProvisioningIncomplete {
                    stage: "validate",
                    ..
                }
            ),
            "validation must precede initialization"
        );
        assert!(!dir.join("drive").exists());
        assert!(!registry.exists());
    }

    pub(super) fn reconfigure_no_service(
        dir: &Path,
        registry: &Path,
        relays: Option<Vec<String>>,
        mode: ServiceMode,
        mountpoint: Option<PathBuf>,
    ) -> ReconfigureConfig {
        ReconfigureConfig {
            drive_dir: dir.join("drive"),
            relays,
            mode,
            mountpoint,
            identity: identity(),
            passphrase: "bootstrap-test-pass".to_string(),
            registry_path: registry.to_path_buf(),
            wyrd_bin: Some(dir.join("wyrd")),
            supervise_bin: Some(dir.join("wyrd-supervise")),
            identity_file: dir.join("identity"),
            passphrase_file: dir.join("passphrase"),
            // A scratch definition that never exists: hermetic tests
            // must stop and clear nothing real, and the missing file
            // is the not-managed case — never a manager touch.
            service_definition: Some(dir.join("wyrd-test-definition")),
            manage_service: false,
            verify_timeout: Duration::from_secs(10),
        }
    }

    /// Reconfigure rewrites the entry and verifies the socket: the
    /// relay set is replaced, the stub owner's pre-first-pass
    /// refusal verifies like provisioning, and the registry carries
    /// the new set.
    #[test]
    fn reconfigure_rewrites_relays_and_verifies_the_socket() {
        let dir = scratch("reconfigure");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(
            matches!(first, ProvisionState::RegisteredButUnavailable { .. }),
            "no owner serves the fresh drive: {first:?}"
        );
        let sock = dir.join("drive").join("owner.sock");
        let server = stub_owner(sock, 1, "Unobserved");
        let report = reconfigure(reconfigure_no_service(
            &dir,
            &registry,
            Some(vec!["relay://new".to_string()]),
            ServiceMode::Vault,
            None,
        ));
        let ProvisionState::VerifiedAvailable { entry, .. } = report else {
            panic!("a stubbed owner must verify: {report:?}");
        };
        assert_eq!(entry.relays, vec!["relay://new".to_string()]);
        assert!(matches!(entry.mode, ServiceMode::Vault));
        let read = read_registry(&registry).unwrap();
        assert_eq!(read.drives["demo"].relays, vec!["relay://new".to_string()]);
        server.join().unwrap();
    }

    /// `None` relays keep the registered set: a credential-path-only
    /// reconfigure never clears relays by accident.
    #[test]
    fn reconfigure_keeps_relays_when_none_given() {
        let dir = scratch("reconf-keep");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        // Hand the entry a relay set, then reconfigure without one.
        let mut read = read_registry(&registry).unwrap();
        read.drives.get_mut("demo").unwrap().relays = vec!["relay://kept".to_string()];
        write_registry(&registry, &read.drives).unwrap();
        let sock = dir.join("drive").join("owner.sock");
        let server = stub_owner(sock, 1, "Unobserved");
        let report = reconfigure(reconfigure_no_service(
            &dir,
            &registry,
            None,
            ServiceMode::Vault,
            None,
        ));
        let ProvisionState::VerifiedAvailable { entry, .. } = report else {
            panic!("a stubbed owner must verify: {report:?}");
        };
        assert_eq!(entry.relays, vec!["relay://kept".to_string()]);
        server.join().unwrap();
    }

    /// Unknown modes refuse before anything stops: the registry
    /// keeps the old entry and no service state is touched — only
    /// `vault` and `mount` reconfigure, and anything else is a
    /// validation refusal, not a guess.
    #[test]
    fn reconfigure_refuses_unknown_modes_before_stopping() {
        let dir = scratch("reconfigure-mode");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let before = std::fs::read(&registry).unwrap();
        let report = reconfigure(reconfigure_no_service(
            &dir,
            &registry,
            None,
            ServiceMode::Unknown("fuse".to_string()),
            None,
        ));
        match report {
            ProvisionState::ProvisioningIncomplete {
                kind,
                stage,
                reason,
            } => {
                assert_eq!(kind, ProvisionFailureKind::Validate);
                assert_eq!(Some(stage), kind.stage());
                assert!(
                    reason.contains("unknown service mode"),
                    "refusal names the unknown mode: {reason}"
                );
            }
            other => panic!("an unknown mode must refuse: {other:?}"),
        }
        assert_eq!(std::fs::read(&registry).unwrap(), before);
        assert_eq!(
            Some("stop-service"),
            ProvisionFailureKind::StopService.stage()
        );
    }

    /// Reconfigure moves a vault entry to mount: the mountpoint is
    /// recorded, the stub owner's pre-first-pass refusal verifies
    /// like provisioning, and the registry carries the new mode.
    #[test]
    fn reconfigure_moves_vault_to_mount_and_verifies_the_socket() {
        let dir = scratch("rec-mnt");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let mountpoint = dir.join("mnt");
        std::fs::create_dir_all(&mountpoint).unwrap();
        let sock = dir.join("drive").join("owner.sock");
        let server = stub_owner(sock, 1, "Unobserved");
        let report = reconfigure(reconfigure_no_service(
            &dir,
            &registry,
            None,
            ServiceMode::Mount,
            Some(mountpoint.clone()),
        ));
        let ProvisionState::VerifiedAvailable { entry, .. } = report else {
            panic!("a stubbed owner must verify a mount reconfigure: {report:?}");
        };
        assert!(matches!(entry.mode, ServiceMode::Mount));
        assert_eq!(entry.mountpoint, Some(mountpoint.clone()));
        let read = read_registry(&registry).unwrap();
        assert!(matches!(read.drives["demo"].mode, ServiceMode::Mount));
        assert_eq!(read.drives["demo"].mountpoint, Some(mountpoint));
        server.join().unwrap();
    }

    /// A mount reconfigure without a mountpoint refuses before
    /// anything stops: the registry keeps its bytes and no service
    /// state moves — the live check runs at the validate stage, like
    /// the missing-mountpoint shape refusal.
    #[test]
    fn reconfigure_refuses_mount_without_a_mountpoint() {
        let dir = scratch("rec-mnt-none");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let before = std::fs::read(&registry).unwrap();
        let report = reconfigure(reconfigure_no_service(
            &dir,
            &registry,
            None,
            ServiceMode::Mount,
            None,
        ));
        match report {
            ProvisionState::ProvisioningIncomplete {
                kind,
                stage,
                reason,
            } => {
                assert_eq!(kind, ProvisionFailureKind::Validate);
                assert_eq!(Some(stage), kind.stage());
                assert!(reason.contains("needs a mountpoint"), "{reason}");
            }
            other => panic!("a mount without a mountpoint must refuse: {other:?}"),
        }
        assert_eq!(std::fs::read(&registry).unwrap(), before);
    }

    /// No entry for the directory refuses without writing: the drive
    /// exists (verify-open passes) but was registered elsewhere —
    /// directories match by exact path only.
    #[test]
    fn reconfigure_refuses_an_unregistered_directory() {
        let dir = scratch("reconfigure-elsewhere");
        let registry = dir.join("drives.json");
        let other = dir.join("other.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let report = reconfigure(reconfigure_no_service(
            &dir,
            &other,
            None,
            ServiceMode::Vault,
            None,
        ));
        match report {
            ProvisionState::ProvisioningIncomplete {
                kind,
                stage,
                reason,
            } => {
                assert_eq!(kind, ProvisionFailureKind::Validate);
                assert_eq!(Some(stage), kind.stage());
                assert!(reason.contains("no registry entry"), "{reason}");
            }
            other => panic!("an unregistered directory must refuse: {other:?}"),
        }
        assert!(!other.exists(), "a refusal writes nothing");
    }

    /// Duplicate registrations for one directory refuse loudly with
    /// both key names: the command never guesses across conflicts.
    #[test]
    fn reconfigure_refuses_conflicting_registrations() {
        let dir = scratch("reconfigure-dupe");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let raw = serde_json::json!({
            "version": 1,
            "drives": {
                "demo": {
                    "name": "demo",
                    "drive_dir": dir.join("drive").to_string_lossy(),
                    "relays": [],
                    "service": "drive-demo",
                    "mode": "vault",
                },
                "alias": {
                    "name": "alias",
                    "drive_dir": dir.join("drive").to_string_lossy(),
                    "relays": [],
                    "service": "drive-alias",
                    "mode": "vault",
                },
            },
        });
        std::fs::write(&registry, serde_json::to_vec(&raw).unwrap()).unwrap();
        let report = reconfigure(reconfigure_no_service(
            &dir,
            &registry,
            None,
            ServiceMode::Vault,
            None,
        ));
        match report {
            ProvisionState::ProvisioningIncomplete { kind, reason, .. } => {
                assert_eq!(kind, ProvisionFailureKind::Validate);
                assert!(
                    reason.contains("demo") && reason.contains("alias"),
                    "conflict names both keys: {reason}"
                );
            }
            other => panic!("a duplicate registration must refuse: {other:?}"),
        }
        // Both entries survive the refusal.
        let read = read_registry(&registry).unwrap();
        assert!(read.drives.contains_key("demo") && read.drives.contains_key("alias"));
    }

    /// A wrong passphrase fails the open before anything stops:
    /// the staged definition file survives, and the registry keeps
    /// its bytes exactly. Credential failures must never take a
    /// service down.
    #[test]
    fn reconfigure_verify_open_failure_leaves_the_registry_untouched() {
        let dir = scratch("reconfigure-locked");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let before = std::fs::read(&registry).unwrap();
        let definition = dir.join("wyrd-test-definition");
        std::fs::write(&definition, "leftover").unwrap();
        let mut config = reconfigure_no_service(&dir, &registry, None, ServiceMode::Vault, None);
        config.passphrase = "wrong-pass".to_string();
        match reconfigure(config) {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, ProvisionFailureKind::VerifyOpen);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("a wrong passphrase must stop at verify-open: {other:?}"),
        }
        assert!(definition.exists(), "the stop never ran");
        assert_eq!(std::fs::read(&registry).unwrap(), before);
    }

    /// The open runs before the stop even when managed: a wrong
    /// passphrase with a staged definition refuses at verify-open,
    /// and the definition survives for the corrected retry. The
    /// explicit binary paths resolve without a manager, and the open
    /// fails before any service state moves, so the test is hermetic.
    #[test]
    fn reconfigure_proves_the_open_before_stopping() {
        let dir = scratch("rec-firstopen");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let definition = dir.join("wyrd-test-definition");
        std::fs::write(&definition, "leftover").unwrap();
        let before = std::fs::read(&registry).unwrap();
        let mut config = reconfigure_no_service(
            &dir,
            &registry,
            Some(vec!["relay://kept".to_string()]),
            ServiceMode::Vault,
            None,
        );
        config.manage_service = true;
        config.passphrase = "wrong-pass".to_string();
        match reconfigure(config) {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, ProvisionFailureKind::VerifyOpen);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("a wrong passphrase must stop at verify-open: {other:?}"),
        }
        assert!(definition.exists(), "the stop never ran");
        assert_eq!(std::fs::read(&registry).unwrap(), before);
    }

    /// A live managed lock stops the definition and retries the open
    /// once: the retry still fails here (the test holds the lock), so
    /// the terminal kind reports, and the re-run converges after the
    /// lock releases. Hermetic: the override names a missing file, so
    /// the stop is a no-op and no manager is probed.
    #[test]
    fn reconfigure_retries_the_open_past_a_live_managed_lock() {
        let dir = scratch("rec-retry");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let held =
            Engine::open_keystore(dir.join("drive"), "bootstrap-test-pass", identity()).unwrap();
        let mut config = reconfigure_no_service(
            &dir,
            &registry,
            Some(vec!["relay://kept".to_string()]),
            ServiceMode::Vault,
            None,
        );
        config.manage_service = true;
        match reconfigure(config) {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, ProvisionFailureKind::StoreLocked);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("a held managed drive must report the lock: {other:?}"),
        }
        drop(held);
    }

    /// A managed reconfigure without relays refuses at validation,
    /// before any service state moves: reaching the stop/install
    /// steps end to end would touch a real manager, so the managed
    /// path past validation stays literal-adjacent like the
    /// provision kind test — hermetic tests stop here.
    #[test]
    fn reconfigure_refuses_a_relay_less_managed_service() {
        let dir = scratch("reconfigure-norelay");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let mut config =
            reconfigure_no_service(&dir, &registry, Some(Vec::new()), ServiceMode::Vault, None);
        config.manage_service = true;
        match reconfigure(config) {
            ProvisionState::ProvisioningIncomplete {
                kind,
                stage,
                reason,
            } => {
                assert_eq!(kind, ProvisionFailureKind::Validate);
                assert_eq!(Some(stage), kind.stage());
                assert!(reason.contains("--no-service"), "{reason}");
            }
            other => panic!("a relay-less managed reconfigure must refuse: {other:?}"),
        }
    }

    /// A rewrite carries skipped entries forward verbatim: a future
    /// `fuse` entry survives a vault reconfigure with its value
    /// intact, and the caller still warns about the skip instead of
    /// silently forgetting the entry.
    #[test]
    fn reconfigure_preserves_skipped_entries_verbatim() {
        let dir = scratch("rec-presv");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        // A future-mode entry beside the managed one, staged by hand
        // like a newer writer would leave it.
        let future_raw = serde_json::json!({
            "name": "future",
            "drive_dir": dir.join("drive").to_string_lossy(),
            "relays": ["relay://future"],
            "service": "drive-future",
            "mode": "fuse",
        });
        let mut raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
        raw["drives"]["future"] = future_raw.clone();
        std::fs::write(&registry, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
        let sock = dir.join("drive").join("owner.sock");
        let server = stub_owner(sock, 1, "Unobserved");
        let report = reconfigure(reconfigure_no_service(
            &dir,
            &registry,
            Some(vec!["relay://new".to_string()]),
            ServiceMode::Vault,
            None,
        ));
        let ProvisionState::VerifiedAvailable { entry, skipped } = report else {
            panic!("a stubbed owner must verify: {report:?}");
        };
        assert_eq!(entry.relays, vec!["relay://new".to_string()]);
        assert!(
            skipped.iter().any(|reason| reason.contains("future")),
            "the skip is still surfaced: {skipped:?}"
        );
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
        assert_eq!(
            after["drives"]["future"], future_raw,
            "the unknown entry survives with its value intact"
        );
        assert_eq!(
            after["drives"]["demo"]["relays"],
            serde_json::json!(["relay://new"])
        );
        server.join().unwrap();
    }

    /// `--no-service` disables and clears: the definition file staged
    /// at the override path is gone after the run, and the entry is
    /// rewritten. The override keeps the removal hermetic, and the
    /// unique service name keeps the manager probes hermetic too — a
    /// developer's real `drive-demo` service must never see this
    /// test's `list`/disable traffic.
    #[test]
    fn reconfigure_no_service_removes_the_definition_file() {
        let dir = scratch("rec-clear");
        let registry = dir.join("drives.json");
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let name = format!("rectest-{n}");
        let first = provision(provision_no_service(&dir, &registry, &name));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        // A leftover definition from the managed past the entry
        // remembers, at the path the test override names.
        let definition = dir.join("wyrd-test-definition");
        std::fs::write(&definition, "leftover").unwrap();
        let sock = dir.join("drive").join("owner.sock");
        let server = stub_owner(sock, 1, "Unobserved");
        let report = reconfigure(reconfigure_no_service(
            &dir,
            &registry,
            None,
            ServiceMode::Vault,
            None,
        ));
        let ProvisionState::VerifiedAvailable { entry, .. } = report else {
            panic!("a stubbed owner must verify: {report:?}");
        };
        assert!(!definition.exists(), "the definition is cleared");
        assert!(entry.relays.is_empty());
        server.join().unwrap();
    }

    /// Invalid relay values refuse before anything stops: the staged
    /// definition file survives, and the registry keeps its bytes.
    #[test]
    fn reconfigure_validates_relays_before_stopping() {
        let dir = scratch("reconf-badrelay");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let definition = dir.join("wyrd-test-definition");
        std::fs::write(&definition, "leftover").unwrap();
        let before = std::fs::read(&registry).unwrap();
        let report = reconfigure(reconfigure_no_service(
            &dir,
            &registry,
            Some(vec!["not a relay".to_string()]),
            ServiceMode::Vault,
            None,
        ));
        match report {
            ProvisionState::ProvisioningIncomplete {
                kind,
                stage,
                reason,
            } => {
                assert_eq!(kind, ProvisionFailureKind::Validate);
                assert_eq!(Some(stage), kind.stage());
                assert!(reason.contains("relays"), "{reason}");
            }
            other => panic!("a bad relay must refuse: {other:?}"),
        }
        assert!(definition.exists(), "the stop never ran");
        assert_eq!(std::fs::read(&registry).unwrap(), before);
    }

    /// A live owner holding the store fails the open with the
    /// terminal kind: the registry keeps its bytes, and the caller
    /// can return the held-drive exit instead of a usage error.
    #[test]
    fn reconfigure_reports_a_held_drive() {
        let dir = scratch("reconf-held");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let before = std::fs::read(&registry).unwrap();
        // Hold the store open: a second opener fails the lock, the
        // same contention the sync lock test pins.
        let held =
            Engine::open_keystore(dir.join("drive"), "bootstrap-test-pass", identity()).unwrap();
        let report = reconfigure(reconfigure_no_service(
            &dir,
            &registry,
            None,
            ServiceMode::Vault,
            None,
        ));
        match report {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, ProvisionFailureKind::StoreLocked);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("a held drive must stop at verify-open: {other:?}"),
        }
        assert_eq!(std::fs::read(&registry).unwrap(), before);
        drop(held);
    }

    /// A socket that appears while waiting retries the connect until
    /// the deadline: a freshly restarted owner still binding does not
    /// read as unavailable.
    #[test]
    fn reconfigure_waits_for_a_late_owner_socket() {
        let dir = scratch("reconf-late");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let sock = dir.join("drive").join("owner.sock");
        let server = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            stub_owner(sock, 1, "Unobserved").join().unwrap();
        });
        let report = reconfigure(reconfigure_no_service(
            &dir,
            &registry,
            None,
            ServiceMode::Vault,
            None,
        ));
        let ProvisionState::VerifiedAvailable { .. } = report else {
            panic!("a late owner must still verify: {report:?}");
        };
        server.join().unwrap();
    }

    /// The install writes the updated entry to the override path:
    /// the managed handoff hermetically proven end to install, short
    /// of the manager itself. `updated_entry` builds the working copy
    /// `reconfigure` installs and rewrites, so asserting the written
    /// file carries the new set pins the order through the real
    /// serialize-and-write path. Enable/start stay out of hermetic
    /// tests — they would touch a real manager.
    #[test]
    fn install_writes_the_updated_relay_set() {
        let dir = scratch("rec-install");
        let owner = SupervisedOwner {
            supervise_bin: dir.join("wyrd-supervise"),
            wyrd_bin: dir.join("wyrd"),
            identity_file: dir.join("identity"),
            passphrase_file: dir.join("passphrase"),
        };
        let stored = entry("demo", &dir);
        let updated = updated_entry(
            stored,
            vec!["relay://new".to_string()],
            ServiceMode::Vault,
            None,
        );
        assert_eq!(updated.name, "demo");
        assert_eq!(updated.service, "drive-demo");
        assert_eq!(updated.mode, ServiceMode::Vault);
        let definition = dir.join("wyrd-test-definition");
        let written = install_service_definition(&updated, &owner, Some(&definition)).unwrap();
        assert_eq!(written, definition);
        let content = std::fs::read_to_string(&definition).unwrap();
        assert!(
            content.contains("relay://new"),
            "the definition carries the new set"
        );
        assert!(
            !content.contains("relay://example"),
            "no stale relay survives"
        );
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::tests::{entry, provision_no_service, scratch, stub_owner};
    use super::*;

    #[test]
    fn registry_skips_unknown_modes_without_failing_the_file() {
        let dir = scratch("mode");
        let path = dir.join("drives.json");
        let raw = serde_json::json!({
            "version": 1,
            "drives": {
                "future": {
                    "name": "future",
                    "drive_dir": dir.join("drive").to_string_lossy(),
                    "relays": [],
                    "service": "drive-future",
                    "mode": "mount"
                }
            }
        });
        std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
        let read = read_registry(&path).unwrap();
        assert!(read.drives.is_empty());
        assert_eq!(read.skipped.len(), 1, "{:?}", read.skipped);
        assert!(read.skipped[0].contains("mount"), "{:?}", read.skipped);
    }

    #[test]
    fn registry_file_is_owner_only() {
        let dir = scratch("perms");
        let path = dir.join("drives.json");
        let mut drives = BTreeMap::new();
        drives.insert("demo".to_string(), entry("demo", &dir));
        write_registry(&path, &drives).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "registry is owner-only, got {mode:o}");
        }
    }

    #[test]
    fn service_files_write_atomically_and_owner_only() {
        let dir = scratch("svcfile");
        let path = dir.join("wyrd-drive-demo.service");
        let owner = SupervisedOwner {
            supervise_bin: PathBuf::from("/opt/wyrd/bin/wyrd-supervise"),
            wyrd_bin: PathBuf::from("/opt/wyrd/bin/wyrd"),
            identity_file: dir.join("identity"),
            passphrase_file: dir.join("passphrase"),
        };
        let owned = entry("demo", &dir);
        write_service_file(
            &path,
            &render_systemd_unit("Wyrd drive demo", &owner, &owned),
        )
        .unwrap();
        assert!(path.exists());
        assert!(
            !dir.join(format!("wyrd-drive-demo.tmp.{}", std::process::id()))
                .exists(),
            "no temporary sibling remains"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "service definition is owner-only, got {mode:o}"
            );
        }
    }

    #[test]
    fn templates_escape_operator_input() {
        let dir = scratch("escape");
        let mut owned = entry("demo", &dir);
        owned.relays = vec!["relay://exa mple".to_string(), "relay://%i".to_string()];
        let owner = SupervisedOwner {
            supervise_bin: PathBuf::from("/opt/wyrd/bin/with space/wyrd-supervise"),
            wyrd_bin: PathBuf::from("/opt/wyrd/bin/wyrd"),
            identity_file: dir.join("identity"),
            passphrase_file: dir.join("passphrase"),
        };
        let plist = render_launchd_plist("md.enesis.wyrd.drive-demo", &owner, &owned);
        assert!(plist.contains("with space"), "paths survive escaped");
        let unit = render_systemd_unit("Wyrd drive demo", &owner, &owned);
        assert!(
            unit.contains("\"/opt/wyrd/bin/with space/wyrd-supervise\""),
            "spaces are quoted, got:\n{unit}"
        );
        assert!(
            unit.contains("\"relay://exa mple\""),
            "relay spaces are quoted, got:\n{unit}"
        );
        assert!(
            unit.contains("\"relay://%%i\""),
            "relay specifiers are doubled, got:\n{unit}"
        );
    }

    #[test]
    fn only_ok_and_unobserved_verify() {
        let dir = scratch("whitelist");
        let registry = dir.join("drives.json");
        let sock = dir.join("drive").join("owner.sock");
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let server = stub_owner(sock, 1, "Malformed");
        let report = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(
            matches!(report, ProvisionState::RegisteredButUnavailable { .. }),
            "an unexpected refusal shape must not verify"
        );
        server.join().unwrap();
    }

    #[test]
    fn managing_a_service_needs_a_relay() {
        let dir = scratch("norelay");
        let registry = dir.join("drives.json");
        let mut config = provision_no_service(&dir, &registry, "demo");
        config.manage_service = true;
        config.relays = vec![];
        let report = provision(config);
        match report {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, ProvisionFailureKind::Validate);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("a relay-less managed service is refused, not installed: {other:?}"),
        }
        assert!(
            !registry.exists(),
            "refusal precedes registration side effects"
        );
    }
}
