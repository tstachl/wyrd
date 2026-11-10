//! Application settings file: the per-user budgets a person configures
//! outside any drive directory (OD-25-A, recommendation A).
//!
//! One JSON file per user holds per-drive sections keyed by absolute
//! drive directory: `{"version": 1, "drives": {"<abs path>":
//! {budget keys...}}}`. It is authoritative local state that derives
//! from nothing durable — not signed archive state, not a format a
//! peer reads — so it lives outside the drive directory (which is
//! Wyrd-owned state under `docs/contracts/upgrade-contract.md`) and
//! outside invariant 2's format list; the contract note names the
//! fourth category. Unknown keys are refused with their name (a
//! typo'd key silently discarded is the failure mode that makes
//! configuration files untrustworthy), and a newer schema version
//! refuses loudly rather than guessing.
//!
//! The file holds only the five bounds the surface exposes
//! (`retained_bytes_quota`, `max_admit_per_pass`,
//! `fetch_pass_budget_secs`, `max_mutation_wait_secs`,
//! `open_timeout_secs`); relays are read-only on the surface and
//! live in the drive deployment configuration, never here. Values
//! convert into [`BudgetOverrides`](wyrd_core::settings::BudgetOverrides)
//! — the file never constructs a config itself, so it cannot build
//! the quota-without-accountant mismatch — and command-line flags
//! win over the file, which wins over compiled defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wyrd_core::settings::BudgetOverrides;

/// Schema version of the application settings file. Bump only with
/// a migration that reads the previous version; readers refuse
/// newer versions loudly rather than guessing.
pub const APP_SETTINGS_VERSION: u32 = 1;

/// Filename of the application settings file inside the per-user
/// state directory.
pub const SETTINGS_FILENAME: &str = "settings.json";

/// One drive's settings section: the five surfaced bounds, all
/// optional — absent keys leave the compiled defaults in place.
/// Durations are whole seconds on the wire; sub-second tuning is
/// not a person-scale setting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriveSettingsSection {
    /// Ceiling on retained object bytes (`None` — absent — is
    /// unlimited). Wires its accountant when applied, always.
    #[serde(default)]
    pub retained_bytes_quota: Option<u64>,
    /// Wants admitted per sync pass.
    #[serde(default)]
    pub max_admit_per_pass: Option<usize>,
    /// Wall-clock budget for one pass's fetch phase, in seconds.
    #[serde(default)]
    pub fetch_pass_budget_secs: Option<u64>,
    /// Wall-clock deadline for a held mutation, in seconds.
    #[serde(default)]
    pub max_mutation_wait_secs: Option<u64>,
    /// Wall-clock budget for one `open` demand wait, in seconds.
    #[serde(default)]
    pub open_timeout_secs: Option<u64>,
}

/// The whole file: one version plus per-drive sections keyed by
/// absolute drive directory. Sections for drives that do not exist
/// are inert data, never an error — a setting outlives a deleted
/// drive the way a preference outlives an uninstalled drive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettingsFile {
    pub version: u32,
    pub drives: BTreeMap<String, DriveSettingsSection>,
}

impl Default for AppSettingsFile {
    fn default() -> Self {
        AppSettingsFile {
            version: APP_SETTINGS_VERSION,
            drives: BTreeMap::new(),
        }
    }
}

/// Where the settings file lives when no explicit path is given:
/// per-user state, resolved through the same platform conventions
/// as the drive registry. The writer creates it with owner-only
/// permissions; registration and settings flows are its normal
/// writers, never hand-editing.
pub fn default_settings_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library/Application Support/Wyrd")
                .join(SETTINGS_FILENAME);
        }
    }
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("wyrd").join(SETTINGS_FILENAME);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local/state/wyrd")
            .join(SETTINGS_FILENAME);
    }
    PathBuf::from(SETTINGS_FILENAME)
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
/// The lookup key for one drive's section: the absolute drive
/// directory as a string. Relative arguments resolve against the
/// current directory so the same drive finds its section however
/// it was named on the command line.
pub fn settings_key_for(drive_dir: &Path) -> String {
    if drive_dir.is_absolute() {
        drive_dir.to_string_lossy().into_owned()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
            .join(drive_dir)
            .to_string_lossy()
            .into_owned()
    }
}

fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("settings drive key must not be empty".to_string());
    }
    let path = Path::new(key);
    if !path.is_absolute() {
        return Err(format!("settings drive key {key:?} must be absolute"));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("settings drive key {key:?} must not contain .."));
    }
    if key.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
        return Err(format!(
            "settings drive key {key:?} must not contain control characters"
        ));
    }
    Ok(())
}

/// Read and validate the settings file. A missing file is empty
/// settings, not an error — a fresh installation has configured
/// nothing yet. A newer schema version refuses loudly. Stored keys
/// that are not absolute fail closed with their name.
pub fn read_settings(path: &Path) -> Result<AppSettingsFile, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AppSettingsFile::default());
        }
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let file: AppSettingsFile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    if file.version != APP_SETTINGS_VERSION {
        return Err(format!(
            "settings {} is version {}, this build reads {}",
            path.display(),
            file.version,
            APP_SETTINGS_VERSION
        ));
    }
    for key in file.drives.keys() {
        validate_key(key)?;
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
        validate_key(key).map_err(|reason| format!("refusing to write: {reason}"))?;
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
    if secs == 0 {
        return Err(format!(
            "settings {name} must be a positive number of seconds"
        ));
    }
    if secs > 86_400 {
        return Err(format!("settings {name} exceeds 86400s (got {secs}s)"));
    }
    Ok(Duration::from_secs(secs))
}

/// Convert one drive's section into overrides. The file never
/// constructs a config: quotas become quota overrides and the core
/// applier wires the accountant, so the mismatch is
/// unrepresentable through this path.
pub fn section_to_overrides(section: &DriveSettingsSection) -> Result<BudgetOverrides, String> {
    if let Some(admit) = section.max_admit_per_pass {
        if admit == 0 || admit > 1_000_000 {
            return Err(format!(
                "settings max_admit_per_pass must be 1..=1000000 (got {admit})"
            ));
        }
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

/// Overrides for one drive from the default settings path.
pub fn overrides_for(drive_dir: &Path) -> Result<BudgetOverrides, String> {
    overrides_for_path(&default_settings_path(), drive_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn scratch(case: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("wyrd-settings-{case}-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn drive_key(dir: &Path) -> String {
        dir.join("drive").to_string_lossy().into_owned()
    }

    fn file_with(key: &str, section: DriveSettingsSection) -> AppSettingsFile {
        let mut drives = BTreeMap::new();
        drives.insert(key.to_string(), section);
        AppSettingsFile {
            version: APP_SETTINGS_VERSION,
            drives,
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
}
