//! Application settings file: the per-user budgets a person configures
//! outside any drive directory (OD-25-A, recommendation A).
//!
//! One JSON file per user holds one app-wide diagnostics section
//! beside per-drive sections keyed by absolute drive directory:
//! `{"version": 2, "diagnostics": {"level": "detailed"}, "drives":
//! {"<abs path>": {budget keys...}}}`. The diagnostics section is
//! absent until detailed mode is set. It is authoritative local
//! state that derives from nothing durable — not signed archive
//! state, not a format a peer reads — so it lives outside the drive
//! directory (Wyrd-owned state under
//! `docs/contracts/upgrade-contract.md`) and
//! outside invariant 2's format list; the contract note names the
//! fourth category. Unknown keys are refused with their name (a
//! typo'd key silently discarded is the failure mode that makes
//! configuration files untrustworthy), and a newer schema version
//! refuses loudly rather than guessing.
//!
//! The file holds the five surfaced bounds
//! (`retained_bytes_quota`, `max_admit_per_pass`,
//! `fetch_pass_budget_secs`, `max_mutation_wait_secs`,
//! `open_timeout_secs`) plus the notification records
//! (`notified`); relays are read-only on the surface and live in
//! the drive deployment configuration, never here. Values convert
//! into [`BudgetOverrides`]
//! — the file never constructs a config itself, so it cannot build
//! the quota-without-accountant mismatch — and command-line flags
//! win over the file, which wins over compiled defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use wyrd_core::settings::BudgetOverrides;
use wyrd_settings::{
    check_duration_secs, check_max_admit_per_pass, check_notified_key, parse_settings,
    validate_drive_key, validate_sparse_update, SparseUpdate,
};

/// Schema version of the application settings file. Re-exported
/// from the shared contract: bump only with a migration that reads
/// the previous version; readers refuse newer versions loudly
/// rather than guessing.
pub use wyrd_settings::APP_SETTINGS_VERSION;
/// Filename of the application settings file inside the per-user
/// state directory.
pub use wyrd_settings::SETTINGS_FILENAME;

/// Ceiling on notification records per drive section. Re-exported
/// from the shared contract; see there for the rationale.
pub use wyrd_settings::MAX_NOTIFIED_PER_DRIVE;
/// One drive's settings section: the five surfaced bounds plus the
/// notification records. Re-exported from the shared contract —
/// the single definition both readers enforce.
pub use wyrd_settings::{
    AppSettingsFile, DiagnosticLevel, DiagnosticSettingsSection, DiagnosticsUpdate,
    DriveSettingsSection,
};

/// Where the settings file lives when no explicit path is given:
/// per-user state, resolved through the same platform conventions
/// as the drive registry. The writer creates it with owner-only
/// permissions; registration and settings flows are its normal
/// writers, never hand-editing. Fails loudly when no per-user
/// directory resolves (no `HOME`, no `XDG_STATE_HOME`): falling
/// back to a cwd-relative file could place settings inside a drive
/// directory, which the upgrade contract forbids — so there is no
/// fallback, only an error naming both variables.
pub fn default_settings_path() -> Result<PathBuf, String> {
    resolve_settings_path(
        std::env::var("HOME").ok().as_deref(),
        std::env::var("XDG_STATE_HOME").ok().as_deref(),
    )
}

/// Pure half of [`default_settings_path`]: resolve the per-user
/// settings path from explicit inputs so the no-home refusal pins
/// without touching the process environment.
pub fn resolve_settings_path(
    home: Option<&str>,
    xdg_state_home: Option<&str>,
) -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    if let Some(home) = home {
        if !home.is_empty() {
            return Ok(PathBuf::from(home)
                .join("Library/Application Support/Wyrd")
                .join(SETTINGS_FILENAME));
        }
    }
    if let Some(dir) = xdg_state_home {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir).join("wyrd").join(SETTINGS_FILENAME));
        }
    }
    if let Some(home) = home {
        if !home.is_empty() {
            return Ok(PathBuf::from(home)
                .join(".local/state/wyrd")
                .join(SETTINGS_FILENAME));
        }
    }
    Err(
        "no per-user state directory: set HOME or XDG_STATE_HOME to locate settings.json"
            .to_string(),
    )
}

/// The `open` demand-wait the composers pass at the split when the
/// settings file names none: the historical hardcoded 30s, now the
/// default a per-drive section overrides.
pub const DEFAULT_OPEN_TIMEOUT: Duration = Duration::from_secs(30);

/// Split one drive's file overrides into the quota (which must be
/// known before the store opens, so the store constructor shares
/// the accountant) and the rest (applied onto the config after the
/// store opens). Flags win: callers pass the flag quota separately
/// and only consult the file quota when the flag is absent.
pub fn split_file_overrides(drive_dir: &Path) -> Result<(Option<u64>, BudgetOverrides), String> {
    let file = overrides_for(drive_dir)?;
    let quota = file.retained_bytes_quota;
    let rest = BudgetOverrides {
        retained_bytes_quota: None,
        max_admit_per_pass: file.max_admit_per_pass,
        fetch_pass_budget: file.fetch_pass_budget,
        max_mutation_wait: file.max_mutation_wait,
        open_timeout: file.open_timeout,
    };
    Ok((quota, rest))
}
/// The lookup key for one drive's section. Re-exported from the
/// shared contract: the absolute drive directory as an exact
/// string, one spelling one section, never canonicalized through
/// symlinks.
pub use wyrd_settings::settings_key_for;

/// Read and validate the settings file. A missing file is empty
/// settings, not an error — a fresh installation has configured
/// nothing yet. A v1 file migrates in memory to v2; a newer schema
/// version refuses loudly. Stored keys that are not absolute fail
/// closed with their name.
pub fn read_settings(path: &Path) -> Result<AppSettingsFile, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AppSettingsFile::default());
        }
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let file =
        parse_settings(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))?;
    for key in file.drives.keys() {
        validate_drive_key(key)?;
    }
    Ok(file)
}

/// Write the settings file crash-safely: temporary sibling in the
/// same directory, synced, atomically replaced, directory synced —
/// so an interrupted write can never leave a truncated settings
/// file. Owner-only permissions throughout: the file holds
/// operator tuning, still per-user state.
pub fn write_settings(path: &Path, settings: &AppSettingsFile) -> Result<(), String> {
    if settings.version != APP_SETTINGS_VERSION {
        return Err(format!(
            "refusing to write settings version {} (this build writes {})",
            settings.version, APP_SETTINGS_VERSION
        ));
    }
    for key in settings.drives.keys() {
        validate_drive_key(key).map_err(|reason| format!("refusing to write: {reason}"))?;
    }
    let bytes =
        serde_json::to_vec_pretty(settings).map_err(|error| format!("encode settings: {error}"))?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("settings path {} has no file name", path.display()))?;
    wyrd_sync::durable::atomic_write_mode(parent, &file_name.to_string_lossy(), &bytes, 0o600)
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(())
}

fn secs_to_duration(name: &str, secs: u64) -> Result<Duration, String> {
    check_duration_secs(name, secs)?;
    Ok(Duration::from_secs(secs))
}

/// Failure of the sparse settings transaction: three outcomes,
/// never conflated. Conflict is its own variant — never a message
/// convention — so the future IPC surface maps it to a distinct
/// error; Refused means nothing was committed; CommittedUnverified
/// means the commit landed but the final re-read failed. Only
/// conflict invites the caller to re-read and retry.
#[derive(Debug, thiserror::Error)]
pub enum SettingsWriteError {
    /// The file changed between the transaction's read and commit:
    /// a non-cooperating writer bypassed the lock. Never auto-merge
    /// or retry inside the transaction; the caller decides.
    #[error("settings at {path} changed under the transaction: {reason}")]
    Conflict {
        /// The settings file whose read and commit diverged.
        path: PathBuf,
        /// What moved: always the read-vs-commit mismatch, never a guess.
        reason: String,
    },
    /// Validation, I/O, or permission failure before any commit.
    #[error("refusing settings write to {path}: {reason}")]
    Refused {
        /// The settings file the transaction refused to touch.
        path: PathBuf,
        /// The validation, I/O, or permission failure, with its cause.
        reason: String,
    },
    /// The commit landed but the verification re-read failed: the
    /// file changed, the refusal taxonomy must not claim otherwise.
    /// OD §6 never conflates outcomes — a committed write is
    /// reported committed, even when only the final re-read failed.
    #[error("settings at {path} committed but unverified: {reason}")]
    CommittedUnverified {
        /// The settings file the transaction committed.
        path: PathBuf,
        /// The re-read failure, with its cause.
        reason: String,
    },
}

