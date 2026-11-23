//! Read-only view of the application-wide diagnostics level.
//!
//! The desktop shows the level persisted in `settings.json`, never an
//! owner's in-memory override (T20 in `docs/contracts/trust.md`,
//! `docs/design/settings-ownership.md` §9). Like
//! [`crate::settings_view`] this module parses the shared settings
//! file with the same strictness as the writer; it never writes
//! anything and never opens a drive directory. Changing the level is
//! an owner operation (`diagnostics-set`) — the frontend has no
//! filesystem authority here.
//!
//! Absence is meaningful: no file, no `diagnostics` section, and an
//! empty section all read as `default`, but `explicit` says whether
//! the file actually carries a level so the UI never presents an
//! absent setting as something the user chose.

use std::path::Path;

use wyrd_settings::{parse_settings, DiagnosticLevel};

use crate::settings_view::default_settings_path;

/// The persisted application-wide diagnostics level as the shell can
/// observe it. Configured values are the real file contents; missing
/// and malformed files are explicit states, never guessed defaults.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Diagnostics {
    /// The file is readable. `level` is the value the file would
    /// start every owner with; `explicit` is false when the key was
    /// absent (the genuine `default` state).
    Configured {
        /// The level read from the file.
        level: DiagnosticLevel,
        /// Whether the file carries an explicit `level` key.
        explicit: bool,
        /// The settings file this was read from.
        path: String,
    },
    /// No settings file exists. Absence means `default`, but the
    /// shell says so rather than inventing a saved choice.
    Nofile {
        /// Where the settings file would live for this user.
        path: String,
    },
    /// File present but unreadable with the reason: loud, never
    /// guessed.
    Unreadable {
        /// Why the file is unreadable: version skew, parse, or an
        /// invalid diagnostics shape.
        reason: String,
    },
}

/// Read the persisted level from an explicit file. The explicit path
/// exists for tests; production passes the default derivation.
pub fn diagnostics_at(settings_path: &Path) -> Diagnostics {
    let path = settings_path.to_string_lossy().into_owned();
    let bytes = match std::fs::read(settings_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Diagnostics::Nofile { path };
        }
        Err(error) => {
            return Diagnostics::Unreadable {
                reason: format!("read {}: {error}", settings_path.display()),
            };
        }
    };
    // The same parse-and-migrate function the daemon's writer uses:
    // a v1 file reads as v2 with no diagnostics section, so no two
    // surfaces can disagree about a version or an unknown key.
    let file = match parse_settings(&bytes) {
        Ok(file) => file,
        Err(error) => {
            return Diagnostics::Unreadable {
                reason: format!("parse {}: {error}", settings_path.display()),
            };
        }
    };
    let explicit = file
        .diagnostics
        .as_ref()
        .and_then(|section| section.level)
        .is_some();
    Diagnostics::Configured {
        level: file.diagnostics_level(),
        explicit,
        path,
    }
}

/// Read the persisted level from the default per-user file. An
/// unresolvable per-user directory is an explicit unreadable state,
/// never a cwd-relative guess.
pub fn diagnostics() -> Diagnostics {
    match default_settings_path() {
        Ok(path) => diagnostics_at(&path),
        Err(reason) => Diagnostics::Unreadable { reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The observed (level, explicit) pair from a file body, or the
    /// state itself when the file is missing or unreadable. The path
    /// is asserted separately so the fixtures stay readable.
    fn at(body: &str) -> (DiagnosticLevel, bool, String) {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-diagnostics-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, body).unwrap();
        let result = match diagnostics_at(&path) {
            Diagnostics::Configured {
                level,
                explicit,
                path,
            } => (level, explicit, path),
            other => panic!("a readable file configures: {other:?}"),
        };
        std::fs::remove_dir_all(&dir).unwrap();
        result
    }

    /// Absence of the section and of the key both read as `default`,
    /// but only an explicit key is reported as chosen. A v1 file
    /// still migrates to the same read.
    #[test]
    fn absence_is_default_and_never_explicit() {
        for body in [
            r#"{"version":2,"drives":{}}"#,
            r#"{"version":2,"drives":{},"diagnostics":{}}"#,
            r#"{"version":1,"drives":{}}"#,
        ] {
            let (level, explicit, _) = at(body);
            assert_eq!(level, DiagnosticLevel::Default, "{body}");
            assert!(!explicit, "absence is not a choice: {body}");
        }
    }

    /// An explicit `detailed` key is reported as chosen, and the
    /// path carried in the answer names the file that was read.
    #[test]
    fn explicit_detailed_is_reported_as_chosen() {
        let (level, explicit, path) =
            at(r#"{"version":2,"drives":{},"diagnostics":{"level":"detailed"}}"#);
        assert_eq!(level, DiagnosticLevel::Detailed);
        assert!(explicit);
        assert!(path.ends_with("settings.json"), "path: {path}");
    }

    /// A missing file is an explicit nofile state; version skew,
    /// malformed JSON, and an unknown level are unreadable with the
    /// reason, never defaulted.
    #[test]
    fn missing_skew_and_bad_levels_are_explicit() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-diagnostics-missing-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("settings.json");
        let _ = std::fs::remove_file(&missing);
        assert_eq!(
            diagnostics_at(&missing),
            Diagnostics::Nofile {
                path: missing.to_string_lossy().into_owned()
            }
        );
        for (name, body, expected) in [
            (
                "newer.json",
                r#"{"version":999,"drives":{}}"#,
                "version 999",
            ),
            ("broken.json", r#"{"version":2,"drives":{"#, "parse"),
            (
                "bad-level.json",
                r#"{"version":2,"drives":{},"diagnostics":{"level":"verbose"}}"#,
                "verbose",
            ),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, body).unwrap();
            match diagnostics_at(&path) {
                Diagnostics::Unreadable { reason } => {
                    assert!(reason.contains(expected), "{name}: {reason}");
                }
                other => panic!("{name} must be unreadable: {other:?}"),
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
