//! The `settings apply` command: one drive plus one sparse update
//! through the shared settings transaction — the same
//! [`wyrd_daemon::apply_sparse_update`] the notification path
//! commits through, so an operator hand-edit and the editor share
//! one lock, one strict read, and one commit guard.
//!
//! No keystore opens and no credentials load: the file is per-user
//! state beside every drive directory, and the transaction runs in
//! this process whether the drive's owner runs or not. A saved edit
//! applies at the next owner start (restart-to-apply), never live.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::args::SettingsAction;
use crate::CliError;

/// Parse one `--set KEY=VALUE` element: the key as given, the value
/// as a whole number. Splits on the first `=` so a value can never
/// smuggle a second key; a missing `=` or a non-numeric value is a
/// usage error naming the element.
fn parse_set_element(element: &str) -> Result<(String, u64), CliError> {
    let (key, value) = element.split_once('=').ok_or_else(|| {
        CliError::Usage(format!(
            "--set expects KEY=VALUE, got {element:?}: e.g. --set max_admit_per_pass=64"
        ))
    })?;
    if key.is_empty() {
        return Err(CliError::Usage(format!(
            "--set expects KEY=VALUE with a non-empty key, got {element:?}"
        )));
    }
    let value: u64 = value.parse().map_err(|_| {
        CliError::Usage(format!(
            "--set value for {key:?} must be a whole number of the bound's unit, got {value:?}"
        ))
    })?;
    Ok((key.to_string(), value))
}

/// Run one `settings` action. The drive directory is the section
/// key verbatim: absolute only, never resolved against the current
/// directory — the spelling written here is the spelling the owner
/// looks up at startup, and a relative spelling would key a
/// different section per process directory.
pub(crate) fn settings(drive_dir: PathBuf, action: SettingsAction) -> Result<(), CliError> {
    let SettingsAction::Apply {
        set,
        clear,
        settings_path,
    } = action;
    if !drive_dir.is_absolute() {
        return Err(CliError::Usage(format!(
            "settings drive must be absolute (the section key is the exact path): got {}",
            drive_dir.display()
        )));
    }
    let mut set_map = BTreeMap::new();
    for element in &set {
        let (key, value) = parse_set_element(element)?;
        set_map.insert(key, value);
    }
    let update = wyrd_daemon::SparseUpdate {
        set: set_map,
        clear: clear.clone(),
    };
    // Pre-lock validation lives inside the transaction (unknown
    // keys, contradictions, and absurd values refuse before anything
    // is read), so this call site maps rather than re-checks.
    let path = match settings_path {
        Some(path) => path,
        None => wyrd_daemon::default_settings_path().map_err(CliError::Usage)?,
    };
    let observed =
        wyrd_daemon::apply_sparse_update(&path, &drive_dir, &update).map_err(|error| {
            use wyrd_daemon::SettingsWriteError as E;
            match error {
                E::Conflict { reason, .. } => CliError::SettingsConflict { reason },
                E::CommittedUnverified { reason, .. } => CliError::SettingsUnverified { reason },
                E::Refused { reason, .. } => {
                    CliError::Usage(format!("settings apply refused: {reason}"))
                }
            }
        })?;
    println!(
        "settings applied for {}: {} bound(s) configured",
        drive_dir.display(),
        observed_configured_count(&observed)
    );
    Ok(())
}

