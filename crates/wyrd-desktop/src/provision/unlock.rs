//! Session unlock: pipe-shape validation, the held-child spawn, and
//! bounded detail collection. The passphrase crosses only as pipe
//! bytes. The parent module owns the authority contract.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::argv::unlock_argv;
use super::process::{check_cli_version, spawn_drain, truncate};
use super::{CredentialSource, ProvisionResult, PROVISION_TIMEOUT};

/// Unlock one registered drive for the session: start the
/// persistent owner in the foreground with a passphrase from the
/// stdin pipe (`--passphrase-stdin`), never from a file. The
/// identity secret still comes from its file; only the passphrase
/// rides the pipe. Nothing is installed, enabled, or rewritten —
/// the manager learns nothing, so a reboot or an explicit lock
/// locks the drive again.
#[derive(Debug, Clone)]
pub struct UnlockRequest {
    /// Absolute drive directory, exact string (shared drive-identity rule).
    pub drive_dir: PathBuf,
    /// Where the CLI reads the drive identity: the device identity
    /// file the start-up gate verified, or one OS credential-store
    /// item. The session passphrase always rides the stdin pipe
    /// (`--passphrase-stdin`), never a file and never the store —
    /// unlock borrows the store's identity and proves the keystore
    /// with what the operator types. Seeds never apply.
    pub credentials: CredentialSource,
    /// Control-plane relays for the owner session: the registry's
    /// registered set for the drive. Non-empty emits `--relay`
    /// flags, which *replace* the drive config file (CLI precedence:
    /// flags win when given); empty omits the flags and the CLI
    /// falls back to the config file. Unusable values never reach
    /// this field — `registry::registry_relays_for` skips them, so
    /// an all-unusable set falls back exactly like no set.
    pub relays: Vec<String>,
    /// Per-device retained-bytes ceiling in bytes. `None` enforces
    /// no ceiling, like `owner start`.
    pub retained_quota: Option<u64>,
}

/// Bound on one piped unlock passphrase: the CLI's stdin bound
/// (`read_passphrase_stdin`), mirrored here so an overlong pipe
/// refuses before any process spawns instead of failing closed
/// inside the child.
pub const MAX_UNLOCK_PASSPHRASE_BYTES: usize = 4096;

/// How long `drive_unlock` waits for the spawned owner to answer
/// before reporting unknown: long enough for the keystore KDF plus
/// the socket bind, bounded so a wedged owner cannot hold the
/// flight past the operation. Expiry resolves `Unknown`, never
/// failure — the owner may still come up under the session
/// monitor, and the paired observation decides the display.
pub const UNLOCK_VERIFY_TIMEOUT: Duration = Duration::from_secs(30);

/// Check one unlock passphrase's pipe shape before any process
/// spawns: non-empty, bounded, UTF-8, no line breaks. The CLI
/// re-validates on read (bounded take, UTF-8, single trailing
/// newline stripped, empty refused) — this mirror only fails fast
/// with the adapter's wording. Line breaks always refuse: the
/// commit path never writes one, so a break is certainly a paste
/// error, never a real passphrase.
pub fn validate_unlock_passphrase(passphrase: &[u8]) -> Result<(), String> {
    if passphrase.is_empty() {
        return Err(
            "no passphrase given: type the drive passphrase to unlock (the owner never starts without it)"
                .to_string(),
        );
    }
    if passphrase.len() > MAX_UNLOCK_PASSPHRASE_BYTES {
        return Err("passphrase exceeds the 4096-byte size limit".to_string());
    }
    let text =
        std::str::from_utf8(passphrase).map_err(|_| "passphrase must be UTF-8 text".to_string())?;
    if text.contains('\n') || text.contains('\r') {
        return Err(
            "passphrase must not contain line breaks: retype the single-line passphrase"
                .to_string(),
        );
    }
    Ok(())
}

/// One spawned unlock owner plus its pipe-drain threads. The child
/// runs the persistent owner — it is held, never waited on here:
/// the session monitor owns the wait, and `drive_lock` owns the
/// kill. Drop detaches the drain threads (they consume and discard
/// until the last pipe writer closes); the child itself is owned
/// by the session entry and is never orphaned by a drop.
///
/// `Debug` is safe: the struct carries pipe handles, never the
/// secret (the caller keeps that in its own `Zeroizing`).
#[derive(Debug)]
pub struct SpawnedUnlock {
    /// The persistent owner child: stdin already carries the
    /// passphrase and is closed; stdout/stderr are piped to the
    /// drain threads below.
    pub child: std::process::Child,
    /// Stdout drain thread: discards everything (retaining the
    /// bounded prefix only so an early exit still reports detail).
    /// Detached where the owner keeps serving; joined where it
    /// exits before the verify bound.
    pub stdout_thread: Option<std::thread::JoinHandle<Vec<u8>>>,
    /// Stderr drain thread, same discipline as stdout.
    pub stderr_thread: Option<std::thread::JoinHandle<Vec<u8>>>,
}

