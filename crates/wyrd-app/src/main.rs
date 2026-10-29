//! Developer-run host binary: the owner process with no presentation
//! attached. Headless by construction — the window attaches later as
//! a non-owner consumer — so this is the shape the Lima order
//! evidence runs against: the long-running process is the host, not
//! the mount.
//!
//! Developer scaffolding, not the product surface (OD-06-D decides
//! the shipped artifact): credentials arrive over environment
//! variables, never a third credential path, and relay set and
//! loopback are flags. Usage:
//! `wyrd-app <drive-dir> --relay <url>... [--loopback]`
//! with `WYRD_PASSPHRASE` and `WYRD_IDENTITY_HEX` (32-byte signer key)
//! in the environment.

use std::path::PathBuf;
use std::sync::Arc;

use wyrd_app::{compose, exit_code, loop_result, open_host, telemetry_emit};
use wyrd_daemon::posture::PostureRecorder;
use wyrd_daemon::process::{bind_bulk_source, install_shutdown_handler, SHUTDOWN};
use wyrd_daemon::{run_composed_owner, VaultOutcome};
use wyrd_sync::keys::DeviceIdentitySecret;

fn usage() -> ! {
    eprintln!("usage: wyrd-app <drive-dir> --relay <url>... [--loopback]");
    eprintln!("env: WYRD_PASSPHRASE, WYRD_IDENTITY_HEX (32-byte hex signer key)");
    std::process::exit(2);
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        eprintln!("missing required environment: {name}");
        usage();
    })
}

fn main() {
    let mut argv = std::env::args().skip(1);
    let drive_dir = argv.next().map(PathBuf::from).unwrap_or_else(|| usage());
    let mut relays: Vec<String> = Vec::new();
    let mut loopback = false;
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--relay" => relays.push(argv.next().unwrap_or_else(|| usage())),
            "--loopback" => loopback = true,
            _ => usage(),
        }
    }
    if relays.is_empty() && !loopback {
        eprintln!("a relay-less host serves routes nothing can discover: pass --relay");
        std::process::exit(2);
    }
    let passphrase = env("WYRD_PASSPHRASE");
    let identity_hex = env("WYRD_IDENTITY_HEX");
    let identity_bytes = hex::decode(identity_hex.trim()).unwrap_or_else(|_| usage());
    let identity_arr: [u8; 32] = identity_bytes.try_into().unwrap_or_else(|_| usage());
    let identity = DeviceIdentitySecret::from_bytes(identity_arr).unwrap_or_else(|_| usage());

    let open = match open_host(drive_dir.clone(), &passphrase, identity.clone()) {
        Ok(open) => open,
        Err(error) => {
            eprintln!("cannot start host: {error}");
            std::process::exit(1);
        }
    };
    install_shutdown_handler().expect("signal handler installs");
    // One shared composition with every other owner-mode entry
    // point: store, retention, node, serving surface, live split.
    let composed = compose(open, loopback).unwrap_or_else(|error| {
        eprintln!("composition refused: {error}");
        std::process::exit(1);
    });
    eprintln!("host serving: {}", composed.serving_id);
    let signer_keys = identity.signer_keys();
    let open_secret = signer_keys.secret_key().clone();
    let seen_path = drive_dir.join("mailbox.seen");
    let configured_relays = relays.len();
    let mailbox =
        wyrd_core::mailbox::LiveMailbox::connect(signer_keys, open_secret, relays, seen_path)
            .unwrap_or_else(|error| {
                eprintln!("mailbox connect failed: {error}");
                std::process::exit(1);
            });
    // Arrival short-circuits the loop's idle wait the way it does for
    // the vault: without this, every park burns its full window even
    // as mail lands.
    mailbox.attach_waker(Arc::clone(composed.live.waker()));
    let bulk = bind_bulk_source().unwrap_or_else(|error| {
        eprintln!("bulk bind failed: {error}");
        std::process::exit(1);
    });
    let recorder = Arc::new(
        PostureRecorder::new(&drive_dir, composed.serving_id.clone()).unwrap_or_else(|error| {
            eprintln!("posture init failed: {error}");
            std::process::exit(1);
        }),
    );
    recorder.note_startup();
    let outcome: VaultOutcome = run_composed_owner(
        composed,
        &SHUTDOWN,
        mailbox,
        Some(bulk),
        telemetry_emit(Arc::clone(&recorder)),
        configured_relays,
    )
    .unwrap_or_else(|error| {
        eprintln!("run failed: {error}");
        std::process::exit(1);
    });
    if let Err(reason) = loop_result(&outcome) {
        eprintln!("{reason}");
    }
    std::process::exit(exit_code(&outcome));
}
