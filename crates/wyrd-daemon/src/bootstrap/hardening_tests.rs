use super::tests::{entry, provision_no_service, scratch, stub_owner};
use super::*;

#[test]
fn registry_skips_unknown_modes_without_failing_the_file() {
    let dir = scratch("mode");
    let path = dir.join("drives.json");
    let raw = serde_json::json!({
        "version": 1,
        "drives": {
            "future": {
                "name": "future",
                "drive_dir": dir.join("drive").to_string_lossy(),
                "relays": [],
                "service": "drive-future",
                "mode": "mount"
            }
        }
    });
    std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
    let read = read_registry(&path).unwrap();
    assert!(read.drives.is_empty());
    assert_eq!(read.skipped.len(), 1, "{:?}", read.skipped);
    assert!(read.skipped[0].contains("mount"), "{:?}", read.skipped);
}

#[test]
fn registry_file_is_owner_only() {
    let dir = scratch("perms");
    let path = dir.join("drives.json");
    let mut drives = BTreeMap::new();
    drives.insert("demo".to_string(), entry("demo", &dir));
    write_registry(&path, &drives).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "registry is owner-only, got {mode:o}");
    }
}

#[test]
fn service_files_write_atomically_and_owner_only() {
    let dir = scratch("svcfile");
    let path = dir.join("wyrd-drive-demo.service");
    let owner = SupervisedOwner {
        supervise_bin: PathBuf::from("/opt/wyrd/bin/wyrd-supervise"),
        wyrd_bin: PathBuf::from("/opt/wyrd/bin/wyrd"),
        credential: ServiceCredential::File {
            identity_file: dir.join("identity"),
            passphrase_file: Some(dir.join("passphrase")),
        },
    };
    let owned = entry("demo", &dir);
    write_service_file(
        &path,
        &render_systemd_unit("Wyrd drive demo", &owner, &owned),
    )
    .unwrap();
    assert!(path.exists());
    assert!(
        !dir.join(format!("wyrd-drive-demo.tmp.{}", std::process::id()))
            .exists(),
        "no temporary sibling remains"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "service definition is owner-only, got {mode:o}"
        );
    }
}

#[test]
fn templates_escape_operator_input() {
    let dir = scratch("escape");
    let mut owned = entry("demo", &dir);
    owned.relays = vec!["relay://exa mple".to_string(), "relay://%i".to_string()];
    let owner = SupervisedOwner {
        supervise_bin: PathBuf::from("/opt/wyrd/bin/with space/wyrd-supervise"),
        wyrd_bin: PathBuf::from("/opt/wyrd/bin/wyrd"),
        credential: ServiceCredential::File {
            identity_file: dir.join("identity"),
            passphrase_file: Some(dir.join("passphrase")),
        },
    };
    let plist = render_launchd_plist("md.enesis.wyrd.drive-demo", &owner, &owned);
    assert!(plist.contains("with space"), "paths survive escaped");
    let unit = render_systemd_unit("Wyrd drive demo", &owner, &owned);
    assert!(
        unit.contains("\"/opt/wyrd/bin/with space/wyrd-supervise\""),
        "spaces are quoted, got:\n{unit}"
    );
    assert!(
        unit.contains("\"relay://exa mple\""),
        "relay spaces are quoted, got:\n{unit}"
    );
    assert!(
        unit.contains("\"relay://%%i\""),
        "relay specifiers are doubled, got:\n{unit}"
    );
}

#[test]
fn only_ok_and_unobserved_verify() {
    let dir = scratch("whitelist");
    let registry = dir.join("drives.json");
    let sock = dir.join("drive").join("owner.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let server = stub_owner(sock, 1, "Malformed");
    let report = provision(provision_no_service(&dir, &registry, "demo"));
    assert!(
        matches!(report, ProvisionState::RegisteredButUnavailable { .. }),
        "an unexpected refusal shape must not verify"
    );
    server.join().unwrap();
}

#[test]
fn managing_a_service_needs_a_relay() {
    let dir = scratch("norelay");
    let registry = dir.join("drives.json");
    let mut config = provision_no_service(&dir, &registry, "demo");
    config.manage_service = true;
    config.relays = vec![];
    let report = provision(config);
    match report {
        ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
            assert_eq!(kind, ProvisionFailureKind::Validate);
            assert_eq!(Some(stage), kind.stage());
        }
        other => panic!("a relay-less managed service is refused, not installed: {other:?}"),
    }
    assert!(
        !registry.exists(),
        "refusal precedes registration side effects"
    );
}