/// Spawn one `drive unlock` through the version-matched CLI:
/// version check first (hard failure on mismatch, like every
/// other runner), then the structured argv run with the passphrase
/// piped on stdin and the pipe closed. Returns the held child —
/// never waits: `drive unlock` runs the owner until SIGINT/SIGTERM,
/// so the wait-to-completion `run_cli` would wedge past its
/// timeout and then kill a healthy owner. The caller verifies
/// readiness (socket probe) and hands the child to the session
/// monitor; an early exit joins the drains for the exit-code
/// detail, a live owner detaches them.
///
/// The secret crosses only as pipe bytes, never argv, never a
/// file, never a log: validation bounds it before the spawn, the
/// write is one bounded `write_all` to a fresh pipe (never
/// blocking on a reader that never drains — the CLI reads stdin
/// first), and the caller's `Zeroizing` wrapper scrubs the source
/// on drop.
pub fn spawn_unlock(
    cli_bin: &Path,
    request: &UnlockRequest,
    passphrase: &[u8],
) -> Result<SpawnedUnlock, ProvisionResult> {
    use std::io::Write as _;
    if let Err(reason) = validate_unlock_passphrase(passphrase) {
        return Err(ProvisionResult::ExpectedFailure { detail: reason });
    }
    check_cli_version(cli_bin, PROVISION_TIMEOUT)?;
    let argv = match unlock_argv(request) {
        Ok(argv) => argv,
        Err(reason) => {
            return Err(ProvisionResult::ExpectedFailure { detail: reason });
        }
    };
    let mut command = std::process::Command::new(cli_bin);
    command.args(&argv);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Err(ProvisionResult::Unknown {
                detail: format!("cannot execute {}: {error}", cli_bin.display()),
            });
        }
    };
    let stdout_thread = spawn_drain("wyrd-unlock-stdout", child.stdout.take());
    let stderr_thread = spawn_drain("wyrd-unlock-stderr", child.stderr.take());
    // A drain thread that never starts must fail closed, never leak
    // a live owner holding the drive: kill, reap, and resolve
    // unknown — the paired observation decides, never a retry.
    if stdout_thread.as_ref().is_some_and(|r| r.is_err())
        || stderr_thread.as_ref().is_some_and(|r| r.is_err())
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ProvisionResult::Unknown {
            detail: "cannot spawn unlock drain thread; outcome unknown — re-reading observed state"
                .to_string(),
        });
    }
    if let Some(mut stdin) = child.stdin.take() {
        // One bounded write to a fresh pipe, then close: the CLI
        // reads stdin before serving, so this never blocks — and
        // closing tells the CLI the line is complete.
        if let Err(error) = stdin.write_all(passphrase) {
            if error.kind() == std::io::ErrorKind::BrokenPipe {
                // The child closed its read end before the write
                // landed: it exited early (a rejected passphrase, a
                // held drive) without reading stdin. That is an
                // early exit, not an unknown outcome — hand the
                // child back so the caller's try_wait loop maps its
                // code (78 -> held drive) and joins the drains for
                // the bounded detail. Killing here discards exactly
                // the code the caller needs.
            } else {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProvisionResult::Unknown {
                    detail: format!(
                        "cannot pipe the passphrase to the unlock owner: {error}; outcome unknown — re-reading observed state"
                    ),
                });
            }
        }
    }
    Ok(SpawnedUnlock {
        child,
        stdout_thread: stdout_thread.and_then(|r| r.ok()),
        stderr_thread: stderr_thread.and_then(|r| r.ok()),
    })
}

/// Collect the bounded display detail from an unlock owner that
/// exited before serving: join both drains (EOF follows the exit)
/// and truncate like every other runner. Call only where the
/// child already exited — joining a live owner's drains would
/// wedge past the verify bound.
pub fn collect_unlock_exit_detail(spawned: SpawnedUnlock) -> (Option<i32>, String) {
    let mut child = spawned.child;
    let code = child.wait().ok().and_then(|status| status.code());
    let stdout_bytes = spawned
        .stdout_thread
        .map(|handle| handle.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr_bytes = spawned
        .stderr_thread
        .map(|handle| handle.join().unwrap_or_default())
        .unwrap_or_default();
    (
        code,
        truncate(
            &format!(
                "{}\n{}",
                String::from_utf8_lossy(&stdout_bytes),
                String::from_utf8_lossy(&stderr_bytes)
            ),
            500,
        ),
    )
}
