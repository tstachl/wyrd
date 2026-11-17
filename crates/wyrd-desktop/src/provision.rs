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

/// Exit codes the CLI contract pins (`lib.rs`, `docs/design/cli.md`):
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
    /// Credential *paths*, never contents.
    pub passphrase_file: PathBuf,
    /// `None` = create, `Some(path)` = join from that invitation file.
    pub invitation_file: Option<PathBuf>,
    /// Explicit registry path (tests); `None` = platform default.
    pub registry_path: Option<PathBuf>,
    /// Explicit owner/supervise binaries (tests / non-standard layout);
    /// `None` = sibling-of-CLI default the CLI applies.
    pub owner_bin: Option<PathBuf>,
    /// Explicit supervise binary (tests / non-standard layout); `None` = CLI default.
    pub supervise_bin: Option<PathBuf>,
    /// `false` (default) installs, enables, and starts the per-drive
    /// service; `true` registers only.
    pub no_service: bool,
    /// Service mode display string: `vault` serves headless, `mount`
    /// serves the FUSE projection at `mountpoint`. Any other value
    /// refuses before any process spawns — the adapter never
    /// provisions a service the registry reader would skip.
    pub mode: String,
    /// FUSE mountpoint. Required in `mount` mode, refused in `vault`
    /// mode.
    pub mountpoint: Option<PathBuf>,
}

/// Leave the drive: end this device's participation where the trust
/// contract admits it. A non-owner authors nothing (the CLI reports
/// the refusal at exit 0); a sole owner freezes the drive permanently
/// and needs `acknowledged = true` (`--yes` at the CLI boundary — a
/// frontend omitting the dialog cannot bypass the requirement).
#[derive(Debug, Clone)]
pub struct LeaveRequest {
    /// Absolute drive directory, exact string (shared drive-identity rule).
    pub drive_dir: PathBuf,
    /// Credential *paths*, never contents.
    pub identity_file: PathBuf,
    /// Credential *paths*, never contents.
    pub passphrase_file: PathBuf,
    /// Sole-owner terminal confirmation. The UI explains the
    /// consequences; this flag carries the acknowledgement to the CLI.
    pub acknowledged: bool,
}

/// Reconfigure a registered drive's supervision in place: stop and
/// clear the old service definition, rewrite the registry entry,
/// reinstall and restart when managed, and verify the socket.
/// `relays` replaces the registered set when non-empty and keeps the
/// fresh set when empty (a credential-path-only reconfigure never
/// clears relays by accident); `mode` is `vault` or `mount` (`mount`
/// needs `mountpoint`), and any other value refuses before any
/// process spawns, like creation.
#[derive(Debug, Clone)]
pub struct ReconfigureRequest {
    /// Absolute drive directory, exact string (shared drive-identity rule).
    pub drive_dir: PathBuf,
    /// Replacement relays when non-empty; empty keeps the registered set.
    pub relays: Vec<String>,
    /// Credential *paths*, never contents.
    pub identity_file: PathBuf,
    /// Credential *paths*, never contents.
    pub passphrase_file: PathBuf,
    /// Explicit registry path (tests); `None` = platform default.
    pub registry_path: Option<PathBuf>,
    /// Explicit owner/supervise binaries (tests / non-standard layout);
    /// `None` = sibling-of-CLI default the CLI applies.
    pub owner_bin: Option<PathBuf>,
    /// Explicit supervise binary (tests / non-standard layout); `None` = CLI default.
    pub supervise_bin: Option<PathBuf>,
    /// `false` (default) disables the old service, rewrites the
    /// entry, and reinstalls, enables, and restarts; `true`
    /// rewrites without installing any OS service.
    pub no_service: bool,
    /// Service mode display string: `vault` serves headless, `mount`
    /// serves the FUSE projection at `mountpoint`. Any other value
    /// refuses before any process spawns.
    pub mode: String,
    /// FUSE mountpoint. Required in `mount` mode, refused in `vault`
    /// mode.
    pub mountpoint: Option<PathBuf>,
}

