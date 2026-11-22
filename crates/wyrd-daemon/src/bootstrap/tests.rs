use super::*;
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub(super) fn scratch(case: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("wyrd-bootstrap-{case}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub(super) fn entry(name: &str, dir: &Path) -> RegistryEntry {
    RegistryEntry {
        name: name.to_string(),
        drive_dir: dir.join("drive"),
        relays: vec!["relay://example".to_string()],
        service: format!("drive-{name}"),
        mode: ServiceMode::Vault,
        mountpoint: None,
    }
}

pub(super) fn mount_entry(name: &str, dir: &Path, mountpoint: &Path) -> RegistryEntry {
    RegistryEntry {
        name: name.to_string(),
        drive_dir: dir.join("drive"),
        relays: vec!["relay://example".to_string()],
        service: format!("drive-{name}"),
        mode: ServiceMode::Mount,
        mountpoint: Some(mountpoint.to_path_buf()),
    }
}

#[test]
fn registry_round_trips_and_rejects_a_newer_schema() {
    let dir = scratch("schema");
    let path = dir.join("drives.json");
    let mut drives = BTreeMap::new();
    drives.insert("demo".to_string(), entry("demo", &dir));
    write_registry(&path, &drives).unwrap();

    let read = read_registry(&path).unwrap();
    assert!(read.skipped.is_empty());
    assert_eq!(read.drives["demo"], drives["demo"]);

    std::fs::write(&path, r#"{"version": 999, "drives": {}}"#).unwrap();
    let refused = read_registry(&path).unwrap_err();
    assert!(refused.to_string().contains("version 999"), "{refused}");
}

#[test]
fn registry_skips_invalid_entries_with_reasons_and_keeps_valid_ones() {
    let dir = scratch("invalid");
    let path = dir.join("drives.json");
    let good = entry("good", &dir);
    let mut bad_relative = entry("bad", &dir);
    bad_relative.drive_dir = PathBuf::from("relative/path");
    let mut bad_name = entry("BAD NAME!", &dir);
    bad_name.name = "BAD NAME!".to_string();
    let mut drives = BTreeMap::new();
    drives.insert("good".to_string(), good);
    // Written by hand, bypassing the validating writer.
    let raw = serde_json::json!({
        "version": 1,
        "drives": {
            "good": drives["good"],
            "bad": bad_relative,
            "BAD NAME!": bad_name,
        }
    });
    std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();

    let read = read_registry(&path).unwrap();
    assert_eq!(read.drives.len(), 1);
    assert!(read.drives.contains_key("good"));
    assert_eq!(read.skipped.len(), 2, "{:?}", read.skipped);
}

#[test]
fn registry_write_refuses_invalid_entries_and_never_writes_partial() {
    let dir = scratch("nowrite");
    let path = dir.join("drives.json");
    let mut drives = BTreeMap::new();
    let mut bad = entry("bad", &dir);
    bad.drive_dir = PathBuf::from("relative/path");
    drives.insert("bad".to_string(), bad);
    let refused = write_registry(&path, &drives).unwrap_err();
    assert!(refused.to_string().contains("absolute"), "{refused}");
    assert!(!path.exists(), "no partial registry may remain");
}

/// A present-but-unreadable registry refuses the write instead
/// of being replaced: malformed JSON fails loud, and a newer
/// schema is never downgraded to version 1 with only the
/// candidates. Either way nothing is written.
#[test]
fn registry_write_refuses_unreadable_and_newer_files() {
    let dir = scratch("refuse-unreadable");
    let malformed = dir.join("drives.json");
    std::fs::write(&malformed, r#"{"version": 1, "drives": {"#).unwrap();
    let mut drives = BTreeMap::new();
    drives.insert("new".to_string(), entry("new", &dir));
    let refused = write_registry(&malformed, &drives).unwrap_err();
    assert!(refused.to_string().contains("parse"), "{refused}");
    assert_eq!(
        std::fs::read(&malformed).unwrap(),
        b"{\"version\": 1, \"drives\": {",
        "the malformed file is untouched"
    );

    let newer = dir.join("newer.json");
    std::fs::write(&newer, r#"{"version": 999, "drives": {}}"#).unwrap();
    let refused = write_registry(&newer, &drives).unwrap_err();
    assert!(refused.to_string().contains("999"), "{refused}");
    assert_eq!(
        std::fs::read(&newer).unwrap(),
        br#"{"version": 999, "drives": {}}"#,
        "the newer file is never downgraded"
    );
}

/// A structurally corrupt entry refuses the write: it decodes
/// as a raw value but not as a registry entry, exactly like the
/// reader, so carrying it and reporting success would contradict
/// the reader's loud verdict on the same file. Nothing is written.
#[test]
fn registry_write_refuses_structurally_corrupt_entries() {
    let dir = scratch("refuse-corrupt");
    let path = dir.join("drives.json");
    let corrupt = r#"{"version": 1, "drives": {"x": {"drive_dir": "/d"}}}"#;
    std::fs::write(&path, corrupt).unwrap();
    let mut drives = BTreeMap::new();
    drives.insert("new".to_string(), entry("new", &dir));
    let refused = write_registry(&path, &drives).unwrap_err();
    assert!(refused.to_string().contains("parse"), "{refused}");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        corrupt.as_bytes(),
        "the corrupt file is untouched"
    );
}

/// Concurrent writers converge without lost entries: every thread
/// writes one distinct key through the same read-modify-write,
/// and the registry lock serializes them, so the final read holds
/// every key. Without the lock, two writers racing on one base
/// would leave only the second rename's entries (atomic rename
/// does not stop lost updates). Threads share the process but
/// open separate lock descriptions, so the kernel lock contends
/// exactly like two processes.
#[test]
fn concurrent_registration_writes_converge_without_lost_entries() {
    let dir = scratch("concurrent");
    let path = dir.join("drives.json");
    let count = 8usize;
    std::thread::scope(|scope| {
        for i in 0..count {
            let path = &path;
            let dir = &dir;
            scope.spawn(move || {
                let name = format!("w{i:02}");
                let mut drives = BTreeMap::new();
                drives.insert(name.clone(), entry(&name, dir));
                write_registry(path, &drives).unwrap();
            });
        }
    });
    let read = read_registry(&path).unwrap();
    assert!(read.skipped.is_empty(), "{:?}", read.skipped);
    assert_eq!(read.drives.len(), count, "{:?}", read.drives.keys());
    for i in 0..count {
        assert!(
            read.drives.contains_key(&format!("w{i:02}")),
            "writer w{i:02} survived: {:?}",
            read.drives.keys()
        );
    }
}

/// Every reachable terminal provision site carries its
/// machine-readable kind: direct callers match on `kind`, never
/// on `reason` text. Rendered text is pinned where the CLI owns
/// it (`crates/wyrd-cli/tests/bootstrap.rs`, contains-pins on
/// the state lines); this test pins the mapping from site to
/// kind, plus the `stage` agreement for the incomplete sites.
#[test]
fn provision_failures_carry_their_machine_readable_kind() {
    use ProvisionFailureKind as Kind;
    // Validation refuses before anything is created.
    let dir = scratch("kind-validate");
    let registry = dir.join("drives.json");
    let mut config = provision_no_service(&dir, &registry, "BAD NAME!");
    config.name = "BAD NAME!".to_string();
    match provision(config) {
        ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
            assert_eq!(kind, Kind::Validate);
            assert_eq!(Some(stage), kind.stage());
        }
        other => panic!("validation must stop first: {other:?}"),
    }
    // An undecodable invitation stops at initialization.
    let dir = scratch("kind-invitation");
    let registry = dir.join("drives.json");
    let mut config = provision_no_service(&dir, &registry, "demo");
    config.init = Init::Join {
        sealed_invitation: b"not a sealed invitation".to_vec(),
        expected: JoinExpectation::none(),
    };
    match provision(config) {
        ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
            assert_eq!(kind, Kind::DecodeInvitation);
            assert_eq!(Some(stage), kind.stage());
        }
        other => panic!("a bad invitation must stop at initialize: {other:?}"),
    }
    // A drive directory that is a regular file cannot be
    // initialized as a store.
    let dir = scratch("kind-initialize");
    let blocker = dir.join("file");
    std::fs::write(&blocker, b"x").unwrap();
    let registry = dir.join("drives.json");
    let mut config = provision_no_service(&dir, &registry, "demo");
    config.drive_dir = blocker;
    match provision(config) {
        ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
            assert_eq!(kind, Kind::Initialize);
            assert_eq!(Some(stage), kind.stage());
        }
        other => panic!("a file cannot be initialized: {other:?}"),
    }
    // A registry path that is a directory cannot be read.
    let dir = scratch("kind-read-registry");
    let config = provision_no_service(&dir, &dir, "demo");
    match provision(config) {
        ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
            assert_eq!(kind, Kind::ReadRegistry);
            assert_eq!(Some(stage), kind.stage());
        }
        other => panic!("an unreadable registry must stop: {other:?}"),
    }
    // Managed-service binary lookup stops before any service
    // operation: this is pinned by construction, not by test —
    // exercising it end to end would install a real service
    // definition whenever the test binary has version-matched
    // siblings, so `NoWyrdBinary` and `NoSuperviseBinary` stay
    // literal-adjacent to their reason strings like
    // `WriteRegistry` and `VerifyOpen` below. (`WriteRegistry`
    // needs a write failure after a successful read of the same
    // path, and `VerifyOpen` needs a drive that creates but does
    // not open; neither is reachable hermetically.)
    // No owner socket verifies nothing, with the socket kind.
    let dir = scratch("kind-socket");
    let registry = dir.join("drives.json");
    match provision(provision_no_service(&dir, &registry, "demo")) {
        ProvisionState::RegisteredButUnavailable { kind, reason, .. } => {
            assert_eq!(kind, Kind::VerifySocket);
            assert!(reason.contains("verify-socket"), "{reason}");
        }
        other => panic!("a missing owner must never verify: {other:?}"),
    }
}

