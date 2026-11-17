//! Settings writes through the installed CLI: the constrained
//! adapter the stage-2 editor commits through.
//!
//! Authority contract (mirroring `provision.rs`): the desktop
//! launches the operation but never becomes the source of truth. It
//! invokes the version-matched CLI with structured arguments (argv
//! arrays, never shell-constructed), waits for process completion,
//! then re-reads the settings file through the existing read path —
//! observed state determines the display; exit codes indicate
//! operation outcomes, never configured state.
//!
//! Why the CLI and not the owner socket: the transaction must work
//! whether the drive's owner runs or not (a stopped drive saves
//! without starting), and the owner interface carries no settings
//! operation — the CLI runs the same daemon transaction in-process,
//! so one implementation serves both. Layering: `std::process`
//! only — no new Cargo edge.
//!
//! Exit codes: 0 applied, 2 refused (validation, unreadable file,
//! usage), 3 conflict (the file moved under the transaction —
//! re-read and retry, never terminal), anything else unknown. Only
//! `settings apply` exits 3; the provisioning contract (0/2/78)
//! is untouched.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use wyrd_settings::SparseUpdate;

use super::provision::{
    check_cli_version, resolve_cli_bin, run_cli, truncate, CliOutput, ProvisionResult,
};

/// How long one settings apply may run before the adapter gives up
/// waiting. The transaction is one local file under a lock — fast —
/// so expiry is a wedged CLI, never a slow commit: kill, reap, and
/// resolve unknown, triggering a re-read like every other timeout.
pub const SETTINGS_APPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// The exit code `settings apply` reports for a lost commit race.
/// The only settings-scoped addition to the CLI contract: 0
/// applied, 2 refused, 78 held-drive — plus 3 conflict.
pub const EXIT_SETTINGS_CONFLICT: i32 = 3;

/// One sparse edit for exactly one drive: the section key plus the
/// shared update both sides validate.
#[derive(Debug, Clone)]
pub struct SettingsApplyRequest {
    /// Absolute drive directory, exact string (shared drive-identity rule).
    pub drive_dir: PathBuf,
    /// Bounds to set and bounds to clear, by editable-key name.
    pub update: SparseUpdate,
    /// Explicit CLI binary (tests / non-standard layouts); `None` =
    /// the version-matched default the provision adapter resolves.
    pub cli_bin: Option<PathBuf>,
    /// Explicit settings file (tests); `None` = the per-user default
    /// the CLI resolves.
    pub settings_path: Option<PathBuf>,
}

/// The adapter's verdict on one sparse edit. Only the exit code
/// decides; CLI stdout/stderr travel as opaque display detail and
/// are never parsed — the caller pairs every path with a settings
/// re-read, and only that observed section decides the display.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "result", rename_all = "lowercase")]
pub enum SettingsApplyResult {
    /// Exit 0: the CLI reported no error. The paired re-read alone
    /// decides what is configured — this never claims a state.
    Applied {
        /// Opaque display detail; the paired re-read decides.
        detail: String,
    },
    /// Exit 2 or an adapter-side refusal: usage, validation, an
    /// unreadable file, or a refusal that never spawned a process.
    /// Opaque detail; the paired re-read says what is configured.
    Refused {
        /// Opaque display detail; the paired re-read decides.
        reason: String,
    },
    /// Exit 3: the file moved under the transaction. The edit is
    /// valid — re-read and retry, never auto-merge. Opaque detail;
    /// the paired re-read shows the current values.
    Conflict {
        /// Opaque display detail naming the race; the paired re-read
        /// shows what actually stands.
        reason: String,
    },
    /// Version mismatch, spawn failure, timeout, or signal death:
    /// the outcome is unknown. Triggers a re-read, never a retry.
    Failed {
        /// Why the outcome is unknown; triggers a re-read, never a retry.
        reason: String,
    },
}

/// One completed edit: the outcome verdict plus the raw CLI stderr
/// (display detail, never parsed) and the exit code observed.
#[derive(Debug, Clone)]
pub struct SettingsApplyOutcome {
    /// Operation verdict: exit codes only, never configured state.
    pub result: SettingsApplyResult,
    /// Raw CLI stderr as opaque display detail, never parsed.
    pub stderr: String,
    /// Observed exit code, absent where no process completed.
    pub code: Option<i32>,
}

