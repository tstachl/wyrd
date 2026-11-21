use super::*;

fn create_request() -> ProvisionRequest {
    ProvisionRequest {
        name: "alpha".to_string(),
        drive_dir: PathBuf::from("/drives/alpha"),
        relays: vec!["wss://relay.example".to_string()],
        credentials: CredentialSource::File {
            identity_file: Some(PathBuf::from("/keys/identity")),
            passphrase_file: Some(PathBuf::from("/keys/passphrase")),
        },
        invitation_file: None,
        registry_path: None,
        owner_bin: None,
        supervise_bin: None,
        no_service: false,
        mode: "vault".to_string(),
        mountpoint: None,
    }
}

/// The bundle-layout candidate is pure path math:
/// `Contents/MacOS` -> `Contents/Resources/wyrd`, and the sibling
/// stays the fallback. The existence gate lives in
/// `resolve_cli_bin`, not here.
#[test]
fn bundled_cli_path_maps_macos_to_resources() {
    let macos = Path::new("/Applications/Wyrd.app/Contents/MacOS");
    assert_eq!(
        bundled_cli_path(macos),
        Some(PathBuf::from(
            "/Applications/Wyrd.app/Contents/Resources/wyrd"
        ))
    );
    assert_eq!(
        resolve_cli_bin(Some(PathBuf::from("/custom/wyrd"))).unwrap(),
        PathBuf::from("/custom/wyrd"),
        "an explicit path passes through untouched"
    );
}

/// The create argv is one structured element per argument: no
/// shell, no joined command line, credential paths as bare
/// values — contents never travel.
#[test]
fn create_argv_is_structured_per_element() {
    let argv = bootstrap_argv(&create_request()).unwrap();
    let rendered: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        rendered,
        vec![
            "drive",
            "/drives/alpha",
            "--identity-file",
            "/keys/identity",
            "--passphrase-file",
            "/keys/passphrase",
            "bootstrap",
            "--name",
            "alpha",
            "--mode",
            "vault",
            "--relay",
            "wss://relay.example",
        ]
    );
    assert!(
        !rendered
            .iter()
            .any(|arg| arg.contains(' ') && arg.contains("--")),
        "no joined shell string: {rendered:?}"
    );
}

/// Join adds exactly the invitation-file pair; `--no-service`
/// and explicit paths thread through untouched.
#[test]
fn join_argv_adds_the_invitation_pair() {
    let mut request = create_request();
    request.invitation_file = Some(PathBuf::from("/tmp/invite.sealed"));
    request.no_service = true;
    request.registry_path = Some(PathBuf::from("/tmp/drives.json"));
    let argv = bootstrap_argv(&request).unwrap();
    let rendered: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(rendered.contains(&"--invitation-file".to_string()));
    assert!(rendered.contains(&"/tmp/invite.sealed".to_string()));
    assert!(rendered.contains(&"--no-service".to_string()));
    assert!(rendered.contains(&"--registry".to_string()));
}

/// Mount argv carries `--mode mount` plus the mountpoint pair:
/// the FUSE projection the managed mount mode serves.
#[test]
fn mount_argv_carries_the_mode_and_mountpoint_pairs() {
    let mut request = create_request();
    request.mode = "mount".to_string();
    request.mountpoint = Some(PathBuf::from("/mnt/alpha"));
    let argv = bootstrap_argv(&request).unwrap();
    let rendered: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(rendered.contains(&"--mode".to_string()));
    assert!(rendered.contains(&"mount".to_string()));
    assert!(rendered.contains(&"--mountpoint".to_string()));
    assert!(rendered.contains(&"/mnt/alpha".to_string()));
}

/// Mount without a mountpoint refuses before any process spawns,
/// like vault carrying one: the pair travels together or not at
/// all.
#[test]
fn mount_without_a_mountpoint_refuses_before_spawning() {
    let mut request = create_request();
    request.mode = "mount".to_string();
    assert!(request.mountpoint.is_none());
    let reason = bootstrap_argv(&request).unwrap_err();
    assert!(reason.contains("needs a mountpoint"), "{reason}");
    let mut stray = create_request();
    stray.mountpoint = Some(PathBuf::from("/mnt/stray"));
    let reason = bootstrap_argv(&stray).unwrap_err();
    assert!(reason.contains("carries no mountpoint"), "{reason}");
}

/// Unknown modes refuse before any process spawns: the adapter
/// never provisions a service the registry reader would skip.
#[test]
fn unknown_mode_refuses_before_spawning() {
    let mut request = create_request();
    request.mode = "fuse".to_string();
    match bootstrap_argv(&request) {
        Err(reason) => assert!(
            reason.contains("unknown service mode"),
            "reason names the unknown mode: {reason}"
        ),
        Ok(_) => panic!("an unknown mode must refuse"),
    }
}

/// A fresh store-backed bootstrap seeds the item from the
/// one-time input files and names the store reference: seed
/// flags first (the CLI reads them as input), then the backend
/// pair the dispatcher routes on. No seed material in the
/// reference — only file paths and the account label.
#[test]
fn store_backed_bootstrap_seeds_then_references() {
    let mut request = create_request();
    request.credentials = CredentialSource::OsStore {
        account: "wyrd-drive-alpha".to_string(),
        seed: Some(SeedFiles {
            identity_file: PathBuf::from("/keys/identity"),
            passphrase_file: PathBuf::from("/keys/passphrase"),
        }),
    };
    let rendered: Vec<String> = bootstrap_argv(&request)
        .unwrap()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        rendered,
        vec![
            "drive",
            "/drives/alpha",
            "--identity-file",
            "/keys/identity",
            "--passphrase-file",
            "/keys/passphrase",
            "--credential-backend",
            "os-store",
            "--credential-account",
            "wyrd-drive-alpha",
            "bootstrap",
            "--name",
            "alpha",
            "--mode",
            "vault",
            "--relay",
            "wss://relay.example",
        ]
    );
}

