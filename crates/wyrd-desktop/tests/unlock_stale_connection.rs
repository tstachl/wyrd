//! Stale cached connections do not survive owner replacement: after
//! an owner restart out from under the app, the next exchange mints a
//! fresh connection instead of breaking on the pre-restart stream and
//! poisoning the client (which would report every later probe as
//! Unconfirmed "the connection broke on an earlier request" until a
//! manual reconnect).
//!
//! Setup per test: a fake owner socket whose peer reads one request
//! frame and then answers torn (two garbage bytes, then EOF), so the
//! first exchange breaks mid-read and poisons the cached client — the
//! pre-restart stream. The peer drops only after reading the request,
//! which the client writes strictly after arming its timeouts, so no
//! scheduling order can fail the connect itself (on macOS a peer that
//! drops first makes `set_read_timeout` fail with EINVAL). Then the
//! lifecycle operation under test runs against an immediately-exiting
//! fake CLI, and a second `liveness` decides: evicting operations
//! reconnect fresh and report Unavailable (nothing serves); the
//! non-evicting leave path keeps the poisoned client and still
//! reports Unconfirmed.
//!
//! Process note: the registry path resolves from `HOME` (macOS) and
//! `XDG_STATE_HOME` (Linux), so the harness points both at a temp
//! dir. Those variables are process-global and this binary runs its
//! tests on multiple threads, so the tests serialize on `ENV_LOCK`;
//! the values are restored afterwards.

use std::os::unix::net::UnixListener;

/// Serializes the tests in this binary: `HOME`/`XDG_STATE_HOME`
/// point at a per-test temp dir and are process-global.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// One poisoned drive: temp dirs, isolated env, a torn-reply fake
/// owner socket, and a mock app whose CLI override exits at once.
struct Harness {
    dir: std::path::PathBuf,
    drive: String,
    identity_file: std::path::PathBuf,
    passphrase_file: std::path::PathBuf,
    app: tauri::App<tauri::test::MockRuntime>,
    _restore: RestoreEnv,
    acceptor: Option<std::thread::JoinHandle<()>>,
    _listener: Option<UnixListener>,
}

impl Harness {
    fn setup(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-stale-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let home = dir.join("home");
        let xdg = dir.join("xdg");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&xdg).unwrap();

        // Caller holds `ENV_LOCK`: `HOME`/`XDG_STATE_HOME` point at
        // this temp dir and are process-global.
        let restore = RestoreEnv {
            home: std::env::var_os("HOME"),
            xdg: std::env::var_os("XDG_STATE_HOME"),
        };
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_STATE_HOME", &xdg);

        let drive_dir = dir.join("drive");
        std::fs::create_dir_all(&drive_dir).unwrap();
        let identity_file = dir.join("identity");
        std::fs::write(&identity_file, b"identity-bytes").unwrap();
        let passphrase_file = dir.join("passphrase");
        std::fs::write(&passphrase_file, b"passphrase-bytes").unwrap();

        // The pre-restart stream: read one request frame, then answer
        // torn (two garbage bytes, then EOF), so the first exchange
        // breaks mid-read and poisons the cached client. The drop
        // lands strictly after the client's write, which follows its
        // timeout arming — no schedule can fail the connect itself.
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

        // Immediately-exiting fake CLI: the version gate must still
        // answer or nothing spawns; the early exit resolves no
        // session and authorizes nothing.
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

