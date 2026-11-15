//! Per-drive service observation: which of the three "unavailable"
//! causes holds, without supervising anything.
//!
//! The unavailable screen used to show one state ("Owner unavailable"
//! plus Retry) for three different causes: the service definition was
//! never installed, it is installed but stopped, or it is running but
//! the socket is absent (wedged). The probes here distinguish those
//! causes by reading the service definition file and asking the OS
//! service manager (`launchctl` / `systemctl`) whether the unit is
//! loaded — observation only, never supervision. OD-06-E stands: the
//! UI never owns a drive and the OS service manager stays the parent.
//!
//! Layering: `std::process` only, like the provisioning adapter — no
//! new Cargo edge, so the `wyrd-desktop` dependency-DAG policy is
//! untouched. Definition paths mirror the daemon's
//! `service_definition_path` (macOS `~/Library/LaunchAgents/…`,
//! Linux `$XDG_CONFIG_HOME/systemd/user/…`); the mirror is
//! observation-only and documented where it can drift.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long one manager probe may run. A status answer is instant;
/// the bound only fires on a wedged manager, and expiry reports
/// `Unknown` — never a lifecycle state.
const SERVICE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Which unavailable cause holds for one drive. Transport-level
/// availability stays where it is (`DriveAvailability`); this is the
/// service fact beside it. All variants carry the evidence string so
/// the frontend renders without parsing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum ServiceState {
    /// No service definition file exists for this drive: the drive
    /// was registered without a managed service (`--no-service`), or
    /// the definition was removed. A stable first-class state, never
    /// a failure — the recovery is reinstall, never auto-start, and
    /// never a terminal.
    NotInstalled { detail: String },
    /// A definition exists but the manager reports it not
    /// loaded/active: the service is installed but stopped. A stable
    /// first-class state — a deliberately disabled drive stays here
    /// until the person starts it; the UI never auto-starts.
    Stopped { detail: String },
    /// The manager reports the unit loaded/active but the owner
    /// socket is absent: the service is wedged (crashed without the
    /// manager retrying, or bound elsewhere). Points at the service
    /// log, never at the drive contents.
    Wedged { detail: String },
    /// The manager reports the unit loaded/active, or the owner
    /// answered this session (availability already proved it). Whether
    /// the owner answers is the availability verdict's job, not this
    /// enum's — a loaded unit with a live socket is the ready path.
    Running { detail: String },
    /// The service manager did not answer: probe timeout or an
    /// unexpected tool status. Retry the probe — the drive and its
    /// service are unchanged, and this is not evidence about either.
    Unknown { detail: String },
    /// No service manager tool exists in this build (dev `cargo`
    /// binaries beside no definitions, or a platform without
    /// `launchctl`/`systemctl`). Dev-only honesty: "no managed
    /// service in this build, start the owner manually" instead of a
    /// dead Retry. Never a shipped state — shipped builds always
    /// supervise the owner through the OS service manager.
    Unsupported { detail: String },
}

/// Where the per-drive service definition lives on this platform.
/// Mirrors the daemon's `service_definition_path`
/// (`wyrd-daemon/src/bootstrap.rs`): the launchd agent property list
/// on macOS, the systemd user unit elsewhere. Observation only — the
/// installer and the CLI remain the writers.
pub fn service_definition_path(service: &str) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("Library/LaunchAgents");
        dir.join(format!("md.enesis.wyrd.{service}.plist"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join(".config")
            })
            .join("systemd/user");
        dir.join(format!("wyrd-{service}.service"))
    }
}

/// Whether the unit is loaded/active according to the OS service
/// manager. Exit codes are the only signal (output is never parsed):
/// 0 means loaded/active; documented not-loaded codes mean stopped;
/// anything else — missing tool, timeout, unexpected status — is no
/// answer, never stopped.
fn manager_reports_running(service: &str) -> ManagerAnswer {
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{service}");
        // `launchctl list` reports a not-loaded agent with a non-zero
        // status; there is no finer documented code to split tool
        // failures from, so any completed non-zero answer reads as
        // stopped while spawn and timeout stay no-answer below.
        match run_probe(&["launchctl", "list", &label]) {
            ProbeOutcome::Exit(0) => ManagerAnswer::Running,
            ProbeOutcome::Exit(code) => ManagerAnswer::NotRunning { code: Some(code) },
            ProbeOutcome::MissingTool => ManagerAnswer::NoAnswer {
                reason: NoAnswerReason::MissingTool,
            },
            ProbeOutcome::Timeout => ManagerAnswer::NoAnswer {
                reason: NoAnswerReason::Timeout,
            },
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = format!("wyrd-{service}.service");
        // LSB codes via systemd: 0 active, 3 not active, 4 no such
        // unit (installed on disk but never loaded — still stopped).
        // Anything else (bus failure exit 1, unexpected statuses)
        // is tool failure, never a lifecycle state.
        match run_probe(&["systemctl", "--user", "is-active", "--quiet", &unit]) {
            ProbeOutcome::Exit(0) => ManagerAnswer::Running,
            ProbeOutcome::Exit(3 | 4) => ManagerAnswer::NotRunning { code: None },
            ProbeOutcome::Exit(code) => ManagerAnswer::NoAnswer {
                reason: NoAnswerReason::UnexpectedStatus { code },
            },
            ProbeOutcome::MissingTool => ManagerAnswer::NoAnswer {
                reason: NoAnswerReason::MissingTool,
            },
            ProbeOutcome::Timeout => ManagerAnswer::NoAnswer {
                reason: NoAnswerReason::Timeout,
            },
        }
    }
}