/// A converged store-backed re-run names only the reference:
/// the item already holds the material, so no seed files ride
/// along — the CLI reads the provisioned item itself.
#[test]
fn store_backed_bootstrap_without_seed_references_only() {
    let mut request = create_request();
    request.credentials = CredentialSource::OsStore {
        account: "wyrd-drive-alpha".to_string(),
        seed: None,
    };
    let rendered: Vec<String> = bootstrap_argv(&request)
        .unwrap()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        rendered,
        vec![
            "drive",
            "/drives/alpha",
            "--credential-backend",
            "os-store",
            "--credential-account",
            "wyrd-drive-alpha",
            "bootstrap",
            "--name",
            "alpha",
            "--mode",
            "vault",
            "--relay",
            "wss://relay.example",
        ]
    );
    assert!(
        !rendered.iter().any(|arg| arg == "--identity-file"),
        "a converged re-run names no files: {rendered:?}"
    );
}

/// Leave and reconfigure on a store-backed drive name the item,
/// never files: the CLI resolves the provisioned material
/// through the loader boundary, and file flags beside the
/// store backend would refuse as one-backend-per-drive.
#[test]
fn store_leave_and_reconfigure_reference_the_item() {
    let store = CredentialSource::OsStore {
        account: "wyrd-drive-alpha".to_string(),
        seed: None,
    };
    let leave = LeaveRequest {
        drive_dir: PathBuf::from("/drives/alpha"),
        credentials: store.clone(),
        acknowledged: true,
    };
    let rendered: Vec<String> = leave_argv(&leave)
        .unwrap()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        rendered,
        vec![
            "member",
            "/drives/alpha",
            "--credential-backend",
            "os-store",
            "--credential-account",
            "wyrd-drive-alpha",
            "leave",
            "--yes",
        ]
    );
    let mut reconfigure = reconfigure_request();
    reconfigure.credentials = store;
    let rendered: Vec<String> = reconfigure_argv(&reconfigure)
        .unwrap()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(rendered.contains(&"--credential-account".to_string()));
    assert!(rendered.contains(&"wyrd-drive-alpha".to_string()));
    assert!(
        !rendered.iter().any(|arg| arg == "--passphrase-file"),
        "a store reconfigure names no files: {rendered:?}"
    );
}

/// Seeds are bootstrap input only: leave, reconfigure, and
/// unlock read the provisioned credential, so a seed anywhere
/// else refuses before any process spawns — mirroring the
/// CLI's one-backend-per-drive refusal at the adapter.
#[test]
fn seeds_refuse_outside_bootstrap() {
    let seeded = CredentialSource::OsStore {
        account: "wyrd-drive-alpha".to_string(),
        seed: Some(SeedFiles {
            identity_file: PathBuf::from("/keys/identity"),
            passphrase_file: PathBuf::from("/keys/passphrase"),
        }),
    };
    let leave = LeaveRequest {
        drive_dir: PathBuf::from("/drives/alpha"),
        credentials: seeded.clone(),
        acknowledged: false,
    };
    let reason = leave_argv(&leave).unwrap_err();
    assert!(reason.contains("bootstrap input only"), "{reason}");
    let mut reconfigure = reconfigure_request();
    reconfigure.credentials = seeded.clone();
    let reason = reconfigure_argv(&reconfigure).unwrap_err();
    assert!(reason.contains("bootstrap input only"), "{reason}");
    let mut unlock = unlock_request();
    unlock.credentials = seeded;
    let reason = unlock_argv(&unlock).unwrap_err();
    assert!(reason.contains("bootstrap input only"), "{reason}");
}

/// A store-backed unlock loads the identity from the item and
/// still pipes the session passphrase: the store pair plus
/// `--passphrase-stdin`, never `--identity-file` (the CLI
/// refuses file flags beside a store unlock) and never
/// `--passphrase-file`.
#[test]
fn store_unlock_reads_the_item_identity() {
    let mut request = unlock_request();
    request.credentials = CredentialSource::OsStore {
        account: "wyrd-drive-alpha".to_string(),
        seed: None,
    };
    let rendered: Vec<String> = unlock_argv(&request)
        .unwrap()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        rendered,
        vec![
            "drive",
            "/drives/alpha",
            "--credential-backend",
            "os-store",
            "--credential-account",
            "wyrd-drive-alpha",
            "--passphrase-stdin",
            "unlock",
            "--relay",
            "wss://relay.example",
        ]
    );
}

/// An empty store account names no item: every operation
/// refuses before any process spawns instead of rendering
/// flags the CLI would reject without the reference.
#[test]
fn empty_store_account_refuses_before_spawning() {
    let empty = CredentialSource::OsStore {
        account: String::new(),
        seed: None,
    };
    let mut bootstrap = create_request();
    bootstrap.credentials = empty.clone();
    let reason = bootstrap_argv(&bootstrap).unwrap_err();
    assert!(
        reason.contains("no OS credential-store account"),
        "{reason}"
    );
    let leave = LeaveRequest {
        drive_dir: PathBuf::from("/drives/alpha"),
        credentials: empty.clone(),
        acknowledged: false,
    };
    let reason = leave_argv(&leave).unwrap_err();
    assert!(
        reason.contains("no OS credential-store account"),
        "{reason}"
    );
    let unlock = UnlockRequest {
        drive_dir: PathBuf::from("/drives/alpha"),
        credentials: empty,
        relays: Vec::new(),
        retained_quota: None,
    };
    let reason = unlock_argv(&unlock).unwrap_err();
    assert!(
        reason.contains("no OS credential-store account"),
        "{reason}"
    );
}