fn refused(path: &Path, reason: String) -> SettingsWriteError {
    SettingsWriteError::Refused {
        path: path.to_path_buf(),
        reason,
    }
}

/// Exclusive sibling lock for one settings file: `settings.json`
/// pairs with `settings.lock`, derived the same way the registry
/// derives its lock, so no configuration can point two writers at
/// different locks. Blocking, like the registry's: settings writes
/// are rare, and waiting beats losing. The lock serializes
/// cooperating writers (the sparse editor path and the daemon's
/// `notified` writer); the commit hash below detects the rest.
fn settings_lock_path(path: &Path) -> PathBuf {
    path.with_extension("lock")
}

/// Hold the settings file's exclusive lock, creating the lockfile
/// on first use. Owner-only from creation: a world-writable lock
/// lets another local user wedge the settings writer. The handle
/// keeps the lock; dropping it releases.
fn lock_settings(path: &Path) -> Result<std::fs::File, SettingsWriteError> {
    let lock_path = settings_lock_path(path);
    if let Some(parent) = lock_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|error| refused(path, format!("create lock dir: {error}")))?;
        }
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|error| refused(path, format!("open lock {}: {error}", lock_path.display())))?;
    restrict_owner_only(&lock_path)?;
    file.lock()
        .map_err(|error| refused(path, format!("lock {}: {error}", lock_path.display())))?;
    Ok(file)
}

/// Owner-only file permissions (Unix): the settings file holds
/// operator tuning — per-user state, no group or other access, ever.
/// New files land here through the atomic writer's creation mode;
/// the same call corrects an over-permissive file the transaction
/// is about to replace.
#[cfg(unix)]
fn restrict_owner_only(path: &Path) -> Result<(), SettingsWriteError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    // Only tighten: a stricter mode (from a masking umask) stays.
    let mode = std::fs::metadata(path)
        .map_err(|error| refused(path, format!("stat {}: {error}", path.display())))?
        .mode()
        & 0o777;
    if mode & 0o077 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| refused(path, format!("chmod {}: {error}", path.display())))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_owner_only(_path: &Path) -> Result<(), SettingsWriteError> {
    Ok(())
}

/// The direct parent of the settings file is Wyrd's own state leaf:
/// created private when missing, tightened when a looser mode
/// predates us. Higher ancestors are the operator's business and
/// keep their modes — creation hardens, opening never reaches past
/// the leaf. Runs under the transaction lock.
fn ensure_private_parent(settings_path: &Path) -> Result<(), SettingsWriteError> {
    let parent = settings_path.parent().unwrap_or_else(|| Path::new("."));
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        if !parent.is_dir() {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(parent)
                .map_err(|error| {
                    refused(
                        settings_path,
                        format!("create {}: {error}", parent.display()),
                    )
                })?;
            return Ok(());
        }
        let mode = std::fs::metadata(parent)
            .map_err(|error| refused(settings_path, format!("stat {}: {error}", parent.display())))?
            .mode()
            & 0o777;
        if mode & 0o077 != 0 {
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).map_err(
                |error| {
                    refused(
                        settings_path,
                        format!("chmod {}: {error}", parent.display()),
                    )
                },
            )?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            refused(
                settings_path,
                format!("create {}: {error}", parent.display()),
            )
        })
    }
}

