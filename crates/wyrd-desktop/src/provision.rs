//! Runtime provisioning through the installed CLI: the constrained
//! adapter the multi-drive commitment requires.
//!
//! Authority contract (issue `nevent1qqsx9357fqqfup9g7kurf7gdcgtjf8qaceay3a90wzxjw6sresg69eqpz9mhxue69uhkwunpwdczuap49eehgcl6hz9`,
//! architecture in `docs/design/desktop-shell.md`):
//!
//! - The desktop launches operations but never becomes the source of
//!   truth. It invokes the installed, version-matched CLI through this
//!   adapter with structured arguments (argv arrays, never
//!   shell-constructed), waits for process completion, then re-reads
//!   the registry through the existing observation path and probes the
//!   owner socket independently. Observed state determines the
//!   display; exit codes indicate operation outcomes, never
//!   authoritative lifecycle state.
//! - Secret contents never cross IPC or argv (paths only).
//! - Operations are non-interactive, with explicit exit semantics
//!   (0 success, 2 usage-or-expected-failure, 78 terminal held-drive).
//! - No overlapping lifecycle operations on the same drive: a second
//!   request while one is in flight is refused with an explicit
//!   per-drive busy state, never queued.
//! - No implicit retries of destructive operations: timeout or process
//!   failure triggers observation, never re-execution.
//! - Version mismatch is a hard failure, never silent fallback.
//!
//! Layering: `std::process` only — no new Cargo edge, so the
//! `wyrd-desktop` dependency-DAG policy is untouched. CLI stdout and
//! stderr travel as opaque display detail and are never parsed for
//! lifecycle facts; the only stable signal is the exit code (0
//! success, 2 usage-or-expected-failure, 78 terminal held-drive),
//! paired with independent re-observation.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long one CLI lifecycle operation may run before the adapter
/// gives up waiting. Expiry triggers observation, never re-execution:
/// the CLI may have committed before the timeout, so the outcome is
/// unknown and only the re-read decides what the display says.
pub const PROVISION_TIMEOUT: Duration = Duration::from_secs(120);

/// Exit codes the CLI contract pins (`main.rs`, `docs/design/cli.md`):
/// 0 success (including `--help`/`--version` and the non-owner leave
/// refusal, which authors nothing), 78 terminal held-drive (another
/// process holds the store — the supervisor must not retry), 2
/// everything else.
const EXIT_HELD_DRIVE: i32 = 78;

/// Create (no invitation) or join (sealed invitation file) one drive.
#[derive(Debug, Clone)]
pub struct ProvisionRequest {
    /// Registry entry name, 1–58 chars of `[a-z0-9-]`.
    pub name: String,
    /// Absolute drive directory, exact string (shared drive-identity rule).
    pub drive_dir: PathBuf,
    /// Relays for the managed service. Empty is refused for managed
    /// services before anything is created.
    pub relays: Vec<String>,
    /// Credential *paths*, never contents.
    pub identity_file: PathBuf,
    pub passphrase_file: PathBuf,
    /// `None` = create, `Some(path)` = join from that invitation file.
    pub invitation_file: Option<PathBuf>,
    /// Explicit registry path (tests); `None` = platform default.
    pub registry_path: Option<PathBuf>,
    /// Explicit owner/supervise binaries (tests / non-standard layout);
    /// `None` = sibling-of-CLI default the CLI applies.
    pub owner_bin: Option<PathBuf>,
    pub supervise_bin: Option<PathBuf>,
    /// `false` (default) installs, enables, and starts the per-drive
    /// service; `true` registers only.
    pub no_service: bool,
    /// Service mode display string. The platform default is `vault`;
    /// any other value is refused with a pointer at the installer
    /// follow-up (registry-managed mount mode is not provisionable
    /// through this adapter yet — mirrored from the registry
    /// validator, which skips non-vault entries loudly).
    pub mode: String,
}

/// Leave the drive: end this device's participation where the trust
/// contract admits it. A non-owner authors nothing (the CLI reports
/// the refusal at exit 0); a sole owner freezes the drive permanently
/// and needs `acknowledged = true` (`--yes` at the CLI boundary — a
/// frontend omitting the dialog cannot bypass the requirement).
#[derive(Debug, Clone)]
pub struct LeaveRequest {
    pub drive_dir: PathBuf,
    pub identity_file: PathBuf,
    pub passphrase_file: PathBuf,
    /// Sole-owner terminal confirmation. The UI explains the
    /// consequences; this flag carries the acknowledgement to the CLI.
    pub acknowledged: bool,
}

