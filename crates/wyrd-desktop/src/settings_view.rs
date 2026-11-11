//! Read-only view of the per-user application settings file.
//!
//! Observer only: this module parses the schema-version-1 file the
//! daemon's settings surface owns, with the same strictness (version
//! refusal, unknown-key refusal, key validation), and presents one
//! drive's configured bounds. It never writes the file —
//! configuration mutation needs an explicit ownership, concurrency,
//! and runtime-application contract that does not exist yet (OD:
//! desktop settings ownership, stage 2).
//!
//! Presentation rules, enforced by the types:
//!
//! - the view shows configured values only, never compiled defaults
//!   and never effective runtime posture: a missing file or a
//!   missing section is an explicit unknown state, not a silent
//!   default;
//! - malformed, version-skewed, or invalid files are explicit
//!   `Unreadable` states with the reason — loud, never guessed;
//! - the notification dedup records parse (so a file carrying them
//!   still validates exactly like the daemon's reader) but are never
//!   exposed: they are operational records, not settings to display;
//! - relays do not appear here at all: they live in the drive
//!   deployment configuration, never in this file (OD-25-C).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Settings schema version this shell reads. Newer files are
/// reported unreadable with their version, never guessed.
const APP_SETTINGS_VERSION: u32 = 1;

/// Filename of the application settings file inside the per-user
/// state directory.
const SETTINGS_FILENAME: &str = "settings.json";

/// One drive's section as parsed: the five surfaced bounds plus the
/// notification records the daemon's reader validates. Private: the
/// public [`DriveSettingsView`] carries the displayable bounds only.
/// `deny_unknown_fields` keeps typo-refusal parity with the daemon —
/// a misspelled key fails loudly instead of silently discarding.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SectionParse {
    #[serde(default)]
    retained_bytes_quota: Option<u64>,
    #[serde(default)]
    max_admit_per_pass: Option<usize>,
    #[serde(default)]
    fetch_pass_budget_secs: Option<u64>,
    #[serde(default)]
    max_mutation_wait_secs: Option<u64>,
    #[serde(default)]
    open_timeout_secs: Option<u64>,
    // Parses but never reads: a file carrying notification records
    // still validates exactly like the daemon's reader, and the
    // records stay out of the displayable view by construction.
    #[allow(dead_code)]
    #[serde(default)]
    notified: BTreeMap<String, u64>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsFileParse {
    version: u32,
    drives: BTreeMap<String, SectionParse>,
}

/// One drive's configured bounds, as configured — not as applied.
/// Every field is optional because absent keys leave the compiled
/// defaults in place; the view never fills them in, so configured
/// and defaulted stay distinguishable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DriveSettingsView {
    pub retained_bytes_quota: Option<u64>,
    pub max_admit_per_pass: Option<usize>,
    pub fetch_pass_budget_secs: Option<u64>,
    pub max_mutation_wait_secs: Option<u64>,
    pub open_timeout_secs: Option<u64>,
}

/// What the settings file said about this drive: configured values,
/// no file yet, no section for this drive, or present but unreadable
/// with the reason. Missing and malformed are explicit unknown
/// states — never silent defaults, never guesses.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum DriveSettings {
    Configured(DriveSettingsView),
    Nofile,
    Nosection,
    Unreadable { reason: String },
}

/// Where the settings file lives when no explicit path is given:
/// per-user state, resolved through the same platform conventions
/// as the daemon. Fails loudly when no per-user directory resolves
/// — a cwd-relative guess could place settings inside a drive
/// directory, which the upgrade contract forbids.
fn default_settings_path() -> Result<PathBuf, String> {
    resolve_settings_path(
        std::env::var_os("HOME")
            .map(|home| home.to_string_lossy().into_owned())
            .as_deref(),
        std::env::var("XDG_STATE_HOME").ok().as_deref(),
    )
}