/// Identity of one path for the commit guard: the file the two
/// reads resolved to, not its contents. Same-file-through-replace
/// must conflict even when the bytes coincide — a replace is a
/// writer, and the transaction commits onto what it read.
#[cfg(unix)]
fn file_identity(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

#[cfg(windows)]
fn file_identity(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::windows::fs::MetadataExt;
    (meta.volume_serial_number(), meta.file_index())
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_meta: &std::fs::Metadata) -> (u64, u64) {
    // No stable identity primitive: the content hash still guards
    // the commit, and a same-bytes replace is semantically void.
    (0, 0)
}

/// One transaction's read side: the raw bytes (absent when the file
/// does not exist yet — a fresh installation configures nothing),
/// their identity, and the strictly parsed file. Unknown keys and
/// newer versions refuse here, before anything is applied: the
/// original bytes stay the commit baseline, so a refused edit can
/// never silently drop what this build does not understand.
struct StagedFile {
    base_hash: blake3::Hash,
    base_identity: Option<(u64, u64)>,
    file: AppSettingsFile,
}

fn stage_file(settings_path: &Path) -> Result<StagedFile, SettingsWriteError> {
    let (bytes, identity) = match std::fs::read(settings_path) {
        Ok(bytes) => {
            let meta = std::fs::symlink_metadata(settings_path).map_err(|error| {
                refused(
                    settings_path,
                    format!("stat {}: {error}", settings_path.display()),
                )
            })?;
            let identity = Some(file_identity(&meta));
            (Some(bytes), identity)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, None),
        Err(error) => {
            return Err(refused(
                settings_path,
                format!("read {}: {error}", settings_path.display()),
            ));
        }
    };
    let file = match &bytes {
        None => AppSettingsFile::default(),
        Some(bytes) => {
            let file = parse_settings(bytes).map_err(|error| {
                refused(
                    settings_path,
                    format!("parse {}: {error}", settings_path.display()),
                )
            })?;
            for key in file.drives.keys() {
                validate_drive_key(key).map_err(|reason| refused(settings_path, reason))?;
            }
            file
        }
    };
    Ok(StagedFile {
        base_hash: blake3::hash(bytes.as_deref().unwrap_or_default()),
        base_identity: identity,
        file,
    })
}

/// Commit one staged file: re-read the path (same resolved
/// identity, same content hash — no TOCTOU through symlink or file
/// replacement), then temp + fsync + atomic rename + directory
/// fsync, and re-read the committed file through the normal reader.
/// The lock proves cooperation, never currency: a non-cooperating
/// external modification between the stage and this re-read is a
/// [`SettingsWriteError::Conflict`], never a silent overwrite.
fn commit_staged(
    settings_path: &Path,
    staged: StagedFile,
) -> Result<AppSettingsFile, SettingsWriteError> {
    write_guarded(settings_path, staged)?;
    verify_commit(settings_path)
}

/// Guarded write half of [`commit_staged`]: the re-read, the
/// identity-plus-hash comparison, the parent privacy, and the
/// crash-safe rename. Split from verification so tests can pin the
/// committed-but-unverified outcome deterministically.
fn write_guarded(settings_path: &Path, staged: StagedFile) -> Result<(), SettingsWriteError> {
    let (fresh_bytes, fresh_identity) = match std::fs::read(settings_path) {
        Ok(bytes) => {
            let meta = std::fs::symlink_metadata(settings_path).map_err(|error| {
                refused(
                    settings_path,
                    format!("stat {}: {error}", settings_path.display()),
                )
            })?;
            (Some(bytes), Some(file_identity(&meta)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, None),
        Err(error) => {
            return Err(refused(
                settings_path,
                format!("re-read {}: {error}", settings_path.display()),
            ));
        }
    };
    if fresh_identity != staged.base_identity
        || blake3::hash(fresh_bytes.as_deref().unwrap_or_default()) != staged.base_hash
    {
        return Err(SettingsWriteError::Conflict {
            path: settings_path.to_path_buf(),
            reason: "settings file changed between read and commit".to_string(),
        });
    }
    ensure_private_parent(settings_path)?;
    for key in staged.file.drives.keys() {
        validate_drive_key(key)
            .map_err(|reason| refused(settings_path, format!("refusing to write: {reason}")))?;
    }
    let bytes = serde_json::to_vec_pretty(&staged.file)
        .map_err(|error| refused(settings_path, format!("encode settings: {error}")))?;
    let parent = settings_path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = settings_path
        .file_name()
        .ok_or_else(|| {
            refused(
                settings_path,
                format!("settings path {} has no file name", settings_path.display()),
            )
        })?
        .to_string_lossy()
        .into_owned();
    wyrd_sync::durable::atomic_write_mode(parent, &file_name, &bytes, 0o600)
        .map_err(|error| refused(settings_path, format!("write: {error}")))?;
    // The rename carries the temp's owner-only mode, so the commit
    // corrects an over-permissive file it replaces — no post-rename
    // chmod, and no chmod-failure-after-rename outcome.
    Ok(())
}

/// Verification half of [`commit_staged`]: re-read the committed
/// file through the normal reader. A failure here is reported as
/// committed-but-unverified — never as a refusal of a write that
/// already landed (OD §6 never conflates outcomes).
fn verify_commit(settings_path: &Path) -> Result<AppSettingsFile, SettingsWriteError> {
    read_settings(settings_path).map_err(|reason| SettingsWriteError::CommittedUnverified {
        path: settings_path.to_path_buf(),
        reason,
    })
}

/// Apply one sparse edit for exactly one drive, transactionally:
/// validate before the lock (a refusal touches nothing), then read,
/// apply only the requested changes, verify the file did not move
/// under us, and commit crash-safely. Returns the drive's observed
/// configured section as re-read after the commit — absent when the
/// edit cleared its last bound (an emptied section is removed where
/// safe: no bounds and no notification records).
///
/// Bootstrap is a case of the same path, not a branch: a missing
/// file stages empty, and the commit establishes it at 0600 under
/// the lock. Other drives' sections and notification records pass
/// through semantically unchanged; absent keys stay absent, so no
/// effective defaults are ever materialized.
pub fn apply_sparse_update(
    settings_path: &Path,
    drive_dir: &Path,
    update: &SparseUpdate,
) -> Result<DriveSettingsSection, SettingsWriteError> {
    validate_sparse_update(update).map_err(|reason| refused(settings_path, reason))?;
    let drive_key = settings_key_for(drive_dir);
    validate_drive_key(&drive_key).map_err(|reason| refused(settings_path, reason))?;
    let _lock = lock_settings(settings_path)?;
    let mut staged = stage_file(settings_path)?;
    apply_update_to_file(settings_path, &mut staged.file, &drive_key, update)?;
    let committed = commit_staged(settings_path, staged)?;
    Ok(committed
        .drives
        .get(&drive_key)
        .cloned()
        .unwrap_or_default())
}

/// Fold one validated sparse edit into the staged file: set the
/// listed bounds, clear the listed ones, and drop the drive's
/// section when nothing remains — safe exactly when no bounds and
/// no notification records survive, so clearing never deletes
/// dedup state beside the edited keys.
fn apply_update_to_file(
    settings_path: &Path,
    file: &mut AppSettingsFile,
    drive_key: &str,
    update: &SparseUpdate,
) -> Result<(), SettingsWriteError> {
    let section = file.drives.entry(drive_key.to_string()).or_default();
    for (key, value) in &update.set {
        match key.as_str() {
            "retained_bytes_quota" => section.retained_bytes_quota = Some(*value),
            "max_admit_per_pass" => {
                section.max_admit_per_pass = Some(usize::try_from(*value).map_err(|_| {
                    refused(
                        settings_path,
                        format!(
                            "settings max_admit_per_pass value {value} overflows this platform"
                        ),
                    )
                })?);
            }
            "fetch_pass_budget_secs" => section.fetch_pass_budget_secs = Some(*value),
            "max_mutation_wait_secs" => section.max_mutation_wait_secs = Some(*value),
            "open_timeout_secs" => section.open_timeout_secs = Some(*value),
            _ => unreachable!("update validated before the lock"),
        }
    }
    for key in &update.clear {
        match key.as_str() {
            "retained_bytes_quota" => section.retained_bytes_quota = None,
            "max_admit_per_pass" => section.max_admit_per_pass = None,
            "fetch_pass_budget_secs" => section.fetch_pass_budget_secs = None,
            "max_mutation_wait_secs" => section.max_mutation_wait_secs = None,
            "open_timeout_secs" => section.open_timeout_secs = None,
            _ => unreachable!("update validated before the lock"),
        }
    }
    if *section == DriveSettingsSection::default() {
        file.drives.remove(drive_key);
    }
    Ok(())
}

pub use wyrd_settings::{
    app_group_identifier, app_group_identifier_from, APP_GROUP_ENV, APP_TEAM_ENV,
    DEFAULT_APP_GROUP_IDENTIFIER,
};
/// Mirror location inside the App Group container, relative to its
/// root. The extension reads exactly this path
/// (`Sources/Diagnostics.swift`), so the two spellings must agree.
pub const DIAGNOSTICS_MIRROR_RELATIVE_PATH: &str = "Diagnostics/level.json";

/// Resolve the App Group container from HOME for tests and
/// non-standard layouts. The container is
/// `~/Library/Group Containers/<group>` on macOS; the extension
/// resolves the same directory through `containerURL(forSecurity-
/// ApplicationGroupIdentifier:)`. No HOME is a loud refusal — no
/// cwd-relative guess, like the settings path itself.
pub fn diagnostics_mirror_container_for(home: Option<&str>) -> Result<PathBuf, String> {
    let home = home
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "no home directory: cannot locate the App Group container".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library/Group Containers")
        .join(app_group_identifier()))
}

/// The container for the running user.
pub fn default_diagnostics_mirror_container() -> Result<PathBuf, String> {
    diagnostics_mirror_container_for(std::env::var("HOME").ok().as_deref())
}

/// Mirror path inside a resolved container, for readers such as the
/// local bundle export. Publishing uses the container itself so it
/// can refuse to create a decoy.
pub fn diagnostics_mirror_path_in(container: &Path) -> PathBuf {
    container.join(DIAGNOSTICS_MIRROR_RELATIVE_PATH)
}

/// The exact document the extension parses: a version tag it
/// understands and the closed level. No extra fields, no settings
/// bytes and nothing from any other surface.
#[derive(Serialize)]
struct DiagnosticsMirrorDocument<'a> {
    version: u32,
    level: &'a str,
}

/// Create or tighten the `Diagnostics/` leaf inside an existing
/// container. Only the leaf is created: the container itself belongs
/// to the app's launch-time initialization, never to this writer.
fn ensure_private_mirror_dir(leaf: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        if !leaf.is_dir() {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(leaf)
                .map_err(|error| format!("create {}: {error}", leaf.display()))?;
            return Ok(());
        }
        let mode = std::fs::metadata(leaf)
            .map_err(|error| format!("stat {}: {error}", leaf.display()))?
            .mode()
            & 0o777;
        if mode & 0o077 != 0 {
            std::fs::set_permissions(leaf, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("chmod {}: {error}", leaf.display()))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(leaf).map_err(|error| format!("create {}: {error}", leaf.display()))
    }
}

/// Publish the authoritative level to the read-only mirror inside
/// an existing App Group container. This is distribution, not a
/// second source of truth: the app never reads the mirror, and a
/// stale or hand-edited one never changes the persisted setting.
///
/// The container must already exist — the same rule the owner's
/// socket bind follows. A hand-created container is a decoy the
/// sandboxed extension cannot reach, so creating one here would
/// report a false success. Only the `Diagnostics/` leaf is made,
/// owner-only, and the file is owner-only like settings. Returns
/// the path written for callers that need it.
pub fn publish_diagnostics_mirror(
    container: &Path,
    level: DiagnosticLevel,
) -> Result<PathBuf, String> {
    if !container.is_dir() {
        return Err(format!(
            "App Group container {} is absent; a hand-created container is a decoy the extension cannot reach",
            container.display()
        ));
    }
    let path = diagnostics_mirror_path_in(container);
    let parent = path
        .parent()
        .ok_or_else(|| format!("mirror path {} has no parent", path.display()))?;
    ensure_private_mirror_dir(parent)?;
    let document = DiagnosticsMirrorDocument {
        version: 1,
        level: level.as_str(),
    };
    let bytes = serde_json::to_vec_pretty(&document)
        .map_err(|error| format!("encode diagnostics mirror: {error}"))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("mirror path {} has no file name", path.display()))?;
    wyrd_sync::durable::atomic_write_mode(parent, &file_name.to_string_lossy(), &bytes, 0o600)
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(path)
}