        Harness {
            dir,
            drive: drive_dir.to_string_lossy().into_owned(),
            identity_file,
            passphrase_file,
            app,
            _restore: restore,
            acceptor: Some(acceptor),
            _listener: None,
        }
    }

    /// Break the first exchange on the pre-restart stream and prove
    /// the cached client poisoned; drop the listener so the next
    /// connect fails fast instead of hanging on a live socket.
    fn poison(&mut self) {
        let handle = self.app.handle().clone();
        let stale = tauri::async_runtime::block_on(wyrd_desktop::commands::liveness(
            handle,
            self.drive.clone(),
        ));
        assert!(
            matches!(stale, Err(wyrd_desktop::BackendError::Unconfirmed { .. })),
            "the pre-restart stream breaks the first exchange: {stale:?}"
        );
        self.acceptor
            .take()
            .expect("one acceptor")
            .join()
            .expect("the acceptor drops its end");
    }

    /// One liveness probe through the app's connection map.
    fn liveness(&self) -> Result<wyrd_owner_client::WireLiveness, wyrd_desktop::BackendError> {
        let handle = self.app.handle().clone();
        tauri::async_runtime::block_on(wyrd_desktop::commands::liveness(handle, self.drive.clone()))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Unlock evicts the stale cached connection: the exchange past the
/// restart connects fresh.
#[test]
fn unlock_evicts_the_stale_cached_connection() {
    let _env = ENV_LOCK.lock().unwrap();
    let mut harness = Harness::setup("unlock");
    harness.poison();

    // Unlock spawns (the fake exits at once, so no session lingers)
    // and evicts the cached connection with the spawn.
    let handle = harness.app.handle().clone();
    let report = tauri::async_runtime::block_on(wyrd_desktop::commands::drive_unlock(
        handle,
        wyrd_desktop::commands::DriveUnlockArgs {
            drive_dir: harness.drive.clone(),
            identity_file: Some(harness.identity_file.to_string_lossy().into_owned()),
            credential_backend: None,
            credential_account: None,
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
    let fresh = harness.liveness();
    assert!(
        matches!(fresh, Err(wyrd_desktop::BackendError::Unavailable { .. })),
        "a fresh connection reports unavailable, not poisoned: {fresh:?}"
    );
}

/// Reconfigure evicts the stale cached connection: a provisioned
/// restart mints a fresh connection on the next probe.
#[test]
fn reconfigure_evicts_the_stale_cached_connection() {
    let _env = ENV_LOCK.lock().unwrap();
    let mut harness = Harness::setup("reconfigure");
    harness.poison();

    let handle = harness.app.handle().clone();
    let report = tauri::async_runtime::block_on(wyrd_desktop::commands::provision_reconfigure(
        handle,
        wyrd_desktop::commands::ProvisionReconfigureArgs {
            drive_dir: harness.drive.clone(),
            relays: Vec::new(),
            identity_file: Some(harness.identity_file.to_string_lossy().into_owned()),
            passphrase_file: Some(harness.passphrase_file.to_string_lossy().into_owned()),
            credential_backend: None,
            credential_account: None,
            mode: "vault".to_string(),
            mountpoint: None,
            no_service: true,
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

    let fresh = harness.liveness();
    assert!(
        matches!(fresh, Err(wyrd_desktop::BackendError::Unavailable { .. })),
        "a fresh connection reports unavailable, not poisoned: {fresh:?}"
    );
}

/// Leave keeps its connection: authoring membership does not replace
/// the server, so a poisoned cache stays poisoned — the evict flag
/// is load-bearing, and this pins `false`.
#[test]
fn leave_keeps_the_cached_connection() {
    let _env = ENV_LOCK.lock().unwrap();
    let mut harness = Harness::setup("leave");
    harness.poison();

    let handle = harness.app.handle().clone();
    let report = tauri::async_runtime::block_on(wyrd_desktop::commands::provision_leave(
        handle,
        wyrd_desktop::commands::ProvisionLeaveArgs {
            drive_dir: harness.drive.clone(),
            identity_file: Some(harness.identity_file.to_string_lossy().into_owned()),
            passphrase_file: Some(harness.passphrase_file.to_string_lossy().into_owned()),
            credential_backend: None,
            credential_account: None,
            acknowledged: true,
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

    let still = harness.liveness();
    assert!(
        matches!(still, Err(wyrd_desktop::BackendError::Unconfirmed { .. })),
        "leave evicts nothing: the poisoned cache still reports unconfirmed: {still:?}"
    );
}