/// Reconfigure a registered drive's supervision in place: stop and
/// clear the old service definition, rewrite the registry entry,
/// reinstall and restart when managed, and verify the socket.
/// `relays` replaces the registered set when non-empty and keeps it
/// when empty (a credential-path-only reconfigure never clears
/// relays by accident); `mode` is the platform default `vault` and
/// any other value refuses with the installer-follow-up pointer
/// before any process spawns, like creation.
#[derive(Debug, Clone)]
pub struct ReconfigureRequest {
    pub drive_dir: PathBuf,
    pub relays: Vec<String>,
    /// Credential *paths*, never contents.
    pub identity_file: PathBuf,
    pub passphrase_file: PathBuf,
    /// Explicit registry path (tests); `None` = platform default.
    pub registry_path: Option<PathBuf>,
    /// Explicit owner/supervise binaries (tests / non-standard layout);
    /// `None` = sibling-of-CLI default the CLI applies.
    pub owner_bin: Option<PathBuf>,
    pub supervise_bin: Option<PathBuf>,
    /// `false` (default) disables the old service, rewrites the
    /// entry, and reinstalls, enables, and restarts; `true`
    /// rewrites without installing any OS service.
    pub no_service: bool,
    /// Service mode display string. The platform default is `vault`;
    /// any other value is refused with a pointer at the installer
    /// follow-up (registry-managed mount mode is not provisionable
    /// through this adapter yet — mirrored from the registry
    /// validator, which skips non-vault entries loudly).
    pub mode: String,
}

/// The adapter's verdict on the *operation*. Never lifecycle state:
/// the caller pairs every variant with a registry re-read, a socket
/// probe, and (for the operated drive) a membership observation, and
/// only that observed state decides the display.
///
/// Deliberately coarse, per the settled contract
/// (`docs/design/desktop-shell.md`): only the exit code decides.
/// CLI stdout/stderr travel as opaque display detail and are never
/// parsed for lifecycle facts — not even the fixed success line.
/// Exit 0 reports no error (including the non-owner leave refusal,
/// which authors nothing); exit 2 reports an expected failure; 78
/// is the terminal held-drive.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "result", rename_all = "lowercase")]
pub enum ProvisionResult {
    /// Exit 0: the CLI reported no error. What actually happened is
    /// decided by the paired observation alone — this never claims a
    /// lifecycle state.
    Completed { detail: String },
    /// Exit 2 or an adapter-side refusal: usage, expected failure,
    /// or a validation refusal that never spawned a process. Opaque
    /// detail; the paired observation says what actually exists.
    ExpectedFailure { detail: String },
    /// Exit 78: another process holds the drive. Terminal — the
    /// supervisor must not retry.
    HeldDrive { detail: String },
    /// CLI binary missing, version-mismatched, or unparsable
    /// `__build_info`: hard failure, never silent fallback.
    VersionMismatch { reason: String },
    /// A lifecycle operation is already in flight on this drive.
    Busy { drive_dir: String },
    /// Timeout, spawn failure, or signal death: the outcome is
    /// unknown. Triggers observation, never re-execution.
    Unknown { detail: String },
}

/// One completed operation: the opaque outcome plus the raw CLI
/// stderr (display detail, never parsed) and the exit code observed.
#[derive(Debug, Clone)]
pub struct ProvisionOutcome {
    pub result: ProvisionResult,
    pub stderr: String,
    pub code: Option<i32>,
}

