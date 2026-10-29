//! Host lifecycle composition tests: the owner path over the public
//! API, with a real drive per test. Fakes (silent mailbox, loopback
//! serving) live here, never in `src/`.

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;

use wyrd_app::{open_host, HostError};
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::Engine;

const PASS: &str = "host-lifecycle-pass";

/// Process-unique scratch directory: parallel tests must never share
/// a LOCK file, and a crashed run's leftovers must never poison init.
fn scratch(label: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("wyrd-app-{label}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn identity() -> DeviceIdentitySecret {
    DeviceIdentitySecret::from_bytes([0x11; 32]).expect("fixture scalar")
}

fn create_drive(dir: PathBuf) {
    Engine::create(dir, PASS, identity()).expect("drive creates");
}

/// The host against a directory a live host holds returns
/// `DriveLocked` at once — no retry loop, no lock-file deletion, no
/// second store handle — and the error names the holding process.
#[test]
fn host_refuses_a_drive_another_process_owns() {
    let dir = scratch("refused");
    create_drive(dir.clone());
    let _held = open_host(dir.clone(), PASS, identity()).expect("first host opens");
    match open_host(dir.clone(), PASS, identity()) {
        Err(HostError::DriveLocked { drive, holder }) => {
            assert_eq!(drive, dir, "the refusal names the drive");
            assert!(
                holder.contains("no posture recorded"),
                "without a sidecar the refusal says so: {holder}"
            );
        }
        _ => panic!("second open must refuse with DriveLocked"),
    }
}

/// With a holder sidecar present, the refusal names the holding
/// process (instance, serving id, recency) — the operator sentence,
/// still without claiming aliveness.
#[test]
fn host_refusal_names_the_holding_process() {
    let dir = scratch("named-holder");
    create_drive(dir.clone());
    let _held = open_host(dir.clone(), PASS, identity()).expect("first host opens");
    let sidecar = dir.join("vault.posture");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let file = serde_json::json!({
        "schema": 1,
        "instance": "0123456789abcdef0123456789abcdef",
        "serving_id": "aa",
        "started_unix": now - 10,
        "ready_unix": now - 5,
        "last_pass_unix": now,
        "last_pass_verdict": "ok",
        "last_pass_class": null,
        "last_pass_consecutive": null,
        "passes": 41,
        "errors": 0,
        "sent": 7,
        "uptime_secs": 312,
        "posture_unix": now,
    });
    std::fs::write(&sidecar, serde_json::to_vec(&file).unwrap()).unwrap();
    match open_host(dir.clone(), PASS, identity()) {
        Err(HostError::DriveLocked { holder, .. }) => {
            assert!(
                holder.contains("0123456789abcdef0123456789abcdef")
                    && holder.contains("liveness unverified"),
                "the refusal names the holder without claiming aliveness: {holder}"
            );
        }
        _ => panic!("second open must refuse with DriveLocked"),
    }
}