/// Leave argv carries `--yes` only on explicit acknowledgement:
/// a frontend omitting the dialog cannot bypass the CLI boundary.
/// Credentials precede the `leave` verb: they are flattened on the
/// parent, not global, so anything after the verb is rejected.
#[test]
fn leave_argv_gates_yes_on_acknowledgement() {
    let base = LeaveRequest {
        drive_dir: PathBuf::from("/drives/alpha"),
        credentials: CredentialSource::File {
            identity_file: Some(PathBuf::from("/keys/identity")),
            passphrase_file: Some(PathBuf::from("/keys/passphrase")),
        },
        acknowledged: false,
    };
    let plain: Vec<String> = leave_argv(&base)
        .unwrap()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        !plain.contains(&"--yes".to_string()),
        "no acknowledgement, no --yes: {plain:?}"
    );
    assert_eq!(
        plain,
        vec![
            "member",
            "/drives/alpha",
            "--identity-file",
            "/keys/identity",
            "--passphrase-file",
            "/keys/passphrase",
            "leave",
        ],
        "credentials precede the verb even without --yes: {plain:?}"
    );
    let acked = LeaveRequest {
        acknowledged: true,
        ..base
    };
    let confirmed: Vec<String> = leave_argv(&acked)
        .unwrap()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        confirmed,
        vec![
            "member",
            "/drives/alpha",
            "--identity-file",
            "/keys/identity",
            "--passphrase-file",
            "/keys/passphrase",
            "leave",
            "--yes",
        ]
    );
}

/// `__build_info` parsing reads the fixed first line and refuses
/// output without it — the version gate never guesses.
#[test]
fn build_info_parses_the_fixed_line() {
    let stdout = "update build: 0.3.0-alpha\nupdate envelope: 0x01\nupdate control: 0x02\n";
    assert_eq!(parse_build_info(stdout).unwrap(), "0.3.0-alpha");
    assert!(
        parse_build_info("nothing here\n").is_err(),
        "output without the line refuses"
    );
    assert!(
        parse_build_info("update build: \n").is_err(),
        "an empty version refuses"
    );
}

/// Exit mapping uses the exit code only — never the CLI text.
/// Exit 0 completes (the paired observation decides what that
/// means — a non-owner leave also exits 0 while authoring
/// nothing), exit 2 is an expected failure with opaque detail,
/// 78 is the terminal held-drive, and timeouts resolve unknown.
#[test]
fn exit_mapping_uses_the_code_only() {
    // Exit 0 with the success line: completed, and the opaque
    // detail rides along for display — never for mapping.
    let ready = CliOutput {
        code: Some(0),
        status_success: true,
        stdout: "drive alpha verified available (socket /drives/alpha/owner.sock)\n".to_string(),
        stderr: String::new(),
        timed_out: false,
    };
    match map_exit_output(&ready).result {
        ProvisionResult::Completed { detail } => {
            assert!(detail.contains("verified available"), "detail: {detail}")
        }
        other => panic!("exit 0 completes: {other:?}"),
    }
    // A non-owner leave refusal also exits 0: completed as well —
    // exit 0 must never mean "left". The membership observation
    // decides.
    let refusal = CliOutput {
        code: Some(0),
        status_success: true,
        stdout: "only an owner can remove a device: ask the owner to run: member remove <id>\n"
            .to_string(),
        stderr: String::new(),
        timed_out: false,
    };
    assert!(matches!(
        map_exit_output(&refusal).result,
        ProvisionResult::Completed { .. }
    ));
    // Exit 2 with any text: expected failure, detail opaque.
    let failed = CliOutput {
        code: Some(2),
        status_success: false,
        stdout: String::new(),
        stderr: "error: drive alpha registered but unavailable: verify-socket: owner.sock never appeared\n".to_string(),
        timed_out: false,
    };
    match map_exit_output(&failed).result {
        ProvisionResult::ExpectedFailure { detail } => {
            assert!(
                detail.contains("registered but unavailable"),
                "detail: {detail}"
            )
        }
        other => panic!("exit 2 fails expectedly: {other:?}"),
    }
    // An unacknowledged sole-owner leave (exit 2) is the same
    // shape: no confirm-stage taxonomy, just opaque detail plus
    // observation.
    let unacked = CliOutput {
        code: Some(2),
        status_success: false,
        stdout: String::new(),
        stderr: "error: Removing the sole owner is valid but terminal: pass --yes to confirm\n"
            .to_string(),
        timed_out: false,
    };
    assert!(matches!(
        map_exit_output(&unacked).result,
        ProvisionResult::ExpectedFailure { .. }
    ));
    let held = CliOutput {
        code: Some(78),
        status_success: false,
        stdout: String::new(),
        stderr: "error: another process holds the drive\n".to_string(),
        timed_out: false,
    };
    assert!(matches!(
        map_exit_output(&held).result,
        ProvisionResult::HeldDrive { .. }
    ));
    let timed_out = CliOutput {
        code: None,
        status_success: false,
        stdout: String::new(),
        stderr: "CLI did not complete within 120s".to_string(),
        timed_out: true,
    };
    assert!(matches!(
        map_exit_output(&timed_out).result,
        ProvisionResult::Unknown { .. }
    ));
}

