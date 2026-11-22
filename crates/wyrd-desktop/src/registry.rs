//! Read view plus contracted writer for the per-user drive registry
//! (`drives.json`).
//!
//! Observer only for discovery: this module parses the schema-version-1
//! file the installer and the `wyrd bootstrap` flow write, validates
//! entries with the same rules as the daemon's registry reader, and
//! probes each drive's owner socket. It never opens a drive.
//! Creation, joining, and membership changes belong to the
//! provisioning and membership flows, not to this shell (OD:
//! desktop lifecycle authority, stage 2).
//!
//! Writer under contract for registry facts: the desktop shell is the
//! registration flow's co-writer under the single-writer rule (one
//! logical writer role, two processes allowed). Every desktop write
//! holds the registry lockfile exclusive for the whole
//! read-modify-write, validates candidates with exactly the reader's
//! rules, carries forward entries it does not understand verbatim,
//! follows the crash-safe tmp-write/sync/atomic-rename/dir-sync
//! order, and verifies by rereading. The daemon honors the same
//! lockfile, so concurrent registration-flow and desktop rewrites
//! serialize instead of losing entries (atomic rename alone does not
//! stop lost updates).
//!
//! Every answer is a fresh observation, never cached authority:
//!
//! - a missing registry file is an empty drive list (a fresh
//!   installation registered nothing yet), not an error;
//! - a newer schema version or a malformed file is an explicit
//!   `Unreadable` state with the reason — loud, never guessed, never
//!   silently defaulted;
//! - invalid entries are skipped with their reasons; valid ones are
//!   unaffected, exactly like the daemon's reader;
//! - per-drive reachability is verified this session (connect plus a
//!   terminal `liveness` answer): `Available` means the owner
//!   answered, `Unavailable` means no transport, `Unknown` means the
//!   transport exists but no terminal answer arrived (or the peer
//!   speaks another wire version). A refusal to the probe `liveness`
//!   still proves the owner answers, so it reports `Available` —
//!   availability is transport-level, never a judgment on the drive.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use wyrd_owner_client::{ClientError, OwnerClient, WireLiveness};

/// Registry schema version this shell reads. Newer files are
/// reported unreadable with their version, never guessed.
const REGISTRY_VERSION: u32 = 1;

/// Registry filename, fixed so installer, command, and desktop read
/// the same path derivation.
const REGISTRY_FILENAME: &str = "drives.json";

/// Bound on one drive's reachability probe. A status read answers in
/// milliseconds; the bound only fires on a wedged owner, and expiry
/// reports `Unknown` — never `Unavailable`, never success. It also
/// bounds the whole list: one hung drive must not wedge the view.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// One registry entry as parsed: discovery facts, never secrets.
/// Private: the public [`DriveView`] carries the validated subset
/// plus the live reachability verdict.
#[derive(Debug, Clone, serde::Deserialize)]
struct RegistryEntryParse {
    name: String,
    drive_dir: PathBuf,
    #[serde(default)]
    relays: Vec<String>,
    service: String,
    mode: String,
    /// FUSE mountpoint: `Some` in `mount` mode, absent otherwise.
    /// Defaults so pre-mount files decode.
    #[serde(default)]
    mountpoint: Option<PathBuf>,
}

#[derive(Debug, serde::Deserialize)]
struct RegistryFileParse {
    version: u32,
    drives: BTreeMap<String, RegistryEntryParse>,
}

/// How the registry file itself read: ready with entries, absent
/// (fresh installation), or present but unreadable with the reason.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum RegistryStatus {
    /// File read with its schema version; entries follow below.
    Ready,
    /// No file exists: a fresh installation registered nothing yet.
    Nofile,
    /// File present but unreadable with the reason: never guessed, never defaulted.
    Unreadable {
        /// Why the file is unreadable: version skew or parse reason.
        reason: String,
    },
}

/// One drive's session gate for the per-session posture: whether
/// this drive is open in this session or needs an explicit unlock.
/// `None` where a service definition exists — managed recovery
/// (`ServiceState`) owns that drive, and no passphrase gate
/// applies. `Some` only for definition-less drives: the
/// per-session posture (or a removed definition), where the only
/// ways back to serving are an explicit `drive_unlock` with the
/// passphrase or a reinstalled managed definition.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum SessionState {
    /// The owner answered this session: the drive is open.
    Unlocked {
        /// Evidence string: the owner answered this session.
        detail: String,
    },
    /// No owner serves and no service definition exists to start:
    /// the drive needs an explicit unlock with its passphrase (or
    /// a reinstalled managed definition). A stable first-class
    /// state, never a failure — the shell never auto-starts.
    Locked {
        /// Evidence string naming both recoveries: session unlock
        /// and managed reinstall.
        detail: String,
    },
}

/// One drive's reachability, verified this session. Transport-level
/// only: `Available` never claims anything about the drive's
/// contents, and `Unavailable` never claims the drive is gone.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum DriveAvailability {
    /// Owner answered this session, terminally or by refusal: transport exists.
    Available,
    /// No transport answered: the owner is down, never evidence about the drive.
    Unavailable {
        /// Why no transport answered, never a claim about the drive.
        detail: String,
    },
    /// Transport exists but no terminal answer arrived: availability unknown.
    Unknown {
        /// Why no terminal answer arrived; retry the probe.
        detail: String,
    },
}

/// One registered drive for the lifecycle view: validated facts
/// plus the live verdict. `mode` is a display string end to end —
/// the shell shows the registry's service mode and never interprets
/// it. `mountpoint` carries the FUSE mountpoint in `mount` mode
/// (`None` in `vault` mode). Drives match by exact path only.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DriveView {
    /// Registry entry name, 1–64 chars of `[a-z0-9-]`.
    pub name: String,
    /// Absolute drive directory as an exact string; drives match by exact path only.
    pub drive_dir: String,
    /// Relay URLs from the registry entry, shown verbatim.
    pub relays: Vec<String>,
    /// Service name from the registry entry, shown verbatim.
    pub service: String,
    /// Service mode display string, shown verbatim and never interpreted.
    pub mode: String,
    /// FUSE mountpoint in `mount` mode; `None` in `vault` mode.
    pub mountpoint: Option<String>,
    /// Per-drive reachability, verified this session.
    pub availability: DriveAvailability,
    /// The service-manager fact beside the transport verdict:
    /// not-installed vs stopped vs wedged vs running vs unknown vs
    /// unsupported. Observation only — never supervision.
    pub service_state: super::service::ServiceState,
    /// The login-start preference as the manager records it:
    /// enabled vs disabled vs unknown. Independent of
    /// `service_state` — a manually started service may run while
    /// disabled at login. Probed wherever a definition exists on
    /// explicit refresh (one cheap fork per managed drive, read by
    /// the toggle); `Unknown` where there is no definition to
    /// enable (managed-only toggle) or the manager did not answer.
    /// Observation only — never supervision.
    pub enablement: super::service::EnablementState,
    /// The per-session gate beside the transport verdict: locked
    /// vs unlocked for definition-less drives, `None` where a
    /// service definition exists (managed recovery owns that
    /// drive). Observation only — never supervision.
    pub session: Option<SessionState>,
}

