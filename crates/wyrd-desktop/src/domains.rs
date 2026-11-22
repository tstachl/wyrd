//! macOS File Provider domain reconciliation.
//!
//! macOS registers a File Provider domain only when the containing
//! app asks for it. The bundled Swift helper makes that call; Wyrd.app
//! invokes it as a subprocess, so no Cargo dependency edge or unsafe
//! FFI bridge is needed. The helper is signed with the app's bundle
//! identifier, and discovers drives from the group socket aliases the
//! owner publishes, so the shell needs no drive-id knowledge and the
//! owner protocol stays unchanged.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const HELPER: &str = "WyrdDomainRegistrar";
const CONFIG: &str = r#"{"group":"group.md.enesis.wyrd"}"#;
const POLL_INTERVAL: Duration = Duration::from_secs(5);

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

/// One reconciliation call. The helper is idempotent: adding an
/// already-registered domain is a benign race, and stale domains are
/// removed by identifier. Its stdout is the added/removed count.
fn reconcile_once() -> Result<String, String> {
    let path = helper_path()?;
    let output = Command::new(&path)
        .arg(CONFIG)
        .output()
        .map_err(|error| format!("spawn {}: {error}", path.display()))?;
    let counts = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if output.status.success() {
        Ok(counts)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "registrar exited {}: {}",
            output.status,
            stderr.trim()
        ))
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
