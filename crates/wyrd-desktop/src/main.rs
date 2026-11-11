//! The Wyrd desktop application: a non-owning Tauri client of the
//! drive-owner interface, one window over many independently managed
//! drives.
//!
//! Startup takes zero or one argument: an optional drive directory
//! to pre-connect (`--drive-dir <DIR>`). No environment variables,
//! no config-file search, no discovery service — the installer
//! provisions the owner and may launch this binary against its
//! drive, and the drive-owner bootstrap contract
//! (`docs/design/desktop-shell.md`) owns everything fancier. The
//! shell keeps no implicit active drive: the argument only warms
//! the connection map, and the frontend's selection is presentation
//! state, never an authorization boundary.
//!
//! Nothing about startup is fatal: a missing argument, an unknown
//! drive directory, or an unreachable owner starts the empty shell
//! anyway. The lifecycle view then shows the registry (driveless
//! first-run where nothing is registered yet) with per-drive
//! reachability, and each stopped owner reads as its own
//! unavailable state. An owner that goes down *after* startup
//! shows the same per-drive unavailable view instead, and coming
//! back resumes without restart.
//!
//! Shutdown drains outstanding work first. Closing the window
//! prevents the close, waits until every dispatched exchange has
//! its terminal answer or the drain bound fires, then exits —
//! nothing is reported successful without the owner's authoritative
//! answer, and exchanges still outstanding past the bound stay
//! unconfirmed: the next launch reconciles them through the owner
//! before any retry. New work may still dispatch during the drain
//! (the window is closing, so in practice none does); the bound
//! keeps a wedged owner from wedging quit.
//!
//! The binary exists so `cargo tauri dev` has a target to run; the
//! launchable artifact (bundle, icons, services) lands with the
//! installer work.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::Manager;
use wyrd_desktop::{invoke_handler, AppState, DesktopBackend, FlightTracker};

/// Bound on the quit drain: every dispatched exchange gets this
/// long past close-request to reach its terminal answer. Past the
/// bound the shell exits anyway and the abandoned exchanges stay
/// unconfirmed — the forced-termination path, reconciled on next
/// launch. Thirty seconds is a UX judgment, not a protocol fact: a
/// hung owner must not wedge quit, and a healthy one answers in
/// milliseconds.
const QUIT_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Poll interval while draining: frequent enough to exit promptly
/// when the last answer lands, sparse enough to stay out of the way.
const QUIT_DRAIN_POLL: Duration = Duration::from_millis(50);

/// Usage: `wyrd-desktop [--drive-dir <DIR>]`. Hand-rolled argv: the
/// layer gate reserves this crate's third-party edges for the Tauri
/// toolkit, serde, and error plumbing — no CLI parser. The argument
/// is optional: without it the shell starts with an empty connection
/// map and the lifecycle view carries first-run.
fn drive_dir_from_argv() -> Result<Option<PathBuf>, String> {
    let mut args = std::env::args().skip(1);
    let mut drive_dir: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        if arg == "--drive-dir" {
            match args.next() {
                Some(dir) if drive_dir.is_none() => drive_dir = Some(PathBuf::from(dir)),
                Some(_) => return Err("duplicate --drive-dir".to_string()),
                None => return Err("--drive-dir needs a directory".to_string()),
            }
        } else {
            return Err(format!("unexpected argument: {arg}"));
        }
    }
    Ok(drive_dir)
}

fn main() {
    let drive_dir = match drive_dir_from_argv() {
        Ok(dir) => dir,
        Err(message) => {
            eprintln!("wyrd-desktop: {message}");
            std::process::exit(2);
        }
    };
    let state = AppState::new();
    // Best-effort pre-connect: where the owner answers, the map
    // starts warm; where it does not, startup still succeeds and
    // the lifecycle view reports the drive unavailable per-drive.
    if let Some(dir) = drive_dir {
        match DesktopBackend::connect(&dir) {
            Ok(backend) => state.insert(backend),
            Err(error) => eprintln!("wyrd-desktop: pre-connect {dir:?} failed: {error}"),
        }
    }
    // One drain per process: a second close request while draining
    // is already covered by the spawned task, never a second drain.
    static DRAINING: AtomicBool = AtomicBool::new(false);
    tauri::Builder::default()
        .manage(state)
        .manage(FlightTracker::new())
        .invoke_handler(invoke_handler())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if DRAINING.swap(true, Ordering::SeqCst) {
                    return;
                }
                let app = window.app_handle().clone();
                // A plain OS thread, not the async runtime: the drain
                // only polls an atomic and sleeps, and must never
                // compete with exchanges for runtime threads.
                std::thread::spawn(move || {
                    let deadline = Instant::now() + QUIT_DRAIN_TIMEOUT;
                    loop {
                        let outstanding = app.state::<FlightTracker>().outstanding();
                        if outstanding == 0 || Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(QUIT_DRAIN_POLL);
                    }
                    // Exit without re-firing close-requested: the
                    // drain already ran, and a second handler pass
                    // could only delay what the user asked for.
                    app.exit(0);
                });
            }
        })
        .run(tauri::generate_context!())
        .expect("wyrd-desktop: failed to run the Tauri application");
}