/// The per-session gate for one drive: locked vs unlocked for
/// definition-less drives, `None` everywhere else. Pure over the
/// transport verdict and the definition fact, so the rule pins
/// without staging an owner. Managed drives (definition exists)
/// carry no gate — their service verdict owns recovery. A probe
/// timeout (`Unknown`) carries no gate either: an unresolved probe
/// changes nothing, so the tag stays absent until the probe
/// resolves instead of declaring the drive locked.
pub(crate) fn session_for(
    availability: &DriveAvailability,
    definition_exists: bool,
) -> Option<SessionState> {
    if definition_exists {
        return None;
    }
    match availability {
        DriveAvailability::Available => Some(SessionState::Unlocked {
            detail: "the owner answered this session — the drive is open".to_string(),
        }),
        DriveAvailability::Unavailable { .. } => Some(SessionState::Locked {
            detail: "no owner serves and no service definition exists: unlock the drive for this session with its passphrase, or reinstall the managed definition".to_string(),
        }),
        DriveAvailability::Unknown { .. } => None,
    }
}

/// Whether the registry names a managed service definition for one
/// drive directory: the unlock command's posture gate. Reads the
/// default registry file without probing any socket — a missing or
/// unreadable file answers `false`, so unlocking an unregistered
/// drive (or one whose registry is momentarily unreadable) still
/// proceeds to the CLI instead of refusing on a read failure.
pub(crate) fn managed_definition_exists(drive_dir: &Path) -> bool {
    managed_definition_exists_at(&default_registry_path(), drive_dir)
}

/// The test seam for [`managed_definition_exists`]: the registry
/// path is injected, so the gate pins without touching the
/// operator's real registry file.
fn managed_definition_exists_at(registry_path: &Path, drive_dir: &Path) -> bool {
    let bytes = match std::fs::read(registry_path) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    let file: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let want = drive_dir.to_string_lossy();
    let Some(drives) = file.get("drives").and_then(|drives| drives.as_object()) else {
        return false;
    };
    drives.values().any(|entry| {
        entry.get("drive_dir").and_then(|dir| dir.as_str()) == Some(want.as_ref())
            && entry
                .get("service")
                .and_then(|service| service.as_str())
                .is_some_and(|service| super::service::service_definition_path(service).exists())
    })
}

/// The registered relay set for one drive directory: the unlock
/// command's relay source. Reads the default registry file without
/// probing any socket — a missing or unreadable file, or a drive
/// with no entry, answers empty, so the CLI falls back to the
/// drive config file exactly as before. Non-string relay values
/// are skipped, never guessed: the writer validates relays, and an
/// entry the reader cannot parse must not invent network targets.
pub(crate) fn registry_relays_for(drive_dir: &Path) -> Vec<String> {
    registry_relays_for_at(&default_registry_path(), drive_dir)
}