/// Pure argv builder for `wyrd settings <drive> apply …`: every
/// argument is a separate argv element, never shell-constructed.
/// Mirrors `crates/wyrd-cli/src/args.rs` (`SettingsAction::Apply`).
/// Drive identity and the shared update validate before any process
/// spawns — a typo'd key refuses here, not in a child.
pub fn settings_apply_argv(request: &SettingsApplyRequest) -> Result<Vec<OsString>, String> {
    if !request.drive_dir.is_absolute() {
        return Err(format!(
            "settings drive must be absolute (the section key is the exact path): got {}",
            request.drive_dir.display()
        ));
    }
    wyrd_settings::validate_sparse_update(&request.update)
        .map_err(|reason| format!("settings apply refused: {reason}"))?;
    let mut argv: Vec<OsString> = vec![
        "settings".into(),
        request.drive_dir.as_os_str().to_owned(),
        "apply".into(),
    ];
    let mut set: Vec<(&String, &u64)> = request.update.set.iter().collect();
    set.sort_by(|left, right| left.0.cmp(right.0));
    for (key, value) in set {
        argv.push("--set".into());
        argv.push(format!("{key}={value}").into());
    }
    let mut clear = request.update.clear.clone();
    clear.sort();
    for key in &clear {
        argv.push("--clear".into());
        argv.push(key.into());
    }
    if let Some(path) = &request.settings_path {
        argv.push("--settings-path".into());
        argv.push(path.as_os_str().to_owned());
    }
    Ok(argv)
}

/// Map a completed process run onto the edit verdict. Only the exit
/// code decides: 0 applies, 3 conflicts, 2 refuses, and anything
/// else (plus timeouts and spawn failures) is unknown — the paired
/// re-read decides what the display says.
fn map_settings_exit(output: &CliOutput) -> SettingsApplyOutcome {
    if output.timed_out {
        return SettingsApplyOutcome {
            result: SettingsApplyResult::Failed {
                reason: output.stderr.clone(),
            },
            stderr: output.stderr.clone(),
            code: None,
        };
    }
    let Some(code) = output.code else {
        return SettingsApplyOutcome {
            result: SettingsApplyResult::Failed {
                reason: output.stderr.clone(),
            },
            stderr: output.stderr.clone(),
            code: None,
        };
    };
    let detail = truncate(&format!("{}\n{}", output.stdout, output.stderr), 500);
    if output.status_success {
        return SettingsApplyOutcome {
            result: SettingsApplyResult::Applied { detail },
            stderr: output.stderr.clone(),
            code: Some(code),
        };
    }
    if code == EXIT_SETTINGS_CONFLICT {
        return SettingsApplyOutcome {
            result: SettingsApplyResult::Conflict { reason: detail },
            stderr: output.stderr.clone(),
            code: Some(code),
        };
    }
    // Every other non-zero code — including usage, unreadable files,
    // and validation refusals — is a refusal: nothing was committed
    // past the transaction's own guard, and the paired re-read says
    // what stands.
    SettingsApplyOutcome {
        result: SettingsApplyResult::Refused { reason: detail },
        stderr: output.stderr.clone(),
        code: Some(code),
    }
}

/// Run one `settings apply` through the version-matched CLI: version
/// check first (hard failure on mismatch, like every provisioned
/// operation), then the structured argv run, then the exit-code map.
/// The caller pairs every path with a settings re-read; no path
/// reports configured state.
pub fn run_settings_apply(
    cli_bin: &Path,
    request: &SettingsApplyRequest,
    timeout: Duration,
) -> SettingsApplyOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return SettingsApplyOutcome {
            result: SettingsApplyResult::Failed {
                reason: match mismatch {
                    ProvisionResult::VersionMismatch { reason } => reason,
                    other => format!("version check failed: {other:?}"),
                },
            },
            stderr: String::new(),
            code: None,
        };
    }
    let argv = match settings_apply_argv(request) {
        Ok(argv) => argv,
        Err(reason) => {
            return SettingsApplyOutcome {
                result: SettingsApplyResult::Refused { reason },
                stderr: String::new(),
                code: None,
            };
        }
    };
    let output = run_cli(cli_bin, &argv, None, timeout);
    map_settings_exit(&output)
}