/// What one manager probe concluded. Exit codes carry the verdict;
/// anything that prevented an answer carries its reason instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagerAnswer {
    Running,
    NotRunning { code: Option<i32> },
    NoAnswer { reason: NoAnswerReason },
}

/// Why a manager probe produced no verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoAnswerReason {
    /// The tool binary could not be spawned (absent in this build).
    MissingTool,
    /// The tool did not answer within the bound.
    Timeout,
    /// The tool answered with an undocumented status (e.g. a bus
    /// failure): not a lifecycle state. Only constructed on the
    /// systemd branch, where exit codes beyond 0/3/4 carry meaning;
    /// `launchctl` reports every not-loaded agent non-zero, so macOS
    /// maps those to not-loaded instead.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    UnexpectedStatus { code: i32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeOutcome {
    Exit(i32),
    MissingTool,
    Timeout,
}

/// Run one manager probe argv with a hard bound. Never parses
/// output: only completion and the exit status decide. A missing
/// binary is `MissingTool`, an overrun is `Timeout` — both no-answer
/// by construction, never a lifecycle state.
fn run_probe(argv: &[&str]) -> ProbeOutcome {
    let (program, args) = match argv.split_first() {
        Some((program, args)) => (*program, args),
        None => return ProbeOutcome::MissingTool,
    };
    let mut child = match std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return ProbeOutcome::MissingTool,
    };
    let deadline = std::time::Instant::now() + SERVICE_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // `code()` is `None` on signal death: without a status
                // there is no verdict, so signal death reads as a
                // timeout-class no-answer rather than a lifecycle
                // state.
                return match status.code() {
                    Some(code) => ProbeOutcome::Exit(code),
                    None => ProbeOutcome::Timeout,
                };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return ProbeOutcome::Timeout;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return ProbeOutcome::Timeout,
        }
    }
}

/// Observe one drive's service state: definition file plus manager
/// verdict plus socket presence. Pure observation — never starts,
/// stops, installs, or removes anything.
///
/// `socket_present` is the caller's socket fact (the registry probe
/// already connected): `true` where the owner answered or refused
/// terminally (transport exists), `false` where no transport
/// answered. The function never touches the socket itself, so the
/// service probe and the socket probe stay independent observations
/// paired by the caller.
pub fn probe_service_state(service: &str, socket_present: bool) -> ServiceState {
    let definition = service_definition_path(service);
    probe_service_state_at(service, &definition, socket_present)
}

/// The test seam: definition path injected, manager reached through
/// the real `PATH` (tests inject a fake `launchctl`/`systemctl`).
fn probe_service_state_at(service: &str, definition: &Path, socket_present: bool) -> ServiceState {
    probe_with(service, definition, socket_present, manager_reports_running)
}

