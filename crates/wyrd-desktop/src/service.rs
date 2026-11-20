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
    NotInstalled {
        /// Evidence string naming the missing definition file.
        detail: String,
    },
    /// A definition exists but the manager reports it not
    /// loaded/active: the service is installed but stopped. A stable
    /// first-class state — a deliberately disabled drive stays here
    /// until the person starts it; the UI never auto-starts.
    Stopped {
        /// Evidence string naming the present definition and the not-loaded unit.
        detail: String,
    },
    /// The manager reports the unit loaded/active but the owner
    /// socket is absent: the service is wedged (crashed without the
    /// manager retrying, or bound elsewhere). Points at the service
    /// log, never at the drive contents.
    Wedged {
        /// Evidence string pointing at the service log, never the drive contents.
        detail: String,
    },
    /// The manager reports the unit loaded/active, or the owner
    /// answered this session (availability already proved it). Whether
    /// the owner answers is the availability verdict's job, not this
    /// enum's — a loaded unit with a live socket is the ready path.
    Running {
        /// Evidence string: the manager reports the owner running.
        detail: String,
    },
    /// The service manager did not answer: probe timeout or an
    /// unexpected tool status. Retry the probe — the drive and its
    /// service are unchanged, and this is not evidence about either.
    Unknown {
        /// Evidence string: the manager did not answer; retry the probe.
        detail: String,
    },
    /// No service manager tool exists in this build (dev `cargo`
    /// binaries beside no definitions, or a platform without
    /// `launchctl`/`systemctl`). Dev-only honesty: "no managed
    /// service in this build, start the owner manually" instead of a
    /// dead Retry. Never a shipped state — shipped builds always
    /// supervise the owner through the OS service manager.
    Unsupported {
        /// Evidence string: no managed service verdict in this build.
        detail: String,
    },
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

/// Persistent login-start preference as the service manager
/// records it: whether the unit starts at login. Independent of
/// [`ServiceState`] — a manually started service may run while
/// disabled at login, so the two observations pair instead of
/// merging (no `Stopped*` variants). All variants carry the
/// evidence string so the frontend renders without parsing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "enablement", rename_all = "lowercase")]
pub enum EnablementState {
    /// The manager starts the unit at login.
    Enabled {
        /// Evidence string naming the record.
        detail: String,
    },
    /// The manager does not start the unit at login.
    Disabled {
        /// Evidence string naming the record.
        detail: String,
    },
    /// The manager did not answer, or keeps no record for this
    /// drive: retry the probe. Not evidence about login behavior —
    /// a session-posture drive (no definition) also reads
    /// `Unknown`, since there is no unit to enable.
    Unknown {
        /// Evidence string: the manager did not answer, or no
        /// record exists; retry the probe.
        detail: String,
    },
}

/// Run one manager probe and capture its stdout with the same hard
/// bound as [`run_probe`]. A missing binary or an overrun is
/// `None` — no answer, never an enablement state. A completed
/// non-zero answer still yields its stdout: `is-enabled` exits
/// non-zero for a disabled unit, and the word decides (see
/// [`parse_is_enabled_output`]).
fn run_probe_capture(argv: &[&str]) -> Option<String> {
    let (program, args) = match argv.split_first() {
        Some((program, args)) => (*program, args),
        None => return None,
    };
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + SERVICE_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let mut stdout = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    use std::io::Read as _;
                    let _ = pipe.read_to_string(&mut stdout);
                }
                let _ = child.wait();
                return Some(stdout);
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Map a `launchctl print-disabled` dump onto one drive's record:
/// the `"label" => enabled|disabled` line decides; an absent
/// record is `Unknown` (never `Disabled`); garbage reads
/// `Unknown`. The label match includes the closing quote so
/// `drive-foo` never matches a `drive-foo-bar` record.
///
/// Mirror: the daemon holds the same parser beside its own probe
/// (`wyrd-daemon/src/bootstrap.rs`, `parse_print_disabled`) —
/// layering keeps manager reach out of the desktop's dependency
/// closure, so the two copies evolve together by contract, not by
/// import. The Unknown-on-absent/garbage rule is the shared
/// invariant; evidence wording differs per surface.
#[cfg(any(test, target_os = "macos"))]
fn parse_print_disabled_output(output: &str, service: &str) -> EnablementState {
    let prefix = format!("\"md.enesis.wyrd.{service}\" => ");
    for line in output.lines() {
        if let Some(value) = line.trim().strip_prefix(prefix.as_str()) {
            let detail = format!("login-start record for this drive reads {}", value.trim());
            return match value.trim() {
                "enabled" => EnablementState::Enabled { detail },
                "disabled" => EnablementState::Disabled { detail },
                _ => EnablementState::Unknown {
                    detail: format!("{detail} — not a known value; retry the probe"),
                },
            };
        }
    }
    EnablementState::Unknown {
        detail: "the service manager keeps no login-start record for this drive: retry the probe — this is not evidence the drive is disabled".to_string(),
    }
}