/// Resolve the CLI binary for one settings edit: an explicit path
/// (tests / non-standard layouts) or the version-matched default
/// the provision adapter resolves.
pub fn resolve_settings_cli(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    resolve_cli_bin(explicit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn request() -> SettingsApplyRequest {
        let mut set = BTreeMap::new();
        set.insert("open_timeout_secs".to_string(), 30);
        SettingsApplyRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            update: SparseUpdate {
                set,
                clear: vec!["fetch_pass_budget_secs".to_string()],
            },
            cli_bin: None,
            settings_path: None,
        }
    }

    /// The apply argv is one structured element per argument: the
    /// family, the drive, the verb, sorted `--set KEY=VALUE` pairs
    /// and `--clear` keys — never shell-constructed.
    #[test]
    fn apply_argv_is_structured_per_element() {
        let rendered: Vec<String> = settings_apply_argv(&request())
            .unwrap()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "settings",
                "/drives/alpha",
                "apply",
                "--set",
                "open_timeout_secs=30",
                "--clear",
                "fetch_pass_budget_secs",
            ]
        );
    }

    /// Identity and update validate before any process spawns: a
    /// relative drive and an unknown key refuse naming the cause.
    #[test]
    fn apply_argv_refuses_before_spawning() {
        let mut relative = request();
        relative.drive_dir = PathBuf::from("drives/alpha");
        assert!(
            settings_apply_argv(&relative)
                .unwrap_err()
                .contains("must be absolute"),
            "a relative drive refuses"
        );
        let mut unknown = request();
        unknown.update.set.insert("admit_all".to_string(), 1);
        assert!(
            settings_apply_argv(&unknown)
                .unwrap_err()
                .contains("admit_all"),
            "an unknown key refuses by name"
        );
    }

    /// Exit mapping uses the code only: 0 applies, 3 conflicts, 2
    /// refuses, and timeouts resolve unknown with no code.
    #[test]
    fn exit_mapping_uses_the_code_only() {
        let applied = CliOutput {
            code: Some(0),
            status_success: true,
            stdout: "settings applied for /drives/alpha: 1 bound(s) configured\n".to_string(),
            stderr: String::new(),
            timed_out: false,
        };
        assert!(matches!(
            map_settings_exit(&applied).result,
            SettingsApplyResult::Applied { .. }
        ));
        // The success line never decides: even an empty stdout with
        // exit 0 applies, and the re-read says what stands.
        let conflict = CliOutput {
            code: Some(3),
            status_success: false,
            stdout: String::new(),
            stderr: "error: settings changed under the transaction: settings file changed between read and commit\n".to_string(),
            timed_out: false,
        };
        match map_settings_exit(&conflict).result {
            SettingsApplyResult::Conflict { reason } => {
                assert!(reason.contains("changed under"), "detail: {reason}")
            }
            other => panic!("exit 3 conflicts: {other:?}"),
        }
        let refused = CliOutput {
            code: Some(2),
            status_success: false,
            stdout: String::new(),
            stderr: "error: settings apply refused: unknown settings key \"admit_all\"\n"
                .to_string(),
            timed_out: false,
        };
        assert!(matches!(
            map_settings_exit(&refused).result,
            SettingsApplyResult::Refused { .. }
        ));
        // Exit 78 (a held drive — impossible on this path, but the
        // mapper must not invent a meaning) refuses generically.
        let held = CliOutput {
            code: Some(78),
            status_success: false,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
        };
        assert!(matches!(
            map_settings_exit(&held).result,
            SettingsApplyResult::Refused { .. }
        ));
        let wedged = CliOutput {
            code: None,
            status_success: false,
            stdout: String::new(),
            stderr: "CLI did not complete".to_string(),
            timed_out: true,
        };
        let outcome = map_settings_exit(&wedged);
        assert!(matches!(outcome.result, SettingsApplyResult::Failed { .. }));
        assert_eq!(outcome.code, None);
    }

    /// A fake CLI behind the adapter proves the full run: version
    /// check, argv, exit-code map — exit 3 through a shell script,
    /// like the provision wedge test.
    #[test]
    fn fake_cli_exit_codes_map_end_to_end() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-settings-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, code) in [("conflict", 3), ("refused", 2)] {
            let cli = dir.join(name);
            let version = env!("CARGO_PKG_VERSION");
            std::fs::write(
                &cli,
                format!(
                    "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nexit {code}\n"
                ),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let outcome = run_settings_apply(&cli, &request(), Duration::from_secs(10));
            match name {
                "conflict" => assert!(
                    matches!(outcome.result, SettingsApplyResult::Conflict { .. }),
                    "exit 3 conflicts: {:?}",
                    outcome.result
                ),
                _ => assert!(
                    matches!(outcome.result, SettingsApplyResult::Refused { .. }),
                    "exit 2 refuses: {:?}",
                    outcome.result
                ),
            }
        }
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }
}