/// Why an app-wide diagnostics edit did not reach both the file and
/// the mirror. The two outcomes are distinct: a committed setting
/// with a failed mirror is never reported as a clean refusal.
#[derive(Debug, thiserror::Error)]
pub enum DiagnosticsUpdateError {
    /// The settings transaction refused, conflicted, or committed
    /// unverified — nothing here touched the mirror.
    #[error(transparent)]
    Settings(#[from] SettingsWriteError),
    /// The setting committed, but the read-only mirror could not be
    /// published. The authoritative value stands; the extension
    /// keeps its last level until the mirror is refreshed.
    #[error("diagnostics setting committed but mirror not published: {0}")]
    Mirror(String),
}

/// Apply one app-wide diagnostics edit through the same transaction
/// as a per-drive sparse update, then publish the read-only mirror.
/// The closed enum is the whole validation surface: an unknown
/// level is unrepresentable here, so there is nothing to check
/// before the lock.
pub fn apply_diagnostics_update(
    settings_path: &Path,
    container: &Path,
    update: &DiagnosticsUpdate,
) -> Result<DiagnosticLevel, DiagnosticsUpdateError> {
    let _lock = lock_settings(settings_path)?;
    let mut staged = stage_file(settings_path)?;
    apply_diagnostics_to_file(&mut staged.file, update);
    let committed = commit_staged(settings_path, staged)?;
    let level = committed.diagnostics_level();
    publish_diagnostics_mirror(container, level).map_err(DiagnosticsUpdateError::Mirror)?;
    Ok(level)
}

/// Fold one validated diagnostics edit into the staged file. An
/// absent level and an explicit `default` both mean the genuine
/// absence state: the key is cleared and the section removed where
/// nothing else survives, the same rule the per-drive sections use.
/// Only `detailed` is ever materialized.
fn apply_diagnostics_to_file(file: &mut AppSettingsFile, update: &DiagnosticsUpdate) {
    match update.level {
        Some(DiagnosticLevel::Detailed) => {
            file.diagnostics
                .get_or_insert_with(DiagnosticSettingsSection::default)
                .level = Some(DiagnosticLevel::Detailed);
        }
        Some(DiagnosticLevel::Default) | None => {
            if let Some(section) = file.diagnostics.as_mut() {
                section.level = None;
                if *section == DiagnosticSettingsSection::default() {
                    file.diagnostics = None;
                }
            }
        }
    }
}

/// Read the persisted level for an owner starting up. This is the
/// authoritative value; `diagnostics-set` / `diagnostics-status`
/// only deliver live changes to a running owner, never replace the
/// persisted level. The mirror is deliberately not consulted.
pub fn diagnostics_level_at(settings_path: &Path) -> Result<DiagnosticLevel, String> {
    Ok(read_settings(settings_path)?.diagnostics_level())
}

/// Convert one drive's section into overrides. The file never
/// constructs a config: quotas become quota overrides and the core
/// applier wires the accountant, so the mismatch is
/// unrepresentable through this path.
pub fn section_to_overrides(section: &DriveSettingsSection) -> Result<BudgetOverrides, String> {
    if let Some(admit) = section.max_admit_per_pass {
        check_max_admit_per_pass(admit)?;
    }
    Ok(BudgetOverrides {
        retained_bytes_quota: section.retained_bytes_quota,
        max_admit_per_pass: section.max_admit_per_pass,
        fetch_pass_budget: section
            .fetch_pass_budget_secs
            .map(|secs| secs_to_duration("fetch_pass_budget_secs", secs))
            .transpose()?,
        max_mutation_wait: section
            .max_mutation_wait_secs
            .map(|secs| secs_to_duration("max_mutation_wait_secs", secs))
            .transpose()?,
        open_timeout: section
            .open_timeout_secs
            .map(|secs| secs_to_duration("open_timeout_secs", secs))
            .transpose()?,
    })
}

/// Overrides for one drive from an explicit settings path.
/// Missing file or missing section is empty overrides — the
/// compiled defaults — never an error.
pub fn overrides_for_path(
    settings_path: &Path,
    drive_dir: &Path,
) -> Result<BudgetOverrides, String> {
    let file = read_settings(settings_path)?;
    let key = settings_key_for(drive_dir);
    match file.drives.get(&key) {
        Some(section) => section_to_overrides(section),
        None => Ok(BudgetOverrides::default()),
    }
}

/// Overrides for one drive from the default settings path. A
/// missing file is empty overrides; an unresolvable per-user
/// directory is a loud refusal, never a cwd-relative guess.
pub fn overrides_for(drive_dir: &Path) -> Result<BudgetOverrides, String> {
    overrides_for_path(&default_settings_path()?, drive_dir)
}

/// The application's own unix-seconds: the stamp recorded beside a
/// notification key. std-only, like the storage screen's stamp; the
/// core notifications module reads no clock at all, so this daemon
/// helper is the one place the stamp is minted.
pub fn app_unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs())
        .unwrap_or(0)
}

/// Notification keys already recorded for one drive, for seeding
/// the notification center. A missing file or missing section is
/// an empty record — first run — never an error.
pub fn notified_for_path(
    settings_path: &Path,
    drive_dir: &Path,
) -> Result<BTreeMap<String, u64>, String> {
    let file = read_settings(settings_path)?;
    let key = settings_key_for(drive_dir);
    Ok(file
        .drives
        .get(&key)
        .map(|section| section.notified.clone())
        .unwrap_or_default())
}