/// Truncation never splits a multi-byte character: the cut lands
/// on a character boundary even when the budget falls mid-glyph,
/// so non-ASCII drive directories in CLI output cannot panic.
#[test]
fn truncate_never_splits_a_character() {
    assert_eq!(truncate("hello", 10), "hello");
    assert_eq!(truncate("  padded  ", 10), "padded");
    let text = "drive /drives/\u{00e9}t\u{00e9} ready";
    assert!(truncate(text, 100).contains("\u{00e9}t\u{00e9}"));
    // A budget landing inside the two-byte \u{00e9} shortens to the
    // boundary instead of panicking.
    let cut = truncate(text, 15);
    assert!(cut.is_ascii() || cut.len() <= 18, "cut: {cut}");
    // Full multi-byte stress: every budget is a valid string.
    let wide = "\u{65e5}\u{672c}\u{8a9e}\u{30c6}\u{30b9}\u{30c8} drive";
    for max in 0..=wide.len() + 4 {
        let out = truncate(wide, max);
        assert!(out.len() <= max + 3, "budget {max}: {out}");
    }
}

/// A wedged CLI is killed and reaped past the timeout: the
/// adapter resolves unknown, the spawned process is gone, and no
/// wait is left hanging on it. (The kill targets the spawned
/// shell itself; a grandchild it spawned is the shell's problem,
/// not the adapter's — which is why this test asserts the
/// bounded return and the release, not process-table absence.)
#[test]
fn wedged_cli_is_killed_past_the_timeout() {
    let dir =
        std::env::temp_dir().join(format!("wyrd-desktop-fakecli-{}-wedge", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cli = dir.join("wyrd");
    // Answers the version check instantly, then sleeps past any
    // test timeout on every other argv.
    let version = env!("CARGO_PKG_VERSION");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nsleep 30\nexit 0\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let start = std::time::Instant::now();
    let outcome = run_bootstrap(&cli, &create_request(), Duration::from_secs(2));
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "the wait is bounded by the timeout"
    );
    assert!(
        matches!(outcome.result, ProvisionResult::Unknown { .. }),
        "a wedged CLI resolves unknown: {:?}",
        outcome.result
    );
    // The wait was bounded by the timeout, and the reaped child
    // leaves the flight free: the IPC flight test pins the
    // release, and a leaked child would wedge the next claim.
    // (The shell script's own `sleep` grandchild is not the
    // adapter's to reap.)
    std::fs::remove_dir_all(&dir).unwrap_or(());
}

/// A chatty CLI that writes 1 MiB to stdout and exits 2 must not
/// wedge the adapter: the pipes are drained concurrently with the
/// wait, so the exit code (expected failure) arrives well under
/// the timeout instead of resolving `Unknown` after a kill. Both
/// pipes are drained (1 MiB each) so a stderr flood cannot wedge
/// a stdout drain either; capture keeps only a bounded prefix.
#[test]
fn chatty_cli_drains_pipes_instead_of_wedging() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-fakecli-{}-chatty",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let cli = dir.join("wyrd");
    std::fs::write(
        &cli,
        "#!/bin/sh\nhead -c 1048576 /dev/zero | tr '\\0' 'A'\nhead -c 1048576 /dev/zero | tr '\\0' 'B' >&2\nexit 2\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let start = std::time::Instant::now();
    let output = run_cli(&cli, &["probe".into()], None, Duration::from_secs(10));
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "a 1 MiB exit-2 must return well under the timeout, took {elapsed:?}"
    );
    assert!(
        !output.timed_out,
        "a fast exit-2 is not a timeout: {output:?}"
    );
    assert_eq!(output.code, Some(2), "exit code survives: {output:?}");
    assert!(
        output.stdout.len() <= super::MAX_CLI_CAPTURE_BYTES,
        "stdout capture stays bounded: {} bytes",
        output.stdout.len()
    );
    assert!(
        output.stderr.len() <= super::MAX_CLI_CAPTURE_BYTES,
        "stderr capture stays bounded: {} bytes",
        output.stderr.len()
    );
    assert!(
        !output.stdout.is_empty() && !output.stderr.is_empty(),
        "bounded prefix still carries output for display"
    );
    assert!(
        matches!(
            map_exit_output(&output).result,
            ProvisionResult::ExpectedFailure { .. }
        ),
        "exit 2 maps to expected failure: {:?}",
        map_exit_output(&output).result
    );
    std::fs::remove_dir_all(&dir).unwrap_or(());
}

/// Write a fake `wyrd` CLI: a shell script answering
/// `__build_info` with the workspace version and every other
/// invocation with the given stdout and exit code. The adapter
/// spawns it exactly like the installed binary — no special
/// path, no mock.
fn fake_cli(tag: &str, body_stdout: &str, body_code: i32) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-fakecli-{}-{}",
        std::process::id(),
        tag
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("wyrd");
    let version = env!("CARGO_PKG_VERSION");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\necho \"update envelope: 0x01\"\necho \"update control: 0x02\"\nexit 0\nfi\nprintf '%s' \"{body_stdout}\"\nexit {body_code}\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// End to end through a fake CLI binary: version check passes
/// against the workspace version, the structured argv run exits
/// 0, and the outcome completes with the opaque CLI text — the
/// full `run_bootstrap` path the IPC command drives, with no
/// mock. What the text says is display detail; the code decides.
#[test]
fn run_bootstrap_completes_through_a_fake_cli() {
    let dir =
        std::env::temp_dir().join(format!("wyrd-desktop-fakecli-{}-ready", std::process::id()));
    let cli = fake_cli(
        "ready",
        "drive alpha verified available (socket /drives/alpha/owner.sock)",
        0,
    );
    let outcome = run_bootstrap(&cli, &create_request(), Duration::from_secs(30));
    match &outcome.result {
        ProvisionResult::Completed { detail } => {
            assert!(detail.contains("verified available"), "detail: {detail}")
        }
        other => panic!("fake exit 0 completes: {other:?}"),
    }
    assert_eq!(outcome.code, Some(0));
    std::fs::remove_dir_all(&dir).unwrap_or(());
}

