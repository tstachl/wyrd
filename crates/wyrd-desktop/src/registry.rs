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
    Ready,
    Nofile,
    Unreadable { reason: String },
}

/// One drive's reachability, verified this session. Transport-level
/// only: `Available` never claims anything about the drive's
/// contents, and `Unavailable` never claims the drive is gone.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum DriveAvailability {
    Available,
    Unavailable { detail: String },
    Unknown { detail: String },
}

/// One registered drive for the lifecycle view: validated facts
/// plus the live verdict. `mode` is a display string end to end —
/// the shell shows the registry's service mode and never interprets
/// it. Drives match by exact path only.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DriveView {
    pub name: String,
    pub drive_dir: String,
    pub relays: Vec<String>,
    pub service: String,
    pub mode: String,
    pub availability: DriveAvailability,
}

/// The lifecycle view's registry answer: the file status, one view
/// per valid entry, and one reason per skipped entry. The status
/// flattens into the payload (`{"status": "ready", "drives": ...}`)
/// so the file verdict reads without stutter.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RegistryState {
    #[serde(flatten)]
    pub status: RegistryStatus,
    pub drives: Vec<DriveView>,
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
    if mode != "vault" {
        return Err(
            "unknown service mode: registry-managed mount mode travels with the installer follow-up"
                .to_string(),
        );
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
                drives.push(DriveView {
                    name: entry.name.clone(),
                    drive_dir: entry.drive_dir.to_string_lossy().into_owned(),
                    relays: entry.relays.clone(),
                    service: entry.service.clone(),
                    mode: entry.mode.clone(),
                    availability: probe_drive(&entry.drive_dir),
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
    pub name: String,
    pub drive_dir: PathBuf,
    pub relays: Vec<String>,
    pub service: String,
    pub mode: String,
}

/// Every way a desktop registry write can fail. Variants carry the
/// failing identity — paths, names — so future commands can branch
/// on failure instead of parsing text.
#[derive(Debug, thiserror::Error)]
pub enum RegistryWriteError {
    /// The registry lockfile could not be opened or locked.
    #[error("lock {}: {source}", path.display())]
    Lock {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A candidate entry breaks the registry rules: nothing was written.
    #[error("refusing to write: {reason}")]
    RefusedWrite { reason: String },
    /// The registry schema version is newer than this build writes:
    /// writing would silently replace a file this build cannot read.
    #[error("registry {} is version {found}, this shell writes {REGISTRY_VERSION}", path.display())]
    UnsupportedVersion { path: PathBuf, found: u64 },
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
        source: std::io::Error,
    },
    /// The registry file could not be encoded.
    #[error("encode registry: {source}")]
    Encode {
        #[source]
        source: serde_json::Error,
    },
    /// A directory for the registry could not be created.
    #[error("create {}: {source}", path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The temporary registry file could not be written.
    #[error("write {}: {source}", path.display())]
    WriteFile {
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
    /// The atomic replace of the registry file failed.
    #[error("replace {}: {source}", path.display())]
    ReplaceFile {
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
    /// The reread after the replace does not show the written
    /// entries: something outside the lock rewrote the file between
    /// the rename and the verification. Loud, never silent.
    #[error("verify {}: written entries do not match reread ({detail})", path.display())]
    VerifyMismatch { path: PathBuf, detail: String },
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
    // (see the command-surface rule in `commands.rs`).
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
    /// understand — including a future mount mode the desktop
    /// reader skips today.
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
                        "mode": "mount",
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
            "mount",
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
