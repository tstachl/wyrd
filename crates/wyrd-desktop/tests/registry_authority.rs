//! Registry-write authority: the registration flow
//! (`wyrd_daemon::bootstrap`) and the desktop shell
//! (`wyrd_desktop::registry`) are co-writers under one contract.
//! This test faults both writers at once — interleaved desktop and
//! registration rewrites against one file — and proves they converge
//! without lost entries, carry skipped entries forward verbatim, and
//! never leave a torn registry behind. Production code cannot stage
//! this: the desktop crate must not link the daemon (the layer gate
//! denies the edge), so the meeting happens here, in dev-dependencies
//! both crates already carry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use wyrd_daemon::bootstrap as daemon;
use wyrd_desktop::registry as shell;

fn daemon_entry(name: &str, dir: &Path) -> daemon::RegistryEntry {
    daemon::RegistryEntry {
        name: name.to_string(),
        drive_dir: dir.join(format!("drive-{name}")),
        relays: vec![],
        service: format!("drive-{name}"),
        mode: daemon::ServiceMode::Vault,
    }
}

fn shell_entry(name: &str, dir: &Path) -> shell::RegistryEntryWrite {
    shell::RegistryEntryWrite {
        name: name.to_string(),
        drive_dir: dir.join(format!("drive-{name}")),
        relays: vec![],
        service: format!("drive-{name}"),
        mode: "vault".to_string(),
    }
}

/// Concurrent registration-flow and desktop writes converge: eight
/// writers (four per side, one distinct key each) plus readers race
/// on one registry, and the final file holds every key under both
/// readers. A pre-existing unknown-mode entry rides through both
/// writers verbatim. Readers interleaved with the writers always see
/// a complete, parseable file.
#[test]
fn registration_flow_and_desktop_writes_converge() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-authority-{}-converge",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("drives.json");
    // An entry no current build manages: both writers must carry it
    // forward instead of deleting it.
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "drives": {
                "legacy": {
                    "name": "legacy",
                    "drive_dir": "/drives/legacy",
                    "relays": [],
                    "service": "drive-legacy",
                    "mode": "mount",
                },
            },
        })
        .to_string(),
    )
    .unwrap();

    std::thread::scope(|scope| {
        let path = &path;
        let dir = &dir;
        for i in 0..4 {
            scope.spawn(move || {
                let name = format!("daemon-{i:02}");
                let mut drives = BTreeMap::new();
                drives.insert(name.clone(), daemon_entry(&name, dir));
                daemon::write_registry(path, &drives).unwrap();
            });
            scope.spawn(move || {
                let name = format!("shell-{i:02}");
                let mut drives = BTreeMap::new();
                drives.insert(name.clone(), shell_entry(&name, dir));
                shell::write_registry_at(path, &drives).unwrap();
            });
        }
        for _ in 0..4 {
            scope.spawn(move || {
                for _ in 0..16 {
                    let bytes = std::fs::read(path).unwrap_or_default();
                    if bytes.is_empty() {
                        continue;
                    }
                    let file: serde_json::Value = serde_json::from_slice(&bytes)
                        .expect("every observed registry is complete and parseable");
                    assert_eq!(file.get("version").and_then(|v| v.as_u64()), Some(1));
                }
            });
        }
    });

    // Both readers agree on the same eight live entries.
    let daemon_read = daemon::read_registry(&path).unwrap();
    assert_eq!(
        daemon_read.drives.len(),
        8,
        "{:?}",
        daemon_read.drives.keys()
    );
    for i in 0..4 {
        assert!(daemon_read.drives.contains_key(&format!("daemon-{i:02}")));
        assert!(daemon_read.drives.contains_key(&format!("shell-{i:02}")));
    }
    // The legacy entry survived both writers and still skips loudly.
    assert_eq!(daemon_read.skipped.len(), 1, "{:?}", daemon_read.skipped);
    assert!(
        daemon_read.skipped[0].contains("legacy"),
        "{}",
        daemon_read.skipped[0]
    );

    let shell_state = shell::list_drives_at(&path);
    assert_eq!(
        shell_state.status,
        wyrd_desktop::RegistryStatus::Ready,
        "{:?}",
        shell_state.status
    );
    assert_eq!(shell_state.drives.len(), 8);
    assert_eq!(shell_state.skipped.len(), 1);
    assert!(
        shell_state.skipped[0].contains("legacy"),
        "{}",
        shell_state.skipped[0]
    );

    // The desktop-written entries decode under the daemon's reader
    // with identical facts, and vice versa: one file, two readers,
    // no interpretation gap.
    for drive in &shell_state.drives {
        let entry = &daemon_read.drives[&drive.name];
        assert_eq!(
            entry.drive_dir,
            PathBuf::from(&drive.drive_dir),
            "drive_dir agrees for {:?}",
            drive.name
        );
        assert_eq!(entry.service, drive.service);
    }

    std::fs::remove_dir_all(&dir).unwrap();
}