/// Map a `systemctl --user is-enabled` word onto the preference:
/// `enabled`/`disabled`/`masked` decide; any other word (static,
/// alias, indirect) or a bus failure printing no word reads
/// `Unknown`, never `Disabled`.
///
/// Mirror: the daemon holds the same word map beside its own probe
/// (`wyrd-daemon/src/bootstrap.rs`, `parse_is_enabled`) — same
/// layering reason as above; the word map is the shared invariant.
#[cfg(any(test, not(target_os = "macos")))]
fn parse_is_enabled_output(output: &str) -> EnablementState {
    let word = output.trim();
    let detail = format!("login-start record for this drive reads {word}");
    match word {
        "enabled" => EnablementState::Enabled { detail },
        "disabled" | "masked" => EnablementState::Disabled { detail },
        _ => EnablementState::Unknown {
            detail: format!("{detail} — not a known value; retry the probe"),
        },
    }
}

/// Observe one drive's login-start preference: the manager record
/// for the label/unit, nothing else. Pure observation — never
/// starts, stops, enables, or installs anything.
pub fn probe_enablement(service: &str) -> EnablementState {
    probe_enablement_with_fetcher(service, fetch_enablement_output)
}

/// The test seam: production fetches through the real `PATH`;
/// tests inject a fake dump without touching the environment.
fn probe_enablement_with_fetcher(
    service: &str,
    fetch: impl Fn(&str) -> Option<String>,
) -> EnablementState {
    match fetch(service) {
        Some(output) => parse_enablement_output(&output, service),
        None => EnablementState::Unknown {
            detail: "the service manager did not answer the enablement probe (probe timeout or missing tool): retry the probe — the drive and its service are unchanged".to_string(),
        },
    }
}

#[cfg(target_os = "macos")]
fn parse_enablement_output(output: &str, service: &str) -> EnablementState {
    parse_print_disabled_output(output, service)
}

#[cfg(not(target_os = "macos"))]
fn parse_enablement_output(output: &str, _service: &str) -> EnablementState {
    parse_is_enabled_output(output)
}

/// Fetch one drive's raw enablement dump from the manager, or
/// `None` where the manager did not answer.
#[cfg(target_os = "macos")]
fn fetch_enablement_output(_service: &str) -> Option<String> {
    let domain = std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
            } else {
                None
            }
        });
    let domain = format!("user/{}", domain?);
    run_probe_capture(&["launchctl", "print-disabled", &domain])
}