/// The enablement parsers decide by record/word, never by
/// exit code: present records map to their value, absent
/// records and garbage read Unknown (never Disabled), and a
/// label never matches a longer label's record.
#[test]
fn enablement_parsers_map_records_not_exit_codes() {
    // macOS `print-disabled` shape, as observed live.
    let dump = "\tdisabled services = {\n\t\t\"md.enesis.wyrd.drive-alpha\" => enabled\n\t\t\"md.enesis.wyrd.drive-foobar\" => disabled\n\t}\n";
    assert_eq!(
        parse_print_disabled(dump, "drive-alpha"),
        ServiceEnabled::Enabled
    );
    assert_eq!(
        parse_print_disabled(dump, "drive-foobar"),
        ServiceEnabled::Disabled
    );
    // Prefix collision: `drive-foo` must not read the
    // `drive-foobar` record; the closing quote guards it.
    assert_eq!(
        parse_print_disabled(dump, "drive-foo"),
        ServiceEnabled::Unknown
    );
    assert_eq!(
        parse_print_disabled(dump, "drive-missing"),
        ServiceEnabled::Unknown
    );
    assert_eq!(
        parse_print_disabled("\"md.enesis.wyrd.drive-x\" => maybe\n", "drive-x"),
        ServiceEnabled::Unknown
    );
    // Linux `is-enabled` words.
    assert_eq!(parse_is_enabled("enabled\n"), ServiceEnabled::Enabled);
    assert_eq!(parse_is_enabled("disabled\n"), ServiceEnabled::Disabled);
    assert_eq!(parse_is_enabled("masked\n"), ServiceEnabled::Disabled);
    assert_eq!(parse_is_enabled("static\n"), ServiceEnabled::Unknown);
    // A bus failure prints no word: Unknown, never Disabled.
    assert_eq!(
        parse_is_enabled("Failed to connect to bus.\n"),
        ServiceEnabled::Unknown
    );
    assert_eq!(parse_is_enabled(""), ServiceEnabled::Unknown);
}

/// The live-manager probe on a service that was never
/// registered reads Unknown without writing anything: no
/// record exists, and the probe performs no mutation. Read
/// only — safe on any machine.
#[test]
fn enablement_probe_on_unknown_service_is_unknown() {
    let entry = RegistryEntry {
        name: "wyrd-probe-missing-xyz".to_string(),
        drive_dir: PathBuf::from("/tmp/wyrd-probe-missing-xyz"),
        relays: Vec::new(),
        service: "wyrd-probe-missing-xyz".to_string(),
        mode: ServiceMode::Vault,
        mountpoint: None,
    };
    assert_eq!(service_enabled(&entry), ServiceEnabled::Unknown);
}

/// A wedged manager probe reads as no answer, bounded: a
/// child that never exits is killed past the probe bound, so
/// a hung `launchctl`/`systemctl` hangs the toggle only up to
/// the bound, never the desktop adapter's full timeout.
#[test]
fn wedged_probe_output_reads_as_no_answer() {
    let start = std::time::Instant::now();
    assert_eq!(probe_output("sleep", &["30"]), None);
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "the probe is bounded by its own deadline"
    );
}
/// `run` reports spawn and command failures with their program
/// and detail: missing programs never spawn, failing programs
/// report their own text. The service-stage `ProvisionFailureKind`
/// variants (`NoWyrdBinary`, `NoSuperviseBinary`,
/// `InstallService`, `EnableService`, `StartService`,
/// `StopService`) and the service-manager `BootstrapError`
/// variants beneath them (`Spawn`, `CommandFailed`,
/// `ReloadUnits`, `AgentNotLoaded`, `RestartService`,
/// `StopService`) stay literal-adjacent to their reason
/// strings — reaching them end to end would touch a real
/// manager, as the provision kind test documents.
#[test]
fn service_commands_report_spawn_and_command_failures() {
    // `run` itself: missing program spawns nothing, a failing
    // program reports its own text.
    match run("wyrd-definitely-missing-binary-xyz", &[]) {
        Err(BootstrapError::Spawn { program, .. }) => {
            assert_eq!(program, "wyrd-definitely-missing-binary-xyz");
        }
        other => panic!("a missing program must fail spawn: {other:?}"),
    }
    match run("sh", &["-c", "exit 3"]) {
        Err(BootstrapError::CommandFailed {
            program, detail, ..
        }) => {
            assert_eq!(program, "sh");
            assert!(detail.is_empty(), "exit 3 carries no stderr: {detail:?}");
        }
        other => panic!("a failing program must report: {other:?}"),
    }
}

#[test]
fn registry_missing_file_is_an_empty_registry() {
    let dir = scratch("missing");
    let read = read_registry(&dir.join("absent.json")).unwrap();
    assert!(read.drives.is_empty() && read.skipped.is_empty());
}

/// Every `Display` string pinned exactly: a future edit that
/// changes user-visible text fails here. Variants over foreign
/// errors (`Parse`, `Encode`) pin the format around the live
/// source error instead of the dependency's wording.
#[test]
fn bootstrap_display_reproduces_the_historical_messages_exactly() {
    use std::io::{Error as IoError, ErrorKind};
    let io = |kind: ErrorKind, message: &str| IoError::new(kind, message.to_owned());
    let parse_source = serde_json::from_slice::<serde_json::Value>(b"xx").unwrap_err();
    struct AlwaysFails;
    impl Serialize for AlwaysFails {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("boom"))
        }
    }
    let encode_source = serde_json::to_vec(&AlwaysFails).unwrap_err();
    let command = BootstrapError::CommandFailed {
        program: "launchctl".to_string(),
        args: "list missing".to_string(),
        detail: "No such process".to_string(),
    };
    let cases: Vec<(BootstrapError, String)> = vec![
            (
                BootstrapError::Read {
                    path: "/reg/drives.json".into(),
                    source: io(ErrorKind::NotFound, "missing"),
                },
                "read /reg/drives.json: missing".to_string(),
            ),
            (
                BootstrapError::Parse {
                    path: "/reg/drives.json".into(),
                    source: parse_source,
                },
                format!(
                    "parse /reg/drives.json: {}",
                    serde_json::from_slice::<serde_json::Value>(b"xx").unwrap_err()
                ),
            ),
            (
                BootstrapError::UnsupportedVersion {
                    path: "/reg/drives.json".into(),
                    found: 999,
                },
                format!("registry /reg/drives.json is version 999, this build reads {REGISTRY_VERSION}"),
            ),
            (
                BootstrapError::KeyMismatch {
                    key: "a".to_string(),
                    name: "b".to_string(),
                },
                "key \"a\" does not match entry name \"b\"".to_string(),
            ),
            (
                BootstrapError::InvalidName {
                    name: "BAD NAME!".to_string(),
                },
                "name \"BAD NAME!\" must be 1-64 chars of [a-z0-9-]".to_string(),
            ),
            (
                BootstrapError::NameTooLongForServicePrefix {
                    name: "a-very-long-drive-name-that-leaves-no-room-at-all-xyz".to_string(),
                },
                "name \"a-very-long-drive-name-that-leaves-no-room-at-all-xyz\" leaves no room for the drive- service prefix (max 58 chars)".to_string(),
            ),
            (
                BootstrapError::InvalidService {
                    service: "BAD!".to_string(),
                },
                "service \"BAD!\" must be 1-64 chars of [a-z0-9-]".to_string(),
            ),
            (
                BootstrapError::RelativeDriveDir {
                    dir: "relative/path".into(),
                },
                "drive_dir relative/path must be absolute".to_string(),
            ),
            (
                BootstrapError::DriveDirHasDotDot {
                    dir: "/reg/../drive".into(),
                },
                "drive_dir /reg/../drive must not contain ..".to_string(),
            ),
            (BootstrapError::UnknownServiceMode, "unknown service mode: this build manages `vault` and `mount`".to_string()),
            (
                BootstrapError::InvalidRelays,
                "relays must be non-empty URLs without whitespace or control bytes".to_string(),
            ),
            (
                BootstrapError::DriveDirHasControlChars {
                    dir: "/reg/drive".into(),
                },
                "drive_dir /reg/drive must not contain control characters".to_string(),
            ),
            (
                BootstrapError::RefusedWrite {
                    source: Box::new(BootstrapError::RelativeDriveDir {
                        dir: "relative/path".into(),
                    }),
                },
                "refusing to write: drive_dir relative/path must be absolute".to_string(),
            ),
            (
                BootstrapError::Encode {
                    source: encode_source,
                },
                "encode registry: boom".to_string(),
            ),
            (
                BootstrapError::CreateDir {
                    path: "/reg".into(),
                    source: io(ErrorKind::PermissionDenied, "denied"),
                },
                "create /reg: denied".to_string(),
            ),
            (
                BootstrapError::WriteFile {
                    path: "/reg/drives.json".into(),
                    source: io(ErrorKind::StorageFull, "full"),
                },
                "write /reg/drives.json: full".to_string(),
            ),
            (
                BootstrapError::ReplaceFile {
                    path: "/reg/drives.json".into(),
                    source: io(ErrorKind::PermissionDenied, "denied"),
                },
                "replace /reg/drives.json: denied".to_string(),
            ),
            (
                BootstrapError::Chmod {
                    path: "/reg/drives.json".into(),
                    source: io(ErrorKind::PermissionDenied, "denied"),
                },
                "chmod /reg/drives.json: denied".to_string(),
            ),
            (
                BootstrapError::SyncFile {
                    path: "/reg/drives.json".into(),
                    source: io(ErrorKind::Other, "sync lost"),
                },
                "sync /reg/drives.json: sync lost".to_string(),
            ),
            (
                BootstrapError::RemoveFile {
                    path: "/reg/drive-demo.plist".into(),
                    source: io(ErrorKind::PermissionDenied, "denied"),
                },
                "remove /reg/drive-demo.plist: denied".to_string(),
            ),
            (
                BootstrapError::Spawn {
                    program: "launchctl".to_string(),
                    source: io(ErrorKind::NotFound, "missing"),
                },
                "spawn launchctl: missing".to_string(),
            ),
            (
                BootstrapError::CommandFailed {
                    program: "launchctl".to_string(),
                    args: "list missing".to_string(),
                    detail: "No such process".to_string(),
                },
                "launchctl list missing failed: No such process".to_string(),
            ),
            (
                BootstrapError::EnableService {
                    name: "md.enesis.wyrd.drive-demo".to_string(),
                    source: Box::new(command),
                },
                "enable md.enesis.wyrd.drive-demo: launchctl list missing failed: No such process"
                    .to_string(),
            ),
            (
                BootstrapError::ReloadUnits {
                    source: Box::new(BootstrapError::Spawn {
                        program: "systemctl".to_string(),
                        source: io(ErrorKind::NotFound, "missing"),
                    }),
                },
                "reload user units: spawn systemctl: missing".to_string(),
            ),
            (
                BootstrapError::AgentNotLoaded {
                    label: "md.enesis.wyrd.drive-demo".to_string(),
                    source: Box::new(BootstrapError::CommandFailed {
                        program: "launchctl".to_string(),
                        args: "list md.enesis.wyrd.drive-demo".to_string(),
                        detail: "No such process".to_string(),
                    }),
                },
                "start md.enesis.wyrd.drive-demo: agent not loaded: launchctl list md.enesis.wyrd.drive-demo failed: No such process".to_string(),
            ),
            (
                BootstrapError::RestartService {
                    unit: "wyrd-drive-demo.service".to_string(),
                    source: Box::new(BootstrapError::Spawn {
                        program: "systemctl".to_string(),
                        source: io(ErrorKind::NotFound, "missing"),
                    }),
                },
                "restart wyrd-drive-demo.service: spawn systemctl: missing".to_string(),
            ),
            (
                BootstrapError::StopService {
                    name: "wyrd-drive-demo.service".to_string(),
                    source: Box::new(BootstrapError::Spawn {
                        program: "systemctl".to_string(),
                        source: io(ErrorKind::NotFound, "missing"),
                    }),
                },
                "stop wyrd-drive-demo.service: spawn systemctl: missing".to_string(),
            ),
            (
                BootstrapError::ServicesUnsupported,
                "per-drive services are implemented for Unix (launchd agent, systemd user unit)"
                    .to_string(),
            ),
        ];
    assert_eq!(cases.len(), 28, "every variant pins its message");
    for (error, expected) in cases {
        assert_eq!(error.to_string(), expected, "Display changed");
    }
}