/// A version-skewed CLI is a hard failure before any lifecycle
/// argv runs: the body would report ready, but the version gate
/// refuses first — never a silent fallback.
#[test]
fn run_bootstrap_refuses_a_version_skewed_cli() {
    let dir =
        std::env::temp_dir().join(format!("wyrd-desktop-fakecli-{}-skew", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cli = dir.join("wyrd");
    std::fs::write(
        &cli,
        "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: 9.9.9-mismatch\"\nexit 0\nfi\necho \"drive alpha verified available (socket /x/owner.sock)\"\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let outcome = run_bootstrap(&cli, &create_request(), Duration::from_secs(30));
    assert!(
        matches!(outcome.result, ProvisionResult::VersionMismatch { .. }),
        "skew refuses before the lifecycle argv runs: {:?}",
        outcome.result
    );
    std::fs::remove_dir_all(&dir).unwrap_or(());
}

/// A missing CLI binary is the same hard failure as a skewed one:
/// the spawn failure maps to `VersionMismatch` with the binary
/// named, never to a fallback or a silent skip.
#[test]
fn version_check_against_a_missing_binary_is_a_hard_failure() {
    let missing =
        std::env::temp_dir().join(format!("wyrd-desktop-no-such-cli-{}", std::process::id()));
    assert!(
        !missing.exists(),
        "the missing-binary probe must not exist: {}",
        missing.display()
    );
    match check_cli_version(&missing, Duration::from_secs(30)) {
        Err(ProvisionResult::VersionMismatch { reason }) => {
            assert!(
                reason.contains(&missing.display().to_string()),
                "the reason names the missing binary: {reason}"
            );
        }
        other => panic!("a missing CLI hard-fails: {other:?}"),
    }
}

/// A failing fake CLI maps through the exit-code table: exit 2
/// is an expected failure with opaque detail, and the re-read —
/// not the detail — decides lifecycle state.
#[test]
fn run_leave_fails_expectedly_through_a_fake_cli() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-fakecli-{}-leave-refused",
        std::process::id()
    ));
    let cli = fake_cli("leave-refused", "", 2);
    let outcome = run_leave(
        &cli,
        &LeaveRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            credentials: CredentialSource::File {
                identity_file: Some(PathBuf::from("/keys/identity")),
                passphrase_file: Some(PathBuf::from("/keys/passphrase")),
            },
            acknowledged: false,
        },
        Duration::from_secs(30),
    );
    assert!(
        matches!(outcome.result, ProvisionResult::ExpectedFailure { .. }),
        "exit 2 fails expectedly: {:?}",
        outcome.result
    );
    std::fs::remove_dir_all(&dir).unwrap_or(());
}

/// Join runs the same collapsed path as create: the invitation
/// argv is structured (pinned above) and an exit-0 join
/// completes with opaque detail through the fake CLI.
#[test]
fn run_join_completes_through_a_fake_cli() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-fakecli-{}-join-ready",
        std::process::id()
    ));
    let cli = fake_cli(
        "join-ready",
        "drive beta verified available (socket /drives/beta/owner.sock)",
        0,
    );
    let mut request = create_request();
    request.name = "beta".to_string();
    request.drive_dir = PathBuf::from("/drives/beta");
    request.invitation_file = Some(PathBuf::from("/tmp/invite.sealed"));
    let outcome = run_bootstrap(&cli, &request, Duration::from_secs(30));
    assert!(
        matches!(outcome.result, ProvisionResult::Completed { .. }),
        "fake exit-0 join completes: {:?}",
        outcome.result
    );
    std::fs::remove_dir_all(&dir).unwrap_or(());
}

fn reconfigure_request() -> ReconfigureRequest {
    ReconfigureRequest {
        drive_dir: PathBuf::from("/drives/alpha"),
        relays: vec!["wss://relay.example".to_string()],
        credentials: CredentialSource::File {
            identity_file: Some(PathBuf::from("/keys/identity")),
            passphrase_file: Some(PathBuf::from("/keys/passphrase")),
        },
        registry_path: None,
        owner_bin: None,
        supervise_bin: None,
        no_service: false,
        mode: "vault".to_string(),
        mountpoint: None,
    }
}

/// The reconfigure argv is one structured element per argument:
/// `drive <dir> --identity-file … --passphrase-file … reconfigure
/// --mode vault`, relay pairs after the verb — never a joined
/// shell string. Credentials precede the verb because they are
/// flattened on the parent, not global.
#[test]
fn reconfigure_argv_is_structured_per_element() {
    let argv = reconfigure_argv(&reconfigure_request()).unwrap();
    let rendered: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        rendered,
        vec![
            "drive",
            "/drives/alpha",
            "--identity-file",
            "/keys/identity",
            "--passphrase-file",
            "/keys/passphrase",
            "reconfigure",
            "--mode",
            "vault",
            "--relay",
            "wss://relay.example",
        ]
    );
    assert!(
        !rendered
            .iter()
            .any(|arg| arg.contains(' ') && arg.contains("--")),
        "no joined shell string: {rendered:?}"
    );
    // Position pin, not just contents: the credential pair must
    // sit before the subcommand verb (the nix/drive owner-argv
    // order-pin precedent) — after the verb clap rejects it.
    let verb = rendered
        .iter()
        .position(|arg| arg == "reconfigure")
        .unwrap();
    let identity = rendered
        .iter()
        .position(|arg| arg == "--identity-file")
        .unwrap();
    let passphrase = rendered
        .iter()
        .position(|arg| arg == "--passphrase-file")
        .unwrap();
    assert!(
        identity < verb && passphrase < verb,
        "credentials precede the verb: {rendered:?}"
    );
}