/// Resolve the version-matched CLI binary: an explicit path (tests,
/// non-standard layouts) or the `wyrd` sibling of the running desktop
/// binary — the fixed co-installation the installer ships. Never a
/// `PATH` search: a silent fallback to an arbitrary binary would
/// defeat the version match.
pub fn resolve_cli_bin(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    let exe = std::env::current_exe()
        .map_err(|error| format!("cannot locate the desktop binary: {error}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "the desktop binary has no parent directory".to_string())?;
    Ok(dir.join("wyrd"))
}

/// Parse `wyrd __build_info` stdout. Stable lines (`update/preflight.rs`):
/// `update build: <semver>`, `update envelope: 0x<NN>`,
/// `update control: 0x<NN>`. Returns the build version; envelope and
/// control versions ride along for display only — the desktop has no
/// sync edge to interpret them, so drift there is caught by the
/// owner-interface `Incompatible` verdict at verification, not here.
pub fn parse_build_info(stdout: &str) -> Result<String, String> {
    for line in stdout.lines() {
        if let Some(version) = line.strip_prefix("update build: ") {
            let version = version.trim().to_string();
            if version.is_empty() {
                return Err("`__build_info` reported an empty build version".to_string());
            }
            return Ok(version);
        }
    }
    Err("`__build_info` reported no `update build:` line".to_string())
}

/// The desktop's own build version: the workspace version both halves
/// ship from. A mismatch is a hard failure — never a silent fallback
/// to the arbitrary binary.
pub fn desktop_build_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Check the CLI is the version-matched pair: run
/// `<cli> __build_info` (lock-free, touches no store) and require the
/// reported build version to equal the desktop's own. Returns the CLI
/// binary on success for the caller's argv run.
pub fn check_cli_version(cli_bin: &Path, timeout: Duration) -> Result<PathBuf, ProvisionResult> {
    let output = run_cli(cli_bin, &["__build_info".into()], timeout);
    if output.timed_out {
        return Err(ProvisionResult::Unknown {
            detail: format!("version check against {} timed out", cli_bin.display()),
        });
    }
    if !output.status_success {
        return Err(ProvisionResult::VersionMismatch {
            reason: format!(
                "`__build_info` failed ({}): {}",
                describe_code(output.code),
                truncate(&output.stderr, 300)
            ),
        });
    }
    match parse_build_info(&output.stdout) {
        Ok(build) if build == desktop_build_version() => Ok(cli_bin.to_owned()),
        Ok(build) => Err(ProvisionResult::VersionMismatch {
            reason: format!(
                "CLI build {build} does not match desktop {} — reinstall the matched pair",
                desktop_build_version()
            ),
        }),
        Err(reason) => Err(ProvisionResult::VersionMismatch { reason }),
    }
}

/// Pure argv builder for `wyrd bootstrap …`: every argument is a
/// separate argv element, never shell-constructed. Mirrors
/// `crates/wyrd-cli/src/args.rs` (`Command::Bootstrap`).
pub fn bootstrap_argv(request: &ProvisionRequest) -> Result<Vec<OsString>, String> {
    if request.mode != "vault" {
        return Err(format!(
            "unknown service mode {:?}: only `vault` is provisionable through this adapter; registry-managed mount mode travels with the installer follow-up",
            request.mode
        ));
    }
    let mut argv: Vec<OsString> = vec!["bootstrap".into()];
    argv.push("--name".into());
    argv.push(request.name.clone().into());
    argv.push(request.drive_dir.as_os_str().to_owned());
    for relay in &request.relays {
        argv.push("--relay".into());
        argv.push(relay.into());
    }
    argv.push("--identity-file".into());
    argv.push(request.identity_file.as_os_str().to_owned());
    argv.push("--passphrase-file".into());
    argv.push(request.passphrase_file.as_os_str().to_owned());
    if let Some(invitation) = &request.invitation_file {
        argv.push("--invitation-file".into());
        argv.push(invitation.as_os_str().to_owned());
    }
    if let Some(registry) = &request.registry_path {
        argv.push("--registry".into());
        argv.push(registry.as_os_str().to_owned());
    }
    if let Some(owner) = &request.owner_bin {
        argv.push("--owner-bin".into());
        argv.push(owner.as_os_str().to_owned());
    }
    if let Some(supervise) = &request.supervise_bin {
        argv.push("--supervise-bin".into());
        argv.push(supervise.as_os_str().to_owned());
    }
    if request.no_service {
        argv.push("--no-service".into());
    }
    Ok(argv)
}

/// Pure argv builder for `wyrd member <drive> leave [--yes] …`.
/// Credentials ride as file paths, never contents.
pub fn leave_argv(request: &LeaveRequest) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        "member".into(),
        request.drive_dir.as_os_str().to_owned(),
        "leave".into(),
    ];
    if request.acknowledged {
        argv.push("--yes".into());
    }
    argv.push("--identity-file".into());
    argv.push(request.identity_file.as_os_str().to_owned());
    argv.push("--passphrase-file".into());
    argv.push(request.passphrase_file.as_os_str().to_owned());
    argv
}

/// Pure argv builder for `wyrd drive <dir> reconfigure …`: every
/// argument is a separate argv element, never shell-constructed.
/// Mirrors `crates/wyrd-cli/src/args.rs` (`DriveAction::Reconfigure`).
/// Empty `relays` emit no `--relay` flags (the CLI keeps the
/// registered set); non-empty relays replace it.
pub fn reconfigure_argv(request: &ReconfigureRequest) -> Result<Vec<OsString>, String> {
    if request.mode != "vault" {
        return Err(format!(
            "unknown service mode {:?}: only `vault` reconfigures through this adapter; registry-managed mount mode travels with the installer follow-up",
            request.mode
        ));
    }
    let mut argv: Vec<OsString> = vec![
        "drive".into(),
        request.drive_dir.as_os_str().to_owned(),
        "reconfigure".into(),
    ];
    argv.push("--mode".into());
    argv.push(request.mode.clone().into());
    for relay in &request.relays {
        argv.push("--relay".into());
        argv.push(relay.into());
    }
    argv.push("--identity-file".into());
    argv.push(request.identity_file.as_os_str().to_owned());
    argv.push("--passphrase-file".into());
    argv.push(request.passphrase_file.as_os_str().to_owned());
    if let Some(registry) = &request.registry_path {
        argv.push("--registry".into());
        argv.push(registry.as_os_str().to_owned());
    }
    if let Some(owner) = &request.owner_bin {
        argv.push("--owner-bin".into());
        argv.push(owner.as_os_str().to_owned());
    }
    if let Some(supervise) = &request.supervise_bin {
        argv.push("--supervise-bin".into());
        argv.push(supervise.as_os_str().to_owned());
    }
    if request.no_service {
        argv.push("--no-service".into());
    }
    Ok(argv)
}