/// Start one registered drive's installed service without
/// rewriting anything: the recovery for "installed but stopped".
/// No credentials travel — starting proves nothing about membership
/// and the CLI opens no keystore — only the drive directory plus the
/// registry override (tests and non-standard layouts).
#[derive(Debug, Clone)]
pub struct ServiceStartRequest {
    /// Absolute drive directory, exact string (shared drive-identity rule).
    pub drive_dir: PathBuf,
    /// Explicit registry path (tests); `None` = platform default.
    pub registry_path: Option<PathBuf>,
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
    Completed {
        /// Opaque display detail; the paired observation decides what happened.
        detail: String,
    },
    /// Exit 2 or an adapter-side refusal: usage, expected failure,
    /// or a validation refusal that never spawned a process. Opaque
    /// detail; the paired observation says what actually exists.
    ExpectedFailure {
        /// Opaque display detail; the paired observation says what actually exists.
        detail: String,
    },
    /// Exit 78: another process holds the drive. Terminal — the
    /// supervisor must not retry.
    HeldDrive {
        /// Opaque display detail naming the terminal held-drive.
        detail: String,
    },
    /// CLI binary missing, version-mismatched, or unparsable
    /// `__build_info`: hard failure, never silent fallback.
    VersionMismatch {
        /// Why the CLI binary is unusable; never a silent fallback.
        reason: String,
    },
    /// A lifecycle operation is already in flight on this drive.
    Busy {
        /// Drive refused as busy, as an exact directory string.
        drive_dir: String,
    },
    /// Timeout, spawn failure, or signal death: the outcome is
    /// unknown. Triggers observation, never re-execution.
    Unknown {
        /// Why the outcome is unknown; triggers observation, never re-execution.
        detail: String,
    },
}

/// One completed operation: the opaque outcome plus the raw CLI
/// stderr (display detail, never parsed) and the exit code observed.
#[derive(Debug, Clone)]
pub struct ProvisionOutcome {
    /// Operation verdict: exit codes only, never lifecycle state.
    pub result: ProvisionResult,
    /// Raw CLI stderr as opaque display detail, never parsed for lifecycle facts.
    pub stderr: String,
    /// Observed exit code, absent where no process completed.
    pub code: Option<i32>,
}

