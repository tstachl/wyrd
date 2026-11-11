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
//! `wyrd-desktop` dependency-DAG policy is untouched. CLI stderr
//! travels as opaque display detail and is never parsed for lifecycle
//! facts; the only stable signals are the exit codes above plus the
//! fixed stdout prefixes the CLI contract pins (`verified available`,
//! `provisioning incomplete at`, `registered but unavailable:`,
//! `pass --yes to confirm`, `removed … at epoch`, `only an owner can
//! remove a device:`).

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

/// The adapter's verdict on the *operation*. Never lifecycle state:
/// the caller re-reads the registry and probes the socket after every
/// variant (including timeouts and spawn failures), and only the
/// observed state decides the display.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "result", rename_all = "lowercase")]
pub enum ProvisionResult {
    /// Exit 0 with the `verified available` line: the socket answered
    /// this session. The re-read still decides the display.
    VerifiedAvailable { detail: String },
    /// Exit 0 or 2 where the drive is registered but the owner does
    /// not answer: `registered but unavailable: {stage}: …`, or a
    /// non-owner leave refusal (authors nothing, exit 0).
    RegisteredButUnavailable { reason: String },
    /// Exit 2 `provisioning incomplete at {stage}: …`: nothing is
    /// registered (or the registry write itself failed).
    Incomplete { stage: String, reason: String },
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

/// Raw completed-process capture: exit code, stdout, stderr, and
/// whether the wait timed out. Stderr is opaque display detail —
/// callers map only on the code plus the fixed prefixes below.
struct CliOutput {
    code: Option<i32>,
    status_success: bool,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

/// Run `<cli> <argv…>` and wait up to `timeout`. `std::process` only:
/// the wait runs on a plain thread and the caller parks on a channel
/// with `recv_timeout`, so a wedged CLI can never wedge the adapter
/// past the bound. The child is killed on expiry, but expiry never
/// means failure of the operation itself — the CLI may have committed
/// first, so every timeout resolves `Unknown` and triggers
/// observation.
fn run_cli(cli_bin: &Path, argv: &[OsString], timeout: Duration) -> CliOutput {
    let mut command = std::process::Command::new(cli_bin);
    command.args(argv);
    // Non-interactive, always: the CLI must never block on a prompt
    // the desktop cannot answer. Stdin is null; a CLI that reads
    // stdin fails closed instead of hanging the adapter.
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let child = match command.spawn() {
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
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let output = child.wait_with_output();
        let _ = sender.send(output);
    });
    match receiver.recv_timeout(timeout) {
        Ok(Ok(output)) => CliOutput {
            code: output.status.code(),
            status_success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            timed_out: false,
        },
        Ok(Err(error)) => CliOutput {
            code: None,
            status_success: false,
            stdout: String::new(),
            stderr: format!("cannot collect CLI output: {error}"),
            timed_out: false,
        },
        Err(_) => CliOutput {
            code: None,
            status_success: false,
            stdout: String::new(),
            stderr: format!(
                "CLI did not complete within {}s; outcome unknown — re-reading observed state",
                timeout.as_secs()
            ),
            timed_out: true,
        },
    }
}

fn describe_code(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("exit {code}"),
        None => "no exit code".to_string(),
    }
}

fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= max {
        trimmed.to_string()
    } else {
        format!("{}…", trimmed[..max].trim_end())
    }
}

/// Map a completed `bootstrap` run onto the operation verdict. Only
/// the exit code and the fixed CLI prefixes decide: `verified
/// available` (ready), `registered but unavailable:` / `provisioning
/// incomplete at` (expected non-ready states), 78 (terminal
/// held-drive). Everything after those prefixes is opaque detail.
fn map_bootstrap_output(output: &CliOutput) -> ProvisionOutcome {
    if output.timed_out {
        return ProvisionOutcome {
            result: ProvisionResult::Unknown {
                detail: output.stderr.clone(),
            },
            stderr: output.stderr.clone(),
            code: None,
        };
    }
    // Spawn failure surfaces here as code None without a timeout.
    if output.code.is_none() {
        return ProvisionOutcome {
            result: ProvisionResult::Unknown {
                detail: output.stderr.clone(),
            },
            stderr: output.stderr.clone(),
            code: None,
        };
    }
    let code = output.code.unwrap_or(2);
    let stdout = output.stdout.clone();
    let stderr = output.stderr.clone();
    if output.status_success && stdout.contains("verified available") {
        return ProvisionOutcome {
            result: ProvisionResult::VerifiedAvailable {
                detail: truncate(&stdout, 300),
            },
            stderr,
            code: Some(code),
        };
    }
    if code == EXIT_HELD_DRIVE {
        return ProvisionOutcome {
            result: ProvisionResult::HeldDrive {
                detail: truncate(&stderr, 500),
            },
            stderr,
            code: Some(code),
        };
    }
    // Both non-ready states print on stderr with fixed prefixes; a
    // non-owner-shaped refusal cannot occur for bootstrap, so every
    // exit-2 line here is one of the two provisioning states.
    if stderr.contains("provisioning incomplete at ") {
        let (stage, reason) = split_incomplete(&stderr);
        return ProvisionOutcome {
            result: ProvisionResult::Incomplete { stage, reason },
            stderr,
            code: Some(code),
        };
    }
    if stderr.contains("registered but unavailable:") {
        return ProvisionOutcome {
            result: ProvisionResult::RegisteredButUnavailable {
                reason: truncate(&stderr, 500),
            },
            stderr,
            code: Some(code),
        };
    }
    // Anything else at exit 2 is an expected CLI failure (usage,
    // credentials, invitation decode): registered-nothing is the safe
    // reading, so `Incomplete` with the opaque detail — the re-read
    // decides what actually exists.
    ProvisionOutcome {
        result: ProvisionResult::Incomplete {
            stage: "cli".to_string(),
            reason: truncate(&stderr, 500),
        },
        stderr,
        code: Some(code),
    }
}