#[test]
fn bootstrap_failures_match_by_variant_not_by_text() {
    let dir = scratch("typed");
    let mut bad = entry("bad", &dir);
    bad.name = "BAD NAME!".to_string();
    let error = validate_entry("BAD NAME!", &bad).unwrap_err();
    assert!(
        matches!(error, BootstrapError::InvalidName { .. }),
        "callers branch on the variant, never parse the message: {error}"
    );
    // The text readers keep working: Display is unchanged.
    assert!(
        error
            .to_string()
            .contains("must be 1-64 chars of [a-z0-9-]"),
        "{error}"
    );
    let nested = BootstrapError::RefusedWrite {
        source: Box::new(error),
    };
    assert!(
        nested.to_string().starts_with("refusing to write: "),
        "{nested}"
    );
}

#[test]
fn service_templates_name_the_service_paths_and_relays() {
    let dir = scratch("templates");
    let owner = SupervisedOwner {
        supervise_bin: PathBuf::from("/opt/wyrd/bin/wyrd-supervise"),
        wyrd_bin: PathBuf::from("/opt/wyrd/bin/wyrd"),
        credential: ServiceCredential::File {
            identity_file: dir.join("identity"),
            passphrase_file: Some(dir.join("passphrase")),
        },
    };
    let owned = entry("demo", &dir);
    // Whole-argv pin, order-sensitive: clap parses
    // `owner <dir> [parent flags] start` and rejects flags after
    // `start`, so a substring check would pass on an unparseable
    // definition. The supervised tail after `--` must match
    // exactly.
    let argv = owner.argv(&owned, "0");
    let tail = argv
        .iter()
        .skip_while(|arg| *arg != "--")
        .skip(1)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        tail,
        vec![
            "/opt/wyrd/bin/wyrd",
            "owner",
            &owned.drive_dir.to_string_lossy(),
            "--identity-file",
            &dir.join("identity").to_string_lossy(),
            "--passphrase-file",
            &dir.join("passphrase").to_string_lossy(),
            "--relay",
            "relay://example",
            "start",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>(),
        "{argv:?}"
    );
    let plist = render_launchd_plist("md.enesis.wyrd.drive-demo", &owner, &owned);
    assert!(plist.contains("<string>md.enesis.wyrd.drive-demo</string>"));
    assert!(plist.contains("wyrd-supervise"));
    assert!(plist.contains("--restart-status"));
    assert!(plist.contains("<string>0</string>"), "{plist}");
    assert!(plist.contains("vault.posture"));
    assert!(plist.contains("<string>owner</string>"), "{plist}");
    assert!(plist.contains("<string>start</string>"), "{plist}");
    assert!(plist.contains(&*owned.drive_dir.display().to_string()));
    assert!(plist.contains("--identity-file"));
    assert!(plist.contains("--relay"));
    assert!(plist.contains("<key>SuccessfulExit</key>"));
    // Owner-first ordering: the agent starts at login, before any
    // UI opens — the UI reads Unavailable until this socket appears.
    assert!(plist.contains("<key>RunAtLoad</key>"), "{plist}");
    let unit = render_systemd_unit("Wyrd drive demo", &owner, &owned);
    assert!(unit.contains("WantedBy=default.target"));
    assert!(unit.contains("Restart=on-failure"));
    assert!(unit.contains("RestartPreventExitStatus=78"));
    assert!(unit.contains("wyrd-supervise"));
    assert!(unit.contains("--restart-status=1") || unit.contains("--restart-status 1"));
    assert!(unit.contains(&*owned.drive_dir.display().to_string()));
}

/// A store-backed definition names the backend and the account
/// and carries no file flags: the manager starts the store
/// backend, and no credential path can leak from a file
/// posture into a store unit. The whole supervised tail pins
/// exactly like the file posture above, on both renderers.
#[test]
fn store_templates_name_the_backend_and_account_without_files() {
    let dir = scratch("templates-store");
    let owner = SupervisedOwner {
        supervise_bin: PathBuf::from("/opt/wyrd/bin/wyrd-supervise"),
        wyrd_bin: PathBuf::from("/opt/wyrd/bin/wyrd"),
        credential: ServiceCredential::OsStore {
            account: "wyrd-drive-alpha".to_string(),
        },
    };
    let owned = entry("demo", &dir);
    let argv = owner.argv(&owned, "0");
    let tail = argv
        .iter()
        .skip_while(|arg| *arg != "--")
        .skip(1)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        tail,
        vec![
            "/opt/wyrd/bin/wyrd",
            "owner",
            &owned.drive_dir.to_string_lossy(),
            "--credential-backend",
            "os-store",
            "--credential-account",
            "wyrd-drive-alpha",
            "--relay",
            "relay://example",
            "start",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>(),
        "{argv:?}"
    );
    for arg in &argv {
        assert!(
            !arg.contains("identity") && !arg.contains("passphrase"),
            "no file flag survives on a store unit: {argv:?}"
        );
    }
    let plist = render_launchd_plist("md.enesis.wyrd.drive-demo", &owner, &owned);
    assert!(plist.contains("--credential-backend"), "{plist}");
    assert!(plist.contains("--credential-account"), "{plist}");
    assert!(plist.contains("wyrd-drive-alpha"), "{plist}");
    assert!(!plist.contains("--identity-file"), "{plist}");
    assert!(!plist.contains("--passphrase-file"), "{plist}");
    let unit = render_systemd_unit("Wyrd drive demo", &owner, &owned);
    assert!(unit.contains("--credential-backend"), "{unit}");
    assert!(unit.contains("--credential-account"), "{unit}");
    assert!(unit.contains("wyrd-drive-alpha"), "{unit}");
    assert!(!unit.contains("--identity-file"), "{unit}");
    assert!(!unit.contains("--passphrase-file"), "{unit}");
    // Mount shares the credential shape: the same account
    // renders on the mount argv, never file flags.
    let mountpoint = dir.join("mnt");
    let mounted = mount_entry("demo", &dir, &mountpoint);
    let mount_argv = owner.argv(&mounted, "1");
    assert!(mount_argv.contains(&"mount".to_string()), "{mount_argv:?}");
    assert!(
        mount_argv.contains(&"--credential-account".to_string()),
        "{mount_argv:?}"
    );
    assert!(
        !mount_argv.iter().any(|arg| arg.contains("passphrase-file")),
        "{mount_argv:?}"
    );
}

#[test]
fn mount_templates_name_the_mountpoint_and_posture() {
    let dir = scratch("templates-mount");
    let owner = SupervisedOwner {
        supervise_bin: PathBuf::from("/opt/wyrd/bin/wyrd-supervise"),
        wyrd_bin: PathBuf::from("/opt/wyrd/bin/wyrd"),
        credential: ServiceCredential::File {
            identity_file: dir.join("identity"),
            passphrase_file: Some(dir.join("passphrase")),
        },
    };
    // A mountpoint with spaces and an ampersand pins the
    // operator-input escaping on both managers: the argv must
    // carry it whole, never split or reinterpreted.
    let mountpoint = dir.join("my mounts & more");
    let owned = mount_entry("demo", &dir, &mountpoint);
    let argv = owner.argv(&owned, "1");
    assert!(argv.contains(&"mount".to_string()), "{argv:?}");
    assert!(
        argv.contains(&mountpoint.to_string_lossy().into_owned()),
        "{argv:?}"
    );
    assert!(!argv.contains(&"vault".to_string()), "{argv:?}");
    assert!(
        argv.iter().any(|arg| arg.ends_with("mount.posture")),
        "{argv:?}"
    );
    let plist = render_launchd_plist("md.enesis.wyrd.drive-demo", &owner, &owned);
    assert!(plist.contains("my mounts &amp; more"), "{plist}");
    assert!(plist.contains("mount.posture"));
    assert!(plist.contains("<string>mount</string>"), "{plist}");
    assert!(plist.contains("<key>SuccessfulExit</key>"), "{plist}");
    let unit = render_systemd_unit("Wyrd drive demo", &owner, &owned);
    assert!(unit.contains("Restart=on-failure"));
    assert!(unit.contains("RestartPreventExitStatus=78"));
    assert!(unit.contains("mount.posture"), "{unit}");
    assert!(unit.contains(" mount "), "{unit}");
}