/// Raw completed-process capture: exit code, stdout, stderr, and
/// whether the wait timed out. Stdout/stderr are opaque display
/// detail — callers map on the exit code alone, never on text.
struct CliOutput {
    code: Option<i32>,
    status_success: bool,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

/// Run `<cli> <argv…>` and wait up to `timeout`. `std::process` only:
/// the wait runs on a plain thread that polls `try_wait`, so a wedged
/// CLI can never wedge the adapter past the bound. On expiry the
/// child is killed and reaped before returning: the flight releases
/// only after the process is gone, so a timeout can never overlap a
/// later operation on the same drive. Expiry never means failure of
/// the operation itself — the CLI may have committed first, so every
/// timeout resolves `Unknown` and triggers observation.
fn run_cli(cli_bin: &Path, argv: &[OsString], timeout: Duration) -> CliOutput {
    let mut command = std::process::Command::new(cli_bin);
    command.args(argv);
    // Non-interactive, always: the CLI must never block on a prompt
    // the desktop cannot answer. Stdin is null; a CLI that reads
    // stdin fails closed instead of hanging the adapter.
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return CliOutput {
                code: None,
                status_success: false,
                stdout: String::new(),
                stderr: format!("cannot execute {}: {error}", cli_bin.display()),
                timed_out: false,
            };
        }
    };
    // Poll for completion; kill and reap past the bound. The output
    // pipes stay bounded by the CLI's own chatter (a trusted,
    // terse binary) — but the wait itself is bounded regardless.
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child.wait_with_output();
                return match output {
                    Ok(output) => CliOutput {
                        code: output.status.code().or(status.code()),
                        status_success: output.status.success(),
                        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                        timed_out: false,
                    },
                    Err(error) => CliOutput {
                        code: status.code(),
                        status_success: status.success(),
                        stdout: String::new(),
                        stderr: format!("cannot collect CLI output: {error}"),
                        timed_out: false,
                    },
                };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    // Best-effort kill, then reap: the flight must
                    // never release while the process is still alive.
                    let _ = child.kill();
                    let _ = child.wait();
                    return CliOutput {
                        code: None,
                        status_success: false,
                        stdout: String::new(),
                        stderr: format!(
                            "CLI did not complete within {}s and was stopped; outcome unknown — re-reading observed state",
                            timeout.as_secs()
                        ),
                        timed_out: true,
                    };
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                return CliOutput {
                    code: None,
                    status_success: false,
                    stdout: String::new(),
                    stderr: format!("cannot wait on CLI process: {error}"),
                    timed_out: false,
                };
            }
        }
    }
}

fn describe_code(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("exit {code}"),
        None => "no exit code".to_string(),
    }
}

/// Truncate display text to `max` bytes without splitting a
/// multi-byte character: the cut lands on the last character
/// boundary at or under `max`, so non-ASCII drive directories
/// echoed in CLI output can never panic the adapter.
fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= max {
        return trimmed.to_string();
    }
    let mut end = max;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", trimmed[..end].trim_end())
}

/// Map a completed process run onto the operation verdict. Only the
/// exit code decides: 0 reports no error (stdout/stderr ride along
/// as opaque display detail), 2 is an expected failure, 78 is the
/// terminal held-drive. No prefix is matched, no lifecycle fact is
/// derived — the paired observation alone decides what the display
/// says. Shared by `bootstrap` and `member leave`: both CLIs obey
/// the same exit contract, and both need the same honesty (a
/// non-owner leave exits 0 while authoring nothing, so exit 0 must
/// never mean "left").
fn map_exit_output(output: &CliOutput) -> ProvisionOutcome {
    if output.timed_out {
        return ProvisionOutcome {
            result: ProvisionResult::Unknown {
                detail: output.stderr.clone(),
            },
            stderr: output.stderr.clone(),
            code: None,
        };
    }
    // A missing code without a timeout is a spawn or collection
    // failure: the outcome is unknown, same as a timeout.
    let Some(code) = output.code else {
        return ProvisionOutcome {
            result: ProvisionResult::Unknown {
                detail: output.stderr.clone(),
            },
            stderr: output.stderr.clone(),
            code: None,
        };
    };
    // Opaque detail only: the full CLI text (trimmed and bounded)
    // travels for display, never for mapping.
    let detail = truncate(&format!("{}\n{}", output.stdout, output.stderr), 500);
    let stderr = output.stderr.clone();
    if output.status_success {
        return ProvisionOutcome {
            result: ProvisionResult::Completed { detail },
            stderr,
            code: Some(code),
        };
    }
    if code == EXIT_HELD_DRIVE {
        return ProvisionOutcome {
            result: ProvisionResult::HeldDrive { detail },
            stderr,
            code: Some(code),
        };
    }
    ProvisionOutcome {
        result: ProvisionResult::ExpectedFailure { detail },
        stderr,
        code: Some(code),
    }
}