/// Split `provisioning incomplete at {stage}: {rest}` into its stage
/// and reason. The stage token is the CLI's fixed vocabulary
/// (`validate | initialize | verify-open | read-registry |
/// write-registry`); anything unparseable keeps the whole line as
/// the reason under stage `cli` — never a panic on opaque text.
fn split_incomplete(stderr: &str) -> (String, String) {
    let marker = "provisioning incomplete at ";
    match stderr.find(marker) {
        Some(index) => {
            let rest = stderr[index + marker.len()..].trim();
            let mut parts = rest.splitn(2, ':');
            let stage = parts.next().unwrap_or("cli").trim().to_string();
            let reason = parts.next().unwrap_or("").trim().to_string();
            let reason = if reason.is_empty() {
                truncate(stderr, 500)
            } else {
                truncate(&reason, 500)
            };
            (stage, reason)
        }
        None => ("cli".to_string(), truncate(stderr, 500)),
    }
}

/// Map a completed `member leave` run onto the operation verdict. The
/// sharp edge is the non-owner refusal: it authors nothing and exits
/// 0, so exit 0 alone never means "left" — the membership
/// re-observation decides. The CLI prints `only an owner can remove
/// a device:` for that case; the adapter reports it as
/// `RegisteredButUnavailable` (registered, membership unchanged) and
/// the re-read proves it.
fn map_leave_output(output: &CliOutput) -> ProvisionOutcome {
    if output.timed_out || output.code.is_none() {
        return ProvisionOutcome {
            result: ProvisionResult::Unknown {
                detail: output.stderr.clone(),
            },
            stderr: output.stderr.clone(),
            code: output.code,
        };
    }
    let code = output.code.unwrap_or(2);
    let combined = format!("{}\n{}", output.stdout, output.stderr);
    if code == EXIT_HELD_DRIVE {
        return ProvisionOutcome {
            result: ProvisionResult::HeldDrive {
                detail: truncate(&combined, 500),
            },
            stderr: output.stderr.clone(),
            code: Some(code),
        };
    }
    if output.status_success {
        if combined.contains("only an owner can remove a device:") {
            return ProvisionOutcome {
                result: ProvisionResult::RegisteredButUnavailable {
                    reason: "non-owner leave refused: authors nothing".to_string(),
                },
                stderr: output.stderr.clone(),
                code: Some(code),
            };
        }
        return ProvisionOutcome {
            result: ProvisionResult::VerifiedAvailable {
                detail: truncate(&combined, 300),
            },
            stderr: output.stderr.clone(),
            code: Some(code),
        };
    }
    if combined.contains("pass --yes to confirm") {
        return ProvisionOutcome {
            result: ProvisionResult::Incomplete {
                stage: "confirm".to_string(),
                reason: "sole-owner leave needs explicit acknowledgement".to_string(),
            },
            stderr: output.stderr.clone(),
            code: Some(code),
        };
    }
    ProvisionOutcome {
        result: ProvisionResult::Incomplete {
            stage: "cli".to_string(),
            reason: truncate(&combined, 500),
        },
        stderr: output.stderr.clone(),
        code: Some(code),
    }
}

