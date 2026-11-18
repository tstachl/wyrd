//! Unlock relay wiring: `drive_unlock` feeds the registered relay
//! set into the spawned unlock argv.
//!
//! The unit seam (`registry_relays_for_at`) pins the lookup and the
//! argv pin (`unlock_argv_is_structured_with_the_pipe_flag`) pins
//! emission from a hand-built request — this proof closes the loop
//! between them through the real command: a registry entry naming a
//! relay, a recording fake CLI, and an assertion that the spawned
//! argv carries `--relay <registered relay>` while the passphrase
//! rides stdin only.
//!
//! Process note: the registry path resolves from `HOME` (macOS) and
//! `XDG_STATE_HOME` (Linux), so the test points both at a temp dir.
//! This file is its own test binary — no other test in this process
//! reads those variables — and the values are restored afterwards.

use std::path::PathBuf;

fn registry_file(home: &std::path::Path, xdg: &std::path::Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let _ = xdg;
        home.join("Library/Application Support/Wyrd/drives.json")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = home;
        xdg.join("wyrd/drives.json")
    }
}

/// Restore process environment behind the test, even on panic: the
/// registry path resolves from `HOME`/`XDG_STATE_HOME`, which this
/// binary points at a temp dir.
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

/// `drive_unlock` against a registry entry with relays spawns the
/// owner with `--relay` from the registry — the relay-less refusal
/// for registry-provisioned drives without a `default.toml`.
#[test]
fn drive_unlock_spawns_with_the_registered_relays() {
    let dir =
        std::env::temp_dir().join(format!("wyrd-desktop-unlock-relays-{}", std::process::id()));
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

    let registry = registry_file(&home, &xdg);
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    let drive_dir = dir.join("drive");
    std::fs::create_dir_all(&drive_dir).unwrap();
    let identity_file = dir.join("identity");
    std::fs::write(&identity_file, b"identity-bytes").unwrap();
    std::fs::write(
        &registry,
        serde_json::json!({
            "version": 1,
            "drives": {
                "alpha": {
                    "name": "alpha",
                    "drive_dir": drive_dir.to_string_lossy(),
                    "relays": ["wss://registry-relay.example"],
                    "service": "drive-alpha-no-such-definition",
                    "mode": "vault",
                },
            },
        })
        .to_string(),
    )
    .unwrap();

    // Recording fake CLI: version gate, argv log, stdin capture,
    // then an immediate refusal (early exit exercises the
    // collect-detail path; the argv is already recorded).
    let cli = dir.join("wyrd");
    let argv_log = dir.join("argv.log");
    let stdin_captured = dir.join("stdin.bin");
    let version = env!("CARGO_PKG_VERSION");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nprintf '%s\\n' \"$@\" > {}\ncat > {}\nexit 2\n",
            argv_log.display(),
            stdin_captured.display()
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

    let secret = "unlock-relay-wiring-probe";
    let report = tauri::async_runtime::block_on(wyrd_desktop::commands::drive_unlock(
        app.handle().clone(),
        wyrd_desktop::commands::DriveUnlockArgs {
            drive_dir: drive_dir.to_string_lossy().into_owned(),
            identity_file: identity_file.to_string_lossy().into_owned(),
            passphrase: secret.to_string(),
        },
    ));
    // Early exit refuses: the wiring assertion below reads the
    // recorded argv, not the verdict.
    assert!(
        matches!(
            report.result,
            wyrd_desktop::ProvisionResult::ExpectedFailure { .. }
        ),
        "the refusing fake resolves expected-failure: {:?}",
        report.result
    );

    let argv = std::fs::read_to_string(&argv_log).expect("the fake logs its argv");
    let lines: Vec<&str> = argv.lines().collect();
    let relay = lines
        .iter()
        .position(|line| *line == "--relay")
        .expect("the spawned argv carries --relay");
    assert_eq!(
        lines.get(relay + 1),
        Some(&"wss://registry-relay.example"),
        "the relay comes from the registry entry: {lines:?}"
    );
    assert!(
        !argv.contains("unlock-relay-wiring-probe"),
        "the secret never rides argv: {lines:?}"
    );
    assert_eq!(
        std::fs::read(&stdin_captured).expect("the fake captures stdin"),
        secret.as_bytes(),
        "the secret rides stdin only"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