/// The test seam for [`registry_relays_for`]: the registry path is
/// injected, so the lookup pins without touching the operator's
/// real registry file.
fn registry_relays_for_at(registry_path: &Path, drive_dir: &Path) -> Vec<String> {
    let bytes = match std::fs::read(registry_path) {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };
    let file: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let want = drive_dir.to_string_lossy();
    let Some(drives) = file.get("drives").and_then(|drives| drives.as_object()) else {
        return Vec::new();
    };
    drives
        .values()
        .find(|entry| entry.get("drive_dir").and_then(|dir| dir.as_str()) == Some(want.as_ref()))
        .and_then(|entry| entry.get("relays"))
        .and_then(|relays| relays.as_array())
        .map(|relays| {
            relays
                .iter()
                .filter_map(|relay| relay.as_str())
                // Mirror the writer's relay rule (`validate_fields`
                // below): empty, whitespace, or control-byte values
                // are unusable. Skipping them keeps a malformed entry
                // from pointing the owner at a dead relay while
                // suppressing the config-file fallback — an
                // all-unusable set answers empty, exactly like no
                // relays key.
                .filter(|relay| {
                    !relay.is_empty()
                        && !relay
                            .bytes()
                            .any(|byte| byte < 0x20 || byte == 0x7f || byte.is_ascii_whitespace())
                })
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The lifecycle view's registry answer: the file status, one view
/// per valid entry, and one reason per skipped entry. The status
/// flattens into the payload (`{"status": "ready", "drives": ...}`)
/// so the file verdict reads without stutter.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RegistryState {
    /// How the registry file itself read: ready, absent, or unreadable with the reason.
    #[serde(flatten)]
    pub status: RegistryStatus,
    /// One view per valid entry, in registry order.
    pub drives: Vec<DriveView>,
    /// One reason per skipped entry; valid entries are unaffected.
    pub skipped: Vec<String>,
}

/// Where the registry lives when no explicit path is given: the
/// same per-user derivation the installer and the daemon use, so
/// all three read the same file.
fn default_registry_path() -> PathBuf {
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

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The daemon's entry rules, mirrored: the shell must agree with
/// the writer about which entries are valid, or the lifecycle view
/// would show drives the owner tooling refuses (and vice versa).
/// Shared with the writer below, so a candidate the writer accepts
/// is an entry the reader lists.
fn validate_entry(key: &str, entry: &RegistryEntryParse) -> Result<(), String> {
    validate_fields(
        key,
        &entry.name,
        &entry.drive_dir,
        &entry.service,
        &entry.mode,
        entry.mountpoint.as_deref(),
        &entry.relays,
    )
}

/// The field rules both the reader and the writer enforce: one
/// function, so the desktop can never write an entry its own reader
/// skips (or the daemon's reader skips — the rules mirror
/// `wyrd_daemon::bootstrap` entry for entry).
fn validate_fields(
    key: &str,
    name: &str,
    drive_dir: &Path,
    service: &str,
    mode: &str,
    mountpoint: Option<&Path>,
    relays: &[String],
) -> Result<(), String> {
    if key != name {
        return Err(format!("key {key:?} does not match entry name {name:?}",));
    }
    if !valid_token(name) {
        return Err(format!("name {name:?} must be 1-64 chars of [a-z0-9-]"));
    }
    if name.len() + "drive-".len() > 64 {
        return Err(format!(
            "name {name:?} leaves no room for the drive- service prefix (max 58 chars)"
        ));
    }
    if !valid_token(service) {
        return Err(format!(
            "service {service:?} must be 1-64 chars of [a-z0-9-]"
        ));
    }
    // One rule for every drive path in the crate: the shared
    // command-boundary validator, so the registry reader can never
    // list a spelling the connection map refuses (or vice versa).
    // The lossy render feeds the validator exactly like the old
    // control-byte check did; non-UTF8 spellings were never
    // loadable paths here.
    super::validate_drive_path(&drive_dir.to_string_lossy(), "drive_dir").map(|_| ())?;
    match mode {
        "vault" => {
            if mountpoint.is_some() {
                return Err("vault mode carries no mountpoint".to_string());
            }
        }
        "mount" => {
            let mountpoint = mountpoint.ok_or_else(|| {
                "mount mode needs a mountpoint: pass --mountpoint with an absolute path".to_string()
            })?;
            super::validate_drive_path(&mountpoint.to_string_lossy(), "mountpoint")
                .map_err(|reason| format!("invalid mountpoint: {reason}"))?;
            if mountpoint == drive_dir {
                return Err(format!(
                    "invalid mountpoint: mountpoint {} must not equal the drive directory",
                    mountpoint.display()
                ));
            }
        }
        _ => {
            return Err("unknown service mode: this build manages `vault` and `mount`".to_string());
        }
    }
    if relays.iter().any(|relay| {
        relay.is_empty()
            || relay
                .bytes()
                .any(|byte| byte < 0x20 || byte == 0x7f || byte.is_ascii_whitespace())
    }) {
        return Err(
            "relays must be non-empty URLs without whitespace or control bytes".to_string(),
        );
    }
    Ok(())
}

/// Map one probe exchange onto the reachability verdict. Pure, so
/// every taxonomy arm pins without staging an owner.
fn availability_of(result: Result<WireLiveness, ClientError>) -> DriveAvailability {
    match result {
        Ok(_) => DriveAvailability::Available,
        // A terminal refusal still proves the owner answers: the
        // socket serves, whatever it thought of the probe.
        Err(ClientError::Refused(_)) => DriveAvailability::Available,
        Err(ClientError::Unavailable(detail)) => DriveAvailability::Unavailable { detail },
        Err(ClientError::Unconfirmed { detail, .. }) => DriveAvailability::Unknown { detail },
        Err(ClientError::Incompatible(detail)) => DriveAvailability::Unknown { detail },
    }
}

/// Probe one registered drive: connect plus a terminal `liveness`
/// answer under the probe bound. Connecting alone proves nothing —
/// the socket may accept while the owner never answers — so only a
/// terminal answer reports `Available`.
fn probe_drive(drive_dir: &Path) -> DriveAvailability {
    let client = match OwnerClient::connect_drive(drive_dir) {
        Ok(client) => client,
        Err(error) => {
            return match error {
                ClientError::Unavailable(detail) => DriveAvailability::Unavailable { detail },
                other => availability_of(Err(other)),
            };
        }
    };
    let mut client = match client.with_timeout(PROBE_TIMEOUT) {
        Ok(bounded) => bounded,
        Err(error) => return availability_of(Err(error)),
    };
    availability_of(client.liveness())
}

/// Read the registry at an explicit path and probe every valid
/// entry. The explicit path exists for tests; production passes the
/// default derivation.
pub fn list_drives_at(registry_path: &Path) -> RegistryState {
    let bytes = match std::fs::read(registry_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RegistryState {
                status: RegistryStatus::Nofile,
                drives: Vec::new(),
                skipped: Vec::new(),
            };
        }
        Err(error) => {
            return RegistryState {
                status: RegistryStatus::Unreadable {
                    reason: format!("read {}: {error}", registry_path.display()),
                },
                drives: Vec::new(),
                skipped: Vec::new(),
            };
        }
    };
    let file: RegistryFileParse = match serde_json::from_slice(&bytes) {
        Ok(file) => file,
        Err(error) => {
            return RegistryState {
                status: RegistryStatus::Unreadable {
                    reason: format!("parse {}: {error}", registry_path.display()),
                },
                drives: Vec::new(),
                skipped: Vec::new(),
            };
        }
    };
    if file.version != REGISTRY_VERSION {
        return RegistryState {
            status: RegistryStatus::Unreadable {
                reason: format!(
                    "registry {} is version {}, this shell reads {}",
                    registry_path.display(),
                    file.version,
                    REGISTRY_VERSION
                ),
            },
            drives: Vec::new(),
            skipped: Vec::new(),
        };
    }
    let mut drives = Vec::new();
    let mut skipped = Vec::new();
    // One connection per drive needs one entry per directory: two
    // names for one drive_dir would key two map entries (and two
    // list rows under one key) for the same owner socket. The first
    // name in registry order wins; later duplicates skip loudly
    // with their reason, exactly like invalid entries.
    let mut seen_dirs = std::collections::HashSet::new();
    for (key, entry) in file.drives {
        match validate_entry(&key, &entry) {
            Ok(()) => {
                if !seen_dirs.insert(entry.drive_dir.clone()) {
                    skipped.push(format!(
                        "entry {key:?}: duplicate drive_dir {} (first name wins)",
                        entry.drive_dir.display()
                    ));
                    continue;
                }
                drives.push({
                    let availability = probe_drive(&entry.drive_dir);
                    // The definition fact is read once and shared by
                    // the service verdict and the session gate below:
                    // both describe the same file, and two stats could
                    // disagree under a concurrent reinstall.
                    let definition_exists =
                        super::service::service_definition_path(&entry.service).exists();
                    // The service-manager probe runs only where a view
                    // consumes it: the unavailable screen is the sole
                    // reader, and it renders only for `Unavailable`. A
                    // live socket already proves the owner runs, so an
                    // `Available` drive reports that fact without
                    // spawning the manager — one fewer fork per drive
                    // per refresh, and no 5 s ceiling where nothing
                    // reads the answer. `Unknown` availability gets only
                    // the definition-file fact, never a manager spawn:
                    // the screen does not render it either.
                    // The enablement probe runs wherever a definition
                    // exists: the toggle reads it on every managed
                    // drive, available or not — one cheap fork per
                    // managed drive per explicit refresh, beside the
                    // lifecycle probe above. No definition means no
                    // unit to enable (managed-only toggle): Unknown
                    // with the managed-only reason, never a fork.
                    // Unknown availability probes nothing either.
                    let enablement = if !definition_exists {
                        super::service::EnablementState::Unknown {
                            detail: "no service definition installed: enablement applies to managed drives only".to_string(),
                        }
                    } else if matches!(
                        availability,
                        DriveAvailability::Unknown { .. }
                    ) {
                        super::service::EnablementState::Unknown {
                            detail: "enablement not probed while availability is unknown — retry the owner probe".to_string(),
                        }
                    } else {
                        super::service::probe_enablement(&entry.service)
                    };
                    let service_state = match &availability {
                        DriveAvailability::Available => super::service::ServiceState::Running {
                            detail:
                                "the owner answered this session — the manager was not re-probed"
                                    .to_string(),
                        },
                        DriveAvailability::Unavailable { .. } => {
                            // No transport plus a loaded unit reads as
                            // wedged.
                            super::service::probe_service_state(&entry.service, false)
                        }
                        DriveAvailability::Unknown { .. } => {
                            if definition_exists {
                                super::service::ServiceState::Unknown {
                                    detail: "service not probed while availability is unknown — retry the owner probe".to_string(),
                                }
                            } else {
                                super::service::ServiceState::NotInstalled {
                                    detail: "no service definition installed: the drive is registered without a managed service — reinstall the definition to supervise it".to_string(),
                                }
                            }
                        }
                    };
                    // The session gate beside the transport verdict,
                    // derived from the same definition fact: an
                    // unresolved probe carries no gate (it changes
                    // nothing), managed drives carry none (their
                    // service verdict owns recovery) — see
                    // `session_for`.
                    let session = session_for(&availability, definition_exists);
                    DriveView {
                        name: entry.name.clone(),
                        drive_dir: entry.drive_dir.to_string_lossy().into_owned(),
                        relays: entry.relays.clone(),
                        service: entry.service.clone(),
                        mode: entry.mode.clone(),
                        mountpoint: entry
                            .mountpoint
                            .as_ref()
                            .map(|path| path.to_string_lossy().into_owned()),
                        availability,
                        service_state,
                        enablement,
                        session,
                    }
                })
            }
            Err(reason) => skipped.push(format!("entry {key:?}: {reason}")),
        }
    }
    RegistryState {
        status: RegistryStatus::Ready,
        drives,
        skipped,
    }
}

/// Read the default registry and probe every valid entry.
pub fn list_drives() -> RegistryState {
    list_drives_at(&default_registry_path())
}

/// One registry entry as written: discovery facts, never secrets.
/// The shape matches the daemon's writer field for field, so a
/// desktop-written file reads identically under both readers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RegistryEntryWrite {
    /// Registry entry name, 1–64 chars of `[a-z0-9-]`.
    pub name: String,
    /// Absolute drive directory, exact string (shared drive-identity rule).
    pub drive_dir: PathBuf,
    /// Relay URLs; must be non-empty URLs without whitespace or control bytes.
    pub relays: Vec<String>,
    /// Service name token, 1–64 chars of `[a-z0-9-]`.
    pub service: String,
    /// Service mode display string: `vault` or `mount`.
    pub mode: String,
    /// FUSE mountpoint: `Some` in `mount` mode, `None` in `vault`
    /// mode. Serialized always (null for vault) so the daemon's
    /// optional field decodes either way.
    pub mountpoint: Option<PathBuf>,
}

/// Every way a desktop registry write can fail. Variants carry the
/// failing identity — paths, names — so future commands can branch
/// on failure instead of parsing text.
#[derive(Debug, thiserror::Error)]
pub enum RegistryWriteError {
    /// The registry lockfile could not be opened or locked.
    #[error("lock {}: {source}", path.display())]
    Lock {
        /// Lockfile path that could not be opened or locked.
        path: PathBuf,
        /// Underlying OS error from opening or locking.
        #[source]
        source: std::io::Error,
    },
    /// A candidate entry breaks the registry rules: nothing was written.
    #[error("refusing to write: {reason}")]
    RefusedWrite {
        /// Which candidate broke which rule; nothing was written.
        reason: String,
    },
    /// The registry schema version is newer than this build writes:
    /// writing would silently replace a file this build cannot read.
    #[error("registry {} is version {found}, this shell writes {REGISTRY_VERSION}", path.display())]
    UnsupportedVersion {
        /// Registry path whose version is newer than this build writes.
        path: PathBuf,
        /// Schema version found in the file.
        found: u64,
    },
    /// The registry file could not be read.
    #[error("read {}: {source}", path.display())]
    Read {
        /// Registry path that could not be read.
        path: PathBuf,
        /// Underlying OS read error.
        #[source]
        source: std::io::Error,
    },
    /// The registry file is not valid JSON.
    #[error("parse {}: {source}", path.display())]
    Parse {
        /// Registry path that failed to parse.
        path: PathBuf,
        /// Parse failure as an I/O error for the error chain.
        #[source]
        source: std::io::Error,
    },
    /// The registry file could not be encoded.
    #[error("encode registry: {source}")]
    Encode {
        /// Underlying JSON encode error.
        #[source]
        source: serde_json::Error,
    },
    /// A directory for the registry could not be created.
    #[error("create {}: {source}", path.display())]
    CreateDir {
        /// Directory path that could not be created.
        path: PathBuf,
        /// Underlying OS error from directory creation.
        #[source]
        source: std::io::Error,
    },
    /// The temporary registry file could not be written.
    #[error("write {}: {source}", path.display())]
    WriteFile {
        /// Temporary file path that could not be written.
        path: PathBuf,
        /// Underlying OS write error.
        #[source]
        source: std::io::Error,
    },
    /// Owner-only permissions could not be applied.
    #[error("chmod {}: {source}", path.display())]
    Chmod {
        /// Path whose permissions could not be applied.
        path: PathBuf,
        /// Underlying OS permission error.
        #[source]
        source: std::io::Error,
    },
    /// The atomic replace of the registry file failed.
    #[error("replace {}: {source}", path.display())]
    ReplaceFile {
        /// Registry path whose atomic replace failed.
        path: PathBuf,
        /// Underlying OS rename error.
        #[source]
        source: std::io::Error,
    },
    /// A written file could not be synced to disk.
    #[error("sync {}: {source}", path.display())]
    SyncFile {
        /// File path that could not be synced to disk.
        path: PathBuf,
        /// Underlying OS sync error.
        #[source]
        source: std::io::Error,
    },
    /// The reread after the replace does not show the written
    /// entries: something outside the lock rewrote the file between
    /// the rename and the verification. Loud, never silent.
    #[error("verify {}: written entries do not match reread ({detail})", path.display())]
    VerifyMismatch {
        /// Registry path whose reread did not show the written entries.
        path: PathBuf,
        /// Which entry is missing or changed on reread.
        detail: String,
    },
}

/// Decoded per entry by the writer: structurally corrupt values
/// fail the whole write (as they fail the read), while values that
/// decode but fail validation are carried forward verbatim.
#[derive(Debug, serde::Deserialize)]
struct RegistryFileWrite {
    version: u32,
    drives: BTreeMapForWrite,
}

type BTreeMapForWrite = std::collections::BTreeMap<String, serde_json::Value>;

/// The inter-writer serialization, derived exactly like the daemon's:
/// the registration flow and the desktop shell hold an exclusive
/// advisory lock on this sibling for the whole read-modify-write.
/// The lockfile is never renamed, so the lock always names a stable
/// inode; the kernel releases it on crash, so no stale lock can
/// wedge either writer.
pub fn registry_lock_path(path: &Path) -> PathBuf {
    path.with_extension("lock")
}

/// Hold the registry's exclusive lock, creating the lockfile
/// owner-only on first use. The returned handle keeps the lock;
/// dropping it releases.
fn lock_registry(path: &Path) -> Result<std::fs::File, RegistryWriteError> {
    let lock_path = registry_lock_path(path);
    if let Some(parent) = lock_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| RegistryWriteError::CreateDir {
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
        .map_err(|source| RegistryWriteError::Lock {
            path: lock_path.clone(),
            source,
        })?;
    restrict_owner_only(&lock_path)?;
    file.lock().map_err(|source| RegistryWriteError::Lock {
        path: lock_path,
        source,
    })?;
    Ok(file)
}

/// Owner-only file permissions (Unix): the registry holds drive
/// facts, not secrets, but it is still per-user state.
#[cfg(unix)]
fn restrict_owner_only(path: &Path) -> Result<(), RegistryWriteError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
        RegistryWriteError::Chmod {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn restrict_owner_only(_path: &Path) -> Result<(), RegistryWriteError> {
    Ok(())
}

/// Write registry entries crash-safely under the inter-writer lock:
/// validate every candidate (one invalid entry refuses the whole
/// write, mirroring the daemon), carry forward entries the reader
/// skipped verbatim (unknown modes, hand edits that fail validation —
/// a rewrite must never delete what this build does not
/// understand), write a temporary sibling in the same directory,
/// sync it, atomic replace, sync the directory, then verify by
/// rereading. An interrupted write can never leave a truncated
/// registry, and the reread proves this session's write landed.
pub fn write_registry_at(
    path: &Path,
    drives: &std::collections::BTreeMap<String, RegistryEntryWrite>,
) -> Result<(), RegistryWriteError> {
    for (key, entry) in drives {
        validate_fields(
            key,
            &entry.name,
            &entry.drive_dir,
            &entry.service,
            &entry.mode,
            entry.mountpoint.as_deref(),
            &entry.relays,
        )
        .map_err(|reason| RegistryWriteError::RefusedWrite { reason })?;
    }
    // Serialize against the registration flow's writes (and
    // concurrent desktop writes): the lock covers the read below,
    // the merge, the rename, and the reread, so two writers never
    // interleave on one base. Held to end of scope.
    //
    // This primitive blocks on the lock. Like every other blocking
    // call in this crate, future commands must run it on the
    // `spawn_blocking` pool, never on a Tauri async executor thread
    // (see the command-surface rule in `commands/mod.rs`).
    let _lock = lock_registry(path)?;
    let mut merged: std::collections::BTreeMap<String, serde_json::Value> = drives
        .iter()
        .map(|(key, entry)| {
            serde_json::to_value(entry)
                .map(|value| (key.clone(), value))
                .map_err(|source| RegistryWriteError::Encode { source })
        })
        .collect::<Result<_, _>>()?;
    // No silent best-effort: a present-but-unreadable registry
    // (malformed JSON, newer schema) refuses the write instead of
    // replacing the file with only the candidates. The reader
    // reports such files loudly (`Unreadable`, never guessed); the
    // writer must match, or a downgraded or corrupted file would
    // lose every existing entry without a sound. Only a missing
    // file contributes nothing.
    match std::fs::read(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(RegistryWriteError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
        Ok(existing) => {
            let file: RegistryFileWrite =
                serde_json::from_slice(&existing).map_err(|source| RegistryWriteError::Parse {
                    path: path.to_path_buf(),
                    source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
                })?;
            if file.version != REGISTRY_VERSION {
                return Err(RegistryWriteError::UnsupportedVersion {
                    path: path.to_path_buf(),
                    found: file.version as u64,
                });
            }
            for (key, value) in file.drives {
                // Structural gate: the entry must decode like the
                // reader requires. A structurally corrupt entry
                // refuses the write as Parse — carrying it and
                // reporting success would contradict the reader's
                // loud verdict on the same file. Entries that decode
                // but fail validation (unknown modes) are still
                // carried verbatim below.
                serde_json::from_value::<RegistryEntryParse>(value.clone()).map_err(|source| {
                    RegistryWriteError::Parse {
                        path: path.to_path_buf(),
                        source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
                    }
                })?;
                merged.entry(key).or_insert(value);
            }
        }
    }
    // Merged-set uniqueness, scoped exactly like the reader's
    // dedup: only entries that pass validation occupy a drive_dir
    // (`list_drives_at` skips the rest before deduping). Candidates
    // all passed validation above; carried entries participate only
    // when they validate too — a skipped legacy entry never gates a
    // new registration. The writer refuses a set it would only
    // half-list, and the refusal names the repair.
    {
        let mut seen: std::collections::BTreeMap<PathBuf, String> =
            std::collections::BTreeMap::new();
        let mut occupy = |key: &str, dir: PathBuf| -> Result<(), RegistryWriteError> {
            if let Some(first) = seen.insert(dir.clone(), key.to_string()) {
                return Err(RegistryWriteError::RefusedWrite {
                    reason: format!(
                        "duplicate drive_dir {} (keys {first:?} and {key:?}; first wins in the reader)",
                        dir.display()
                    ),
                });
            }
            Ok(())
        };
        for (key, entry) in drives {
            occupy(key, entry.drive_dir.clone())?;
        }
        for (key, value) in &merged {
            if drives.contains_key(key) {
                continue;
            }
            // Structurally sound by the gate above; validation decides
            // visibility, exactly as in the reader.
            if let Ok(parsed) = serde_json::from_value::<RegistryEntryParse>(value.clone()) {
                if validate_fields(
                    key,
                    &parsed.name,
                    &parsed.drive_dir,
                    &parsed.service,
                    &parsed.mode,
                    parsed.mountpoint.as_deref(),
                    &parsed.relays,
                )
                .is_ok()
                {
                    occupy(key, parsed.drive_dir)?;
                }
            }
        }
    }
    let file = serde_json::json!({
        "version": REGISTRY_VERSION,
        "drives": merged,
    });
    let bytes =
        serde_json::to_vec_pretty(&file).map_err(|source| RegistryWriteError::Encode { source })?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| RegistryWriteError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, &bytes).map_err(|source| RegistryWriteError::WriteFile {
        path: tmp.clone(),
        source,
    })?;
    restrict_owner_only(&tmp)?;
    std::fs::File::open(&tmp)
        .and_then(|file| file.sync_all())
        .map_err(|source| RegistryWriteError::SyncFile {
            path: tmp.clone(),
            source,
        })?;
    std::fs::rename(&tmp, path).map_err(|source| RegistryWriteError::ReplaceFile {
        path: path.to_path_buf(),
        source,
    })?;
    restrict_owner_only(path)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    // Verification-via-reread: the entries this write carried must
    // show in the file now. With the lock held, a mismatch means a
    // writer outside the contract replaced the file mid-write.
    let reread = std::fs::read(path).map_err(|source| RegistryWriteError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let reread_file: serde_json::Value =
        serde_json::from_slice(&reread).map_err(|source| RegistryWriteError::Parse {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
        })?;
    let reread_drives = reread_file
        .get("drives")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| RegistryWriteError::VerifyMismatch {
            path: path.to_path_buf(),
            detail: "reread has no drives map".to_string(),
        })?;
    for (key, entry) in drives {
        let expected =
            serde_json::to_value(entry).map_err(|source| RegistryWriteError::Encode { source })?;
        match reread_drives.get(key) {
            Some(found) if found == &expected => {}
            _ => {
                return Err(RegistryWriteError::VerifyMismatch {
                    path: path.to_path_buf(),
                    detail: format!("entry {key:?} missing or changed on reread"),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The session gate pins without an owner: unlocked where the
    /// owner answered a definition-less drive, locked where no
    /// transport answered, absent for managed drives — and absent
    /// for an unresolved probe, which changes nothing instead of
    /// declaring the drive locked.
    #[test]
    fn session_gate_pins_the_posture_rule() {
        assert!(session_for(&DriveAvailability::Available, true).is_none());
        assert!(session_for(
            &DriveAvailability::Unavailable {
                detail: "down".to_string()
            },
            true
        )
        .is_none());
        assert!(session_for(
            &DriveAvailability::Unknown {
                detail: "wedged".to_string()
            },
            true
        )
        .is_none());
        assert!(session_for(
            &DriveAvailability::Unknown {
                detail: "wedged".to_string()
            },
            false
        )
        .is_none());
        assert!(matches!(
            session_for(&DriveAvailability::Available, false),
            Some(SessionState::Unlocked { .. })
        ));
        match session_for(
            &DriveAvailability::Unavailable {
                detail: "down".to_string(),
            },
            false,
        ) {
            Some(SessionState::Locked { detail }) => {
                assert!(detail.contains("unlock"), "detail: {detail}");
                assert!(detail.contains("reinstall"), "detail: {detail}");
            }
            other => panic!("a down definition-less drive is locked: {other:?}"),
        }
    }

    /// The unlock relay lookup reads the registered set without
    /// probing: relays come back in entry order, and a missing or
    /// malformed file, a drive with no entry, or an entry with no
    /// usable relays all answer empty — so unlock falls back to the
    /// drive config file exactly as before. Non-string relay values
    /// are skipped, never guessed, as are empty, whitespace, or
    /// control-byte strings (the writer's relay rule mirrored).
    #[test]
    fn registry_relay_lookup_fails_open_to_the_drive_config() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-reg-{}-relays", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let drive = dir.join("drive");
        // Missing file answers empty.
        assert!(registry_relays_for_at(&dir.join("absent.json"), &drive).is_empty());
        // Malformed file answers empty.
        let broken = dir.join("broken.json");
        std::fs::write(&broken, r#"{"version": 1, "drives": {"#).unwrap();
        assert!(registry_relays_for_at(&broken, &drive).is_empty());
        let path = dir.join("drives.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "drives": {
                    "demo": {
                        "name": "demo",
                        "drive_dir": drive.to_string_lossy(),
                        "relays": ["wss://one.example", "wss://two.example", 7, "", "  ", "wss://bad\u{7f}.example"],
                        "service": "drive-demo",
                        "mode": "vault",
                    },
                    "bare": {
                        "name": "bare",
                        "drive_dir": dir.join("other").to_string_lossy(),
                        "service": "drive-bare",
                        "mode": "vault",
                    },
                },
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            registry_relays_for_at(&path, &drive),
            vec![
                "wss://one.example".to_string(),
                "wss://two.example".to_string()
            ],
            "registered relays come back in order, unusable values skipped"
        );
        // A drive with no entry answers empty.
        assert!(registry_relays_for_at(&path, &dir.join("other-drive")).is_empty());
        // An entry with no relays key answers empty.
        assert!(registry_relays_for_at(&path, &dir.join("other")).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The managed-definition gate reads the registry file without
    /// probing: a missing file, a malformed file, a drive with no
    /// entry, and an entry whose definition is absent all proceed
    /// (answer `false`) — the gate only ever refuses where a
    /// definition file actually exists.
    #[test]
    fn managed_definition_gate_fails_open_to_unlock() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-reg-{}-mngdef", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let drive = dir.join("drive");
        // Missing file proceeds.
        assert!(!managed_definition_exists_at(
            &dir.join("absent.json"),
            &drive
        ));
        // Malformed file proceeds.
        let broken = dir.join("broken.json");
        std::fs::write(&broken, r#"{"version": 1, "drives": {"#).unwrap();
        assert!(!managed_definition_exists_at(&broken, &drive));
        // An entry naming a service with no installed definition proceeds.
        let path = dir.join("drives.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "drives": {
                    "demo": {
                        "name": "demo",
                        "drive_dir": drive.to_string_lossy(),
                        "relays": [],
                        "service": "drive-demo-no-such-definition",
                        "mode": "vault",
                    },
                },
            })
            .to_string(),
        )
        .unwrap();
        assert!(!managed_definition_exists_at(&path, &drive));
        // A drive with no entry proceeds.
        assert!(!managed_definition_exists_at(&path, &dir.join("other")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The taxonomy-to-reachability map pins without an owner: every
    /// client outcome lands on exactly one verdict, and only a
    /// terminal owner answer (success or refusal) reports available.
    #[test]
    fn probe_mapping_covers_the_taxonomy() {
        assert_eq!(
            availability_of(Err(ClientError::Unavailable("down".to_string()))),
            DriveAvailability::Unavailable {
                detail: "down".to_string()
            }
        );
        assert_eq!(
            availability_of(Err(ClientError::Unconfirmed {
                op: "liveness".to_string(),
                detail: "torn".to_string(),
            })),
            DriveAvailability::Unknown {
                detail: "torn".to_string()
            }
        );
        assert_eq!(
            availability_of(Err(ClientError::Incompatible("v9".to_string()))),
            DriveAvailability::Unknown {
                detail: "v9".to_string()
            }
        );
        // A refusal proves the owner answers: transport-level
        // availability, not a judgment on the probe.
        let refused = ClientError::Refused(wyrd_owner_client::WireError {
            code: wyrd_owner_client::WireErrorCode::Store,
            message: "locked".to_string(),
        });
        assert_eq!(availability_of(Err(refused)), DriveAvailability::Available);
    }

    /// A missing registry file is an empty drive list, not an
    /// error: a fresh installation registered nothing yet.
    #[test]
    fn missing_registry_is_empty_not_error() {
        let path = std::env::temp_dir().join(format!(
            "wyrd-desktop-noreg-{}-{}",
            std::process::id(),
            "missing.json"
        ));
        let _ = std::fs::remove_file(&path);
        let state = list_drives_at(&path);
        assert_eq!(state.status, RegistryStatus::Nofile);
        assert!(state.drives.is_empty());
        assert!(state.skipped.is_empty());
    }

    /// Version skew and malformed files surface as explicit
    /// unreadable states with the reason — loud, never guessed.
    #[test]
    fn version_skew_and_malformed_files_are_unreadable() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-reg-{}-skew", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let newer = dir.join("newer.json");
        std::fs::write(&newer, r#"{"version": 2, "drives": {}}"#).unwrap();
        match list_drives_at(&newer).status {
            RegistryStatus::Unreadable { reason } => {
                assert!(reason.contains("version 2"), "reason: {reason}");
                assert!(reason.contains("reads 1"), "reason: {reason}");
            }
            status => panic!("a newer registry refuses loudly: {status:?}"),
        }
        let broken = dir.join("broken.json");
        std::fs::write(&broken, r#"{"version": 1, "drives": {"#).unwrap();
        assert!(
            matches!(
                list_drives_at(&broken).status,
                RegistryStatus::Unreadable { .. }
            ),
            "a malformed registry is unreadable, never guessed"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Invalid entries skip with reasons while valid ones verify:
    /// entries pointing at no owner probe `Unavailable`, which says
    /// nothing about any drive.
    #[test]
    fn invalid_entries_skip_while_valid_ones_probe() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-reg-{}-entries", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let drive = dir.join("drive");
        std::fs::create_dir_all(&drive).unwrap();
        let path = dir.join("drives.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "drives": {
                    "good": {
                        "name": "good",
                        "drive_dir": drive.to_string_lossy(),
                        "relays": [],
                        "service": "drive-good",
                        "mode": "vault",
                    },
                    "bad": {
                        "name": "other",
                        "drive_dir": "/absent",
                        "relays": [],
                        "service": "drive-bad",
                        "mode": "vault",
                    },
                },
            })
            .to_string(),
        )
        .unwrap();
        let state = list_drives_at(&path);
        assert_eq!(state.status, RegistryStatus::Ready);
        assert_eq!(state.drives.len(), 1, "only the valid entry verifies");
        assert_eq!(state.drives[0].name, "good");
        assert!(
            matches!(
                state.drives[0].availability,
                DriveAvailability::Unavailable { .. }
            ),
            "no owner serves the scratch drive: {:?}",
            state.drives[0].availability
        );
        assert_eq!(state.skipped.len(), 1, "the invalid entry skips loudly");
        assert!(
            state.skipped[0].contains("does not match"),
            "skip reason: {}",
            state.skipped[0]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Two names for one directory collapse to one drive: the first
    /// name wins and the duplicate skips loudly. Without this the
    /// frontend's drive-dir-keyed lists would render two rows under
    /// one key, and the backend would hold two connections for one
    /// socket.
    #[test]
    fn duplicate_drive_dirs_skip_loudly() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-reg-{}-dupes", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let drive = dir.join("drive");
        std::fs::create_dir_all(&drive).unwrap();
        let path = dir.join("drives.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "drives": {
                    "alpha": {
                        "name": "alpha",
                        "drive_dir": drive.to_string_lossy(),
                        "relays": [],
                        "service": "drive-alpha",
                        "mode": "vault",
                    },
                    "beta": {
                        "name": "beta",
                        "drive_dir": drive.to_string_lossy(),
                        "relays": [],
                        "service": "drive-beta",
                        "mode": "vault",
                    },
                },
            })
            .to_string(),
        )
        .unwrap();
        let state = list_drives_at(&path);
        assert_eq!(state.status, RegistryStatus::Ready);
        assert_eq!(state.drives.len(), 1, "one directory lists one drive");
        assert_eq!(state.drives[0].name, "alpha", "the first name wins");
        assert_eq!(state.skipped.len(), 1, "the duplicate skips loudly");
        assert!(
            state.skipped[0].contains("duplicate drive_dir"),
            "skip reason: {}",
            state.skipped[0]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Relative and `..`-carrying directories skip with their rule
    /// reasons: they pin the registry call site of the shared
    /// drive-identity rule, so the lifecycle view can never list a
    /// spelling the connection map refuses.
    #[test]
    fn non_canonical_drive_dirs_skip_loudly() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-reg-{}-noncanon", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("drives.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "drives": {
                    "relative": {
                        "name": "relative",
                        "drive_dir": "drives/relative",
                        "relays": [],
                        "service": "drive-relative",
                        "mode": "vault",
                    },
                    "dotdot": {
                        "name": "dotdot",
                        "drive_dir": "/drives/a/../b",
                        "relays": [],
                        "service": "drive-dotdot",
                        "mode": "vault",
                    },
                },
            })
            .to_string(),
        )
        .unwrap();
        let state = list_drives_at(&path);
        assert_eq!(state.status, RegistryStatus::Ready);
        assert!(
            state.drives.is_empty(),
            "no non-canonical spelling lists: {:?}",
            state.drives
        );
        assert_eq!(state.skipped.len(), 2, "both entries skip loudly");
        assert!(
            state.skipped.iter().all(|reason| reason.contains("must")),
            "skip reasons name the rule: {:?}",
            state.skipped
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn write_entry(name: &str, dir: &std::path::Path) -> RegistryEntryWrite {
        RegistryEntryWrite {
            name: name.to_string(),
            drive_dir: dir.join("drive").join(name),
            relays: vec![],
            service: format!("drive-{name}"),
            mode: "vault".to_string(),
            mountpoint: None,
        }
    }

    fn write_mount_entry(
        name: &str,
        dir: &std::path::Path,
        mountpoint: &std::path::Path,
    ) -> RegistryEntryWrite {
        RegistryEntryWrite {
            name: name.to_string(),
            drive_dir: dir.join("drive").join(name),
            relays: vec![],
            service: format!("drive-{name}"),
            mode: "mount".to_string(),
            mountpoint: Some(mountpoint.to_path_buf()),
        }
    }

    fn write_map<const N: usize>(dir: &std::path::Path, names: [&str; N]) -> BTreeMapForTest {
        let mut map = BTreeMapForTest::new();
        for name in names {
            map.insert(name.to_string(), write_entry(name, dir));
        }
        map
    }

    type BTreeMapForTest = std::collections::BTreeMap<String, RegistryEntryWrite>;

    /// A desktop write round-trips through the reader: what the
    /// writer accepts, the reader lists — the shared validator holds
    /// both directions.
    #[test]
    fn desktop_write_round_trips_through_the_reader() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-reg-{}-write", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["alpha", "beta"] {
            std::fs::create_dir_all(dir.join("drive").join(name)).unwrap();
        }
        let path = dir.join("drives.json");
        write_registry_at(&path, &write_map(&dir, ["alpha", "beta"])).unwrap();
        let state = list_drives_at(&path);
        assert_eq!(state.status, RegistryStatus::Ready);
        assert_eq!(state.drives.len(), 2);
        assert!(state.skipped.is_empty(), "{:?}", state.skipped);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A mount write round-trips through the reader with its
    /// mountpoint: the view carries the mode and the mountpoint, and
    /// nothing is skipped.
    #[test]
    fn desktop_mount_write_round_trips_with_its_mountpoint() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-reg-{}-mount", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mountpoint = dir.join("mnt");
        std::fs::create_dir_all(&mountpoint).unwrap();
        std::fs::create_dir_all(dir.join("drive").join("alpha")).unwrap();
        let path = dir.join("drives.json");
        let mut map = BTreeMapForTest::new();
        map.insert(
            "alpha".to_string(),
            write_mount_entry("alpha", &dir, &mountpoint),
        );
        write_registry_at(&path, &map).unwrap();
        let state = list_drives_at(&path);
        assert_eq!(state.status, RegistryStatus::Ready);
        assert_eq!(state.drives.len(), 1);
        assert_eq!(state.drives[0].mode, "mount");
        assert_eq!(
            state.drives[0].mountpoint,
            Some(mountpoint.to_string_lossy().into_owned())
        );
        assert!(state.skipped.is_empty(), "{:?}", state.skipped);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Mount validation pins the mountpoint rules: a missing
    /// mountpoint, a vault entry carrying one, and a mountpoint
    /// equal to the drive directory all refuse — while unknown
    /// future modes skip with the generic reason.
    #[test]
    fn desktop_mount_validation_pins_the_mountpoint_rules() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-reg-{}-mount-rules",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("drives.json");
        // Mount without a mountpoint refuses.
        let mut missing = write_entry("missing", &dir);
        missing.mode = "mount".to_string();
        let refused = write_registry_at(&path, &{
            let mut map = BTreeMapForTest::new();
            map.insert("missing".to_string(), missing);
            map
        })
        .unwrap_err();
        assert!(refused.to_string().contains("mountpoint"), "{refused}");
        // Vault carrying a mountpoint refuses.
        let mut stray = write_entry("stray", &dir);
        stray.mountpoint = Some(dir.join("mnt"));
        let refused = write_registry_at(&path, &{
            let mut map = BTreeMapForTest::new();
            map.insert("stray".to_string(), stray);
            map
        })
        .unwrap_err();
        assert!(refused.to_string().contains("vault"), "{refused}");
        assert!(!path.exists(), "no partial registry may remain");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// One invalid candidate refuses the whole write and leaves no
    /// file behind: batch discipline, mirroring the daemon.
    #[test]
    fn desktop_write_refuses_invalid_entries_and_writes_nothing() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-reg-{}-nowrite", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("drives.json");
        let mut map = write_map(&dir, ["good"]);
        let mut bad = write_entry("bad", &dir);
        bad.drive_dir = std::path::PathBuf::from("relative/path");
        map.insert("bad".to_string(), bad);
        let refused = write_registry_at(&path, &map).unwrap_err();
        assert!(refused.to_string().contains("absolute"), "{refused}");
        assert!(!path.exists(), "no partial registry may remain");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A present-but-unreadable registry refuses the write instead
    /// of being replaced: malformed JSON fails loud, and a newer
    /// schema is never downgraded to version 1 with only the
    /// candidates. Either way nothing is written.
    #[test]
    fn desktop_write_refuses_unreadable_and_newer_files() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-reg-{}-refuse", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let malformed = dir.join("drives.json");
        std::fs::write(&malformed, r#"{"version": 1, "drives": {"#).unwrap();
        let refused = write_registry_at(&malformed, &write_map(&dir, ["new"])).unwrap_err();
        assert!(refused.to_string().contains("parse"), "{refused}");
        assert_eq!(
            std::fs::read(&malformed).unwrap(),
            b"{\"version\": 1, \"drives\": {",
            "the malformed file is untouched"
        );

        let newer = dir.join("newer.json");
        std::fs::write(&newer, r#"{"version": 999, "drives": {}}"#).unwrap();
        let refused = write_registry_at(&newer, &write_map(&dir, ["new"])).unwrap_err();
        assert!(refused.to_string().contains("999"), "{refused}");
        assert_eq!(
            std::fs::read(&newer).unwrap(),
            br#"{"version": 999, "drives": {}}"#,
            "the newer file is never downgraded"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Two candidates sharing one drive directory refuse the whole
    /// write: the reader would skip the duplicate (first name wins),
    /// so the writer never produces a set it only half-lists.
    /// Nothing is written.
    #[test]
    fn desktop_write_refuses_duplicate_drive_dirs() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-reg-{}-write-dupes",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("drives.json");
        let mut map = BTreeMapForTest::new();
        let first = write_entry("alpha", &dir);
        let mut second = write_entry("beta", &dir);
        second.drive_dir = first.drive_dir.clone();
        map.insert("alpha".to_string(), first);
        map.insert("beta".to_string(), second);
        let refused = write_registry_at(&path, &map).unwrap_err();
        assert!(
            refused.to_string().contains("duplicate drive_dir"),
            "{refused}"
        );
        assert!(!path.exists(), "no partial registry may remain");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A structurally corrupt entry refuses the write: it decodes
    /// as a raw value but not as a registry entry, exactly like the
    /// reader, so carrying it and reporting success would contradict
    /// the reader's loud verdict on the same file. Nothing is written.
    /// (Unknown modes still carry: they decode structurally and only
    /// fail validation.)
    #[test]
    fn desktop_write_refuses_structurally_corrupt_entries() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-reg-{}-corrupt", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("drives.json");
        let corrupt = r#"{"version": 1, "drives": {"x": {"drive_dir": "/d"}}}"#;
        std::fs::write(&path, corrupt).unwrap();
        let refused = write_registry_at(&path, &write_map(&dir, ["new"])).unwrap_err();
        assert!(refused.to_string().contains("parse"), "{refused}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            corrupt.as_bytes(),
            "the corrupt file is untouched"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Entries the reader skips are carried forward verbatim: a
    /// rewrite must never delete what this build does not
    /// understand — including a future mode the desktop reader
    /// skips today.
    #[test]
    fn desktop_write_carries_skipped_entries_forward_verbatim() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-reg-{}-carry", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("drives.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "drives": {
                    "future": {
                        "name": "future",
                        "drive_dir": "/drives/future",
                        "relays": [],
                        "service": "drive-future",
                        "mode": "fuse",
                    },
                },
            })
            .to_string(),
        )
        .unwrap();
        write_registry_at(&path, &write_map(&dir, ["new"])).unwrap();
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let drives = raw.get("drives").unwrap().as_object().unwrap();
        assert_eq!(drives.len(), 2, "nothing was deleted: {drives:?}");
        assert_eq!(
            drives["future"].get("mode").unwrap(),
            "fuse",
            "carried verbatim: {:?}",
            drives["future"]
        );
        assert!(drives.contains_key("new"), "{drives:?}");
        // The reader still lists the new entry and skips the future
        // one loudly — carrying is invisible to the view.
        let state = list_drives_at(&path);
        assert_eq!(state.drives.len(), 1);
        assert_eq!(state.skipped.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A torn temporary sibling never affects reads: orphans from an
    /// interrupted write sit beside the registry, and the atomic
    /// rename is the only way content moves.
    #[test]
    fn torn_temporary_siblings_never_affect_reads() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-reg-{}-torn", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("drives.json");
        write_registry_at(&path, &write_map(&dir, ["kept"])).unwrap();
        let orphan = path.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&orphan, b"{truncated").unwrap();
        let state = list_drives_at(&path);
        assert_eq!(state.status, RegistryStatus::Ready);
        assert_eq!(state.drives.len(), 1);
        assert_eq!(state.drives[0].name, "kept");
        // And the next write still lands over the orphan's name.
        write_registry_at(&path, &write_map(&dir, ["kept", "added"])).unwrap();
        let state = list_drives_at(&path);
        assert_eq!(state.drives.len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Concurrent desktop writers converge without lost entries:
    /// every thread writes one distinct key, and the registry lock
    /// serializes the read-modify-writes. Readers interleaved with
    /// the writers always see a complete, parseable file — never a
    /// torn registry.
    #[test]
    fn concurrent_desktop_writes_converge_without_lost_entries() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-reg-{}-concurrent",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("drives.json");
        let count = 8usize;
        std::thread::scope(|scope| {
            for i in 0..count {
                let path = &path;
                let dir = &dir;
                scope.spawn(move || {
                    let name = format!("w{i:02}");
                    let mut map = BTreeMapForTest::new();
                    map.insert(name.clone(), write_entry(&name, dir));
                    write_registry_at(path, &map).unwrap();
                });
            }
            for _ in 0..count {
                let path = &path;
                scope.spawn(move || {
                    for _ in 0..16 {
                        let bytes = std::fs::read(path).unwrap_or_default();
                        if bytes.is_empty() {
                            continue;
                        }
                        let file: serde_json::Value = serde_json::from_slice(&bytes)
                            .expect("every observed registry is complete and parseable");
                        assert_eq!(file.get("version").and_then(|v| v.as_u64()), Some(1));
                    }
                });
            }
        });
        let state = list_drives_at(&path);
        assert_eq!(state.status, RegistryStatus::Ready);
        assert_eq!(state.drives.len(), count, "{:?}", state.skipped);
        for i in 0..count {
            assert!(
                state
                    .drives
                    .iter()
                    .any(|drive| drive.name == format!("w{i:02}")),
                "writer w{i:02} survived: {:?}",
                state
                    .drives
                    .iter()
                    .map(|drive| &drive.name)
                    .collect::<Vec<_>>()
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