/// Record "notified about X" for one drive: the same transaction
/// the sparse editor commits through — lock, strict read, apply,
/// commit guard — so a notification landing mid-save serializes
/// instead of interleaving. Refuses empty keys and control
/// characters — a dedup key is a slug, and an unprintable slug is a
/// bug at the call site, not data to persist.
///
/// A non-cooperating external modification between read and commit
/// is reported, never retried or merged: OD §4 leaves no room for
/// an in-transaction retry, and the next notification re-lands on
/// current bytes.
pub fn record_notified(
    settings_path: &Path,
    drive_dir: &Path,
    key: &str,
    app_unix_secs: u64,
) -> Result<(), String> {
    check_notified_key(key)?;
    let drive_key = settings_key_for(drive_dir);
    validate_drive_key(&drive_key)?;
    let _lock = lock_settings(settings_path).map_err(|error| error.to_string())?;
    let mut staged = stage_file(settings_path).map_err(|error| error.to_string())?;
    let notified = &mut staged.file.drives.entry(drive_key).or_default().notified;
    notified.insert(key.to_string(), app_unix_secs);
    // Oldest stamps evict first: dedup keeps its window without
    // growing the file past the ceiling.
    while notified.len() > MAX_NOTIFIED_PER_DRIVE {
        let oldest = notified
            .iter()
            .min_by_key(|(_, stamp)| *stamp)
            .map(|(key, _)| key.clone())
            .expect("non-empty map has an oldest entry");
        notified.remove(&oldest);
    }
    commit_staged(settings_path, staged)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt as _;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn scratch(case: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("wyrd-settings-{case}-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A scratch stand-in for the App Group container. Publish must
    /// refuse a container it did not find, so tests create this one
    /// the way the app's launch-time initialization would.
    fn scratch_container(dir: &Path) -> PathBuf {
        let container = dir.join("group");
        std::fs::create_dir_all(&container).unwrap();
        container
    }

    fn drive_key(dir: &Path) -> String {
        dir.join("drive").to_string_lossy().into_owned()
    }

    fn sparse(set: &[(&str, u64)], clear: &[&str]) -> SparseUpdate {
        SparseUpdate {
            set: set
                .iter()
                .map(|(key, value)| (key.to_string(), *value))
                .collect(),
            clear: clear.iter().map(|key| key.to_string()).collect(),
        }
    }

    fn commit_is_conflict(error: &SettingsWriteError) -> bool {
        matches!(error, SettingsWriteError::Conflict { .. })
    }

    /// Slice 3: one sparse edit sets and clears exactly one drive's
    /// bounds — other drives pass through, absent keys stay absent,
    /// and the returned section is the re-read commit, not the
    /// staged intent.
    #[test]
    fn sparse_update_sets_and_clears_one_drive() {
        let dir = scratch("sparse-apply");
        let path = dir.join("settings.json");
        let drive = dir.join("drive");
        let other = dir.join("other");

        let observed = apply_sparse_update(
            &path,
            &drive,
            &sparse(
                &[("max_admit_per_pass", 64), ("open_timeout_secs", 30)],
                &[],
            ),
        )
        .unwrap();
        assert_eq!(observed.max_admit_per_pass, Some(64));
        assert_eq!(observed.open_timeout_secs, Some(30));
        assert_eq!(observed.retained_bytes_quota, None);
        assert_eq!(observed.fetch_pass_budget_secs, None);

        // A second drive's section is added without disturbing the first.
        apply_sparse_update(&path, &other, &sparse(&[("open_timeout_secs", 31)], &[])).unwrap();
        let file = read_settings(&path).unwrap();
        assert_eq!(file.drives[&drive_key(&dir)].max_admit_per_pass, Some(64));
        assert_eq!(
            file.drives[&settings_key_for(&other)].open_timeout_secs,
            Some(31)
        );

        // Clear one bound: the key vanishes, the sibling stays, and
        // the returned section is the observed remainder.
        let observed =
            apply_sparse_update(&path, &drive, &sparse(&[], &["max_admit_per_pass"])).unwrap();
        assert_eq!(observed.max_admit_per_pass, None);
        assert_eq!(observed.open_timeout_secs, Some(30));
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("max_admit_per_pass"), "{raw}");
    }

    /// Slice 3: bootstrap is the same path, not a branch — a missing
    /// file stages empty and the commit establishes it owner-only
    /// under the lock, with a private parent.
    #[test]
    fn sparse_update_bootstraps_a_missing_file() {
        let dir = scratch("sparse-bootstrap");
        let parent = dir.join("fresh").join("state");
        let path = parent.join("settings.json");
        let drive = dir.join("drive");

        let observed =
            apply_sparse_update(&path, &drive, &sparse(&[("open_timeout_secs", 30)], &[])).unwrap();
        assert_eq!(observed.open_timeout_secs, Some(30));
        assert!(path.exists());
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(&path).unwrap().mode() & 0o777;
            assert_eq!(mode, 0o600, "new settings are owner-only, got {mode:o}");
            let parent_mode = std::fs::metadata(&parent).unwrap().mode() & 0o777;
            assert_eq!(
                parent_mode, 0o700,
                "state leaf is private, got {parent_mode:o}"
            );
        }
    }

    /// Slice 3: refused edits touch nothing — unknown set/clear
    /// keys, contradictory set+clear, and absurd values fail before
    /// the lock as `Refused` (never `Conflict`), the file bytes are
    /// identical, and no missing file is created.
    #[test]
    fn sparse_update_refusals_touch_nothing() {
        let dir = scratch("sparse-refuse");
        let path = dir.join("settings.json");
        let drive = dir.join("drive");
        apply_sparse_update(&path, &drive, &sparse(&[("open_timeout_secs", 30)], &[])).unwrap();
        let before = std::fs::read(&path).unwrap();

        for update in [
            sparse(&[("admit_all", 1)], &[]),
            sparse(&[], &["relays"]),
            sparse(&[("open_timeout_secs", 31)], &["open_timeout_secs"]),
            sparse(&[("max_admit_per_pass", 0)], &[]),
            sparse(&[("open_timeout_secs", 0)], &[]),
        ] {
            let refused = apply_sparse_update(&path, &drive, &update).unwrap_err();
            assert!(
                !commit_is_conflict(&refused),
                "refusal is not conflict: {refused}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        // Unknown keys name themselves.
        let refused =
            apply_sparse_update(&path, &drive, &sparse(&[("admit_all", 1)], &[])).unwrap_err();
        assert!(refused.to_string().contains("admit_all"), "{refused}");

        // A refused edit never bootstraps a missing file.
        let missing = dir.join("absent.json");
        let refused =
            apply_sparse_update(&missing, &drive, &sparse(&[("admit_all", 1)], &[])).unwrap_err();
        assert!(!commit_is_conflict(&refused));
        assert!(!missing.exists());
    }

    /// Slice 3, OD §1: a file this build does not understand refuses
    /// the edit with the original bytes preserved — unknown keys and
    /// newer versions are never silently dropped or downgraded.
    #[test]
    fn unreadable_file_refuses_and_preserves() {
        let dir = scratch("sparse-preserve");
        let path = dir.join("settings.json");
        let drive = dir.join("drive");
        for contents in [
            format!(
                r#"{{"version": 1, "drives": {{{:?}: {{"admit_all": 1}}}}}}"#,
                drive_key(&dir)
            ),
            r#"{"version": 999, "drives": {}}"#.to_string(),
        ] {
            std::fs::write(&path, &contents).unwrap();
            let refused =
                apply_sparse_update(&path, &drive, &sparse(&[("open_timeout_secs", 30)], &[]))
                    .unwrap_err();
            assert!(!commit_is_conflict(&refused));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
        }
    }

    /// Slice 3, OD §6: clearing the last bound removes the section —
    /// safe exactly when no notification records survive — while a
    /// section with records keeps them beside the cleared bounds.
    #[test]
    fn clearing_the_last_bound_removes_the_section_where_safe() {
        let dir = scratch("sparse-clear");
        let path = dir.join("settings.json");
        let drive = dir.join("drive");

        apply_sparse_update(&path, &drive, &sparse(&[("open_timeout_secs", 30)], &[])).unwrap();
        let observed =
            apply_sparse_update(&path, &drive, &sparse(&[], &["open_timeout_secs"])).unwrap();
        assert_eq!(observed, DriveSettingsSection::default());
        let file = read_settings(&path).unwrap();
        assert!(
            !file.drives.contains_key(&drive_key(&dir)),
            "emptied section is removed"
        );

        // Records survive the clearing: the section stays for them.
        record_notified(&path, &drive, "subscription-closed:1", 1_700_000_001).unwrap();
        apply_sparse_update(&path, &drive, &sparse(&[("open_timeout_secs", 30)], &[])).unwrap();
        let observed =
            apply_sparse_update(&path, &drive, &sparse(&[], &["open_timeout_secs"])).unwrap();
        assert_eq!(observed.open_timeout_secs, None);
        assert_eq!(
            observed.notified.get("subscription-closed:1"),
            Some(&1_700_000_001)
        );
        let file = read_settings(&path).unwrap();
        assert!(file.drives.contains_key(&drive_key(&dir)));
    }

    /// Slice 3: notification landings and sparse saves share the
    /// transaction, so neither loses the other's fields — bounds
    /// beside records, records beside bounds, one observed file.
    #[test]
    fn notified_records_survive_a_sparse_save() {
        let dir = scratch("sparse-notified");
        let path = dir.join("settings.json");
        let drive = dir.join("drive");

        record_notified(&path, &drive, "subscription-closed:1", 1_700_000_001).unwrap();
        let observed = apply_sparse_update(
            &path,
            &drive,
            &sparse(&[("retained_bytes_quota", 4096)], &[]),
        )
        .unwrap();
        assert_eq!(observed.retained_bytes_quota, Some(4096));
        assert_eq!(
            observed.notified.get("subscription-closed:1"),
            Some(&1_700_000_001)
        );
        // The eviction ceiling still applies when the writer runs.
        assert!(observed.notified.len() <= MAX_NOTIFIED_PER_DRIVE);
    }

    /// Slice 3, OD §4.6: a non-cooperating writer that replaces the
    /// file between the transaction's read and commit is a distinct
    /// `Conflict` — the staged intent never overwrites it. Staged
    /// here directly so the test, not timing, provides the
    /// interleaving.
    #[test]
    fn commit_detects_an_external_rewrite() {
        let dir = scratch("sparse-conflict");
        let path = dir.join("settings.json");
        let drive = dir.join("drive");
        apply_sparse_update(&path, &drive, &sparse(&[("open_timeout_secs", 30)], &[])).unwrap();

        let _lock = lock_settings(&path).unwrap();
        let mut staged = stage_file(&path).unwrap();
        apply_update_to_file(
            &path,
            &mut staged.file,
            &drive_key(&dir),
            &sparse(&[("open_timeout_secs", 31)], &[]),
        )
        .unwrap();
        // A lock-bypassing writer commits first: same path, new bytes.
        let external = AppSettingsFile {
            version: APP_SETTINGS_VERSION,
            drives: {
                let mut drives = BTreeMap::new();
                drives.insert(drive_key(&dir), DriveSettingsSection::default());
                drives
            },
            diagnostics: None,
        };
        std::fs::write(&path, serde_json::to_vec_pretty(&external).unwrap()).unwrap();

        let conflict = commit_staged(&path, staged).unwrap_err();
        assert!(commit_is_conflict(&conflict), "{conflict}");
        assert!(conflict.to_string().contains("changed under"), "{conflict}");
        // The external commit stands; the staged intent never landed.
        assert_eq!(read_settings(&path).unwrap(), external);
    }

    /// OD §6, never conflated: when the commit lands but the
    /// verification re-read fails, the transaction reports
    /// committed-but-unverified — not a refusal of a write that
    /// already changed the file. Deterministic by splitting the
    /// guarded write from the verification: vanish the file between
    /// the two.
    #[test]
    fn failed_verification_is_committed_not_refused() {
        let dir = scratch("sparse-unverified");
        let path = dir.join("settings.json");
        let drive = dir.join("drive");
        apply_sparse_update(&path, &drive, &sparse(&[("open_timeout_secs", 30)], &[])).unwrap();

        let _lock = lock_settings(&path).unwrap();
        let staged = stage_file(&path).unwrap();
        write_guarded(&path, staged).unwrap();
        // Vanish the commit's bytes behind garbage: the verification
        // re-read through the normal reader must fail (a missing file
        // would read as empty settings, not an error).
        std::fs::write(&path, b"{invalid json").unwrap();

        let outcome = verify_commit(&path).unwrap_err();
        assert!(
            matches!(outcome, SettingsWriteError::CommittedUnverified { .. }),
            "{outcome}"
        );
        assert!(!matches!(outcome, SettingsWriteError::Refused { .. }));
        assert!(!matches!(outcome, SettingsWriteError::Conflict { .. }));
    }

    /// Slice 3, OD §4 stress: concurrent notification landings
    /// during saves converge — every key and bound survives, the
    /// file always parses, and no thread sees a conflict from a
    /// cooperating writer.
    #[test]
    fn concurrent_saves_and_notifications_converge() {
        let dir = scratch("sparse-stress");
        let path = dir.join("settings.json");
        let drive = dir.join("drive");
        apply_sparse_update(&path, &drive, &sparse(&[("open_timeout_secs", 30)], &[])).unwrap();

        std::thread::scope(|scope| {
            for worker in 0..4 {
                let (path, drive) = (path.clone(), drive.clone());
                scope.spawn(move || {
                    for round in 0..10 {
                        record_notified(
                            &path,
                            &drive,
                            &format!("worker-{worker}-round-{round}"),
                            1_700_000_000 + round as u64,
                        )
                        .unwrap();
                    }
                });
            }
            for (key, value) in [("max_admit_per_pass", 64), ("open_timeout_secs", 31)] {
                let (path, drive) = (path.clone(), drive.clone());
                scope.spawn(move || {
                    for _ in 0..10 {
                        apply_sparse_update(&path, &drive, &sparse(&[(key, value)], &[])).unwrap();
                    }
                });
            }
        });

        let file = read_settings(&path).unwrap();
        let section = &file.drives[&drive_key(&dir)];
        for worker in 0..4 {
            for round in 0..10 {
                assert!(
                    section
                        .notified
                        .contains_key(&format!("worker-{worker}-round-{round}")),
                    "no landed notification is lost"
                );
            }
        }
        assert!(section.notified.len() <= MAX_NOTIFIED_PER_DRIVE);
        // Both bounds converged; the section round-trips the reader.
        assert_eq!(section.max_admit_per_pass, Some(64));
        assert_eq!(section.open_timeout_secs, Some(31));
    }

    /// Slice 3, OD §5: the commit corrects an over-permissive file
    /// under the lock and keeps a private parent — 0600 and 0700
    /// after every save, not only on bootstrap.
    #[cfg(unix)]
    #[test]
    fn commit_corrects_permissions_under_the_lock() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("sparse-perms");
        let parent = dir.join("loose");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = parent.join("settings.json");
        let drive = dir.join("drive");

        std::fs::write(&path, r#"{"version": 1, "drives": {}}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        apply_sparse_update(&path, &drive, &sparse(&[("open_timeout_secs", 30)], &[])).unwrap();
        let mode = std::fs::metadata(&path).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600, "commit corrects the file, got {mode:o}");
        let parent_mode = std::fs::metadata(&parent).unwrap().mode() & 0o777;
        assert_eq!(
            parent_mode, 0o700,
            "commit tightens the leaf, got {parent_mode:o}"
        );
    }

    fn file_with(key: &str, section: DriveSettingsSection) -> AppSettingsFile {
        let mut drives = BTreeMap::new();
        drives.insert(key.to_string(), section);
        AppSettingsFile {
            version: APP_SETTINGS_VERSION,
            drives,
            diagnostics: None,
        }
    }

    #[test]
    fn a_missing_file_is_empty_settings() {
        let dir = scratch("missing");
        let read = read_settings(&dir.join("absent.json")).unwrap();
        assert_eq!(read.version, APP_SETTINGS_VERSION);
        assert!(read.drives.is_empty());
        let overrides = overrides_for_path(&dir.join("absent.json"), Path::new("/drive")).unwrap();
        assert!(overrides.is_empty());
    }

    #[test]
    fn the_file_round_trips_and_refuses_a_newer_schema() {
        let dir = scratch("schema");
        let path = dir.join("settings.json");
        let section = DriveSettingsSection {
            retained_bytes_quota: Some(4096),
            max_admit_per_pass: Some(17),
            fetch_pass_budget_secs: Some(11),
            max_mutation_wait_secs: Some(31),
            open_timeout_secs: Some(32),
            notified: BTreeMap::new(),
        };
        write_settings(&path, &file_with(&drive_key(&dir), section.clone())).unwrap();
        let read = read_settings(&path).unwrap();
        assert_eq!(read.drives[&drive_key(&dir)], section);
        let overrides = overrides_for_path(&path, &dir.join("drive")).unwrap();
        assert_eq!(overrides.retained_bytes_quota, Some(4096));
        assert_eq!(overrides.max_admit_per_pass, Some(17));
        assert_eq!(overrides.fetch_pass_budget, Some(Duration::from_secs(11)));
        assert_eq!(overrides.max_mutation_wait, Some(Duration::from_secs(31)));
        assert_eq!(overrides.open_timeout, Some(Duration::from_secs(32)));

        std::fs::write(&path, r#"{"version": 999, "drives": {}}"#).unwrap();
        let refused = read_settings(&path).unwrap_err();
        assert!(refused.contains("version 999"), "{refused}");
    }

    #[test]
    fn an_unknown_key_is_refused_with_its_name() {
        let dir = scratch("unknown");
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"version": 1, "drives": {{{:?}: {{"admit_all": 1}}}}}}"#,
                drive_key(&dir)
            ),
        )
        .unwrap();
        let refused = read_settings(&path).unwrap_err();
        assert!(refused.contains("admit_all"), "{refused}");
    }

    #[test]
    fn an_unknown_top_level_key_is_refused_with_its_name() {
        let dir = scratch("unknown-top");
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"version": 1, "drives": {}, "setting": 1}"#).unwrap();
        let refused = read_settings(&path).unwrap_err();
        assert!(refused.contains("setting"), "{refused}");
    }

    #[test]
    fn the_per_user_path_fails_loudly_with_no_home_and_no_xdg() {
        let refused = resolve_settings_path(None, None).unwrap_err();
        assert!(
            refused.contains("HOME") && refused.contains("XDG_STATE_HOME"),
            "{refused}"
        );
        // Empty strings are unset, not directories.
        let refused = resolve_settings_path(Some(""), Some("")).unwrap_err();
        assert!(refused.contains("HOME"), "{refused}");
        // A set HOME resolves per-user, never cwd-relative.
        let resolved = resolve_settings_path(Some("/home/op"), None).unwrap();
        #[cfg(target_os = "macos")]
        assert_eq!(
            resolved,
            PathBuf::from("/home/op")
                .join("Library/Application Support/Wyrd")
                .join(SETTINGS_FILENAME)
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            resolved,
            PathBuf::from("/home/op")
                .join(".local/state/wyrd")
                .join(SETTINGS_FILENAME)
        );
        let resolved = resolve_settings_path(Some("/home/op"), Some("/run/user/1000")).unwrap();
        // XDG wins over the `~/.local/state` fallback everywhere; on
        // macOS a set HOME takes the Application Support path first,
        // like the drive registry.
        #[cfg(target_os = "macos")]
        assert_eq!(
            resolved,
            PathBuf::from("/home/op")
                .join("Library/Application Support/Wyrd")
                .join(SETTINGS_FILENAME)
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            resolved,
            PathBuf::from("/run/user/1000")
                .join("wyrd")
                .join(SETTINGS_FILENAME)
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            resolve_settings_path(None, Some("/run/user/1000")).unwrap(),
            PathBuf::from("/run/user/1000")
                .join("wyrd")
                .join(SETTINGS_FILENAME)
        );
    }

    #[test]
    fn a_relative_drive_key_is_refused_with_its_name() {
        let dir = scratch("relative");
        let path = dir.join("settings.json");
        let settings = file_with("relative/drive", DriveSettingsSection::default());
        let refused = write_settings(&path, &settings).unwrap_err();
        assert!(refused.contains("relative/drive"), "{refused}");
        assert!(!path.exists(), "no partial settings may remain");
    }

    #[test]
    fn absurd_values_refuse_before_anything_applies() {
        let zero_admit = section_to_overrides(&DriveSettingsSection {
            max_admit_per_pass: Some(0),
            ..DriveSettingsSection::default()
        })
        .unwrap_err();
        assert!(zero_admit.contains("max_admit_per_pass"), "{zero_admit}");
        let zero_secs = section_to_overrides(&DriveSettingsSection {
            open_timeout_secs: Some(0),
            ..DriveSettingsSection::default()
        })
        .unwrap_err();
        assert!(zero_secs.contains("open_timeout_secs"), "{zero_secs}");
    }

    #[test]
    fn relative_drive_dirs_resolve_against_the_current_directory() {
        let cwd = std::env::current_dir().unwrap();
        let abs = cwd.join("some-drive");
        assert_eq!(settings_key_for(&abs), abs.to_string_lossy());
        assert_eq!(
            settings_key_for(Path::new("some-drive")),
            cwd.join("some-drive").to_string_lossy()
        );
    }

    /// OD-26-C as an invariant: the "notified about X" record
    /// round-trips beside the budget section, seeds the center, and
    /// an absent record is first run, never an error. The stamps
    /// are fixed fixtures, not clock reads — the record holds the
    /// application's time, and the test holds it still.
    #[test]
    fn notified_records_round_trip_and_seed_the_center() {
        let dir = scratch("notified");
        let path = dir.join("settings.json");
        let drive = dir.join("drive");

        assert!(notified_for_path(&path, &drive).unwrap().is_empty());

        record_notified(&path, &drive, "subscription-closed:1", 1_700_000_001).unwrap();
        record_notified(&path, &drive, "epoch-frozen:5", 1_700_000_002).unwrap();

        let seeded = notified_for_path(&path, &drive).unwrap();
        assert_eq!(seeded.len(), 2);
        assert_eq!(seeded["subscription-closed:1"], 1_700_000_001);
        assert_eq!(seeded["epoch-frozen:5"], 1_700_000_002);

        // The budget section beside the records still converts, and
        // the raw file carries the records under their own key.
        let file = read_settings(&path).unwrap();
        let key = settings_key_for(&drive);
        assert_eq!(file.drives[&key].notified, seeded);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"notified\""), "{raw}");
        assert!(raw.contains("subscription-closed:1"), "{raw}");

        // A second drive's record does not disturb the first.
        let other = dir.join("other");
        record_notified(&path, &other, "drive-open-refused", 1_700_000_003).unwrap();
        assert_eq!(notified_for_path(&path, &drive).unwrap(), seeded);
        assert_eq!(
            notified_for_path(&path, &other).unwrap()["drive-open-refused"],
            1_700_000_003
        );

        // The stamp helper mints a plausible unix time.
        assert!(app_unix_now() > 1_700_000_000);
    }

    /// Dedup keys are slugs: empty and control-character keys
    /// refuse loudly instead of persisting a bug.
    #[test]
    fn notified_keys_refuse_empties_and_control_characters() {
        let dir = scratch("notified-keys");
        let path = dir.join("settings.json");
        let drive = dir.join("drive");
        let refused = record_notified(&path, &drive, "", 1_700_000_001).unwrap_err();
        assert!(refused.contains("must not be empty"), "{refused}");
        let refused =
            record_notified(&path, &drive, "epoch-frozen:\u{7}", 1_700_000_001).unwrap_err();
        assert!(refused.contains("control characters"), "{refused}");
        assert!(!path.exists(), "no partial settings may remain");
    }

    /// The record is a bounded dedup window, not history: past the
    /// ceiling the oldest stamps evict and the newest stay.
    #[test]
    fn notified_records_evict_oldest_past_the_ceiling() {
        let dir = scratch("notified-cap");
        let path = dir.join("settings.json");
        let drive = dir.join("drive");
        for index in 0..(MAX_NOTIFIED_PER_DRIVE + 5) {
            record_notified(
                &path,
                &drive,
                &format!("key-{index:04}"),
                1_700_000_000 + index as u64,
            )
            .unwrap();
        }
        let seeded = notified_for_path(&path, &drive).unwrap();
        assert_eq!(seeded.len(), MAX_NOTIFIED_PER_DRIVE);
        assert!(!seeded.contains_key("key-0000"), "oldest evicts first");
        assert!(seeded.contains_key(&format!("key-{:04}", MAX_NOTIFIED_PER_DRIVE + 4)));
    }

    /// OD-25-A as an invariant: writing, changing, and rewriting
    /// settings never touches the drive directory — not one file
    /// added, removed, or resized. The snapshot covers the whole
    /// tree (names plus byte lengths), and the settings land on a
    /// separate path the whole time.
    #[test]
    fn no_setting_is_written_into_a_drive_directory() {
        fn snapshot(drive: &Path) -> BTreeMap<PathBuf, u64> {
            let mut files = BTreeMap::new();
            let mut stack = vec![drive.to_path_buf()];
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(&dir).unwrap() {
                    let entry = entry.unwrap();
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else {
                        files.insert(
                            path.strip_prefix(drive).unwrap().to_path_buf(),
                            entry.metadata().unwrap().len(),
                        );
                    }
                }
            }
            files
        }

        let dir = scratch("nodrivewrites");
        let drive = dir.join("drive");
        std::fs::create_dir_all(drive.join("commits")).unwrap();
        std::fs::write(drive.join("DRIVE"), b"drive-record").unwrap();
        std::fs::write(drive.join("LOCK"), b"lock").unwrap();
        std::fs::write(drive.join("commits").join("head"), b"commit-bytes").unwrap();
        let before = snapshot(&drive);

        let settings_path = dir.join("user-state").join("settings.json");
        write_settings(
            &settings_path,
            &file_with(
                &drive_key(&dir),
                DriveSettingsSection {
                    retained_bytes_quota: Some(4096),
                    ..DriveSettingsSection::default()
                },
            ),
        )
        .unwrap();
        // Change a setting and rewrite: the drive directory must
        // not gain, lose, or resize a byte.
        let mut settings = read_settings(&settings_path).unwrap();
        settings
            .drives
            .get_mut(&drive_key(&dir))
            .expect("section present")
            .max_admit_per_pass = Some(17);
        write_settings(&settings_path, &settings).unwrap();

        assert_eq!(snapshot(&drive), before);
        assert!(settings_path.exists());
    }

    /// A v1 file reads as v2 with no diagnostics section: absent
    /// means `default`, the original bytes are not silently
    /// rewritten on read, and the next write persists v2.
    #[test]
    fn v1_file_migrates_in_memory_and_reports_default() {
        let dir = scratch("v1-migrate");
        let path = dir.join("settings.json");
        let mut drives = serde_json::Map::new();
        drives.insert(
            drive_key(&dir),
            serde_json::json!({"open_timeout_secs": 30}),
        );
        let v1 = serde_json::json!({"version": 1, "drives": drives});
        let bytes = serde_json::to_vec_pretty(&v1).unwrap();
        std::fs::write(&path, &bytes).unwrap();

        let read = read_settings(&path).unwrap();
        assert_eq!(read.version, 2);
        assert!(read.diagnostics.is_none());
        assert_eq!(read.diagnostics_level(), DiagnosticLevel::Default);
        assert_eq!(read.drives[&drive_key(&dir)].open_timeout_secs, Some(30));
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "read never rewrites");
        assert_eq!(
            diagnostics_level_at(&path).unwrap(),
            DiagnosticLevel::Default
        );

        let container = scratch_container(&dir);
        apply_diagnostics_update(
            &path,
            &container,
            &DiagnosticsUpdate {
                level: Some(DiagnosticLevel::Detailed),
            },
        )
        .unwrap();
        let read = read_settings(&path).unwrap();
        assert_eq!(read.version, 2, "the next write persists v2");
        assert_eq!(read.diagnostics_level(), DiagnosticLevel::Detailed);
        assert_eq!(read.drives[&drive_key(&dir)].open_timeout_secs, Some(30));
    }

    /// The restricted writer sets, publishes and clears the level
    /// through the same transaction: other drives survive, the
    /// mirror is exact and owner-only, and clearing removes the
    /// section rather than materializing a default.
    #[test]
    fn diagnostics_update_sets_publishes_and_clears() {
        let dir = scratch("diagnostics-set");
        let path = dir.join("settings.json");
        let other = dir.join("other");
        apply_sparse_update(&path, &other, &sparse(&[("open_timeout_secs", 31)], &[])).unwrap();
        let container = scratch_container(&dir);
        let mirror = diagnostics_mirror_path_in(&container);

        let level = apply_diagnostics_update(
            &path,
            &container,
            &DiagnosticsUpdate {
                level: Some(DiagnosticLevel::Detailed),
            },
        )
        .unwrap();
        assert_eq!(level, DiagnosticLevel::Detailed);
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&mirror).unwrap()).unwrap();
        assert_eq!(raw["version"], 1);
        assert_eq!(raw["level"], "detailed");
        assert_eq!(
            raw.as_object().unwrap().len(),
            2,
            "the mirror carries only two keys"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let mode = std::fs::metadata(&mirror).unwrap().mode() & 0o777;
            assert_eq!(mode, 0o600, "the mirror is owner-only, got {mode:o}");
        }

        let file = read_settings(&path).unwrap();
        assert_eq!(file.diagnostics_level(), DiagnosticLevel::Detailed);
        assert_eq!(
            file.drives[&settings_key_for(&other)].open_timeout_secs,
            Some(31),
            "unrelated drives pass through"
        );

        // Clearing returns to genuine absence; the section vanishes.
        let level =
            apply_diagnostics_update(&path, &container, &DiagnosticsUpdate::default()).unwrap();
        assert_eq!(level, DiagnosticLevel::Default);
        let file = read_settings(&path).unwrap();
        assert!(file.diagnostics.is_none());
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&mirror).unwrap()).unwrap();
        assert_eq!(raw["level"], "default");
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(!persisted.as_object().unwrap().contains_key("diagnostics"));
    }

    /// Refusals touch nothing, and a mirror failure is distinct
    /// from a settings refusal: the authoritative value has already
    /// committed in that case.
    #[test]
    fn diagnostics_refusals_and_mirror_failure_are_distinct() {
        let dir = scratch("diagnostics-refuse");
        let path = dir.join("settings.json");
        let container = scratch_container(&dir);
        let mirror = diagnostics_mirror_path_in(&container);
        let detailed = DiagnosticsUpdate {
            level: Some(DiagnosticLevel::Detailed),
        };
        for contents in [
            r#"{"version":2,"drives":{},"unknown":1}"#.to_string(),
            r#"{"version":2,"drives":{},"diagnostics":{"level":"verbose"}}"#.to_string(),
            r#"{"version":999,"drives":{}}"#.to_string(),
        ] {
            std::fs::write(&path, &contents).unwrap();
            let refused = apply_diagnostics_update(&path, &container, &detailed).unwrap_err();
            assert!(
                matches!(refused, DiagnosticsUpdateError::Settings(_)),
                "refusal is a settings failure, not a mirror failure: {refused}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
            assert!(!mirror.exists(), "a refused edit never writes the mirror");
        }

        // An unwritable mirror reports the committed-but-unpublished
        // outcome with the value still authoritative in settings.
        std::fs::write(&path, r#"{"version":2,"drives":{}}"#).unwrap();
        let blocked = dir.join("blocked");
        std::fs::write(&blocked, b"a file, not a container directory").unwrap();
        let failed = apply_diagnostics_update(&path, &blocked, &detailed).unwrap_err();
        assert!(
            matches!(failed, DiagnosticsUpdateError::Mirror(_)),
            "mirror failure is distinct: {failed}"
        );
        assert_eq!(
            diagnostics_level_at(&path).unwrap(),
            DiagnosticLevel::Detailed,
            "the committed setting stands"
        );
    }

    /// Writing the explicit `default` level collapses to genuine
    /// absence: no diagnostics key survives, while the mirror
    /// still publishes the level the extension should read.
    #[test]
    fn explicit_default_collapses_to_absence() {
        let dir = scratch("diagnostics-explicit-default");
        let path = dir.join("settings.json");
        let container = scratch_container(&dir);
        let mirror = diagnostics_mirror_path_in(&container);
        let set = DiagnosticsUpdate {
            level: Some(DiagnosticLevel::Detailed),
        };
        apply_diagnostics_update(&path, &container, &set).unwrap();
        let level = apply_diagnostics_update(
            &path,
            &container,
            &DiagnosticsUpdate {
                level: Some(DiagnosticLevel::Default),
            },
        )
        .unwrap();
        assert_eq!(level, DiagnosticLevel::Default);
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(!persisted.as_object().unwrap().contains_key("diagnostics"));
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&mirror).unwrap()).unwrap();
        assert_eq!(raw["level"], "default");
    }

    /// The mirror distributes; it is never a second authority. A
    /// stale or hand-edited detailed mirror cannot change what the
    /// persisted file says.
    #[test]
    fn the_mirror_is_never_consulted_as_a_second_authority() {
        let dir = scratch("mirror-not-authority");
        let path = dir.join("settings.json");
        let container = scratch_container(&dir);
        std::fs::write(&path, r#"{"version":2,"drives":{}}"#).unwrap();
        publish_diagnostics_mirror(&container, DiagnosticLevel::Detailed).unwrap();
        assert_eq!(
            diagnostics_level_at(&path).unwrap(),
            DiagnosticLevel::Default,
            "the persisted file wins over the mirror"
        );
        let read = read_settings(&path).unwrap();
        assert!(read.diagnostics.is_none());
    }

    /// The container is derived from the effective App Group id,
    /// matching the extension's rendered value.
    #[test]
    fn mirror_path_uses_the_app_group_literally() {
        let container = diagnostics_mirror_container_for(Some("/home/op")).unwrap();
        assert_eq!(
            container,
            PathBuf::from("/home/op")
                .join("Library/Group Containers")
                .join(app_group_identifier())
        );
        assert_eq!(
            diagnostics_mirror_path_in(&container),
            container.join("Diagnostics/level.json")
        );
        assert!(diagnostics_mirror_container_for(None).is_err());
        assert!(diagnostics_mirror_container_for(Some("")).is_err());
    }

    /// A missing container is never created: a hand-made container
    /// is a decoy the sandboxed extension cannot reach, so the
    /// publish fails as committed-but-unpublished and leaves no
    /// directory behind.
    #[test]
    fn a_missing_container_is_never_created() {
        let dir = scratch("mirror-decoy");
        let path = dir.join("settings.json");
        let missing = dir.join("Group Containers").join(app_group_identifier());
        let detailed = DiagnosticsUpdate {
            level: Some(DiagnosticLevel::Detailed),
        };
        let failed = apply_diagnostics_update(&path, &missing, &detailed).unwrap_err();
        assert!(
            matches!(failed, DiagnosticsUpdateError::Mirror(_)),
            "missing container is a mirror failure: {failed}"
        );
        assert!(failed.to_string().contains("decoy"), "{failed}");
        assert!(!missing.exists(), "no decoy container was created");
        assert_eq!(
            diagnostics_level_at(&path).unwrap(),
            DiagnosticLevel::Detailed,
            "the committed setting stands"
        );
    }

    /// The mirror's own leaf is private: created 0700, tightened when
    /// it predates the rule, with the file itself 0600.
    #[cfg(unix)]
    #[test]
    fn mirror_leaf_is_private() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = scratch("mirror-leaf");
        let container = scratch_container(&dir);
        let leaf = container.join("Diagnostics");
        std::fs::create_dir_all(&leaf).unwrap();
        std::fs::set_permissions(&leaf, std::fs::Permissions::from_mode(0o755)).unwrap();

        publish_diagnostics_mirror(&container, DiagnosticLevel::Detailed).unwrap();
        let mode = std::fs::metadata(&leaf).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o700, "the mirror leaf is private, got {mode:o}");
        let mirror = diagnostics_mirror_path_in(&container);
        let file_mode = std::fs::metadata(&mirror).unwrap().mode() & 0o777;
        assert_eq!(
            file_mode, 0o600,
            "the mirror file is owner-only, got {file_mode:o}"
        );
    }
}