/// Resolve the version-matched CLI binary: an explicit path (tests,
/// non-standard layouts), the `wyrd` binary in the bundle's
/// `Contents/Resources` beside the running app (the fixed
/// co-installation the installer ships), else the `wyrd` sibling of
/// the running desktop binary (devenv/`cargo tauri dev`, where the
/// app binary is `wyrd-desktop` and the CLI is its target-dir
/// sibling). Never a `PATH` search: a silent fallback to an
/// arbitrary binary would defeat the version match.
///
/// The bundle check is existence-gated: on the default
/// case-insensitive APFS volume `Contents/MacOS/wyrd` would resolve
/// to the `Wyrd` app binary itself, so the sibling is only the
/// fallback where no bundled copy exists.
pub fn resolve_cli_bin(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    let exe = std::env::current_exe()
        .map_err(|error| format!("cannot locate the desktop binary: {error}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "the desktop binary has no parent directory".to_string())?;
    if let Some(bundled) = bundled_cli_path(dir) {
        if bundled.is_file() {
            return Ok(bundled);
        }
    }
    Ok(dir.join("wyrd"))
}

/// Bundle-layout candidate for the CLI given the desktop binary's
/// directory: `Contents/MacOS` -> `Contents/Resources/wyrd`. Pure
/// path math, no filesystem probe — the caller existence-gates it.
pub fn bundled_cli_path(exe_dir: &Path) -> Option<PathBuf> {
    exe_dir
        .parent()
        .map(|contents| contents.join("Resources").join("wyrd"))
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
    let output = run_cli(cli_bin, &["__build_info".into()], None, timeout);
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

/// Pure argv builder for `wyrd drive <dir> bootstrap …`: every argument is a
/// separate argv element, never shell-constructed. Mirrors
/// `crates/wyrd-cli/src/args.rs` (`DriveAction::Bootstrap`).
pub fn bootstrap_argv(request: &ProvisionRequest) -> Result<Vec<OsString>, String> {
    match request.mode.as_str() {
        "vault" => {
            if request.mountpoint.is_some() {
                return Err(
                    "vault mode carries no mountpoint: remove the mountpoint for a vault entry"
                        .to_string(),
                );
            }
        }
        "mount" => {
            if request.mountpoint.is_none() {
                return Err(
                    "mount mode needs a mountpoint: pass a mountpoint with an absolute path"
                        .to_string(),
                );
            }
        }
        other => {
            return Err(format!(
                "unknown service mode {other:?}: this build manages `vault` and `mount`"
            ));
        }
    }
    let mut argv: Vec<OsString> = vec![
        "drive".into(),
        request.drive_dir.as_os_str().to_owned(),
        "--identity-file".into(),
        request.identity_file.as_os_str().to_owned(),
        "--passphrase-file".into(),
        request.passphrase_file.as_os_str().to_owned(),
        "bootstrap".into(),
    ];
    argv.push("--name".into());
    argv.push(request.name.clone().into());
    argv.push("--mode".into());
    argv.push(request.mode.clone().into());
    if let Some(mountpoint) = &request.mountpoint {
        argv.push("--mountpoint".into());
        argv.push(mountpoint.as_os_str().to_owned());
    }
    for relay in &request.relays {
        argv.push("--relay".into());
        argv.push(relay.into());
    }
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
/// Credentials ride as file paths, never contents. `Credentials` is
/// flattened on the parent (`Command::Member`), not global, so the
/// credential flags must precede the `leave` verb — clap rejects them
/// after it.
pub fn leave_argv(request: &LeaveRequest) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        "member".into(),
        request.drive_dir.as_os_str().to_owned(),
        "--identity-file".into(),
        request.identity_file.as_os_str().to_owned(),
        "--passphrase-file".into(),
        request.passphrase_file.as_os_str().to_owned(),
        "leave".into(),
    ];
    if request.acknowledged {
        argv.push("--yes".into());
    }
    argv
}

/// Pure argv builder for `wyrd drive <dir> reconfigure …`: every
/// argument is a separate argv element, never shell-constructed.
/// Mirrors `crates/wyrd-cli/src/args.rs` (`DriveAction::Reconfigure`).
/// Empty `relays` emit no `--relay` flags (the CLI keeps the
/// registered set); non-empty relays replace it.
pub fn reconfigure_argv(request: &ReconfigureRequest) -> Result<Vec<OsString>, String> {
    match request.mode.as_str() {
        "vault" => {
            if request.mountpoint.is_some() {
                return Err(
                    "vault mode carries no mountpoint: remove the mountpoint for a vault entry"
                        .to_string(),
                );
            }
        }
        "mount" => {
            if request.mountpoint.is_none() {
                return Err(
                    "mount mode needs a mountpoint: pass a mountpoint with an absolute path"
                        .to_string(),
                );
            }
        }
        other => {
            return Err(format!(
                "unknown service mode {other:?}: this build manages `vault` and `mount`"
            ));
        }
    }
    let mut argv: Vec<OsString> = vec![
        "drive".into(),
        request.drive_dir.as_os_str().to_owned(),
        "--identity-file".into(),
        request.identity_file.as_os_str().to_owned(),
        "--passphrase-file".into(),
        request.passphrase_file.as_os_str().to_owned(),
        "reconfigure".into(),
    ];
    argv.push("--mode".into());
    argv.push(request.mode.clone().into());
    if let Some(mountpoint) = &request.mountpoint {
        argv.push("--mountpoint".into());
        argv.push(mountpoint.as_os_str().to_owned());
    }
    for relay in &request.relays {
        argv.push("--relay".into());
        argv.push(relay.into());
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

/// Pure argv builder for `wyrd drive <dir> service-start …`: every
/// argument is a separate argv element, never shell-constructed.
/// Mirrors `crates/wyrd-cli/src/args.rs`
/// (`DriveAction::ServiceStart`). No credentials: starting opens no
/// keystore and proves nothing about membership.
pub fn service_start_argv(request: &ServiceStartRequest) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        "drive".into(),
        request.drive_dir.as_os_str().to_owned(),
        "service-start".into(),
    ];
    if let Some(registry) = &request.registry_path {
        argv.push("--registry".into());
        argv.push(registry.as_os_str().to_owned());
    }
    argv
}

/// Raw completed-process capture: exit code, stdout, stderr, and
/// whether the wait timed out. Stdout/stderr are opaque display
/// detail — callers map on the exit code alone, never on text.
/// Shared with the onboarding adapter, whose commit carries the
/// passphrase on the stdin pipe (the only secret that ever crosses
/// this boundary, and never via argv).
#[derive(Debug, Clone)]
pub(crate) struct CliOutput {
    pub(crate) code: Option<i32>,
    pub(crate) status_success: bool,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) timed_out: bool,
}

/// Max retained bytes per piped stream: the drain consumes the full
/// pipe (so a chatty CLI can never wedge the wait) but keeps only
/// this prefix. 512 KiB total retained is ample for the 500-byte
/// display detail `map_exit_output` keeps, while bounding memory.
const MAX_CLI_CAPTURE_BYTES: usize = 256 * 1024;

/// Drain `pipe` to EOF, retaining at most `MAX_CLI_CAPTURE_BYTES`.
/// Excess is discarded but still consumed, so the child never blocks
/// on a full pipe while the parent waits.
fn drain_capped<R: std::io::Read>(mut pipe: R) -> Vec<u8> {
    let mut retained = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if retained.len() < MAX_CLI_CAPTURE_BYTES {
                    let room = MAX_CLI_CAPTURE_BYTES - retained.len();
                    retained.extend_from_slice(&buf[..n.min(room)]);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    retained
}

/// Spawn a pipe-drain thread. `Builder` (not `spawn`) so thread-creation
/// exhaustion reports instead of panicking across the adapter boundary.
fn spawn_drain<R: std::io::Read + Send + 'static>(
    name: &str,
    pipe: Option<R>,
) -> Option<std::io::Result<std::thread::JoinHandle<Vec<u8>>>> {
    pipe.map(|p| {
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || drain_capped(p))
    })
}