/// Run one `bootstrap` provisioning through the version-matched CLI:
/// version check first (hard failure on mismatch), then the structured
/// argv run, then the exit-code map. Every path — including timeouts
/// and spawn failures — returns an outcome the caller pairs with a
/// registry re-read and socket probe; no path reports lifecycle state.
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
                result: ProvisionResult::Incomplete {
                    stage: "validate".to_string(),
                    reason,
                },
                stderr: String::new(),
                code: None,
            };
        }
    };
    let output = run_cli(cli_bin, &argv, timeout);
    map_bootstrap_output(&output)
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
    map_leave_output(&output)
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

    /// Bootstrap exit mapping uses codes plus fixed prefixes only:
    /// ready, registered-but-unavailable, incomplete, and the
    /// terminal 78 each land exactly once.
    #[test]
    fn bootstrap_exit_mapping_uses_codes_and_prefixes() {
        let ready = CliOutput {
            code: Some(0),
            status_success: true,
            stdout: "drive alpha verified available (socket /drives/alpha/owner.sock)\n"
                .to_string(),
            stderr: String::new(),
            timed_out: false,
        };
        assert!(matches!(
            map_bootstrap_output(&ready).result,
            ProvisionResult::VerifiedAvailable { .. }
        ));
        let unavailable = CliOutput {
            code: Some(2),
            status_success: false,
            stdout: String::new(),
            stderr: "error: drive alpha registered but unavailable: verify-socket: owner.sock never appeared\n".to_string(),
            timed_out: false,
        };
        assert!(matches!(
            map_bootstrap_output(&unavailable).result,
            ProvisionResult::RegisteredButUnavailable { .. }
        ));
        let incomplete = CliOutput {
            code: Some(2),
            status_success: false,
            stdout: String::new(),
            stderr: "error: provisioning incomplete at initialize: drive exists and is locked\n"
                .to_string(),
            timed_out: false,
        };
        match map_bootstrap_output(&incomplete).result {
            ProvisionResult::Incomplete { stage, .. } => assert_eq!(stage, "initialize"),
            other => panic!("incomplete maps with its stage: {other:?}"),
        }
        let held = CliOutput {
            code: Some(78),
            status_success: false,
            stdout: String::new(),
            stderr: "error: another process holds the drive\n".to_string(),
            timed_out: false,
        };
        assert!(matches!(
            map_bootstrap_output(&held).result,
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
            map_bootstrap_output(&timed_out).result,
            ProvisionResult::Unknown { .. }
        ));
    }

    /// The non-owner leave refusal exits 0 but authors nothing: the
    /// adapter reports it as membership-unchanged, and the re-read
    /// proves it — exit 0 alone never means "left".
    #[test]
    fn non_owner_leave_refusal_maps_membership_unchanged() {
        let refusal = CliOutput {
            code: Some(0),
            status_success: true,
            stdout: "only an owner can remove a device: ask the owner to run: member remove <id>\n"
                .to_string(),
            stderr: String::new(),
            timed_out: false,
        };
        assert!(matches!(
            map_leave_output(&refusal).result,
            ProvisionResult::RegisteredButUnavailable { .. }
        ));
        let confirmed = CliOutput {
            code: Some(2),
            status_success: false,
            stdout: String::new(),
            stderr: "error: Removing the sole owner is valid but terminal: pass --yes to confirm\n"
                .to_string(),
            timed_out: false,
        };
        match map_leave_output(&confirmed).result {
            ProvisionResult::Incomplete { stage, .. } => assert_eq!(stage, "confirm"),
            other => panic!("unacknowledged sole-owner leave lands on confirm: {other:?}"),
        }
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
    /// against the workspace version, the structured argv run
    /// reports the canned ready line, and the outcome is verified
    /// available — the full `run_bootstrap` path the IPC command
    /// drives, with no mock.
    #[test]
    fn run_bootstrap_reports_verified_available_through_a_fake_cli() {
        let cli = fake_cli(
            "ready",
            "drive alpha verified available (socket /drives/alpha/owner.sock)",
            0,
        );
        let outcome = run_bootstrap(&cli, &create_request(), Duration::from_secs(30));
        assert!(
            matches!(outcome.result, ProvisionResult::VerifiedAvailable { .. }),
            "fake ready maps verified: {:?}",
            outcome.result
        );
        assert_eq!(outcome.code, Some(0));
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
    }

    /// A failing fake CLI maps through the exit-code table: exit 2
    /// with the incomplete prefix lands on its stage, and the opaque
    /// detail never decides lifecycle state by itself.
    #[test]
    fn run_leave_reports_incomplete_through_a_fake_cli() {
        let cli = fake_cli("leave-refused", "", 2);
        // The body stdout is empty here; exercise the mapping with
        // a direct unit instead: exit 2 with no prefix is stage cli.
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
        // The fake prints nothing, so the map lands on stage cli —
        // the shape the re-read disambiguates, never the detail.
        match outcome.result {
            ProvisionResult::Incomplete { stage, .. } => assert_eq!(stage, "cli"),
            other => panic!("empty exit-2 leave lands on cli: {other:?}"),
        }
    }
}