#[test]
fn mount_validation_pins_the_mountpoint_rules() {
    let dir = scratch("mount-validate");
    // Mount without a mountpoint refuses.
    let mut missing = entry("demo", &dir);
    missing.mode = ServiceMode::Mount;
    assert!(matches!(
        validate_entry("demo", &missing),
        Err(BootstrapError::MissingMountpoint)
    ));
    // Vault with a mountpoint refuses.
    let mut stray = entry("demo", &dir);
    stray.mountpoint = Some(dir.join("mnt"));
    assert!(matches!(
        validate_entry("demo", &stray),
        Err(BootstrapError::UnexpectedMountpoint)
    ));
    // Relative, escaping, and control-carrying mountpoints refuse.
    let mut relative = mount_entry("demo", &dir, PathBuf::from("relative/mnt").as_path());
    let error = validate_entry("demo", &relative).unwrap_err();
    assert!(error.to_string().contains("must be absolute"), "{error}");
    relative.mountpoint = Some(PathBuf::from("/ok/../evil"));
    let error = validate_entry("demo", &relative).unwrap_err();
    assert!(error.to_string().contains("must not contain .."), "{error}");
    // A mountpoint equal to the drive directory would mount the
    // projection over the store itself, so it refuses.
    let drive = dir.join("drive");
    let mut same = mount_entry("demo", &dir, drive.as_path());
    same.drive_dir = drive.clone();
    let error = validate_entry("demo", &same).unwrap_err();
    assert!(error.to_string().contains("must not equal"), "{error}");
    // Unknown future modes still skip with the generic reason.
    let mut future = entry("demo", &dir);
    future.mode = ServiceMode::Unknown("fuse".to_string());
    let error = validate_entry("demo", &future).unwrap_err();
    assert!(
        error.to_string().contains("unknown service mode"),
        "{error}"
    );
    assert!(
        !error.to_string().contains("installer follow-up"),
        "the follow-up pointer is retired: {error}"
    );
}

#[test]
fn provision_mount_registers_and_verifies_the_socket() {
    let dir = scratch("prov-mnt");
    let registry = dir.join("drives.json");
    let mountpoint = dir.join("mnt");
    std::fs::create_dir_all(&mountpoint).unwrap();
    let sock = dir.join("drive").join("owner.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let server = stub_owner(sock, 1, "Unobserved");
    let report = provision(provision_mount_no_service(
        &dir,
        &registry,
        "demo",
        &mountpoint,
    ));
    let ProvisionState::VerifiedAvailable { entry, .. } = report else {
        panic!("a stubbed owner must verify a mount provision: {report:?}");
    };
    assert!(matches!(entry.mode, ServiceMode::Mount));
    assert_eq!(entry.mountpoint, Some(mountpoint.clone()));
    let read = read_registry(&registry).unwrap();
    assert!(read.skipped.is_empty());
    assert_eq!(read.drives["demo"].mountpoint, Some(mountpoint));
    server.join().unwrap();
}

#[test]
fn provision_mount_without_a_mountpoint_refuses_before_creating() {
    let dir = scratch("provision-mount-nomp");
    let registry = dir.join("drives.json");
    let mut config = provision_no_service(&dir, &registry, "demo");
    config.mode = ServiceMode::Mount;
    config.mountpoint = None;
    let report = provision(config);
    assert!(
        matches!(
            report,
            ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                ..
            }
        ),
        "validation precedes initialization: {report:?}"
    );
    assert!(!dir.join("drive").exists());
    assert!(!registry.exists());
}

/// Concurrent same-key updates through the locked path converge
/// on one update with no torn mix: two writers racing on one key
/// serialize under the lock, so the final entry is one writer's
/// whole update — never one writer's relays beside the other's
/// mode. Without the lock (a stale read plus a whole-map write)
/// the second rename would silently clobber the first update.
#[test]
fn concurrent_same_key_updates_converge_without_a_torn_mix() {
    let dir = scratch("samekey");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    let mountpoint = dir.join("mnt");
    std::fs::create_dir_all(&mountpoint).unwrap();
    let registry_clone = registry.clone();
    let mountpoint_clone = mountpoint.clone();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            modify_registry(&registry, |drives| {
                let stored = drives.get("demo").expect("demo registered").clone();
                let updated = updated_entry(
                    stored,
                    vec!["relay://a".to_string()],
                    ServiceMode::Vault,
                    None,
                );
                drives.insert("demo".to_string(), updated);
                Ok(())
            })
            .unwrap();
        });
        scope.spawn(|| {
            modify_registry(&registry_clone, |drives| {
                let stored = drives.get("demo").expect("demo registered").clone();
                let updated = updated_entry(
                    stored,
                    vec!["relay://b".to_string()],
                    ServiceMode::Mount,
                    Some(mountpoint_clone.clone()),
                );
                drives.insert("demo".to_string(), updated);
                Ok(())
            })
            .unwrap();
        });
    });
    let read = read_registry(&registry).unwrap();
    let entry = &read.drives["demo"];
    // One winner, whole: relays and mode/mountpoint agree with
    // each other, never a mix of both writers.
    match entry.relays.as_slice() {
        [relay] if relay == "relay://a" => {
            assert!(matches!(entry.mode, ServiceMode::Vault));
            assert_eq!(entry.mountpoint, None);
        }
        [relay] if relay == "relay://b" => {
            assert!(matches!(entry.mode, ServiceMode::Mount));
            assert_eq!(entry.mountpoint, Some(mountpoint));
        }
        other => panic!("one writer must win whole: {other:?}"),
    }
}

/// Per-field writers through the locked path both survive: writer
/// A changes only the relays (taking mode and mountpoint from the
/// entry it reads under the lock), writer B changes only the
/// mode and mountpoint (keeping the fresh relays). Either commit
/// order leaves A's relays beside B's mode — a stale whole-map
/// write would lose one side whichever committed last.
#[test]
fn concurrent_disjoint_updates_both_survive() {
    let dir = scratch("disjoint");
    let registry = dir.join("drives.json");
    let mountpoint = dir.join("mnt");
    std::fs::create_dir_all(&mountpoint).unwrap();
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    let registry_clone = registry.clone();
    let mountpoint_clone = mountpoint.clone();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            modify_registry(&registry, |drives| {
                let fresh = drives.get("demo").expect("demo registered").clone();
                let updated = updated_entry(
                    fresh.clone(),
                    vec!["relay://a".to_string()],
                    fresh.mode,
                    fresh.mountpoint,
                );
                drives.insert("demo".to_string(), updated);
                Ok(())
            })
            .unwrap();
        });
        scope.spawn(|| {
            modify_registry(&registry_clone, |drives| {
                let fresh = drives.get("demo").expect("demo registered").clone();
                let updated = updated_entry(
                    fresh.clone(),
                    fresh.relays,
                    ServiceMode::Mount,
                    Some(mountpoint_clone.clone()),
                );
                drives.insert("demo".to_string(), updated);
                Ok(())
            })
            .unwrap();
        });
    });
    let read = read_registry(&registry).unwrap();
    let entry = &read.drives["demo"];
    assert_eq!(entry.relays, vec!["relay://a".to_string()]);
    assert!(matches!(entry.mode, ServiceMode::Mount));
    assert_eq!(entry.mountpoint, Some(mountpoint));
}

/// A kept relay set never reverts a concurrent update: writer B
/// sets an explicit relay set while writer A reconfigures with no
/// `--relay` flags (keep intent) at the `reconfigure` call sites
/// themselves. Whatever order the two rewrites commit, the final
/// entry carries B's set — A's keep resolves fresh, never stale.
/// Retries on the terminal store lock keep the race hermetic: two
/// reconfigures in one process briefly contend on the keystore
/// open, and a refused attempt simply re-runs.
#[test]
fn reconfigure_keep_never_reverts_a_concurrent_relay_update() {
    let dir = scratch("keep-race");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    let sock = dir.join("drive").join("owner.sock");
    // Exactly two connections: each thread verifies once, on the
    // attempt its keystore open succeeds — attempts refused with
    // the store lock never reach the socket.
    let server = stub_owner(sock, 2, "Unobserved");
    let registry_a = registry.clone();
    let registry_b = registry.clone();
    let dir_a = dir.clone();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            for _ in 0..60 {
                let report = reconfigure(reconfigure_no_service(
                    &dir_a,
                    &registry_a,
                    None,
                    ServiceMode::Vault,
                    None,
                ));
                match report {
                    ProvisionState::VerifiedAvailable { .. } => return,
                    ProvisionState::ProvisioningIncomplete {
                        kind: ProvisionFailureKind::StoreLocked,
                        ..
                    } => {
                        // The keystore open holds through the
                        // KDF, so a colliding attempt backs
                        // off instead of spinning against it.
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        continue;
                    }
                    other => panic!("keep must verify or contend: {other:?}"),
                }
            }
            panic!("keep never verified past contention");
        });
        scope.spawn(move || {
            for _ in 0..60 {
                let report = reconfigure(reconfigure_no_service(
                    &dir,
                    &registry_b,
                    Some(vec!["relay://new".to_string()]),
                    ServiceMode::Vault,
                    None,
                ));
                match report {
                    ProvisionState::VerifiedAvailable { .. } => return,
                    ProvisionState::ProvisioningIncomplete {
                        kind: ProvisionFailureKind::StoreLocked,
                        ..
                    } => {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        continue;
                    }
                    other => panic!("explicit update must verify or contend: {other:?}"),
                }
            }
            panic!("explicit update never verified past contention");
        });
    });
    let read = read_registry(&registry).unwrap();
    assert_eq!(
        read.drives["demo"].relays,
        vec!["relay://new".to_string()],
        "the explicit update survives the keep"
    );
    server.join().unwrap();
}