/// Run `<cli> <argv…>` and wait up to `timeout`. `std::process` only:
/// the wait polls `try_wait` while two reader threads drain stdout
/// and stderr concurrently, so a chatty CLI can never wedge the
/// adapter past the bound. Capture keeps at most
/// `MAX_CLI_CAPTURE_BYTES` per stream (the drain still consumes the
/// full pipe). On expiry the child is killed and reaped before
/// returning: the flight releases only after the process is gone, so
/// a timeout can never overlap a later operation on the same drive.
/// Expiry never means failure of the operation itself — the CLI may
/// have committed first, so every timeout resolves `Unknown` and
/// triggers observation.
///
/// `stdin_bytes`, when present, is piped to the child's stdin and
/// the pipe closed; otherwise stdin is null (a CLI that reads stdin
/// fails closed instead of hanging the adapter). Callers must bound
/// piped input: the write runs before the timeout loop, so an
/// unbounded write to a reader that never drains could block past
/// the bound (the sole caller caps stdin at 4096 bytes).
pub(crate) fn run_cli(
    cli_bin: &Path,
    argv: &[OsString],
    stdin_bytes: Option<&[u8]>,
    timeout: Duration,
) -> CliOutput {
    use std::io::Write;
    let mut command = std::process::Command::new(cli_bin);
    command.args(argv);
    if stdin_bytes.is_some() {
        command.stdin(std::process::Stdio::piped());
    } else {
        // Non-interactive, always: the CLI must never block on a prompt
        // the desktop cannot answer. Stdin is null; a CLI that reads
        // stdin fails closed instead of hanging the adapter.
        command.stdin(std::process::Stdio::null());
    }
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
    // Drain both pipes concurrently with the wait: a child writing
    // more than the OS pipe buffer blocks until the parent reads, so
    // polling `try_wait` without reading wedges past the timeout and
    // misclassifies a fast deterministic failure as `Unknown`.
    let stdout_thread = spawn_drain("wyrd-run-cli-stdout", child.stdout.take());
    let stderr_thread = spawn_drain("wyrd-run-cli-stderr", child.stderr.take());
    // A drain thread that never starts must fail closed, never panic
    // and never leak a live child holding the drive: kill, reap, and
    // resolve unknown. The missing code maps to `Unknown`, so the
    // paired observation decides — never a retry.
    if stdout_thread.as_ref().is_some_and(|r| r.is_err())
        || stderr_thread.as_ref().is_some_and(|r| r.is_err())
    {
        let _ = child.kill();
        let _ = child.wait();
        return CliOutput {
            code: None,
            status_success: false,
            stdout: String::new(),
            stderr: "cannot spawn CLI drain thread; outcome unknown — re-reading observed state"
                .to_string(),
            timed_out: false,
        };
    }
    let stdout_handle = stdout_thread.and_then(|r| r.ok());
    let stderr_handle = stderr_thread.and_then(|r| r.ok());
    if let Some(bytes) = stdin_bytes {
        if let Some(mut stdin) = child.stdin.take() {
            // Best-effort write then close: a failed write fails the
            // run, never hangs it (the wait below stays bounded and
            // kills past the timeout).
            let _ = stdin.write_all(bytes);
        }
    }
    // Poll for completion; kill and reap past the bound. The wait
    // itself is bounded regardless of how chatty the CLI is.
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // The child exited, so the pipes hit EOF once drained:
                // join the readers to collect the bounded prefix. A
                // join failure loses detail, never the exit code.
                // Residual: a grandchild that inherited a pipe keeps
                // that write end open, so its reader never sees EOF
                // and this join outlives the timeout. Unreachable
                // today (no provisioning or onboarding CLI argv
                // spawns an inheriting child) and shared with the
                // removed `wait_with_output` — documented, not bounded.
                let stdout_bytes = stdout_handle
                    .map(|handle| handle.join().unwrap_or_default())
                    .unwrap_or_default();
                let stderr_bytes = stderr_handle
                    .map(|handle| handle.join().unwrap_or_default())
                    .unwrap_or_default();
                return CliOutput {
                    code: status.code(),
                    status_success: status.success(),
                    stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
                    stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
                    timed_out: false,
                };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    // Best-effort kill, then reap: the flight must
                    // never release while the process is still alive.
                    // The drain threads are detached here: joining
                    // could block on a grandchild holding the pipe
                    // open (the wedge test's `sleep`), which would
                    // break the bound this branch exists to keep.
                    // Lifetime cost: a detached reader lives until the
                    // last pipe writer closes, so a descendant that
                    // outlives the kill keeps it consuming and
                    // discarding until it exits. The caller's wait
                    // stays bounded; the thread lifetime does not in
                    // that latent case.
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
pub(crate) fn truncate(text: &str, max: usize) -> String {
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
    let output = run_cli(cli_bin, &argv, None, timeout);
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
    let output = run_cli(cli_bin, &argv, None, timeout);
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
    let output = run_cli(cli_bin, &argv, None, timeout);
    map_exit_output(&output)
}

/// Run one `drive service-start` through the version-matched CLI,
/// with the same version-first, observation-after contract as
/// [`run_bootstrap`]. The caller pairs every path with a registry
/// re-read, socket probes, and a membership re-observation; no path
/// reports lifecycle state. No credentials travel: the CLI opens no
/// keystore for a start.
pub fn run_service_start(
    cli_bin: &Path,
    request: &ServiceStartRequest,
    timeout: Duration,
) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = service_start_argv(request);
    let output = run_cli(cli_bin, &argv, None, timeout);
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
            mountpoint: None,
        }
    }

    /// The bundle-layout candidate is pure path math:
    /// `Contents/MacOS` -> `Contents/Resources/wyrd`, and the sibling
    /// stays the fallback. The existence gate lives in
    /// `resolve_cli_bin`, not here.
    #[test]
    fn bundled_cli_path_maps_macos_to_resources() {
        let macos = Path::new("/Applications/Wyrd.app/Contents/MacOS");
        assert_eq!(
            bundled_cli_path(macos),
            Some(PathBuf::from(
                "/Applications/Wyrd.app/Contents/Resources/wyrd"
            ))
        );
        assert_eq!(
            resolve_cli_bin(Some(PathBuf::from("/custom/wyrd"))).unwrap(),
            PathBuf::from("/custom/wyrd"),
            "an explicit path passes through untouched"
        );
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
                "drive",
                "/drives/alpha",
                "--identity-file",
                "/keys/identity",
                "--passphrase-file",
                "/keys/passphrase",
                "bootstrap",
                "--name",
                "alpha",
                "--mode",
                "vault",
                "--relay",
                "wss://relay.example",
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

    /// Mount argv carries `--mode mount` plus the mountpoint pair:
    /// the FUSE projection the managed mount mode serves.
    #[test]
    fn mount_argv_carries_the_mode_and_mountpoint_pairs() {
        let mut request = create_request();
        request.mode = "mount".to_string();
        request.mountpoint = Some(PathBuf::from("/mnt/alpha"));
        let argv = bootstrap_argv(&request).unwrap();
        let rendered: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(rendered.contains(&"--mode".to_string()));
        assert!(rendered.contains(&"mount".to_string()));
        assert!(rendered.contains(&"--mountpoint".to_string()));
        assert!(rendered.contains(&"/mnt/alpha".to_string()));
    }

    /// Mount without a mountpoint refuses before any process spawns,
    /// like vault carrying one: the pair travels together or not at
    /// all.
    #[test]
    fn mount_without_a_mountpoint_refuses_before_spawning() {
        let mut request = create_request();
        request.mode = "mount".to_string();
        assert!(request.mountpoint.is_none());
        let reason = bootstrap_argv(&request).unwrap_err();
        assert!(reason.contains("needs a mountpoint"), "{reason}");
        let mut stray = create_request();
        stray.mountpoint = Some(PathBuf::from("/mnt/stray"));
        let reason = bootstrap_argv(&stray).unwrap_err();
        assert!(reason.contains("carries no mountpoint"), "{reason}");
    }

    /// Unknown modes refuse before any process spawns: the adapter
    /// never provisions a service the registry reader would skip.
    #[test]
    fn unknown_mode_refuses_before_spawning() {
        let mut request = create_request();
        request.mode = "fuse".to_string();
        match bootstrap_argv(&request) {
            Err(reason) => assert!(
                reason.contains("unknown service mode"),
                "reason names the unknown mode: {reason}"
            ),
            Ok(_) => panic!("an unknown mode must refuse"),
        }
    }

    /// Leave argv carries `--yes` only on explicit acknowledgement:
    /// a frontend omitting the dialog cannot bypass the CLI boundary.
    /// Credentials precede the `leave` verb: they are flattened on the
    /// parent, not global, so anything after the verb is rejected.
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
        assert_eq!(
            plain,
            vec![
                "member",
                "/drives/alpha",
                "--identity-file",
                "/keys/identity",
                "--passphrase-file",
                "/keys/passphrase",
                "leave",
            ],
            "credentials precede the verb even without --yes: {plain:?}"
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
                "--identity-file",
                "/keys/identity",
                "--passphrase-file",
                "/keys/passphrase",
                "leave",
                "--yes",
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

    /// A chatty CLI that writes 1 MiB to stdout and exits 2 must not
    /// wedge the adapter: the pipes are drained concurrently with the
    /// wait, so the exit code (expected failure) arrives well under
    /// the timeout instead of resolving `Unknown` after a kill. Both
    /// pipes are drained (1 MiB each) so a stderr flood cannot wedge
    /// a stdout drain either; capture keeps only a bounded prefix.
    #[test]
    fn chatty_cli_drains_pipes_instead_of_wedging() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-chatty",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("wyrd");
        std::fs::write(
            &cli,
            "#!/bin/sh\nhead -c 1048576 /dev/zero | tr '\\0' 'A'\nhead -c 1048576 /dev/zero | tr '\\0' 'B' >&2\nexit 2\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let start = std::time::Instant::now();
        let output = run_cli(&cli, &["probe".into()], None, Duration::from_secs(10));
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "a 1 MiB exit-2 must return well under the timeout, took {elapsed:?}"
        );
        assert!(
            !output.timed_out,
            "a fast exit-2 is not a timeout: {output:?}"
        );
        assert_eq!(output.code, Some(2), "exit code survives: {output:?}");
        assert!(
            output.stdout.len() <= super::MAX_CLI_CAPTURE_BYTES,
            "stdout capture stays bounded: {} bytes",
            output.stdout.len()
        );
        assert!(
            output.stderr.len() <= super::MAX_CLI_CAPTURE_BYTES,
            "stderr capture stays bounded: {} bytes",
            output.stderr.len()
        );
        assert!(
            !output.stdout.is_empty() && !output.stderr.is_empty(),
            "bounded prefix still carries output for display"
        );
        assert!(
            matches!(
                map_exit_output(&output).result,
                ProvisionResult::ExpectedFailure { .. }
            ),
            "exit 2 maps to expected failure: {:?}",
            map_exit_output(&output).result
        );
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
            mountpoint: None,
        }
    }

    /// The reconfigure argv is one structured element per argument:
    /// `drive <dir> --identity-file … --passphrase-file … reconfigure
    /// --mode vault`, relay pairs after the verb — never a joined
    /// shell string. Credentials precede the verb because they are
    /// flattened on the parent, not global.
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
                "--identity-file",
                "/keys/identity",
                "--passphrase-file",
                "/keys/passphrase",
                "reconfigure",
                "--mode",
                "vault",
                "--relay",
                "wss://relay.example",
            ]
        );
        assert!(
            !rendered
                .iter()
                .any(|arg| arg.contains(' ') && arg.contains("--")),
            "no joined shell string: {rendered:?}"
        );
        // Position pin, not just contents: the credential pair must
        // sit before the subcommand verb (the nix/drive owner-argv
        // order-pin precedent) — after the verb clap rejects it.
        let verb = rendered
            .iter()
            .position(|arg| arg == "reconfigure")
            .unwrap();
        let identity = rendered
            .iter()
            .position(|arg| arg == "--identity-file")
            .unwrap();
        let passphrase = rendered
            .iter()
            .position(|arg| arg == "--passphrase-file")
            .unwrap();
        assert!(
            identity < verb && passphrase < verb,
            "credentials precede the verb: {rendered:?}"
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

    /// Mount reconfigure argv carries `--mode mount` plus the
    /// mountpoint pair; unknown modes refuse before any process
    /// spawns — the adapter never reconfigures into a mode the
    /// registry reader skips.
    #[test]
    fn mount_reconfigure_carries_the_mode_and_mountpoint_pairs() {
        let mut request = reconfigure_request();
        request.mode = "mount".to_string();
        request.mountpoint = Some(PathBuf::from("/mnt/alpha"));
        let argv = reconfigure_argv(&request).unwrap();
        let rendered: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(rendered.contains(&"--mode".to_string()));
        assert!(rendered.contains(&"mount".to_string()));
        assert!(rendered.contains(&"--mountpoint".to_string()));
        assert!(rendered.contains(&"/mnt/alpha".to_string()));
    }

    #[test]
    fn unknown_reconfigure_mode_refuses_before_spawning() {
        let mut request = reconfigure_request();
        request.mode = "fuse".to_string();
        match reconfigure_argv(&request) {
            Err(reason) => assert!(
                reason.contains("unknown service mode"),
                "reason names the unknown mode: {reason}"
            ),
            Ok(_) => panic!("an unknown reconfigure must refuse"),
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

    /// A wedged CLI is killed and reaped past the timeout on the
    /// reconfigure path too: the outcome resolves unknown and
    /// triggers observation, never re-execution.
    #[test]
    fn run_reconfigure_times_out_to_unknown() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-reconf-wedge",
            std::process::id()
        ));
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
        let outcome = run_reconfigure(&cli, &reconfigure_request(), Duration::from_secs(2));
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the wait is bounded by the timeout"
        );
        assert!(
            matches!(outcome.result, ProvisionResult::Unknown { .. }),
            "a wedged reconfigure resolves unknown: {:?}",
            outcome.result
        );
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// The service-start argv is one structured element per argument:
    /// `drive <dir> service-start`, no credentials — starting opens
    /// no keystore — plus the registry override where set.
    #[test]
    fn service_start_argv_is_structured_without_credentials() {
        let request = ServiceStartRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            registry_path: Some(PathBuf::from("/tmp/drives.json")),
        };
        let rendered: Vec<String> = service_start_argv(&request)
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "drive",
                "/drives/alpha",
                "service-start",
                "--registry",
                "/tmp/drives.json",
            ]
        );
        assert!(
            !rendered.iter().any(|arg| arg.contains("identity")),
            "no credential paths travel on a start: {rendered:?}"
        );
    }

    /// End to end through a fake CLI binary: version check passes
    /// and an exit-0 start completes with opaque detail — the full
    /// `run_service_start` path the IPC command drives.
    #[test]
    fn run_service_start_completes_through_a_fake_cli() {
        let cli = fake_cli(
            "start-ready",
            "drive alpha service started and verified available (socket /drives/alpha/owner.sock)",
            0,
        );
        let request = ServiceStartRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            registry_path: None,
        };
        let outcome = run_service_start(&cli, &request, Duration::from_secs(30));
        match &outcome.result {
            ProvisionResult::Completed { detail } => {
                assert!(detail.contains("verified available"), "detail: {detail}")
            }
            other => panic!("fake exit-0 start completes: {other:?}"),
        }
        assert_eq!(outcome.code, Some(0));
    }
}