/// Empty relays emit no `--relay` flags: the CLI keeps the
/// registered set, so a credential-path-only reconfigure never
/// clears relays by accident.
#[test]
fn reconfigure_argv_omits_relay_flags_when_empty() {
    let mut request = reconfigure_request();
    request.relays.clear();
    request.no_service = true;
    let argv = reconfigure_argv(&request).unwrap();
    let rendered: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        !rendered.contains(&"--relay".to_string()),
        "no relay flags: {rendered:?}"
    );
    assert!(rendered.contains(&"--no-service".to_string()));
}

/// Mount reconfigure argv carries `--mode mount` plus the
/// mountpoint pair; unknown modes refuse before any process
/// spawns — the adapter never reconfigures into a mode the
/// registry reader skips.
#[test]
fn mount_reconfigure_carries_the_mode_and_mountpoint_pairs() {
    let mut request = reconfigure_request();
    request.mode = "mount".to_string();
    request.mountpoint = Some(PathBuf::from("/mnt/alpha"));
    let argv = reconfigure_argv(&request).unwrap();
    let rendered: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(rendered.contains(&"--mode".to_string()));
    assert!(rendered.contains(&"mount".to_string()));
    assert!(rendered.contains(&"--mountpoint".to_string()));
    assert!(rendered.contains(&"/mnt/alpha".to_string()));
}

#[test]
fn unknown_reconfigure_mode_refuses_before_spawning() {
    let mut request = reconfigure_request();
    request.mode = "fuse".to_string();
    match reconfigure_argv(&request) {
        Err(reason) => assert!(
            reason.contains("unknown service mode"),
            "reason names the unknown mode: {reason}"
        ),
        Ok(_) => panic!("an unknown reconfigure must refuse"),
    }
}

/// End to end through a fake CLI binary: version check passes
/// and an exit-0 reconfigure completes with opaque detail — the
/// full `run_reconfigure` path the IPC command drives.
#[test]
fn run_reconfigure_completes_through_a_fake_cli() {
    let cli = fake_cli(
        "reconf-ready",
        "drive alpha reconfigured and verified available (socket /drives/alpha/owner.sock)",
        0,
    );
    let outcome = run_reconfigure(&cli, &reconfigure_request(), Duration::from_secs(30));
    match &outcome.result {
        ProvisionResult::Completed { detail } => {
            assert!(detail.contains("reconfigured"), "detail: {detail}")
        }
        other => panic!("fake exit-0 reconfigure completes: {other:?}"),
    }
    assert_eq!(outcome.code, Some(0));
}

/// A version-skewed CLI is a hard failure before the reconfigure
/// argv runs — never a silent fallback.
#[test]
fn run_reconfigure_refuses_a_version_skewed_cli() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-fakecli-{}-reconf-skew",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let cli = dir.join("wyrd");
    std::fs::write(
        &cli,
        "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: 9.9.9-mismatch\"\nexit 0\nfi\necho \"drive alpha reconfigured\"\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let outcome = run_reconfigure(&cli, &reconfigure_request(), Duration::from_secs(30));
    assert!(
        matches!(outcome.result, ProvisionResult::VersionMismatch { .. }),
        "skew refuses before the reconfigure argv runs: {:?}",
        outcome.result
    );
    std::fs::remove_dir_all(&dir).unwrap_or(());
}

/// A wedged CLI is killed and reaped past the timeout on the
/// reconfigure path too: the outcome resolves unknown and
/// triggers observation, never re-execution.
#[test]
fn run_reconfigure_times_out_to_unknown() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-fakecli-{}-reconf-wedge",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let cli = dir.join("wyrd");
    // Answers the version check instantly, then sleeps past any
    // test timeout on every other argv.
    let version = env!("CARGO_PKG_VERSION");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nsleep 30\nexit 0\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let start = std::time::Instant::now();
    let outcome = run_reconfigure(&cli, &reconfigure_request(), Duration::from_secs(2));
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "the wait is bounded by the timeout"
    );
    assert!(
        matches!(outcome.result, ProvisionResult::Unknown { .. }),
        "a wedged reconfigure resolves unknown: {:?}",
        outcome.result
    );
    std::fs::remove_dir_all(&dir).unwrap_or(());
}

/// The service-start argv is one structured element per argument:
/// `drive <dir> service-start`, no credentials — starting opens
/// no keystore — plus the registry override where set.
#[test]
fn service_start_argv_is_structured_without_credentials() {
    let request = ServiceStartRequest {
        drive_dir: PathBuf::from("/drives/alpha"),
        registry_path: Some(PathBuf::from("/tmp/drives.json")),
    };
    let rendered: Vec<String> = service_start_argv(&request)
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        rendered,
        vec![
            "drive",
            "/drives/alpha",
            "service-start",
            "--registry",
            "/tmp/drives.json",
        ]
    );
    assert!(
        !rendered.iter().any(|arg| arg.contains("identity")),
        "no credential paths travel on a start: {rendered:?}"
    );
}

/// The service-enable argv is one structured element per
/// argument: `drive <dir> service-enable` — recording a
/// login-start preference opens no keystore, so no credential
/// paths travel — plus the registry override where set.
#[test]
fn service_enable_argv_is_structured_without_credentials() {
    let request = ServiceEnableRequest {
        drive_dir: PathBuf::from("/drives/alpha"),
        registry_path: Some(PathBuf::from("/tmp/drives.json")),
    };
    let rendered: Vec<String> = service_enable_argv(&request)
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        rendered,
        vec![
            "drive",
            "/drives/alpha",
            "service-enable",
            "--registry",
            "/tmp/drives.json",
        ]
    );
    assert!(
        !rendered.iter().any(|arg| arg.contains("identity")),
        "no credential paths travel on an enable: {rendered:?}"
    );
}