/// Post-stop failures classify honestly: resolve failures name
/// the registry state to repair with the stopped-service note,
/// genuine write failures stay `write-registry`.
#[test]
fn post_stop_failures_classify_the_repair() {
    let (stage, kind, reason) = classify_post_stop_failure(&BootstrapError::Read {
        path: "/reg/drives.json".into(),
        source: std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no registry entry for /d: register with `wyrd bootstrap` first",
        ),
    });
    assert_eq!(stage, "validate");
    assert_eq!(kind, ProvisionFailureKind::Validate);
    assert!(reason.contains("already stopped and cleared"), "{reason}");
    assert!(reason.contains("re-register"), "{reason}");

    let (stage, kind, reason) = classify_post_stop_failure(&BootstrapError::ConflictingEntries {
        dir: "/d".into(),
        keys: "a, b".to_string(),
    });
    assert_eq!(stage, "validate");
    assert_eq!(kind, ProvisionFailureKind::Validate);
    assert!(reason.contains("resolve the duplicate"), "{reason}");

    let (stage, kind, reason) = classify_post_stop_failure(&BootstrapError::ManagedWithoutRelays);
    assert_eq!(stage, "validate");
    assert_eq!(kind, ProvisionFailureKind::Validate);
    assert!(reason.contains("--no-service"), "{reason}");

    let (stage, kind, _) = classify_post_stop_failure(&BootstrapError::RefusedWrite {
        source: Box::new(BootstrapError::InvalidRelays),
    });
    assert_eq!(stage, "write-registry");
    assert_eq!(kind, ProvisionFailureKind::WriteRegistry);
}

/// The reinstall decision compares relay sets only: equal sets
/// need no reinstall (order counts — a rewrite is harmless but
/// pointless), differing sets do, and a mode or mountpoint
/// difference alone never triggers one (both copies carry
/// operator intent there).
#[test]
fn reinstall_decision_compares_relay_sets_only() {
    let dir = scratch("reinstall-decide");
    let base = entry("demo", &dir);
    assert!(!definition_needs_reinstall(&base, &base));
    let mut ordered = base.clone();
    ordered.relays = vec!["relay://a".to_string(), "relay://b".to_string()];
    let mut reordered = base.clone();
    reordered.relays = vec!["relay://b".to_string(), "relay://a".to_string()];
    assert!(definition_needs_reinstall(&ordered, &reordered));
    let mut mode_only = base.clone();
    mode_only.mode = ServiceMode::Mount;
    mode_only.mountpoint = Some(dir.join("mnt"));
    assert!(!definition_needs_reinstall(&base, &mode_only));
    let mut differing = base.clone();
    differing.relays = vec!["relay://fresh".to_string()];
    assert!(definition_needs_reinstall(&base, &differing));
}

/// A definition install failure carries the install-service kind:
/// the caller reports it post-registration instead of mistaking
/// it for a write or start failure. Hermetic: the override names
/// a path whose parent is a file, so the atomic write fails
/// before any manager is touched.
#[test]
fn install_failure_carries_the_install_service_kind() {
    let dir = scratch("reinstall-kind");
    let owner = SupervisedOwner {
        supervise_bin: dir.join("wyrd-supervise"),
        wyrd_bin: dir.join("wyrd"),
        credential: ServiceCredential::File {
            identity_file: dir.join("identity"),
            passphrase_file: Some(dir.join("passphrase")),
        },
    };
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    let definition = blocker.join("wyrd-test-definition");
    match install_service_definition(&entry("demo", &dir), &owner, Some(&definition)) {
        Err((kind, reason)) => {
            assert_eq!(kind, ProvisionFailureKind::InstallService);
            assert!(reason.contains("install-service"), "{reason}");
        }
        Ok(_) => panic!("an unwritable definition path must fail install"),
    }
}

/// Starting an unregistered drive refuses before anything is
/// touched: no keystore opens, no manager is contacted, nothing
/// is written. The pointer names registration, not supervision.
#[test]
fn start_unregistered_drive_refuses_with_the_registration_pointer() {
    let dir = scratch("start-unregistered");
    let registry = dir.join("drives.json");
    let report = start_managed_service(StartServiceConfig {
        drive_dir: dir.join("drive"),
        registry_path: registry.clone(),
        service_definition: Some(dir.join("wyrd-test-definition")),
        verify_timeout: Duration::from_secs(1),
    });
    match report {
        ProvisionState::ProvisioningIncomplete { stage, reason, .. } => {
            assert_eq!(stage, "validate");
            assert!(reason.contains("register with"), "{reason}");
        }
        other => panic!("an unregistered drive refuses before anything moves: {other:?}"),
    }
    assert!(!registry.exists(), "a refusal writes nothing");
}

/// Starting without an installed definition refuses with the
/// reinstall pointer — never the manager: a missing file is the
/// reinstall recovery, not this one. Hermetic: the override
/// names a path that never exists, so no manager is touched.
#[test]
fn start_without_a_definition_refuses_with_the_reinstall_pointer() {
    let dir = scratch("start-nodef");
    let registry = dir.join("drives.json");
    let mut drives = BTreeMap::new();
    drives.insert("demo".to_string(), entry("demo", &dir));
    write_registry(&registry, &drives).unwrap();
    let missing = dir.join("wyrd-test-definition-missing");
    assert!(!missing.exists());
    let report = start_managed_service(StartServiceConfig {
        drive_dir: dir.join("drive"),
        registry_path: registry,
        service_definition: Some(missing),
        verify_timeout: Duration::from_secs(1),
    });
    match report {
        ProvisionState::ProvisioningIncomplete { stage, reason, .. } => {
            assert_eq!(stage, "validate");
            assert!(reason.contains("reinstall"), "{reason}");
            assert!(reason.contains("reconfigure"), "{reason}");
            // A deliberately unmanaged (per-session) drive lands
            // here too — no definition ever exists for it — so
            // the refusal names the session start beside the
            // managed repair, never the manager alone.
            assert!(reason.contains("unlock"), "{reason}");
        }
        other => panic!("a missing definition refuses with reinstall: {other:?}"),
    }
}

/// The lingering probe parses `loginctl -p Linger --value`
/// without touching a manager: `yes` refuses, `no` proceeds,
/// anything else is Unknown (never safe). The live `loginctl`
/// invocation is covered by the manual logout drill, not by
/// hermetic tests — unit tests must never shell out to the
/// service manager.
#[test]
fn linger_probe_parses_loginctl_answers() {
    assert_eq!(parse_linger_state("yes\n"), Ok(true));
    assert_eq!(parse_linger_state("no\n"), Ok(false));
    assert_eq!(parse_linger_state("YES"), Ok(true));
    assert!(parse_linger_state("").is_err(), "empty is Unknown");
    assert!(
        parse_linger_state("lingering\n").is_err(),
        "a State word is not a Linger value"
    );
    assert!(
        parse_linger_state("unknown\n").is_err(),
        "garbage is Unknown"
    );
}

/// The lingering refusal names the bound and the remediation —
/// never a silent configuration change, never a generic
/// manager failure a retry could clear.
#[test]
fn lingering_refusals_name_the_bound() {
    let enabled = BootstrapError::LingeringEnabled { uid: 1000 };
    assert!(
        enabled.to_string().contains("disable-linger"),
        "names the remediation: {enabled}"
    );
    assert!(
        enabled.to_string().contains("never changes"),
        "disclaims modification: {enabled}"
    );
    let unknown = BootstrapError::LingerUnknown {
        uid: 1000,
        detail: "loginctl refused".to_string(),
    };
    assert!(
        unknown
            .to_string()
            .contains("refusing rather than assuming"),
        "Unknown is fail-closed: {unknown}"
    );
}

/// Session-supplied credentials never render: the managed
/// install guard fails closed with the install-service kind,
/// naming the session repair instead of a manager retry. Store
/// credentials render as-is: the manager reads the item, no
/// file needed.
#[test]
fn managed_install_without_credential_files_refuses() {
    match managed_credential(&ServiceCredential::File {
        identity_file: PathBuf::from("/keys/identity"),
        passphrase_file: None,
    }) {
        Err((ProvisionFailureKind::InstallService, reason)) => {
            assert!(reason.contains("unlock"), "{reason}");
            assert!(reason.contains("--no-service"), "{reason}");
        }
        other => panic!("a file-less managed install refuses: {other:?}"),
    }
    let path = PathBuf::from("/keys/passphrase");
    assert!(matches!(
        managed_credential(&ServiceCredential::File {
            identity_file: PathBuf::from("/keys/identity"),
            passphrase_file: Some(path.clone()),
        }),
        Ok(ServiceCredential::File { .. })
    ));
    assert!(matches!(
        managed_credential(&ServiceCredential::OsStore {
            account: "wyrd-drive-demo".to_string(),
        }),
        Ok(ServiceCredential::OsStore { .. })
    ));
}

/// The guard wires into provisioning: a managed provision with
/// session-supplied credentials registers the drive, then
/// refuses at install time — no definition is written, no
/// manager is touched. Hermetic: the guard runs before any
/// install, so the platform definition path is never reached.
#[test]
fn managed_provision_without_a_passphrase_file_refuses_at_install() {
    let dir = scratch("provision-nosession-file");
    let registry = dir.join("drives.json");
    let mut config = provision_no_service(&dir, &registry, "demo");
    config.manage_service = true;
    config.relays = vec!["relay://test".to_string()];
    match provision(config) {
        ProvisionState::RegisteredButUnavailable { kind, reason, .. } => {
            assert_eq!(kind, ProvisionFailureKind::InstallService);
            assert!(reason.contains("unlock"), "{reason}");
        }
        other => panic!("a file-less managed provision refuses at install: {other:?}"),
    }
}

/// Starting against a definition the manager cannot load reports
/// unavailable, never success: the enable/start failure maps to
/// its stage kind like every other handoff. Hermetic: the
/// override names a scratch file with garbage content, so the
/// load fails before anything is registered — no live unit is
/// touched and no residue can remain. (On macOS `load` fails on
/// the garbage plist; on Linux the unit name is unknown to the
/// user manager, so enable fails first.)
#[test]
fn start_with_an_unloadable_definition_reports_unavailable() {
    let dir = scratch("start-baddef");
    let registry = dir.join("drives.json");
    let mut drives = BTreeMap::new();
    drives.insert("demo".to_string(), entry("demo", &dir));
    write_registry(&registry, &drives).unwrap();
    let definition = dir.join("wyrd-test-definition");
    std::fs::write(&definition, b"not a service definition").unwrap();
    let report = start_managed_service(StartServiceConfig {
        drive_dir: dir.join("drive"),
        registry_path: registry,
        service_definition: Some(definition),
        verify_timeout: Duration::from_secs(1),
    });
    match report {
        ProvisionState::RegisteredButUnavailable { reason, .. } => {
            assert!(
                reason.contains("enable-service")
                    || reason.contains("start-service")
                    || reason.contains("verify-socket"),
                "a refused start names its stage: {reason}"
            );
        }
        other => panic!("an unloadable definition reports unavailable: {other:?}"),
    }
}