/// Pure half of [`default_settings_path`]: resolve from explicit
/// inputs so the no-home refusal pins without touching the process
/// environment.
fn resolve_settings_path(
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

/// The lookup key for one drive's section: the absolute drive
/// directory as an exact string. Mirrors the daemon's rule —
/// relative arguments resolve once against the current directory,
/// and only the matching spelling finds its section.
fn settings_key_for(drive_dir: &Path) -> String {
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
    // One rule for every drive path in the crate: the shared
    // command-boundary validator, so the settings reader can never
    // accept a spelling the connection map refuses (or vice versa).
    super::validate_drive_dir_str(key).map(|_| ())
}

fn unreadable(reason: String) -> DriveSettings {
    DriveSettings::Unreadable { reason }
}

/// Read one drive's settings from an explicit file. The explicit
/// path exists for tests; production passes the default derivation.
pub fn drive_settings_at(settings_path: &Path, drive_dir: &Path) -> DriveSettings {
    let bytes = match std::fs::read(settings_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return DriveSettings::Nofile;
        }
        Err(error) => {
            return unreadable(format!("read {}: {error}", settings_path.display()));
        }
    };
    let file: SettingsFileParse = match serde_json::from_slice(&bytes) {
        Ok(file) => file,
        Err(error) => {
            return unreadable(format!("parse {}: {error}", settings_path.display()));
        }
    };
    if file.version != APP_SETTINGS_VERSION {
        return unreadable(format!(
            "settings {} is version {}, this shell reads {}",
            settings_path.display(),
            file.version,
            APP_SETTINGS_VERSION
        ));
    }
    for key in file.drives.keys() {
        if let Err(reason) = validate_key(key) {
            return unreadable(reason);
        }
    }
    match file.drives.get(&settings_key_for(drive_dir)) {
        None => DriveSettings::Nosection,
        Some(section) => DriveSettings::Configured(DriveSettingsView {
            retained_bytes_quota: section.retained_bytes_quota,
            max_admit_per_pass: section.max_admit_per_pass,
            fetch_pass_budget_secs: section.fetch_pass_budget_secs,
            max_mutation_wait_secs: section.max_mutation_wait_secs,
            open_timeout_secs: section.open_timeout_secs,
        }),
    }
}

/// Read one drive's settings from the default file. An unresolvable
/// per-user directory is an explicit unreadable state naming both
/// variables — never a cwd-relative guess.
pub fn drive_settings(drive_dir: &Path) -> DriveSettings {
    match default_settings_path() {
        Ok(path) => drive_settings_at(&path, drive_dir),
        Err(reason) => unreadable(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The no-home refusal pins on explicit inputs: no environment,
    /// no fallback, both variables named.
    #[test]
    fn no_home_refuses_loudly() {
        assert_eq!(
            resolve_settings_path(None, None),
            Err(
                "no per-user state directory: set HOME or XDG_STATE_HOME to locate settings.json"
                    .to_string()
            )
        );
        assert!(resolve_settings_path(Some(""), Some("")).is_err());
    }

    /// A missing file is unknown (nothing configured yet), never an
    /// error and never defaults dressed as configuration.
    #[test]
    fn missing_file_is_unknown_not_defaults() {
        let path =
            std::env::temp_dir().join(format!("wyrd-desktop-nosettings-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            drive_settings_at(&path, Path::new("/drives/alpha")),
            DriveSettings::Nofile
        );
    }

    /// Version skew, malformed JSON, unknown keys, and invalid keys
    /// all surface as explicit unreadable states with their reason.
    #[test]
    fn skew_malformed_unknown_and_bad_keys_are_unreadable() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-settings-{}-bad", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let case = |name: &str, body: &str| {
            let path = dir.join(name);
            std::fs::write(&path, body).unwrap();
            match drive_settings_at(&path, Path::new("/drives/alpha")) {
                DriveSettings::Unreadable { reason } => reason,
                state => panic!("{name} must be unreadable: {state:?}"),
            }
        };
        let reason = case("newer.json", r#"{"version": 2, "drives": {}}"#);
        assert!(reason.contains("version 2"), "reason: {reason}");
        let reason = case("broken.json", r#"{"version": 1, "drives": {"#);
        assert!(reason.contains("parse"), "reason: {reason}");
        let reason = case(
            "typo.json",
            r#"{"version": 1, "drives": {"/drives/alpha": {"quotum": 7}}}"#,
        );
        assert!(reason.contains("quotum"), "reason: {reason}");
        let reason = case(
            "relative.json",
            r#"{"version": 1, "drives": {"drives/alpha": {}}}"#,
        );
        assert!(reason.contains("must be absolute"), "reason: {reason}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Configured values round through untouched; a drive without a
    /// section is an explicit unknown, not defaults.
    #[test]
    fn configured_values_round_through_and_absent_sections_stay_unknown() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-settings-{}-good", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"version": 1, "drives": {"/drives/alpha": {"retained_bytes_quota": 0, "max_admit_per_pass": 50, "open_timeout_secs": 45, "notified": {"sync.quiet": 1700000000}}}}"#,
        )
        .unwrap();
        // Zero quota round-trips as zero: the refusal semantics belong
        // to the applier, and the view must not normalize them away.
        assert_eq!(
            drive_settings_at(&path, Path::new("/drives/alpha")),
            DriveSettings::Configured(DriveSettingsView {
                retained_bytes_quota: Some(0),
                max_admit_per_pass: Some(50),
                fetch_pass_budget_secs: None,
                max_mutation_wait_secs: None,
                open_timeout_secs: Some(45),
            })
        );
        assert_eq!(
            drive_settings_at(&path, Path::new("/drives/beta")),
            DriveSettings::Nosection
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