/// The service-disable argv is one structured element per
/// argument: `drive <dir> service-disable` — stopping now and
/// recording no login start opens no keystore — plus the
/// registry override where set.
#[test]
fn service_disable_argv_is_structured_without_credentials() {
    let request = ServiceDisableRequest {
        drive_dir: PathBuf::from("/drives/alpha"),
        registry_path: None,
    };
    let rendered: Vec<String> = service_disable_argv(&request)
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(rendered, vec!["drive", "/drives/alpha", "service-disable"]);
    assert!(
        !rendered.iter().any(|arg| arg.contains("identity")),
        "no credential paths travel on a disable: {rendered:?}"
    );
}

/// End to end through a fake CLI binary: version check passes
/// and an exit-0 start completes with opaque detail — the full
/// `run_service_start` path the IPC command drives.
#[test]
fn run_service_start_completes_through_a_fake_cli() {
    let cli = fake_cli(
        "start-ready",
        "drive alpha service started and verified available (socket /drives/alpha/owner.sock)",
        0,
    );
    let request = ServiceStartRequest {
        drive_dir: PathBuf::from("/drives/alpha"),
        registry_path: None,
    };
    let outcome = run_service_start(&cli, &request, Duration::from_secs(30));
    match &outcome.result {
        ProvisionResult::Completed { detail } => {
            assert!(detail.contains("verified available"), "detail: {detail}")
        }
        other => panic!("fake exit-0 start completes: {other:?}"),
    }
    assert_eq!(outcome.code, Some(0));
}

fn unlock_request() -> UnlockRequest {
    UnlockRequest {
        drive_dir: PathBuf::from("/drives/alpha"),
        credentials: CredentialSource::File {
            identity_file: Some(PathBuf::from("/keys/identity")),
            passphrase_file: Some(PathBuf::from("/keys/passphrase")),
        },
        relays: vec!["wss://relay.example".to_string()],
        retained_quota: None,
    }
}

/// The unlock argv is one structured element per argument:
/// `drive <dir> --identity-file … --passphrase-stdin unlock`,
/// relay pairs and the quota pair after the verb — never a
/// joined shell string, never `--passphrase-file` (unlock
/// refuses a file-backed start by construction), never secret
/// contents (the passphrase rides the stdin pipe).
#[test]
fn unlock_argv_is_structured_with_the_pipe_flag() {
    let argv = unlock_argv(&unlock_request()).unwrap();
    let rendered: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        rendered,
        vec![
            "drive",
            "/drives/alpha",
            "--identity-file",
            "/keys/identity",
            "--passphrase-stdin",
            "unlock",
            "--relay",
            "wss://relay.example",
        ]
    );
    // Position pin, not just contents: the parent credential
    // flags must sit before the `unlock` verb (the `leave`
    // precedent) — after the verb clap rejects them.
    let verb = rendered.iter().position(|arg| arg == "unlock").unwrap();
    let identity = rendered
        .iter()
        .position(|arg| arg == "--identity-file")
        .unwrap();
    let pipe = rendered
        .iter()
        .position(|arg| arg == "--passphrase-stdin")
        .unwrap();
    assert!(
        identity < verb && pipe < verb,
        "credential flags precede the verb: {rendered:?}"
    );
    assert!(
        !rendered.iter().any(|arg| arg == "--passphrase-file"),
        "unlock never names a passphrase file: {rendered:?}"
    );
    // Empty relays and no quota emit no flags: the CLI resolves
    // the drive configuration default, like every other
    // networked command.
    let bare = UnlockRequest {
        relays: Vec::new(),
        retained_quota: None,
        ..unlock_request()
    };
    let rendered: Vec<String> = unlock_argv(&bare)
        .unwrap()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        !rendered.contains(&"--relay".to_string()),
        "no relay flags: {rendered:?}"
    );
    let mut quoted = unlock_request();
    quoted.retained_quota = Some(4096);
    let rendered: Vec<String> = unlock_argv(&quoted)
        .unwrap()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(rendered.contains(&"--retained-quota".to_string()));
    assert!(rendered.contains(&"4096".to_string()));
}

/// Passphrase pipe-shape pins: empty refuses, overlong refuses,
/// non-UTF-8 refuses, line breaks refuse (the commit path never
/// writes one, so a break is a paste error, never a real
/// passphrase) — all before any process spawns.
#[test]
fn unlock_passphrase_validation_fails_closed_before_spawning() {
    assert!(validate_unlock_passphrase(b"hunter2-hunter2").is_ok());
    assert!(validate_unlock_passphrase(b"").is_err());
    assert!(validate_unlock_passphrase(&vec![b'x'; 4097]).is_err());
    assert!(validate_unlock_passphrase(&vec![b'x'; 4096]).is_ok());
    assert!(validate_unlock_passphrase(&[0xff, 0xfe]).is_err());
    assert!(validate_unlock_passphrase(b"two\nlines").is_err());
    assert!(validate_unlock_passphrase(b"trailing-newline\n").is_err());
}