/// The installed definition follows the written entry: when the
/// lock resolves a fresher relay set than the pre-stop working
/// copy carried, the definition is reinstalled from the written
/// entry at the same override path — the manager never serves a
/// stale relay set beside the new registry entry.
#[test]
fn reinstall_follows_the_written_relay_set() {
    let dir = scratch("reinstall");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    // Stage a concurrent update between the pre-stop read and the
    // rewrite by writing it directly: the working copy below
    // carries the stale pre-stop set, the file carries the fresh
    // set the lock will resolve.
    let mut read = read_registry(&registry).unwrap();
    read.drives.get_mut("demo").unwrap().relays = vec!["relay://fresh".to_string()];
    write_registry(&registry, &read.drives).unwrap();
    let stale = RegistryEntry {
        name: "demo".to_string(),
        drive_dir: dir.join("drive"),
        relays: vec![],
        service: "drive-demo".to_string(),
        mode: ServiceMode::Vault,
        mountpoint: None,
    };
    let written = RegistryEntry {
        relays: vec!["relay://fresh".to_string()],
        ..stale.clone()
    };
    let owner = SupervisedOwner {
        supervise_bin: dir.join("wyrd-supervise"),
        wyrd_bin: dir.join("wyrd"),
        credential: ServiceCredential::File {
            identity_file: dir.join("identity"),
            passphrase_file: Some(dir.join("passphrase")),
        },
    };
    let definition = dir.join("wyrd-test-definition");
    install_service_definition(&stale, &owner, Some(&definition)).unwrap();
    assert!(
        !std::fs::read_to_string(&definition)
            .unwrap()
            .contains("relay://fresh"),
        "the stale install carries the stale set"
    );
    install_service_definition(&written, &owner, Some(&definition)).unwrap();
    let content = std::fs::read_to_string(&definition).unwrap();
    assert!(
        content.contains("relay://fresh"),
        "the reinstall carries the written set: {content}"
    );
}

/// Mount provisioning refuses a missing, file, containing, or
/// contained mountpoint before anything is created: no drive
/// directory and no registry appear, and the reason names the
/// rule. Shape problems stay in the entry validator; these live
/// checks run after it.
#[test]
fn provision_mount_refuses_unusable_mountpoints_before_creating() {
    // Missing mountpoint.
    let dir = scratch("prov-mnt-missing");
    let registry = dir.join("drives.json");
    let missing = dir.join("no-such-mnt");
    let report = provision(provision_mount_no_service(
        &dir, &registry, "demo", &missing,
    ));
    match report {
        ProvisionState::ProvisioningIncomplete { stage, reason, .. } => {
            assert_eq!(stage, "validate");
            assert!(reason.contains("does not exist"), "{reason}");
        }
        other => panic!("a missing mountpoint must refuse: {other:?}"),
    }
    assert!(!dir.join("drive").exists());
    assert!(!registry.exists());

    // Mountpoint that is a file, not a directory.
    let dir = scratch("prov-mnt-file");
    let registry = dir.join("drives.json");
    let file = dir.join("file-mnt");
    std::fs::write(&file, b"x").unwrap();
    let report = provision(provision_mount_no_service(&dir, &registry, "demo", &file));
    match report {
        ProvisionState::ProvisioningIncomplete { stage, reason, .. } => {
            assert_eq!(stage, "validate");
            assert!(reason.contains("not a directory"), "{reason}");
        }
        other => panic!("a file mountpoint must refuse: {other:?}"),
    }
    assert!(!registry.exists());

    // Mountpoint containing the drive directory.
    let dir = scratch("prov-mnt-contains");
    let registry = dir.join("drives.json");
    let report = provision(provision_mount_no_service(
        &dir,
        &registry,
        "demo",
        dir.as_path(),
    ));
    match report {
        ProvisionState::ProvisioningIncomplete { stage, reason, .. } => {
            assert_eq!(stage, "validate");
            assert!(reason.contains("must not contain"), "{reason}");
        }
        other => panic!("a containing mountpoint must refuse: {other:?}"),
    }
    assert!(!registry.exists());

    // Mountpoint inside the drive directory.
    let dir = scratch("prov-mnt-inside");
    let registry = dir.join("drives.json");
    let drive = dir.join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    let inside = drive.join("mnt");
    std::fs::create_dir_all(&inside).unwrap();
    let report = provision(provision_mount_no_service(&dir, &registry, "demo", &inside));
    match report {
        ProvisionState::ProvisioningIncomplete { stage, reason, .. } => {
            assert_eq!(stage, "validate");
            assert!(reason.contains("must not sit inside"), "{reason}");
        }
        other => panic!("an inside mountpoint must refuse: {other:?}"),
    }
    assert!(!registry.exists());
}

/// A stub owner socket: answers every request with the given
/// refusal code. It speaks the real framed protocol, so the
/// handoff test exercises the real client, the real framing, and
/// the real refusal mapping — only the owner is fake. The
/// accept loop is deadline-bounded so the thread always returns
/// for `join`, even when the caller makes fewer connections than
/// the cap.
pub(super) fn stub_owner(
    sock: PathBuf,
    max_requests: usize,
    code: &'static str,
) -> std::thread::JoinHandle<()> {
    if sock.exists() {
        std::fs::remove_file(&sock).unwrap();
    }
    std::thread::spawn(move || {
        let listener = UnixListener::bind(&sock).unwrap();
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let mut served = 0;
        while served < max_requests && std::time::Instant::now() < deadline {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            };
            served += 1;
            stream
                .set_read_timeout(Some(Duration::from_secs(30)))
                .unwrap();
            let mut len = [0u8; 4];
            if stream.read_exact(&mut len).is_err() {
                continue;
            }
            let len = u32::from_be_bytes(len) as usize;
            if len > 96 * 1024 * 1024 {
                continue;
            }
            let mut body = vec![0u8; len];
            if stream.read_exact(&mut body).is_err() {
                continue;
            }
            let id: u64 = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value.get("id")?.as_u64())
                .unwrap_or(0);
            let reply = serde_json::json!({
                "v": crate::owner::WIRE_VERSION,
                "id": id,
                "result": {"err": {"code": code, "message": "stub refusal"}},
            });
            let body = serde_json::to_vec(&reply).unwrap();
            let _ = stream.write_all(&(body.len() as u32).to_be_bytes());
            let _ = stream.write_all(&body);
        }
    })
}

pub(super) fn identity() -> DeviceIdentitySecret {
    DeviceIdentitySecret::from_bytes([7u8; 32]).unwrap()
}

pub(super) fn provision_no_service(dir: &Path, registry: &Path, name: &str) -> ProvisionConfig {
    ProvisionConfig {
        name: name.to_string(),
        drive_dir: dir.join("drive"),
        relays: vec![],
        mode: ServiceMode::Vault,
        mountpoint: None,
        identity: identity(),
        passphrase: "bootstrap-test-pass".to_string(),
        init: Init::Create,
        registry_path: registry.to_path_buf(),
        wyrd_bin: Some(dir.join("wyrd")),
        supervise_bin: Some(dir.join("wyrd-supervise")),
        credential: ServiceCredential::File {
            identity_file: dir.join("identity"),
            passphrase_file: None,
        },
        manage_service: false,
        verify_timeout: Duration::from_secs(10),
    }
}

pub(super) fn provision_mount_no_service(
    dir: &Path,
    registry: &Path,
    name: &str,
    mountpoint: &Path,
) -> ProvisionConfig {
    let mut config = provision_no_service(dir, registry, name);
    config.mode = ServiceMode::Mount;
    config.mountpoint = Some(mountpoint.to_path_buf());
    config
}

#[test]
fn provision_verifies_the_socket_and_registers_the_drive() {
    let dir = scratch("provision");
    let registry = dir.join("drives.json");
    let sock = dir.join("drive").join("owner.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let server = stub_owner(sock, 1, "Unobserved");

    let report = provision(provision_no_service(&dir, &registry, "demo"));
    let ProvisionState::VerifiedAvailable { entry, .. } = report else {
        panic!("expected verified available, got terminal state without success");
    };
    assert_eq!(entry.name, "demo");

    let read = read_registry(&registry).unwrap();
    assert!(read.skipped.is_empty());
    assert_eq!(read.drives["demo"].drive_dir, dir.join("drive"));
    server.join().unwrap();
}

#[test]
fn provision_without_an_owner_registers_but_withholds_ready() {
    let dir = scratch("noowner");
    let registry = dir.join("drives.json");
    let report = provision(provision_no_service(&dir, &registry, "demo"));
    let ProvisionState::RegisteredButUnavailable { entry, reason, .. } = report else {
        panic!("a missing owner must never verify");
    };
    assert!(reason.contains("verify-socket"), "{reason}");
    // Registered all the same: the next run reconciles instead of
    // starting over.
    let read = read_registry(&registry).unwrap();
    assert!(read.drives.contains_key(&entry.name));

    // Re-run reconciles the existing drive (create refuses,
    // the flow re-verifies) instead of failing on it.
    let again = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(
        matches!(again, ProvisionState::RegisteredButUnavailable { .. }),
        "re-run must reconcile, not fail on the existing drive"
    );
}

#[test]
fn provision_rejects_bad_names_before_touching_anything() {
    let dir = scratch("badname");
    let registry = dir.join("drives.json");
    let mut config = provision_no_service(&dir, &registry, "BAD NAME!");
    config.name = "BAD NAME!".to_string();
    let report = provision(config);
    assert!(
        matches!(
            report,
            ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                ..
            }
        ),
        "validation must precede initialization"
    );
    assert!(!dir.join("drive").exists());
    assert!(!registry.exists());
}