/// Configured (non-default) bound count in the observed section, for
/// the success line. Display detail only — the desktop decides from
/// the exit code plus its own re-read, never from this number.
fn observed_configured_count(section: &wyrd_daemon::DriveSettingsSection) -> usize {
    [
        section.retained_bytes_quota.is_some(),
        section.max_admit_per_pass.is_some(),
        section.fetch_pass_budget_secs.is_some(),
        section.max_mutation_wait_secs.is_some(),
        section.open_timeout_secs.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--set` splits on the first `=` and takes whole numbers:
    /// a missing `=`, an empty key, or a non-numeric value refuses
    /// naming the element.
    #[test]
    fn set_elements_parse_strictly() {
        assert_eq!(
            parse_set_element("max_admit_per_pass=64").unwrap(),
            ("max_admit_per_pass".to_string(), 64)
        );
        assert_eq!(
            parse_set_element("open_timeout_secs=30=extra").unwrap_err().to_string(),
            "--set value for \"open_timeout_secs\" must be a whole number of the bound's unit, got \"30=extra\""
        );
        assert!(
            parse_set_element("max_admit_per_pass")
                .unwrap_err()
                .to_string()
                .contains("KEY=VALUE"),
            "a missing = names the shape"
        );
        assert!(
            parse_set_element("=64")
                .unwrap_err()
                .to_string()
                .contains("non-empty key"),
            "an empty key refuses"
        );
    }

    /// A relative drive refuses before anything is read: the section
    /// key is the exact path, and resolving here would key a
    /// different section than the owner's lookup.
    #[test]
    fn relative_drive_refuses_before_reading() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-cli-settings-relative-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let refused = settings(
            PathBuf::from("relative/drive"),
            SettingsAction::Apply {
                set: vec!["open_timeout_secs=30".to_string()],
                clear: Vec::new(),
                settings_path: Some(dir.join("settings.json")),
            },
        )
        .unwrap_err();
        assert!(
            refused.to_string().contains("must be absolute"),
            "{refused}"
        );
        assert!(
            !dir.join("settings.json").exists(),
            "a refused edit writes nothing"
        );
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// Unknown keys refuse naming the key, with the file untouched —
    /// through the real transaction, not just the validator.
    #[test]
    fn unknown_key_refuses_through_the_transaction() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-cli-settings-unknown-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let refused = settings(
            PathBuf::from("/drives/alpha"),
            SettingsAction::Apply {
                set: vec!["admit_all=1".to_string()],
                clear: Vec::new(),
                settings_path: Some(path.clone()),
            },
        )
        .unwrap_err();
        assert!(refused.to_string().contains("admit_all"), "{refused}");
        assert!(!path.exists(), "a refused edit writes nothing");
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// The commit race has its own exit status: conflict exits 3 so
    /// the desktop renders it without parsing text; an unverified
    /// commit exits 4, never collapsed into refusal; usage stays 2.
    #[test]
    fn conflict_and_usage_fold_to_distinct_codes() {
        assert_eq!(
            CliError::SettingsConflict {
                reason: "moved".to_string()
            }
            .exit_code(),
            crate::EXIT_SETTINGS_CONFLICT
        );
        assert_eq!(crate::EXIT_SETTINGS_CONFLICT, 3);
        assert_eq!(
            CliError::SettingsUnverified {
                reason: "unreadable".to_string()
            }
            .exit_code(),
            crate::EXIT_SETTINGS_UNVERIFIED
        );
        assert_eq!(crate::EXIT_SETTINGS_UNVERIFIED, 4);
        assert_eq!(CliError::Usage("bad".to_string()).exit_code(), 2);
    }

    /// The full argv path dispatches without credentials: set writes
    /// the section, clear removes the key, and the success line
    /// carries the observed count — through `command()`, like an
    /// operator invocation.
    #[test]
    fn apply_argv_round_trips_without_credentials() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-cli-settings-argv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let run = |args: &[&str]| {
            crate::command(args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
        };
        run(&[
            "settings",
            "/drives/alpha",
            "apply",
            "--set",
            "open_timeout_secs=30",
            "--set",
            "max_admit_per_pass=64",
            "--settings-path",
            path.to_str().unwrap(),
        ])
        .unwrap();
        let file = wyrd_daemon::read_settings(&path).unwrap();
        let section = &file.drives["/drives/alpha"];
        assert_eq!(section.open_timeout_secs, Some(30));
        assert_eq!(section.max_admit_per_pass, Some(64));

        run(&[
            "settings",
            "/drives/alpha",
            "apply",
            "--clear",
            "open_timeout_secs",
            "--settings-path",
            path.to_str().unwrap(),
        ])
        .unwrap();
        let file = wyrd_daemon::read_settings(&path).unwrap();
        assert_eq!(file.drives["/drives/alpha"].open_timeout_secs, None);

        // Unknown keys fail the invocation (exit 2), file untouched.
        let before = std::fs::read(&path).unwrap();
        let refused = run(&[
            "settings",
            "/drives/alpha",
            "apply",
            "--set",
            "admit_all=1",
            "--settings-path",
            path.to_str().unwrap(),
        ])
        .unwrap_err();
        assert_eq!(refused.exit_code(), 2);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }
}
