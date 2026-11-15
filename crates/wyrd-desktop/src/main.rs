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
//! Nothing about startup is fatal except the argument itself: a
//! missing argument, an unknown drive directory, or an unreachable
//! owner starts the empty shell anyway. A malformed `--drive-dir`
//! (relative, `..`-carrying, or control-byte spelling) exits 2 with
//! the drive-identity rule reason — it can never key the connection
//! map. The lifecycle view then shows the registry (driveless
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
use tauri::{AppHandle, Emitter, Manager, Runtime};
use wyrd_desktop::{invoke_handler, tray, AppState, DesktopBackend, FlightTracker};

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
    parse_drive_dir_arg(std::env::args().skip(1))
}

/// Pure half of [`drive_dir_from_argv`]: parse and validate one
/// optional `--drive-dir` from explicit tokens, so the argv call
/// site pins like the registry call site. Validation joins the
/// shared drive-identity inventory before any socket is touched: a
/// relative or `..`-carrying spelling exits 2 with the rule reason
/// instead of keying a map entry no command could address.
fn parse_drive_dir_arg<I>(args: I) -> Result<Option<PathBuf>, String>
where
    I: IntoIterator<Item = String>,
{
    let mut raw: Option<String> = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--drive-dir" {
            match args.next() {
                Some(dir) if raw.is_none() => raw = Some(dir),
                Some(_) => return Err("duplicate --drive-dir".to_string()),
                None => return Err("--drive-dir needs a directory".to_string()),
            }
        } else {
            return Err(format!("unexpected argument: {arg}"));
        }
    }
    match raw {
        None => Ok(None),
        Some(dir) => wyrd_desktop::validate_drive_path(&dir, "drive_dir").map(Some),
    }
}

/// One drain per process: a second quit request while draining
/// is already covered by the spawned task, never a second drain.
static DRAINING: AtomicBool = AtomicBool::new(false);

/// Graceful quit: stop accepting new work (the window is closing or
/// the tray asked), wait until every dispatched exchange has its
/// terminal answer or the bound fires, then exit. Nothing is
/// reported successful without the owner's authoritative answer;
/// exchanges still outstanding past the bound stay unconfirmed —
/// the forced-termination path, reconciled on next launch. A plain
/// OS thread, not the async runtime: the drain only polls an atomic
/// and sleeps, and must never compete with exchanges for threads.
fn drain_and_exit<R: Runtime>(app: AppHandle<R>) {
    if DRAINING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let deadline = Instant::now() + QUIT_DRAIN_TIMEOUT;
        loop {
            let outstanding = app.state::<FlightTracker>().outstanding();
            if outstanding == 0 || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(QUIT_DRAIN_POLL);
        }
        // Exit without re-firing close-requested: the drain already
        // ran, and a second handler pass could only delay what the
        // user asked for.
        app.exit(0);
    });
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
    // The insert re-validates as the map's structural chokepoint;
    // argv already validated, so its refusal here is unreachable —
    // loud, never silent.
    if let Some(dir) = drive_dir {
        match DesktopBackend::connect(&dir) {
            Ok(backend) => {
                if let Err(reason) = state.insert(backend) {
                    eprintln!("wyrd-desktop: pre-connect {dir:?} refused: {reason}");
                    std::process::exit(2);
                }
            }
            Err(error) => eprintln!("wyrd-desktop: pre-connect {dir:?} failed: {error}"),
        }
    }
    // One drain per process lives in `drain_and_exit` above.
    tauri::Builder::default()
        .manage(state)
        .manage(FlightTracker::new())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(invoke_handler())
        .setup(|app| {
            // Menu-bar extra: one tray icon over the same registry
            // probe the lifecycle view shows. A missing icon never
            // fails startup — the window stays the product.
            let state = wyrd_desktop::registry::list_drives();
            match tray::build_tray_menu(app.handle(), &state) {
                Ok(menu) => {
                    let icon =
                        tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"));
                    match icon {
                        Ok(icon) => {
                            if let Err(error) = tauri::tray::TrayIconBuilder::with_id(tray::TRAY_ID)
                                .icon(icon)
                                .tooltip("Wyrd")
                                .menu(&menu)
                                .show_menu_on_left_click(true)
                                .on_menu_event(|app, event| match event.id.as_ref() {
                                    tray::TRAY_SHOW_ID => tray::show_main_window(app),
                                    tray::TRAY_SETUP_ID => {
                                        tray::show_main_window(app);
                                        // Best-effort route: the
                                        // frontend selects the
                                        // lifecycle view on this
                                        // event; a missing
                                        // listener still leaves
                                        // the window shown.
                                        let _ = app.emit(tray::SELECT_LIFECYCLE_EVENT, ());
                                    }
                                    tray::TRAY_QUIT_ID => drain_and_exit(app.clone()),
                                    _ => {}
                                })
                                .build(app)
                            {
                                eprintln!("wyrd-desktop: tray icon build failed: {error}");
                            }
                        }
                        Err(error) => {
                            eprintln!("wyrd-desktop: tray icon load failed: {error}");
                        }
                    }
                }
                Err(error) => eprintln!("wyrd-desktop: tray menu build failed: {error}"),
            }
            // Refresh the menu from fresh registry reads: one slow
            // owner never wedges the tray, because the probe bounds
            // each drive and a failed rebuild keeps the old menu.
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(5));
                tray::refresh_tray(&handle);
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                drain_and_exit(window.app_handle().clone());
            }
        })
        .run(tauri::generate_context!())
        .expect("wyrd-desktop: failed to run the Tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The argv call site pins like the registry call site: a valid
    /// absolute path parses, and relative, `..`, control-byte,
    /// duplicate, missing-value, and unexpected spellings refuse
    /// with their reasons — before any socket is touched.
    fn args(words: Vec<&str>) -> Vec<String> {
        words.into_iter().map(str::to_owned).collect()
    }

    #[test]
    fn argv_parsing_enforces_the_drive_identity_rule() {
        assert_eq!(
            parse_drive_dir_arg(args(vec![])).unwrap(),
            None,
            "no argument starts empty"
        );
        assert_eq!(
            parse_drive_dir_arg(args(vec!["--drive-dir", "/drives/alpha"])).unwrap(),
            Some(PathBuf::from("/drives/alpha")),
            "a valid absolute path parses"
        );
        for bad in [
            vec!["--drive-dir", "drives/alpha"],
            vec!["--drive-dir", "/drives/a/../b"],
            vec!["--drive-dir", "/drives/al\x00pha"],
        ] {
            let label = format!("{bad:?}");
            assert!(
                parse_drive_dir_arg(args(bad)).is_err(),
                "argv refuses {label}"
            );
        }
        assert!(
            parse_drive_dir_arg(args(vec!["--drive-dir", "/a", "--drive-dir", "/b"])).is_err(),
            "a duplicate flag refuses"
        );
        assert!(
            parse_drive_dir_arg(args(vec!["--drive-dir"])).is_err(),
            "a missing value refuses"
        );
        assert!(
            parse_drive_dir_arg(args(vec!["--other"])).is_err(),
            "an unexpected argument refuses"
        );
    }
}