/// Fetch one unit's raw `is-enabled` word, or `None` where the
/// manager did not answer. The exit status is ignored: a disabled
/// unit exits non-zero, and the word decides.
#[cfg(not(target_os = "macos"))]
fn fetch_enablement_output(service: &str) -> Option<String> {
    let unit = format!("wyrd-{service}.service");
    run_probe_capture(&["systemctl", "--user", "is-enabled", &unit])
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
                "no service definition installed ({} missing): the drive is registered without a managed service — unlock the drive for this session with its passphrase, or reinstall the definition to supervise it",
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

    /// The enablement records parse by value, never by exit
    /// code: a present record maps to its value, an absent record
    /// or garbage reads Unknown (never Disabled), and a label never
    /// matches a longer label's record.
    #[test]
    fn enablement_records_parse_by_value() {
        // macOS `print-disabled` shape, as observed live.
        let dump = "\tdisabled services = {\n\t\t\"md.enesis.wyrd.drive-alpha\" => enabled\n\t\t\"md.enesis.wyrd.drive-foobar\" => disabled\n\t}\n";
        assert!(matches!(
            parse_print_disabled_output(dump, "drive-alpha"),
            EnablementState::Enabled { .. }
        ));
        assert!(matches!(
            parse_print_disabled_output(dump, "drive-foobar"),
            EnablementState::Disabled { .. }
        ));
        // Prefix collision: `drive-foo` must not read the
        // `drive-foobar` record; the closing quote guards it.
        assert!(matches!(
            parse_print_disabled_output(dump, "drive-foo"),
            EnablementState::Unknown { .. }
        ));
        assert!(matches!(
            parse_print_disabled_output(dump, "drive-missing"),
            EnablementState::Unknown { .. }
        ));
        match parse_print_disabled_output(dump, "drive-missing") {
            EnablementState::Unknown { detail } => {
                assert!(detail.contains("no login-start record"), "detail: {detail}")
            }
            other => panic!("an absent record is Unknown: {other:?}"),
        }
        // Garbage value: Unknown, never Disabled.
        assert!(matches!(
            parse_print_disabled_output("\"md.enesis.wyrd.drive-x\" => maybe\n", "drive-x"),
            EnablementState::Unknown { .. }
        ));
        // Linux `is-enabled` words.
        assert!(matches!(
            parse_is_enabled_output("enabled\n"),
            EnablementState::Enabled { .. }
        ));
        assert!(matches!(
            parse_is_enabled_output("disabled\n"),
            EnablementState::Disabled { .. }
        ));
        assert!(matches!(
            parse_is_enabled_output("masked\n"),
            EnablementState::Disabled { .. }
        ));
        assert!(matches!(
            parse_is_enabled_output("static\n"),
            EnablementState::Unknown { .. }
        ));
        // A bus failure prints no word: Unknown, never Disabled.
        assert!(matches!(
            parse_is_enabled_output("Failed to connect to bus.\n"),
            EnablementState::Unknown { .. }
        ));
        assert!(matches!(
            parse_is_enabled_output(""),
            EnablementState::Unknown { .. }
        ));
    }

    /// The enablement probe maps fetch outcomes onto states: a
    /// dump parses, a missing tool or timeout reads Unknown. The
    /// fetch is injected — no `PATH` mutation, which would race
    /// parallel tests.
    #[test]
    fn enablement_probe_maps_fetch_onto_states() {
        // The disabled fixture is platform-shaped: the macOS probe
        // parses the `print-disabled` record, the Linux probe parses
        // the `is-enabled` word — each rejects the other's shape as
        // Unknown, so one shared dump cannot serve both.
        #[cfg(target_os = "macos")]
        let dump = "\"md.enesis.wyrd.drive-x\" => disabled\n";
        #[cfg(not(target_os = "macos"))]
        let dump = "disabled\n";
        #[cfg(target_os = "macos")]
        let enabled_word = "\"md.enesis.wyrd.drive-x\" => enabled\n";
        #[cfg(not(target_os = "macos"))]
        let enabled_word = "enabled\n";
        assert!(matches!(
            probe_enablement_with_fetcher("drive-x", |_| Some(enabled_word.to_string())),
            EnablementState::Enabled { .. }
        ));
        assert!(matches!(
            probe_enablement_with_fetcher("drive-x", |_| Some(dump.to_string())),
            EnablementState::Disabled { .. }
        ));
        match probe_enablement_with_fetcher("drive-x", |_| None) {
            EnablementState::Unknown { detail } => {
                assert!(detail.contains("did not answer"), "detail: {detail}")
            }
            other => panic!("no answer is Unknown: {other:?}"),
        }
    }

    /// A missing manager tool captures nothing: the probe has no
    /// answer, never a state.
    #[test]
    fn missing_probe_tool_captures_nothing() {
        assert_eq!(
            run_probe_capture(&["wyrd-definitely-missing-binary-xyz"]),
            None,
            "a missing tool is no answer"
        );
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
