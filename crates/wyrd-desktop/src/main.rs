//! The Wyrd desktop application: a non-owning Tauri client of the
//! drive-owner interface.
//!
//! Startup takes exactly one argument: the drive directory whose
//! owner socket this instance serves (`--drive-dir <DIR>`). No
//! environment variables, no config-file search, no discovery
//! service — the installer provisions the owner and launches this
//! binary against its drive, and the drive-owner bootstrap contract
//! (`docs/design/desktop-shell.md`) owns everything fancier. A
//! missing or unreachable owner is fatal at startup (exit 2 with
//! the reason on stderr): an owner that goes down *after* startup
//! shows the UI's unavailable view instead, and coming back
//! resumes without restart.
//!
//! The binary exists so `cargo tauri dev` has a target to run; the
//! launchable artifact (bundle, icons, services) lands with the
//! installer work.

use std::path::PathBuf;
use wyrd_desktop::{invoke_handler, AppState, DesktopBackend};

/// Usage: `wyrd-desktop --drive-dir <DIR>`. Hand-rolled argv: the
/// layer gate reserves this crate's third-party edges for the Tauri
/// toolkit, serde, and error plumbing — no CLI parser.
fn drive_dir_from_argv() -> Result<PathBuf, String> {
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
    drive_dir.ok_or_else(|| "usage: wyrd-desktop --drive-dir <DIR>".to_string())
}

fn main() {
    let drive_dir = match drive_dir_from_argv() {
        Ok(dir) => dir,
        Err(message) => {
            eprintln!("wyrd-desktop: {message}");
            std::process::exit(2);
        }
    };
    let backend = match DesktopBackend::connect(&drive_dir) {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!("wyrd-desktop: {error}");
            eprintln!("wyrd-desktop: start the owner for this drive, then relaunch");
            std::process::exit(2);
        }
    };
    tauri::Builder::default()
        .manage(AppState::new(backend))
        .invoke_handler(invoke_handler())
        .run(tauri::generate_context!())
        .expect("wyrd-desktop: failed to run the Tauri application");
}