pub(super) fn reconfigure_no_service(
    dir: &Path,
    registry: &Path,
    relays: Option<Vec<String>>,
    mode: ServiceMode,
    mountpoint: Option<PathBuf>,
) -> ReconfigureConfig {
    ReconfigureConfig {
        drive_dir: dir.join("drive"),
        relays,
        mode,
        mountpoint,
        identity: identity(),
        passphrase: "bootstrap-test-pass".to_string(),
        registry_path: registry.to_path_buf(),
        wyrd_bin: Some(dir.join("wyrd")),
        supervise_bin: Some(dir.join("wyrd-supervise")),
        credential: ServiceCredential::File {
            identity_file: dir.join("identity"),
            passphrase_file: None,
        },
        // A scratch definition that never exists: hermetic tests
        // must stop and clear nothing real, and the missing file
        // is the not-managed case — never a manager touch.
        service_definition: Some(dir.join("wyrd-test-definition")),
        manage_service: false,
        verify_timeout: Duration::from_secs(10),
    }
}

/// Reconfigure rewrites the entry and verifies the socket: the
/// relay set is replaced, the stub owner's pre-first-pass
/// refusal verifies like provisioning, and the registry carries
/// the new set.
#[test]
fn reconfigure_rewrites_relays_and_verifies_the_socket() {
    let dir = scratch("reconfigure");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(
        matches!(first, ProvisionState::RegisteredButUnavailable { .. }),
        "no owner serves the fresh drive: {first:?}"
    );
    let sock = dir.join("drive").join("owner.sock");
    let server = stub_owner(sock, 1, "Unobserved");
    let report = reconfigure(reconfigure_no_service(
        &dir,
        &registry,
        Some(vec!["relay://new".to_string()]),
        ServiceMode::Vault,
        None,
    ));
    let ProvisionState::VerifiedAvailable { entry, .. } = report else {
        panic!("a stubbed owner must verify: {report:?}");
    };
    assert_eq!(entry.relays, vec!["relay://new".to_string()]);
    assert!(matches!(entry.mode, ServiceMode::Vault));
    let read = read_registry(&registry).unwrap();
    assert_eq!(read.drives["demo"].relays, vec!["relay://new".to_string()]);
    server.join().unwrap();
}

/// `None` relays keep the registered set: a credential-path-only
/// reconfigure never clears relays by accident.
#[test]
fn reconfigure_keeps_relays_when_none_given() {
    let dir = scratch("reconf-keep");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    // Hand the entry a relay set, then reconfigure without one.
    let mut read = read_registry(&registry).unwrap();
    read.drives.get_mut("demo").unwrap().relays = vec!["relay://kept".to_string()];
    write_registry(&registry, &read.drives).unwrap();
    let sock = dir.join("drive").join("owner.sock");
    let server = stub_owner(sock, 1, "Unobserved");
    let report = reconfigure(reconfigure_no_service(
        &dir,
        &registry,
        None,
        ServiceMode::Vault,
        None,
    ));
    let ProvisionState::VerifiedAvailable { entry, .. } = report else {
        panic!("a stubbed owner must verify: {report:?}");
    };
    assert_eq!(entry.relays, vec!["relay://kept".to_string()]);
    server.join().unwrap();
}

/// Unknown modes refuse before anything stops: the registry
/// keeps the old entry and no service state is touched — only
/// `vault` and `mount` reconfigure, and anything else is a
/// validation refusal, not a guess.
#[test]
fn reconfigure_refuses_unknown_modes_before_stopping() {
    let dir = scratch("reconfigure-mode");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    let before = std::fs::read(&registry).unwrap();
    let report = reconfigure(reconfigure_no_service(
        &dir,
        &registry,
        None,
        ServiceMode::Unknown("fuse".to_string()),
        None,
    ));
    match report {
        ProvisionState::ProvisioningIncomplete {
            kind,
            stage,
            reason,
        } => {
            assert_eq!(kind, ProvisionFailureKind::Validate);
            assert_eq!(Some(stage), kind.stage());
            assert!(
                reason.contains("unknown service mode"),
                "refusal names the unknown mode: {reason}"
            );
        }
        other => panic!("an unknown mode must refuse: {other:?}"),
    }
    assert_eq!(std::fs::read(&registry).unwrap(), before);
    assert_eq!(
        Some("stop-service"),
        ProvisionFailureKind::StopService.stage()
    );
}

/// Reconfigure moves a vault entry to mount: the mountpoint is
/// recorded, the stub owner's pre-first-pass refusal verifies
/// like provisioning, and the registry carries the new mode.
#[test]
fn reconfigure_moves_vault_to_mount_and_verifies_the_socket() {
    let dir = scratch("rec-mnt");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    let mountpoint = dir.join("mnt");
    std::fs::create_dir_all(&mountpoint).unwrap();
    let sock = dir.join("drive").join("owner.sock");
    let server = stub_owner(sock, 1, "Unobserved");
    let report = reconfigure(reconfigure_no_service(
        &dir,
        &registry,
        None,
        ServiceMode::Mount,
        Some(mountpoint.clone()),
    ));
    let ProvisionState::VerifiedAvailable { entry, .. } = report else {
        panic!("a stubbed owner must verify a mount reconfigure: {report:?}");
    };
    assert!(matches!(entry.mode, ServiceMode::Mount));
    assert_eq!(entry.mountpoint, Some(mountpoint.clone()));
    let read = read_registry(&registry).unwrap();
    assert!(matches!(read.drives["demo"].mode, ServiceMode::Mount));
    assert_eq!(read.drives["demo"].mountpoint, Some(mountpoint));
    server.join().unwrap();
}

/// A mount reconfigure without a mountpoint refuses before
/// anything stops: the registry keeps its bytes and no service
/// state moves — the live check runs at the validate stage, like
/// the missing-mountpoint shape refusal.
#[test]
fn reconfigure_refuses_mount_without_a_mountpoint() {
    let dir = scratch("rec-mnt-none");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    let before = std::fs::read(&registry).unwrap();
    let report = reconfigure(reconfigure_no_service(
        &dir,
        &registry,
        None,
        ServiceMode::Mount,
        None,
    ));
    match report {
        ProvisionState::ProvisioningIncomplete {
            kind,
            stage,
            reason,
        } => {
            assert_eq!(kind, ProvisionFailureKind::Validate);
            assert_eq!(Some(stage), kind.stage());
            assert!(reason.contains("needs a mountpoint"), "{reason}");
        }
        other => panic!("a mount without a mountpoint must refuse: {other:?}"),
    }
    assert_eq!(std::fs::read(&registry).unwrap(), before);
}

/// No entry for the directory refuses without writing: the drive
/// exists (verify-open passes) but was registered elsewhere —
/// directories match by exact path only.
#[test]
fn reconfigure_refuses_an_unregistered_directory() {
    let dir = scratch("reconfigure-elsewhere");
    let registry = dir.join("drives.json");
    let other = dir.join("other.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    let report = reconfigure(reconfigure_no_service(
        &dir,
        &other,
        None,
        ServiceMode::Vault,
        None,
    ));
    match report {
        ProvisionState::ProvisioningIncomplete {
            kind,
            stage,
            reason,
        } => {
            assert_eq!(kind, ProvisionFailureKind::Validate);
            assert_eq!(Some(stage), kind.stage());
            assert!(reason.contains("no registry entry"), "{reason}");
        }
        other => panic!("an unregistered directory must refuse: {other:?}"),
    }
    assert!(!other.exists(), "a refusal writes nothing");
}

/// Duplicate registrations for one directory refuse loudly with
/// both key names: the command never guesses across conflicts.
#[test]
fn reconfigure_refuses_conflicting_registrations() {
    let dir = scratch("reconfigure-dupe");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    let raw = serde_json::json!({
        "version": 1,
        "drives": {
            "demo": {
                "name": "demo",
                "drive_dir": dir.join("drive").to_string_lossy(),
                "relays": [],
                "service": "drive-demo",
                "mode": "vault",
            },
            "alias": {
                "name": "alias",
                "drive_dir": dir.join("drive").to_string_lossy(),
                "relays": [],
                "service": "drive-alias",
                "mode": "vault",
            },
        },
    });
    std::fs::write(&registry, serde_json::to_vec(&raw).unwrap()).unwrap();
    let report = reconfigure(reconfigure_no_service(
        &dir,
        &registry,
        None,
        ServiceMode::Vault,
        None,
    ));
    match report {
        ProvisionState::ProvisioningIncomplete { kind, reason, .. } => {
            assert_eq!(kind, ProvisionFailureKind::Validate);
            assert!(
                reason.contains("demo") && reason.contains("alias"),
                "conflict names both keys: {reason}"
            );
        }
        other => panic!("a duplicate registration must refuse: {other:?}"),
    }
    // Both entries survive the refusal.
    let read = read_registry(&registry).unwrap();
    assert!(read.drives.contains_key("demo") && read.drives.contains_key("alias"));
}

/// A wrong passphrase fails the open before anything stops:
/// the staged definition file survives, and the registry keeps
/// its bytes exactly. Credential failures must never take a
/// service down.
#[test]
fn reconfigure_verify_open_failure_leaves_the_registry_untouched() {
    let dir = scratch("reconfigure-locked");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    let before = std::fs::read(&registry).unwrap();
    let definition = dir.join("wyrd-test-definition");
    std::fs::write(&definition, "leftover").unwrap();
    let mut config = reconfigure_no_service(&dir, &registry, None, ServiceMode::Vault, None);
    config.passphrase = "wrong-pass".to_string();
    match reconfigure(config) {
        ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
            assert_eq!(kind, ProvisionFailureKind::VerifyOpen);
            assert_eq!(Some(stage), kind.stage());
        }
        other => panic!("a wrong passphrase must stop at verify-open: {other:?}"),
    }
    assert!(definition.exists(), "the stop never ran");
    assert_eq!(std::fs::read(&registry).unwrap(), before);
}