/// `spawn_unlock` pipes the passphrase on stdin and holds the
/// child: the fake CLI (version gate, then drain stdin to a
/// file, then sleep) proves the pipe carries the exact bytes —
/// never argv — and the child stays alive past the spawn, so
/// the session monitor owns the wait.
#[test]
fn spawn_unlock_pipes_the_passphrase_and_holds_the_child() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-fakecli-{}-unlock-pipe",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let cli = dir.join("wyrd");
    let captured = dir.join("stdin-captured");
    let version = env!("CARGO_PKG_VERSION");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\ncat > {}\nsleep 30\nexit 0\n",
            captured.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut spawned =
        spawn_unlock(&cli, &unlock_request(), b"hunter2-hunter2").expect("spawn holds");
    // The fake drains stdin before sleeping, so the bytes land
    // promptly; the child must still be alive (persistent owner).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !captured.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        std::fs::read(&captured).unwrap(),
        b"hunter2-hunter2",
        "the pipe carries the exact passphrase bytes"
    );
    assert!(
        spawned.child.try_wait().unwrap().is_none(),
        "the unlock child is held, not reaped"
    );
    let _ = spawned.child.kill();
    let _ = spawned.child.wait();
    std::fs::remove_dir_all(&dir).unwrap_or(());
}

/// Credential isolation, spawn half: the passphrase crosses to
/// the unlock owner only as stdin pipe bytes — never argv,
/// never the environment. The recording fake logs its argv and
/// environment and captures stdin; the test asserts the secret
/// appears in exactly one of the three. (Argv shape pins
/// statically in `unlock_argv_is_structured_with_the_pipe_flag`;
/// `Debug` redaction pins in
/// `unlock_args_debug_redacts_the_passphrase` and
/// `debug_redacts_the_secret` in `commands.rs`/`session.rs`.)
#[test]
fn spawn_unlock_confines_the_secret_to_stdin() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-fakecli-{}-unlock-isolation",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let cli = dir.join("wyrd");
    let argv_log = dir.join("argv.log");
    let env_log = dir.join("env.log");
    let stdin_captured = dir.join("stdin.bin");
    let version = env!("CARGO_PKG_VERSION");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nprintf '%s\\n' \"$@\" > {}\nenv > {}\ncat > {}\nexit 2\n",
            argv_log.display(),
            env_log.display(),
            stdin_captured.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Distinctive enough that no path, flag, or inherited
    // environment variable can contain it by accident.
    let secret = b"isolation-probe-per-session-9x";
    let mut spawned = spawn_unlock(&cli, &unlock_request(), secret).expect("spawn holds");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !stdin_captured.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let stdin = std::fs::read(&stdin_captured).unwrap();
    let argv = std::fs::read_to_string(&argv_log).unwrap();
    let env = std::fs::read_to_string(&env_log).unwrap();
    assert_eq!(
        stdin, secret,
        "stdin carries the exact secret bytes, nothing else"
    );
    assert!(
        !argv.contains("isolation-probe"),
        "the secret never rides argv: {argv:?}"
    );
    assert!(
        !env.contains("isolation-probe"),
        "the secret never rides the environment"
    );
    let _ = spawned.child.kill();
    let _ = spawned.child.wait();
    std::fs::remove_dir_all(&dir).unwrap_or(());
}

/// A version-skewed CLI refuses before spawning the unlock argv:
/// the pipe never opens, the secret never leaves the caller.
#[test]
fn spawn_unlock_refuses_a_version_skewed_cli() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-fakecli-{}-unlock-skew",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let cli = dir.join("wyrd");
    std::fs::write(
        &cli,
        "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: 9.9.9-mismatch\"\nexit 0\nfi\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    match spawn_unlock(&cli, &unlock_request(), b"hunter2-hunter2") {
        Err(ProvisionResult::VersionMismatch { .. }) => {}
        other => panic!("skew refuses before spawning: {other:?}"),
    }
    std::fs::remove_dir_all(&dir).unwrap_or(());
}

/// An invalid passphrase refuses before any process spawns: no
/// version check runs, no child exists to hold a secret.
#[test]
fn spawn_unlock_validates_before_spawning() {
    let missing =
        std::env::temp_dir().join(format!("wyrd-desktop-no-such-cli-{}", std::process::id()));
    match spawn_unlock(&missing, &unlock_request(), b"") {
        Err(ProvisionResult::ExpectedFailure { detail }) => {
            assert!(detail.contains("no passphrase"), "detail: {detail}");
        }
        other => panic!("an empty passphrase refuses first: {other:?}"),
    }
}

/// An unlock owner that exits before serving reports its bounded
/// detail: the drains join past EOF and the exit code survives
/// for the caller's mapping.
#[test]
fn collect_unlock_exit_detail_joins_the_drains() {
    let cli = fake_cli("unlock-exit", "wrong passphrase\n", 2);
    let spawned = spawn_unlock(&cli, &unlock_request(), b"wrong-passphrase").expect("spawn holds");
    let (code, detail) = collect_unlock_exit_detail(spawned);
    assert_eq!(code, Some(2));
    assert!(detail.contains("wrong passphrase"), "detail: {detail}");
}

/// An unlock owner that closes its stdin before the passphrase
/// write lands still maps its exit code: the write sees EPIPE, but
/// the child is handed back for the caller to reap, so exit 78
/// reports a held drive instead of an unknown outcome. The fake
/// closes fd 0 explicitly and lingers, making the broken-pipe write
/// deterministic rather than scheduler-dependent.
#[test]
fn spawn_unlock_maps_an_early_exit_that_closed_stdin() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-fakecli-{}-unlock-early-exit",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let cli = dir.join("wyrd");
    let version = env!("CARGO_PKG_VERSION");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nexec 0<&-\necho \"another process holds the drive\" >&2\nsleep 1\nexit 78\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let spawned = spawn_unlock(&cli, &unlock_request(), b"hunter2-hunter2").expect("spawn holds");
    let (code, detail) = collect_unlock_exit_detail(spawned);
    assert_eq!(
        code,
        Some(78),
        "the early exit code is reaped, not killed: {detail}"
    );
    assert!(
        detail.contains("another process holds the drive"),
        "detail: {detail}"
    );
    std::fs::remove_dir_all(&dir).unwrap_or(());
}
