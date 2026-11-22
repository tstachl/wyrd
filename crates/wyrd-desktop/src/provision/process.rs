//! CLI binary resolution, version gating, and the bounded process
//! runner. Exit codes are the only stable signal; stdout and stderr
//! are opaque display detail. The parent module owns the authority
//! contract.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{ProvisionOutcome, ProvisionResult, EXIT_HELD_DRIVE};

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
/// reported build version to equal the desktop's own. Match policy is
/// exact equality (decided in `docs/design/desktop-shell.md`):
/// pre-1.0 versions promise nothing across minor or patch, and both
/// halves ship from one workspace version, so only an exact pair may
/// operate. A missing binary fails here too — the spawn failure maps
/// to `VersionMismatch`, never to a fallback — so every lifecycle
/// runner that gates on this check hard-fails before spawning its
/// argv. Returns the CLI binary on success for the caller's argv run.
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
pub(crate) const MAX_CLI_CAPTURE_BYTES: usize = 256 * 1024;

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
pub(crate) fn spawn_drain<R: std::io::Read + Send + 'static>(
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
pub(crate) fn map_exit_output(output: &CliOutput) -> ProvisionOutcome {
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