/// Run one `bootstrap` provisioning through the version-matched CLI:
/// version check first (hard failure on mismatch), then the structured
/// argv run, then the exit-code map. Every path — including timeouts
/// (killed and reaped) and spawn failures — returns an outcome the
/// caller pairs with a registry re-read, socket probes, and a
/// membership re-observation; no path reports lifecycle state.
pub fn run_bootstrap(
    cli_bin: &Path,
    request: &ProvisionRequest,
    timeout: Duration,
) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = match bootstrap_argv(request) {
        Ok(argv) => argv,
        Err(reason) => {
            return ProvisionOutcome {
                result: ProvisionResult::ExpectedFailure { detail: reason },
                stderr: String::new(),
                code: None,
            };
        }
    };
    let output = run_cli(cli_bin, &argv, timeout);
    map_exit_output(&output)
}

/// Run one `member leave` through the version-matched CLI, with the
/// same version-first, observation-after contract as
/// [`run_bootstrap`].
pub fn run_leave(cli_bin: &Path, request: &LeaveRequest, timeout: Duration) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = leave_argv(request);
    let output = run_cli(cli_bin, &argv, timeout);
    map_exit_output(&output)
}

/// Run one `drive reconfigure` through the version-matched CLI,
/// with the same version-first, observation-after contract as
/// [`run_bootstrap`]. The caller pairs every path with a registry
/// re-read, socket probes, and a membership re-observation; no path
/// reports lifecycle state.
pub fn run_reconfigure(
    cli_bin: &Path,
    request: &ReconfigureRequest,
    timeout: Duration,
) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = match reconfigure_argv(request) {
        Ok(argv) => argv,
        Err(reason) => {
            return ProvisionOutcome {
                result: ProvisionResult::ExpectedFailure { detail: reason },
                stderr: String::new(),
                code: None,
            };
        }
    };
    let output = run_cli(cli_bin, &argv, timeout);
    map_exit_output(&output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_request() -> ProvisionRequest {
        ProvisionRequest {
            name: "alpha".to_string(),
            drive_dir: PathBuf::from("/drives/alpha"),
            relays: vec!["wss://relay.example".to_string()],
            identity_file: PathBuf::from("/keys/identity"),
            passphrase_file: PathBuf::from("/keys/passphrase"),
            invitation_file: None,
            registry_path: None,
            owner_bin: None,
            supervise_bin: None,
            no_service: false,
            mode: "vault".to_string(),
        }
    }

    /// The create argv is one structured element per argument: no
    /// shell, no joined command line, credential paths as bare
    /// values — contents never travel.
    #[test]
    fn create_argv_is_structured_per_element() {
        let argv = bootstrap_argv(&create_request()).unwrap();
        let rendered: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "bootstrap",
                "--name",
                "alpha",
                "/drives/alpha",
                "--relay",
                "wss://relay.example",
                "--identity-file",
                "/keys/identity",
                "--passphrase-file",
                "/keys/passphrase",
            ]
        );
        assert!(
            !rendered
                .iter()
                .any(|arg| arg.contains(' ') && arg.contains("--")),
            "no joined shell string: {rendered:?}"
        );
    }

    /// Join adds exactly the invitation-file pair; `--no-service`
    /// and explicit paths thread through untouched.
    #[test]
    fn join_argv_adds_the_invitation_pair() {
        let mut request = create_request();
        request.invitation_file = Some(PathBuf::from("/tmp/invite.sealed"));
        request.no_service = true;
        request.registry_path = Some(PathBuf::from("/tmp/drives.json"));
        let argv = bootstrap_argv(&request).unwrap();
        let rendered: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(rendered.contains(&"--invitation-file".to_string()));
        assert!(rendered.contains(&"/tmp/invite.sealed".to_string()));
        assert!(rendered.contains(&"--no-service".to_string()));
        assert!(rendered.contains(&"--registry".to_string()));
    }

    /// Non-vault modes refuse before any process spawns: the adapter
    /// never provisions a service the registry reader would skip.
    #[test]
    fn non_vault_mode_refuses_with_the_installer_pointer() {
        let mut request = create_request();
        request.mode = "mount".to_string();
        match bootstrap_argv(&request) {
            Err(reason) => assert!(
                reason.contains("installer follow-up"),
                "reason points at the follow-up: {reason}"
            ),
            Ok(_) => panic!("a mount mode must refuse"),
        }
    }

    /// Leave argv carries `--yes` only on explicit acknowledgement:
    /// a frontend omitting the dialog cannot bypass the CLI boundary.
    #[test]
    fn leave_argv_gates_yes_on_acknowledgement() {
        let base = LeaveRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            identity_file: PathBuf::from("/keys/identity"),
            passphrase_file: PathBuf::from("/keys/passphrase"),
            acknowledged: false,
        };
        let plain: Vec<String> = leave_argv(&base)
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            !plain.contains(&"--yes".to_string()),
            "no acknowledgement, no --yes: {plain:?}"
        );
        let acked = LeaveRequest {
            acknowledged: true,
            ..base
        };
        let confirmed: Vec<String> = leave_argv(&acked)
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            confirmed,
            vec![
                "member",
                "/drives/alpha",
                "leave",
                "--yes",
                "--identity-file",
                "/keys/identity",
                "--passphrase-file",
                "/keys/passphrase",
            ]
        );
    }

    /// `__build_info` parsing reads the fixed first line and refuses
    /// output without it — the version gate never guesses.
    #[test]
    fn build_info_parses_the_fixed_line() {
        let stdout = "update build: 0.3.0-alpha\nupdate envelope: 0x01\nupdate control: 0x02\n";
        assert_eq!(parse_build_info(stdout).unwrap(), "0.3.0-alpha");
        assert!(
            parse_build_info("nothing here\n").is_err(),
            "output without the line refuses"
        );
        assert!(
            parse_build_info("update build: \n").is_err(),
            "an empty version refuses"
        );
    }

    /// Exit mapping uses the exit code only — never the CLI text.
    /// Exit 0 completes (the paired observation decides what that
    /// means — a non-owner leave also exits 0 while authoring
    /// nothing), exit 2 is an expected failure with opaque detail,
    /// 78 is the terminal held-drive, and timeouts resolve unknown.
    #[test]
    fn exit_mapping_uses_the_code_only() {
        // Exit 0 with the success line: completed, and the opaque
        // detail rides along for display — never for mapping.
        let ready = CliOutput {
            code: Some(0),
            status_success: true,
            stdout: "drive alpha verified available (socket /drives/alpha/owner.sock)\n"
                .to_string(),
            stderr: String::new(),
            timed_out: false,
        };
        match map_exit_output(&ready).result {
            ProvisionResult::Completed { detail } => {
                assert!(detail.contains("verified available"), "detail: {detail}")
            }
            other => panic!("exit 0 completes: {other:?}"),
        }
        // A non-owner leave refusal also exits 0: completed as well —
        // exit 0 must never mean "left". The membership observation
        // decides.
        let refusal = CliOutput {
            code: Some(0),
            status_success: true,
            stdout: "only an owner can remove a device: ask the owner to run: member remove <id>\n"
                .to_string(),
            stderr: String::new(),
            timed_out: false,
        };
        assert!(matches!(
            map_exit_output(&refusal).result,
            ProvisionResult::Completed { .. }
        ));
        // Exit 2 with any text: expected failure, detail opaque.
        let failed = CliOutput {
            code: Some(2),
            status_success: false,
            stdout: String::new(),
            stderr: "error: drive alpha registered but unavailable: verify-socket: owner.sock never appeared\n".to_string(),
            timed_out: false,
        };
        match map_exit_output(&failed).result {
            ProvisionResult::ExpectedFailure { detail } => {
                assert!(
                    detail.contains("registered but unavailable"),
                    "detail: {detail}"
                )
            }
            other => panic!("exit 2 fails expectedly: {other:?}"),
        }
        // An unacknowledged sole-owner leave (exit 2) is the same
        // shape: no confirm-stage taxonomy, just opaque detail plus
        // observation.
        let unacked = CliOutput {
            code: Some(2),
            status_success: false,
            stdout: String::new(),
            stderr: "error: Removing the sole owner is valid but terminal: pass --yes to confirm\n"
                .to_string(),
            timed_out: false,
        };
        assert!(matches!(
            map_exit_output(&unacked).result,
            ProvisionResult::ExpectedFailure { .. }
        ));
        let held = CliOutput {
            code: Some(78),
            status_success: false,
            stdout: String::new(),
            stderr: "error: another process holds the drive\n".to_string(),
            timed_out: false,
        };
        assert!(matches!(
            map_exit_output(&held).result,
            ProvisionResult::HeldDrive { .. }
        ));
        let timed_out = CliOutput {
            code: None,
            status_success: false,
            stdout: String::new(),
            stderr: "CLI did not complete within 120s".to_string(),
            timed_out: true,
        };
        assert!(matches!(
            map_exit_output(&timed_out).result,
            ProvisionResult::Unknown { .. }
        ));
    }

    /// Truncation never splits a multi-byte character: the cut lands
    /// on a character boundary even when the budget falls mid-glyph,
    /// so non-ASCII drive directories in CLI output cannot panic.
    #[test]
    fn truncate_never_splits_a_character() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("  padded  ", 10), "padded");
        let text = "drive /drives/\u{00e9}t\u{00e9} ready";
        assert!(truncate(text, 100).contains("\u{00e9}t\u{00e9}"));
        // A budget landing inside the two-byte \u{00e9} shortens to the
        // boundary instead of panicking.
        let cut = truncate(text, 15);
        assert!(cut.is_ascii() || cut.len() <= 18, "cut: {cut}");
        // Full multi-byte stress: every budget is a valid string.
        let wide = "\u{65e5}\u{672c}\u{8a9e}\u{30c6}\u{30b9}\u{30c8} drive";
        for max in 0..=wide.len() + 4 {
            let out = truncate(wide, max);
            assert!(out.len() <= max + 3, "budget {max}: {out}");
        }
    }

    /// A wedged CLI is killed and reaped past the timeout: the
    /// adapter resolves unknown, the spawned process is gone, and no
    /// wait is left hanging on it. (The kill targets the spawned
    /// shell itself; a grandchild it spawned is the shell's problem,
    /// not the adapter's — which is why this test asserts the
    /// bounded return and the release, not process-table absence.)
    #[test]
    fn wedged_cli_is_killed_past_the_timeout() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-fakecli-{}-wedge", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("wyrd");
        // Answers the version check instantly, then sleeps past any
        // test timeout on every other argv.
        let version = env!("CARGO_PKG_VERSION");
        std::fs::write(
            &cli,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nsleep 30\nexit 0\n"
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let start = std::time::Instant::now();
        let outcome = run_bootstrap(&cli, &create_request(), Duration::from_secs(2));
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the wait is bounded by the timeout"
        );
        assert!(
            matches!(outcome.result, ProvisionResult::Unknown { .. }),
            "a wedged CLI resolves unknown: {:?}",
            outcome.result
        );
        // The wait was bounded by the timeout, and the reaped child
        // leaves the flight free: the IPC flight test pins the
        // release, and a leaked child would wedge the next claim.
        // (The shell script's own `sleep` grandchild is not the
        // adapter's to reap.)
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// Write a fake `wyrd` CLI: a shell script answering
    /// `__build_info` with the workspace version and every other
    /// invocation with the given stdout and exit code. The adapter
    /// spawns it exactly like the installed binary — no special
    /// path, no mock.
    fn fake_cli(tag: &str, body_stdout: &str, body_code: i32) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-{}",
            std::process::id(),
            tag
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wyrd");
        let version = env!("CARGO_PKG_VERSION");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\necho \"update envelope: 0x01\"\necho \"update control: 0x02\"\nexit 0\nfi\nprintf '%s' \"{body_stdout}\"\nexit {body_code}\n"
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    /// End to end through a fake CLI binary: version check passes
    /// against the workspace version, the structured argv run exits
    /// 0, and the outcome completes with the opaque CLI text — the
    /// full `run_bootstrap` path the IPC command drives, with no
    /// mock. What the text says is display detail; the code decides.
    #[test]
    fn run_bootstrap_completes_through_a_fake_cli() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-fakecli-{}-ready", std::process::id()));
        let cli = fake_cli(
            "ready",
            "drive alpha verified available (socket /drives/alpha/owner.sock)",
            0,
        );
        let outcome = run_bootstrap(&cli, &create_request(), Duration::from_secs(30));
        match &outcome.result {
            ProvisionResult::Completed { detail } => {
                assert!(detail.contains("verified available"), "detail: {detail}")
            }
            other => panic!("fake exit 0 completes: {other:?}"),
        }
        assert_eq!(outcome.code, Some(0));
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// A version-skewed CLI is a hard failure before any lifecycle
    /// argv runs: the body would report ready, but the version gate
    /// refuses first — never a silent fallback.
    #[test]
    fn run_bootstrap_refuses_a_version_skewed_cli() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-fakecli-{}-skew", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("wyrd");
        std::fs::write(
            &cli,
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: 9.9.9-mismatch\"\nexit 0\nfi\necho \"drive alpha verified available (socket /x/owner.sock)\"\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let outcome = run_bootstrap(&cli, &create_request(), Duration::from_secs(30));
        assert!(
            matches!(outcome.result, ProvisionResult::VersionMismatch { .. }),
            "skew refuses before the lifecycle argv runs: {:?}",
            outcome.result
        );
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// A failing fake CLI maps through the exit-code table: exit 2
    /// is an expected failure with opaque detail, and the re-read —
    /// not the detail — decides lifecycle state.
    #[test]
    fn run_leave_fails_expectedly_through_a_fake_cli() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-leave-refused",
            std::process::id()
        ));
        let cli = fake_cli("leave-refused", "", 2);
        let outcome = run_leave(
            &cli,
            &LeaveRequest {
                drive_dir: PathBuf::from("/drives/alpha"),
                identity_file: PathBuf::from("/keys/identity"),
                passphrase_file: PathBuf::from("/keys/passphrase"),
                acknowledged: false,
            },
            Duration::from_secs(30),
        );
        assert!(
            matches!(outcome.result, ProvisionResult::ExpectedFailure { .. }),
            "exit 2 fails expectedly: {:?}",
            outcome.result
        );
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// Join runs the same collapsed path as create: the invitation
    /// argv is structured (pinned above) and an exit-0 join
    /// completes with opaque detail through the fake CLI.
    #[test]
    fn run_join_completes_through_a_fake_cli() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-join-ready",
            std::process::id()
        ));
        let cli = fake_cli(
            "join-ready",
            "drive beta verified available (socket /drives/beta/owner.sock)",
            0,
        );
        let mut request = create_request();
        request.name = "beta".to_string();
        request.drive_dir = PathBuf::from("/drives/beta");
        request.invitation_file = Some(PathBuf::from("/tmp/invite.sealed"));
        let outcome = run_bootstrap(&cli, &request, Duration::from_secs(30));
        assert!(
            matches!(outcome.result, ProvisionResult::Completed { .. }),
            "fake exit-0 join completes: {:?}",
            outcome.result
        );
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    fn reconfigure_request() -> ReconfigureRequest {
        ReconfigureRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            relays: vec!["wss://relay.example".to_string()],
            identity_file: PathBuf::from("/keys/identity"),
            passphrase_file: PathBuf::from("/keys/passphrase"),
            registry_path: None,
            owner_bin: None,
            supervise_bin: None,
            no_service: false,
            mode: "vault".to_string(),
        }
    }

    /// The reconfigure argv is one structured element per argument:
    /// `drive <dir> reconfigure --mode vault`, relay pairs, and
    /// credential paths — never a joined shell string.
    #[test]
    fn reconfigure_argv_is_structured_per_element() {
        let argv = reconfigure_argv(&reconfigure_request()).unwrap();
        let rendered: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "drive",
                "/drives/alpha",
                "reconfigure",
                "--mode",
                "vault",
                "--relay",
                "wss://relay.example",
                "--identity-file",
                "/keys/identity",
                "--passphrase-file",
                "/keys/passphrase",
            ]
        );
        assert!(
            !rendered
                .iter()
                .any(|arg| arg.contains(' ') && arg.contains("--")),
            "no joined shell string: {rendered:?}"
        );
    }

    /// Empty relays emit no `--relay` flags: the CLI keeps the
    /// registered set, so a credential-path-only reconfigure never
    /// clears relays by accident.
    #[test]
    fn reconfigure_argv_omits_relay_flags_when_empty() {
        let mut request = reconfigure_request();
        request.relays.clear();
        request.no_service = true;
        let argv = reconfigure_argv(&request).unwrap();
        let rendered: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            !rendered.contains(&"--relay".to_string()),
            "no relay flags: {rendered:?}"
        );
        assert!(rendered.contains(&"--no-service".to_string()));
    }

    /// Non-vault modes refuse before any process spawns: the adapter
    /// never reconfigures into a mode the registry reader skips.
    #[test]
    fn non_vault_reconfigure_refuses_with_the_installer_pointer() {
        let mut request = reconfigure_request();
        request.mode = "mount".to_string();
        match reconfigure_argv(&request) {
            Err(reason) => assert!(
                reason.contains("installer follow-up"),
                "reason points at the follow-up: {reason}"
            ),
            Ok(_) => panic!("a mount reconfigure must refuse"),
        }
    }

    /// End to end through a fake CLI binary: version check passes
    /// and an exit-0 reconfigure completes with opaque detail — the
    /// full `run_reconfigure` path the IPC command drives.
    #[test]
    fn run_reconfigure_completes_through_a_fake_cli() {
        let cli = fake_cli(
            "reconf-ready",
            "drive alpha reconfigured and verified available (socket /drives/alpha/owner.sock)",
            0,
        );
        let outcome = run_reconfigure(&cli, &reconfigure_request(), Duration::from_secs(30));
        match &outcome.result {
            ProvisionResult::Completed { detail } => {
                assert!(detail.contains("reconfigured"), "detail: {detail}")
            }
            other => panic!("fake exit-0 reconfigure completes: {other:?}"),
        }
        assert_eq!(outcome.code, Some(0));
    }

    /// A version-skewed CLI is a hard failure before the reconfigure
    /// argv runs — never a silent fallback.
    #[test]
    fn run_reconfigure_refuses_a_version_skewed_cli() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-reconf-skew",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("wyrd");
        std::fs::write(
            &cli,
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: 9.9.9-mismatch\"\nexit 0\nfi\necho \"drive alpha reconfigured\"\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let outcome = run_reconfigure(&cli, &reconfigure_request(), Duration::from_secs(30));
        assert!(
            matches!(outcome.result, ProvisionResult::VersionMismatch { .. }),
            "skew refuses before the reconfigure argv runs: {:?}",
            outcome.result
        );
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }
}
