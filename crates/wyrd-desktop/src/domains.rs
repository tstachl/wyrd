//! macOS File Provider domain reconciliation.
//!
//! macOS registers a File Provider domain only when the containing
//! app asks for it. The bundled Swift helper makes that call; Wyrd.app
//! invokes it as a subprocess, so no Cargo dependency edge or unsafe
//! FFI bridge is needed. The helper is signed with the app's bundle
//! identifier, and discovers drives from the group socket aliases the
//! owner publishes, so the shell needs no drive-id knowledge and the
//! owner protocol stays unchanged.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const HELPER: &str = "WyrdDomainRegistrar";
const CONFIG: &str = r#"{"group":"group.md.enesis.wyrd"}"#;
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const HELPER_TIMEOUT: Duration = Duration::from_secs(15);

/// Bundled helper first, staged worktree copy for `cargo tauri dev`.
/// A missing helper is an honest error, never a silent skip: without
/// it a drive can never reach Finder.
fn helper_path() -> Result<PathBuf, String> {
    let executable = std::env::current_exe().map_err(|error| format!("current_exe: {error}"))?;
    if let Some(macos) = executable.parent() {
        let bundled = macos.join(HELPER);
        if bundled.is_file() {
            return Ok(bundled);
        }
    }
    let staged = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/macos/stage")
        .join(HELPER);
    if staged.is_file() {
        return Ok(staged);
    }
    Err("no WyrdDomainRegistrar in the bundle or staging dir".to_string())
}

/// Read a finished child's pipes, with a hard deadline. Polling
/// `try_wait` first keeps the reconciliation thread from blocking on
/// a wedged helper forever; the helper's own 10-second per-callback
/// waits are not a bound on the whole process.
fn wait_bounded(mut child: Child, deadline: Instant) -> Result<(bool, String, String), String> {
    loop {
        match child
            .try_wait()
            .map_err(|error| format!("wait for registrar: {error}"))?
        {
            Some(status) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_end(&mut stdout);
                }
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_end(&mut stderr);
                }
                return Ok((
                    status.success(),
                    String::from_utf8_lossy(&stdout).trim().to_string(),
                    String::from_utf8_lossy(&stderr).trim().to_string(),
                ));
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("registrar timed out and was killed".to_string());
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// One reconciliation call. The helper is idempotent: adding an
/// already-registered domain is a benign race, and stale domains are
/// removed by identifier. Its stdout is the added/removed count.
fn reconcile_once() -> Result<String, String> {
    let path = helper_path()?;
    let child = Command::new(&path)
        .arg(CONFIG)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn {}: {error}", path.display()))?;
    let (success, counts, stderr) = wait_bounded(child, Instant::now() + HELPER_TIMEOUT)?;
    if success {
        Ok(counts)
    } else {
        Err(format!("registrar exited unsuccessfully: {stderr}"))
    }
}

/// Start the reconciliation loop. Polling (rather than hooking one UI
/// command) means a drive created through any path is noticed: the
/// group socket alias appears when its owner binds, and the next tick
/// registers the matching domain. No state is shared with the app.
pub(crate) fn spawn() {
    if let Err(error) = std::thread::Builder::new()
        .name("wyrd-domain-registrar".to_string())
        .spawn(|| loop {
            match reconcile_once() {
                Ok(counts) if counts != "added 0 removed 0" => {
                    eprintln!("wyrd-desktop: File Provider domains: {counts}");
                }
                Ok(_) => {}
                Err(error) => eprintln!("wyrd-desktop: domain registrar: {error}"),
            }
            std::thread::sleep(POLL_INTERVAL);
        })
    {
        eprintln!("wyrd-desktop: cannot start domain registrar: {error}");
    }
}