/// The runner seam: production passes [`manager_reports_running`];
/// tests pass a fake verdict without touching `PATH` (process-wide
/// `PATH` mutation races parallel tests, so the fake never goes
/// through the environment).
fn probe_with(
    service: &str,
    definition: &Path,
    socket_present: bool,
    manager: impl Fn(&str) -> ManagerAnswer,
) -> ServiceState {
    if !definition.exists() {
        return ServiceState::NotInstalled {
            detail: format!(
                "no service definition installed ({} missing): the drive is registered without a managed service — reinstall the definition to supervise it",
                definition.display()
            ),
        };
    }
    match manager(service) {
        ManagerAnswer::NoAnswer {
            reason: NoAnswerReason::MissingTool,
        } => ServiceState::Unsupported {
            detail: "no service manager tool in this build: no managed service verdict here — start the owner manually. Shipped builds always supervise the owner through the OS service manager".to_string(),
        },
        ManagerAnswer::NoAnswer { .. } => ServiceState::Unknown {
            detail: "the service manager did not answer (probe timeout or unexpected tool status): retry the probe — the drive and its service are unchanged".to_string(),
        },
        ManagerAnswer::NotRunning { .. } => ServiceState::Stopped {
            detail: format!(
                "service definition installed but the manager reports it stopped ({} present, unit not loaded): start the installed service — a stopped drive is a stable state, never auto-started",
                definition.display()
            ),
        },
        ManagerAnswer::Running if !socket_present => ServiceState::Wedged {
            detail: "the service manager reports the owner running but its socket is absent: the owner is wedged — check the drive's service log (service.log in the drive directory on macOS, the user journal on Linux), then restart the service".to_string(),
        },
        ManagerAnswer::Running => ServiceState::Running {
            detail: "the service manager reports the owner running".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing definition file is `NotInstalled` without touching
    /// the manager: the probe never supervises, and a drive
    /// registered with `--no-service` reads as a stable state.
    #[test]
    fn missing_definition_is_not_installed_without_manager_contact() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-service-{}-nodef", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let definition = dir.join("md.enesis.wyrd.drive-x.plist");
        assert!(!definition.exists());
        match probe_service_state_at("drive-x", &definition, false) {
            ServiceState::NotInstalled { detail } => {
                assert!(
                    detail.contains("without a managed service"),
                    "detail: {detail}"
                );
            }
            other => panic!("a missing definition is NotInstalled: {other:?}"),
        }
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// Definition paths mirror the daemon's layout per platform:
    /// macOS launchd agents, elsewhere systemd user units.
    #[test]
    fn definition_path_mirrors_the_daemon_layout() {
        let path = service_definition_path("drive-alpha");
        #[cfg(target_os = "macos")]
        assert!(
            path.to_string_lossy()
                .ends_with("Library/LaunchAgents/md.enesis.wyrd.drive-alpha.plist"),
            "macOS agent path: {}",
            path.display()
        );
        #[cfg(not(target_os = "macos"))]
        assert!(
            path.to_string_lossy()
                .ends_with("systemd/user/wyrd-drive-alpha.service"),
            "systemd unit path: {}",
            path.display()
        );
    }

    /// The manager verdict maps onto `Stopped` / `Wedged` /
    /// `Running`: stopped where the manager reports not-loaded, and
    /// the socket fact splits loaded into wedged vs running. The
    /// manager is a fake closure — no `PATH` mutation (process-wide
    /// env races parallel tests), no real service touched.
    #[test]
    fn manager_verdict_maps_onto_stopped_wedged_running() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-service-{}-manager",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let definition = dir.join("definition.plist");
        std::fs::write(&definition, "installed").unwrap();

        // Manager down with a definition present: stopped.
        assert!(matches!(
            probe_with("drive-x", &definition, false, |_| {
                ManagerAnswer::NotRunning { code: None }
            }),
            ServiceState::Stopped { .. }
        ));
        // Manager up without a socket: wedged, pointing at the
        // service log — never at the drive contents.
        match probe_with("drive-x", &definition, false, |_| ManagerAnswer::Running) {
            ServiceState::Wedged { detail } => {
                assert!(detail.contains("service log"), "detail: {detail}");
                assert!(
                    detail.contains("user journal"),
                    "detail names the Linux journal too: {detail}"
                );
            }
            other => panic!("a loaded unit without a socket is wedged: {other:?}"),
        }
        // Manager up with a socket: running (availability owns liveness).
        assert!(matches!(
            probe_with("drive-x", &definition, true, |_| ManagerAnswer::Running),
            ServiceState::Running { .. }
        ));
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// A missing manager tool is `Unsupported`, never stopped: dev-build
    /// honesty — "no managed service in this build" — instead of a
    /// state the recovery would misread. A timeout or an unexpected
    /// tool status is `Unknown` instead: the drive and its service
    /// are unchanged, and a manual start is not the move.
    #[test]
    fn missing_tool_is_unsupported_while_timeout_is_unknown() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-service-{}-notool",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let definition = dir.join("definition.plist");
        std::fs::write(&definition, "installed").unwrap();
        match probe_with("drive-x", &definition, false, |_| ManagerAnswer::NoAnswer {
            reason: NoAnswerReason::MissingTool,
        }) {
            ServiceState::Unsupported { detail } => {
                assert!(detail.contains("no managed service"), "detail: {detail}");
            }
            other => panic!("a missing manager tool is Unsupported: {other:?}"),
        }
        for reason in [
            NoAnswerReason::Timeout,
            NoAnswerReason::UnexpectedStatus { code: 1 },
        ] {
            match probe_with("drive-x", &definition, false, |_| ManagerAnswer::NoAnswer {
                reason,
            }) {
                ServiceState::Unknown { detail } => {
                    assert!(detail.contains("did not answer"), "detail: {detail}");
                    assert!(
                        !detail.contains("manually"),
                        "a timeout names no manual start: {detail}"
                    );
                }
                other => panic!("a timeout is Unknown: {other:?}"),
            }
        }
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }
}
