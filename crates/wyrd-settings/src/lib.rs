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
    /// Absent keys serialize absent, never null: clearing a key
    /// removes it, so a saved file holds configured values only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_bytes_quota: Option<u64>,
    /// Wants admitted per sync pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_admit_per_pass: Option<usize>,
    /// Wall-clock budget for one pass's fetch phase, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_pass_budget_secs: Option<u64>,
    /// Wall-clock deadline for a held mutation, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_mutation_wait_secs: Option<u64>,
    /// Wall-clock budget for one `open` demand wait, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

/// Shared spelling rule for one drive path, attributed to the
/// artifact at fault (`noun`): non-empty, absolute, no `..`, no
/// control characters. One implementation so the settings-file key,
/// the desktop command boundary, and the registry can never drift —
/// the same rule, attributed to the surface that rejected it.
/// `validate_drive_key` is the settings-file spelling of this rule.
pub fn validate_drive_spelling(value: &str, noun: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{noun} must not be empty"));
    }
    let path = std::path::Path::new(value);
    if !path.is_absolute() {
        return Err(format!("{noun} {value:?} must be absolute"));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("{noun} {value:?} must not contain .."));
    }
    if value.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
        return Err(format!(
            "{noun} {value:?} must not contain control characters"
        ));
    }
    Ok(())
}

/// Shared rule for one drive key, in a file or about to be written:
/// the shared spelling rule under the settings-file noun, so a key
/// one side accepts is a key every side accepts.
pub fn validate_drive_key(key: &str) -> Result<(), String> {
    validate_drive_spelling(key, "settings drive key")
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

/// The five keys the editor owns, in file order. One list so the
/// sparse-update validator, the writer, and the future IPC surface
/// name the same keys — a sixth key is refused everywhere by name,
/// never silently dropped.
pub const EDITABLE_KEYS: [&str; 5] = [
    "retained_bytes_quota",
    "max_admit_per_pass",
    "fetch_pass_budget_secs",
    "max_mutation_wait_secs",
    "open_timeout_secs",
];

/// One sparse edit: values to set plus keys to clear, for exactly
/// one drive. Never a whole-document replacement — the writer
/// applies these onto the current file under the transaction lock,
/// preserving every other drive's section and the notification
/// records beside the edited keys. A key in both lists is refused
/// as contradictory, never resolved by precedence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SparseUpdate {
    /// Bounds to write, keyed by [`EDITABLE_KEYS`] name.
    #[serde(default)]
    pub set: BTreeMap<String, u64>,
    /// Bounds to remove (restore the compiled default), by name.
    #[serde(default)]
    pub clear: Vec<String>,
}

/// Shared validation for one sparse edit: every named key is one of
/// [`EDITABLE_KEYS`], no key is both set and cleared, and every set
/// value passes that key's range rule. Runs before the lock, like
/// every candidate validation — a refusal touches nothing.
pub fn validate_sparse_update(update: &SparseUpdate) -> Result<(), String> {
    for key in update.set.keys() {
        if !EDITABLE_KEYS.contains(&key.as_str()) {
            return Err(format!("unknown settings key {key:?}"));
        }
    }
    for key in &update.clear {
        if !EDITABLE_KEYS.contains(&key.as_str()) {
            return Err(format!("unknown settings key {key:?}"));
        }
        if update.set.contains_key(key) {
            return Err(format!(
                "settings key {key:?} is both set and cleared: contradictory edit"
            ));
        }
    }
    for (key, value) in &update.set {
        match key.as_str() {
            "retained_bytes_quota" => {
                // Any u64 is representable: zero means what it says
                // (every write refuses), absence means unlimited.
            }
            "max_admit_per_pass" => {
                let admit = usize::try_from(*value).map_err(|_| {
                    format!("settings max_admit_per_pass must be 1..=1000000 (got {value})")
                })?;
                check_max_admit_per_pass(admit)?;
            }
            "fetch_pass_budget_secs" | "max_mutation_wait_secs" | "open_timeout_secs" => {
                check_duration_secs(key, *value)?;
            }
            _ => unreachable!("key membership checked above"),
        }
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

    /// Absent bounds serialize absent, never null: a saved file
    /// holds configured values only, and clearing a key removes it
    /// from the wire. Explicit nulls from older writers still parse
    /// to absent, so the sparser encoding needs no migration.
    #[test]
    fn absent_bounds_serialize_absent() {
        let section = DriveSettingsSection {
            open_timeout_secs: Some(30),
            ..DriveSettingsSection::default()
        };
        let rendered = serde_json::to_value(&section).unwrap();
        let fields = rendered.as_object().unwrap();
        assert!(!fields.contains_key("max_admit_per_pass"), "{rendered}");
        assert_eq!(fields["open_timeout_secs"], 30);
        let legacy: DriveSettingsSection =
            serde_json::from_str(r#"{"max_admit_per_pass": null}"#).unwrap();
        assert_eq!(legacy.max_admit_per_pass, None);
    }

    /// The default file carries the current version and no drives.
    #[test]
    fn default_file_is_current_and_empty() {
        let file = AppSettingsFile::default();
        assert_eq!(file.version, APP_SETTINGS_VERSION);
        assert!(file.drives.is_empty());
    }

    /// The sparse edit pins its refusals: unknown set/clear keys
    /// name the key, a both-set-and-cleared key names the
    /// contradiction, and per-key ranges reuse the shared rules —
    /// while a well-formed mixed edit validates.
    #[test]
    fn sparse_update_pins_its_refusals() {
        let mut set = BTreeMap::new();
        set.insert("max_admit_per_pass".to_string(), 64);
        set.insert("open_timeout_secs".to_string(), 30);
        let update = SparseUpdate {
            set,
            clear: vec!["fetch_pass_budget_secs".to_string()],
        };
        assert!(validate_sparse_update(&update).is_ok());

        let mut set = BTreeMap::new();
        set.insert("admit_all".to_string(), 1);
        let refused = validate_sparse_update(&SparseUpdate {
            set,
            clear: Vec::new(),
        })
        .unwrap_err();
        assert!(refused.contains("admit_all"), "{refused}");

        let refused = validate_sparse_update(&SparseUpdate {
            set: BTreeMap::new(),
            clear: vec!["relays".to_string()],
        })
        .unwrap_err();
        assert!(refused.contains("relays"), "{refused}");

        let mut set = BTreeMap::new();
        set.insert("max_admit_per_pass".to_string(), 64);
        let refused = validate_sparse_update(&SparseUpdate {
            set,
            clear: vec!["max_admit_per_pass".to_string()],
        })
        .unwrap_err();
        assert!(refused.contains("both set and cleared"), "{refused}");

        for (key, value) in [
            ("max_admit_per_pass", 0),
            ("max_admit_per_pass", 1_000_001),
            ("open_timeout_secs", 0),
            ("open_timeout_secs", 86_401),
        ] {
            let mut set = BTreeMap::new();
            set.insert(key.to_string(), value);
            let refused = validate_sparse_update(&SparseUpdate {
                set,
                clear: Vec::new(),
            })
            .unwrap_err();
            assert!(refused.contains(key), "{refused}");
        }
        // Zero quota is representable (refuse-all), not absurd.
        let mut set = BTreeMap::new();
        set.insert("retained_bytes_quota".to_string(), 0);
        assert!(validate_sparse_update(&SparseUpdate {
            set,
            clear: Vec::new(),
        })
        .is_ok());
    }
}