/// The open runs before the stop even when managed: a wrong
/// passphrase with a staged definition refuses at verify-open,
/// and the definition survives for the corrected retry. The
/// explicit binary paths resolve without a manager, and the open
/// fails before any service state moves, so the test is hermetic.
#[test]
fn reconfigure_proves_the_open_before_stopping() {
    let dir = scratch("rec-firstopen");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    let definition = dir.join("wyrd-test-definition");
    std::fs::write(&definition, "leftover").unwrap();
    let before = std::fs::read(&registry).unwrap();
    let mut config = reconfigure_no_service(
        &dir,
        &registry,
        Some(vec!["relay://kept".to_string()]),
        ServiceMode::Vault,
        None,
    );
    config.manage_service = true;
    config.passphrase = "wrong-pass".to_string();
    match reconfigure(config) {
        ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
            assert_eq!(kind, ProvisionFailureKind::VerifyOpen);
            assert_eq!(Some(stage), kind.stage());
        }
        other => panic!("a wrong passphrase must stop at verify-open: {other:?}"),
    }
    assert!(definition.exists(), "the stop never ran");
    assert_eq!(std::fs::read(&registry).unwrap(), before);
}

/// A live managed lock stops the definition and retries the open
/// once: the retry still fails here (the test holds the lock), so
/// the terminal kind reports, and the re-run converges after the
/// lock releases. Hermetic: the override names a missing file, so
/// the stop is a no-op and no manager is probed.
#[test]
fn reconfigure_retries_the_open_past_a_live_managed_lock() {
    let dir = scratch("rec-retry");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    let held = Engine::open_keystore(dir.join("drive"), "bootstrap-test-pass", identity()).unwrap();
    let mut config = reconfigure_no_service(
        &dir,
        &registry,
        Some(vec!["relay://kept".to_string()]),
        ServiceMode::Vault,
        None,
    );
    config.manage_service = true;
    match reconfigure(config) {
        ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
            assert_eq!(kind, ProvisionFailureKind::StoreLocked);
            assert_eq!(Some(stage), kind.stage());
        }
        other => panic!("a held managed drive must report the lock: {other:?}"),
    }
    drop(held);
}

/// A managed reconfigure without relays refuses at validation,
/// before any service state moves: reaching the stop/install
/// steps end to end would touch a real manager, so the managed
/// path past validation stays literal-adjacent like the
/// provision kind test — hermetic tests stop here.
#[test]
fn reconfigure_refuses_a_relay_less_managed_service() {
    let dir = scratch("reconfigure-norelay");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    let mut config =
        reconfigure_no_service(&dir, &registry, Some(Vec::new()), ServiceMode::Vault, None);
    config.manage_service = true;
    match reconfigure(config) {
        ProvisionState::ProvisioningIncomplete {
            kind,
            stage,
            reason,
        } => {
            assert_eq!(kind, ProvisionFailureKind::Validate);
            assert_eq!(Some(stage), kind.stage());
            assert!(reason.contains("--no-service"), "{reason}");
        }
        other => panic!("a relay-less managed reconfigure must refuse: {other:?}"),
    }
}

/// A rewrite carries skipped entries forward verbatim: a future
/// `fuse` entry survives a vault reconfigure with its value
/// intact, and the caller still warns about the skip instead of
/// silently forgetting the entry.
#[test]
fn reconfigure_preserves_skipped_entries_verbatim() {
    let dir = scratch("rec-presv");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    // A future-mode entry beside the managed one, staged by hand
    // like a newer writer would leave it.
    let future_raw = serde_json::json!({
        "name": "future",
        "drive_dir": dir.join("drive").to_string_lossy(),
        "relays": ["relay://future"],
        "service": "drive-future",
        "mode": "fuse",
    });
    let mut raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
    raw["drives"]["future"] = future_raw.clone();
    std::fs::write(&registry, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
    let sock = dir.join("drive").join("owner.sock");
    let server = stub_owner(sock, 1, "Unobserved");
    let report = reconfigure(reconfigure_no_service(
        &dir,
        &registry,
        Some(vec!["relay://new".to_string()]),
        ServiceMode::Vault,
        None,
    ));
    let ProvisionState::VerifiedAvailable { entry, skipped } = report else {
        panic!("a stubbed owner must verify: {report:?}");
    };
    assert_eq!(entry.relays, vec!["relay://new".to_string()]);
    assert!(
        skipped.iter().any(|reason| reason.contains("future")),
        "the skip is still surfaced: {skipped:?}"
    );
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
    assert_eq!(
        after["drives"]["future"], future_raw,
        "the unknown entry survives with its value intact"
    );
    assert_eq!(
        after["drives"]["demo"]["relays"],
        serde_json::json!(["relay://new"])
    );
    server.join().unwrap();
}

/// `--no-service` disables and clears: the definition file staged
/// at the override path is gone after the run, and the entry is
/// rewritten. The override keeps the removal hermetic, and the
/// unique service name keeps the manager probes hermetic too — a
/// developer's real `drive-demo` service must never see this
/// test's `list`/disable traffic.
#[test]
fn reconfigure_no_service_removes_the_definition_file() {
    let dir = scratch("rec-clear");
    let registry = dir.join("drives.json");
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let name = format!("rectest-{n}");
    let first = provision(provision_no_service(&dir, &registry, &name));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    // A leftover definition from the managed past the entry
    // remembers, at the path the test override names.
    let definition = dir.join("wyrd-test-definition");
    std::fs::write(&definition, "leftover").unwrap();
    let sock = dir.join("drive").join("owner.sock");
    let server = stub_owner(sock, 1, "Unobserved");
    let report = reconfigure(reconfigure_no_service(
        &dir,
        &registry,
        None,
        ServiceMode::Vault,
        None,
    ));
    let ProvisionState::VerifiedAvailable { entry, .. } = report else {
        panic!("a stubbed owner must verify: {report:?}");
    };
    assert!(!definition.exists(), "the definition is cleared");
    assert!(entry.relays.is_empty());
    server.join().unwrap();
}

/// Invalid relay values refuse before anything stops: the staged
/// definition file survives, and the registry keeps its bytes.
#[test]
fn reconfigure_validates_relays_before_stopping() {
    let dir = scratch("reconf-badrelay");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    let definition = dir.join("wyrd-test-definition");
    std::fs::write(&definition, "leftover").unwrap();
    let before = std::fs::read(&registry).unwrap();
    let report = reconfigure(reconfigure_no_service(
        &dir,
        &registry,
        Some(vec!["not a relay".to_string()]),
        ServiceMode::Vault,
        None,
    ));
    match report {
        ProvisionState::ProvisioningIncomplete {
            kind,
            stage,
            reason,
        } => {
            assert_eq!(kind, ProvisionFailureKind::Validate);
            assert_eq!(Some(stage), kind.stage());
            assert!(reason.contains("relays"), "{reason}");
        }
        other => panic!("a bad relay must refuse: {other:?}"),
    }
    assert!(definition.exists(), "the stop never ran");
    assert_eq!(std::fs::read(&registry).unwrap(), before);
}

/// A live owner holding the store fails the open with the
/// terminal kind: the registry keeps its bytes, and the caller
/// can return the held-drive exit instead of a usage error.
#[test]
fn reconfigure_reports_a_held_drive() {
    let dir = scratch("reconf-held");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    let before = std::fs::read(&registry).unwrap();
    // Hold the store open: a second opener fails the lock, the
    // same contention the sync lock test pins.
    let held = Engine::open_keystore(dir.join("drive"), "bootstrap-test-pass", identity()).unwrap();
    let report = reconfigure(reconfigure_no_service(
        &dir,
        &registry,
        None,
        ServiceMode::Vault,
        None,
    ));
    match report {
        ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
            assert_eq!(kind, ProvisionFailureKind::StoreLocked);
            assert_eq!(Some(stage), kind.stage());
        }
        other => panic!("a held drive must stop at verify-open: {other:?}"),
    }
    assert_eq!(std::fs::read(&registry).unwrap(), before);
    drop(held);
}

/// A socket that appears while waiting retries the connect until
/// the deadline: a freshly restarted owner still binding does not
/// read as unavailable.
#[test]
fn reconfigure_waits_for_a_late_owner_socket() {
    let dir = scratch("reconf-late");
    let registry = dir.join("drives.json");
    let first = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(matches!(
        first,
        ProvisionState::RegisteredButUnavailable { .. }
    ));
    let sock = dir.join("drive").join("owner.sock");
    let server = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        stub_owner(sock, 1, "Unobserved").join().unwrap();
    });
    let report = reconfigure(reconfigure_no_service(
        &dir,
        &registry,
        None,
        ServiceMode::Vault,
        None,
    ));
    let ProvisionState::VerifiedAvailable { .. } = report else {
        panic!("a late owner must still verify: {report:?}");
    };
    server.join().unwrap();
}

/// The install writes the updated entry to the override path:
/// the managed handoff hermetically proven end to install, short
/// of the manager itself. `updated_entry` builds the working copy
/// `reconfigure` installs and rewrites, so asserting the written
/// file carries the new set pins the order through the real
/// serialize-and-write path. Enable/start stay out of hermetic
/// tests — they would touch a real manager.
#[test]
fn install_writes_the_updated_relay_set() {
    let dir = scratch("rec-install");
    let owner = SupervisedOwner {
        supervise_bin: dir.join("wyrd-supervise"),
        wyrd_bin: dir.join("wyrd"),
        credential: ServiceCredential::File {
            identity_file: dir.join("identity"),
            passphrase_file: Some(dir.join("passphrase")),
        },
    };
    let stored = entry("demo", &dir);
    let updated = updated_entry(
        stored,
        vec!["relay://new".to_string()],
        ServiceMode::Vault,
        None,
    );
    assert_eq!(updated.name, "demo");
    assert_eq!(updated.service, "drive-demo");
    assert_eq!(updated.mode, ServiceMode::Vault);
    let definition = dir.join("wyrd-test-definition");
    let written = install_service_definition(&updated, &owner, Some(&definition)).unwrap();
    assert_eq!(written, definition);
    let content = std::fs::read_to_string(&definition).unwrap();
    assert!(
        content.contains("relay://new"),
        "the definition carries the new set"
    );
    assert!(
        !content.contains("relay://example"),
        "no stale relay survives"
    );
}
