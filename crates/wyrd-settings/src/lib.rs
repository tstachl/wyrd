//! Per-user application settings file schema: the budgets a
//! person configures outside any drive directory, one JSON file
//! with per-drive sections keyed by absolute drive directory:
//! `{"version": 1, "drives": {"<abs path>": {budget keys...}}}`.
//!
//! Serialization and validation rules only — no filesystem
//! access, no locking, no atomic writes, no path resolution, no
//! daemon runtime application. The daemon (`wyrd-daemon`) remains
//! the runtime consumer (reading, writing, applying); the desktop
//! (`wyrd-desktop`) is the second consumer of the same contract
//! (reading for display; writing only through the stage-2 sparse
//! writer). One implementation so the two readers can never drift.
//!
//! Unknown keys are refused with their name (a typo'd key silently
//! discarded is the failure mode that makes configuration files
//! untrustworthy), and a newer schema version refuses loudly
//! rather than guessing. Absent keys leave the compiled defaults
//! in place — the schema never materializes defaults.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Schema version of the application settings file. Bump only with
/// a migration that reads the previous version; readers refuse
/// newer versions loudly rather than guessing.
pub const APP_SETTINGS_VERSION: u32 = 1;

/// Filename of the application settings file inside the per-user
/// state directory.
pub const SETTINGS_FILENAME: &str = "settings.json";

/// Ceiling on notification records per drive section. Dedup needs
/// recent keys, not history: a drive notifies a handful of
/// conditions per run, and 128 is headroom, not a target.
pub const MAX_NOTIFIED_PER_DRIVE: usize = 128;

/// One drive's settings section: the five surfaced bounds, all
/// optional — absent keys leave the compiled defaults in place —
/// plus the notification records (OD-26-C, recommendation A).
/// Durations are whole seconds on the wire; sub-second tuning is
/// not a person-scale setting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriveSettingsSection {
    /// Ceiling on retained object bytes (`None` — absent — is
    /// unlimited). Zero is allowed and means what it says: every
    /// write refuses.
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
    /// Notification dedup records: notification key to the
    /// application's own unix-seconds at recording. Absent on first
    /// run; additive under schema version 1.
    #[serde(default)]
    pub notified: BTreeMap<String, u64>,
}

/// The whole file: one version plus per-drive sections keyed by
/// absolute drive directory. Unknown top-level keys are refused
/// with their name, like unknown section keys. Sections for drives
/// that do not exist are inert data, never an error — a setting
/// outlives a deleted drive the way a preference outlives an
/// uninstalled drive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSettingsFile {
    /// Settings schema version.
    pub version: u32,
    /// Per-drive sections keyed by absolute drive directory.
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

/// The lookup key for one drive's section: the absolute drive
/// directory as an exact string. Relative arguments resolve once
/// against the current directory; `/abs/drive`, `drive`, and
/// `./drive` are different spellings and only the spelling that
/// matches the stored key finds its section — the same spelling
/// finds the same section every time, and no spelling is
/// canonicalized through symlinks. A drive path containing `..`
/// can never match a stored section (stored keys refuse `..`).
/// Reads the process current directory for relative inputs only;
/// touches no filesystem state.
pub fn settings_key_for(drive_dir: &std::path::Path) -> String {
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

/// Shared rule for one drive key, in a file or about to be written:
/// non-empty, absolute, no `..`, no control characters. The exact
/// spelling both readers and the writer enforce, so a key one side
/// accepts is a key every side accepts.
pub fn validate_drive_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("settings drive key must not be empty".to_string());
    }
    let path = std::path::Path::new(key);
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

/// Shared range rule for the per-pass admission bound: 1..=1000000.
/// Zero or an absurd count is a bug at the call site, never data to
/// persist or apply.
pub fn check_max_admit_per_pass(admit: usize) -> Result<(), String> {
    if admit == 0 || admit > 1_000_000 {
        return Err(format!(
            "settings max_admit_per_pass must be 1..=1000000 (got {admit})"
        ));
    }
    Ok(())
}

/// Shared range rule for a whole-seconds duration bound: positive,
/// at most a day. Sub-second tuning is not a person-scale setting;
/// past a day the bound stops being pacing.
pub fn check_duration_secs(name: &str, secs: u64) -> Result<(), String> {
    if secs == 0 {
        return Err(format!(
            "settings {name} must be a positive number of seconds"
        ));
    }
    if secs > 86_400 {
        return Err(format!("settings {name} exceeds 86400s (got {secs}s)"));
    }
    Ok(())
}

/// Shared rule for a notification dedup key: non-empty, no control
/// characters — a slug, and an unprintable slug is a bug at the
/// call site, not data to persist.
pub fn check_notified_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("notification key must not be empty".to_string());
    }
    if key.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
        return Err(format!(
            "notification key {key:?} must not contain control characters"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared rules pin their refusals: key shape, admission
    /// range, duration range, notification-key shape. Parse-level
    /// strictness (`deny_unknown_fields`, version pin) is exercised
    /// through both consumers' read paths, which share these types.
    #[test]
    fn shared_rules_pin_their_refusals() {
        assert!(validate_drive_key("").is_err());
        assert!(validate_drive_key("relative/path").is_err());
        assert!(validate_drive_key("/has/../parent").is_err());
        assert!(validate_drive_key("/has/\u{0}control").is_err());
        assert!(validate_drive_key("/drives/alpha").is_ok());
        assert!(check_max_admit_per_pass(0).is_err());
        assert!(check_max_admit_per_pass(1_000_001).is_err());
        assert!(check_max_admit_per_pass(1024).is_ok());
        assert!(check_duration_secs("fetch_pass_budget_secs", 0).is_err());
        assert!(check_duration_secs("fetch_pass_budget_secs", 86_401).is_err());
        assert!(check_duration_secs("fetch_pass_budget_secs", 30).is_ok());
        assert!(check_notified_key("").is_err());
        assert!(check_notified_key("sync.quiet").is_ok());
    }

    /// The default file carries the current version and no drives.
    #[test]
    fn default_file_is_current_and_empty() {
        let file = AppSettingsFile::default();
        assert_eq!(file.version, APP_SETTINGS_VERSION);
        assert!(file.drives.is_empty());
    }
}
