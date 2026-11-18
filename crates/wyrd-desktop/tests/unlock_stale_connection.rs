//! Unlock evicts the stale cached connection: after an owner restart
//! out from under the app, the next exchange mints a fresh connection
//! instead of breaking on the pre-restart stream and poisoning the
//! client (which would report every later probe as Unconfirmed "the
//! connection broke on an earlier request" until a manual reconnect).
//!
//! Setup: a fake owner socket whose peer reads one request frame and
//! then answers torn (two garbage bytes, then EOF), so the first
//! exchange breaks mid-read and poisons the cached client — the
//! pre-restart stream. The peer drops only after reading the request,
//! which the client writes strictly after arming its timeouts, so no
//! scheduling order can fail the connect itself (on macOS a peer that
//! drops first makes `set_read_timeout` fail with EINVAL). Then
//! `drive_unlock` with an immediately-exiting fake CLI (the spawn
//! succeeds, so the evict runs; the early exit stores no session),
//! then a second `liveness`. Fixed: the second exchange connects
//! fresh and reports Unavailable (nothing serves). Broken: it reuses
//! the poisoned client and reports Unconfirmed.
//!
//! Process note: the registry path resolves from `HOME` (macOS) and
//! `XDG_STATE_HOME` (Linux), so the test points both at a temp dir.
//! This file is its own test binary — no other test in this process
//! reads those variables — and the values are restored afterwards.

use std::os::unix::net::UnixListener;

/// Restore process environment behind the test, even on panic.
struct RestoreEnv {
    home: Option<std::ffi::OsString>,
    xdg: Option<std::ffi::OsString>,
}

impl Drop for RestoreEnv {
    fn drop(&mut self) {
        match self.home.take() {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match self.xdg.take() {
            Some(value) => std::env::set_var("XDG_STATE_HOME", value),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
    }
}

/// A stale cached connection does not survive unlock: the exchange
/// past the restart connects fresh.
#[test]
fn unlock_evicts_the_stale_cached_connection() {
    let dir =
        std::env::temp_dir().join(format!("wyrd-desktop-unlock-evict-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let home = dir.join("home");
    let xdg = dir.join("xdg");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&xdg).unwrap();

    // Process-local registry: this binary runs no other test.
    let _restore = RestoreEnv {
        home: std::env::var_os("HOME"),
        xdg: std::env::var_os("XDG_STATE_HOME"),
    };
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_STATE_HOME", &xdg);

    let drive_dir = dir.join("drive");
    std::fs::create_dir_all(&drive_dir).unwrap();
    let identity_file = dir.join("identity");
    std::fs::write(&identity_file, b"identity-bytes").unwrap();
    let drive = drive_dir.to_string_lossy().into_owned();

    // The pre-restart stream: read one request frame, then answer
    // torn (two garbage bytes, then EOF), so the first exchange
    // breaks mid-read and poisons the cached client. The drop lands
    // strictly after the client's write, which follows its timeout
    // arming — no schedule can fail the connect itself.
    let socket = wyrd_owner_client::socket_path_for(&drive_dir);
    let listener = UnixListener::bind(&socket).expect("the fake owner socket binds");
    let acceptor = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let Ok((mut peer, _)) = listener.accept() else {
            return;
        };
        let _ = peer.set_read_timeout(Some(std::time::Duration::from_secs(10)));
        let mut len = [0u8; 4];
        if peer.read_exact(&mut len).is_err() {
            return;
        }
        let len = u32::from_be_bytes(len) as usize;
        let mut body = vec![0u8; len.min(1024)];
        let _ = peer.read_exact(&mut body);
        let _ = peer.write_all(b"hi");
        // Drop here: EOF after the short reply.
    });

    // Recording fake CLI is unnecessary here — an immediately-exiting
    // fake proves the spawn path evicts — but the version gate must
    // still answer or nothing spawns.
    let cli = dir.join("wyrd");
    let version = env!("CARGO_PKG_VERSION");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nexit 2\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let state = wyrd_desktop::AppState::new();
    state.set_cli_override(cli);
    let app = tauri::test::mock_builder()
        .manage(state)
        .manage(wyrd_desktop::FlightTracker::new())
        .invoke_handler(wyrd_desktop::invoke_handler())
        .build(tauri::generate_context!())
        .expect("the mock app builds");
    let handle = app.handle().clone();

    // First exchange breaks on the pre-restart stream and poisons the
    // cached client — the setup this test escapes from.
    let stale = tauri::async_runtime::block_on(wyrd_desktop::commands::liveness(
        handle.clone(),
        drive.clone(),
    ));
    assert!(
        matches!(stale, Err(wyrd_desktop::BackendError::Unconfirmed { .. })),
        "the pre-restart stream breaks the first exchange: {stale:?}"
    );
    acceptor.join().expect("the acceptor drops its end");
    drop(handle);

    // Unlock spawns (the fake exits at once, so no session lingers)
    // and evicts the cached connection with the spawn.
    let handle = app.handle().clone();
    let report = tauri::async_runtime::block_on(wyrd_desktop::commands::drive_unlock(
        handle.clone(),
        wyrd_desktop::commands::DriveUnlockArgs {
            drive_dir: drive.clone(),
            identity_file: identity_file.to_string_lossy().into_owned(),
            passphrase: "stale-connection-probe-passphrase".to_string(),
        },
    ));
    assert!(
        matches!(
            report.result,
            wyrd_desktop::ProvisionResult::ExpectedFailure { .. }
        ),
        "the exiting fake resolves expected-failure: {:?}",
        report.result
    );

    // Past the restart the exchange connects fresh: nothing serves,
    // so Unavailable — never the poisoned "earlier request".
    let fresh =
        tauri::async_runtime::block_on(wyrd_desktop::commands::liveness(handle.clone(), drive));
    assert!(
        matches!(fresh, Err(wyrd_desktop::BackendError::Unavailable { .. })),
        "a fresh connection reports unavailable, not poisoned: {fresh:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
