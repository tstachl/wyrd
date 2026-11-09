//! The bootstrap handoff against the real binary: initialize,
//! register, withhold ready without a verifying socket. The owner
//! itself is out of scope here (the daemon suite covers the socket
//! half against a stub speaking the framed protocol); this file
//! proves the command's observable contract — states reported,
//! registry written, no silent ready.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::AtomicU64;

fn wyrd() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_wyrd"))
}

struct Fixture {
    dir: PathBuf,
    drive: PathBuf,
    identity: PathBuf,
    passphrase: PathBuf,
    registry: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("wyrd-boot-{case}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let drive = dir.join("drive");
        let identity = dir.join("identity");
        let passphrase = dir.join("passphrase");
        let registry = dir.join("drives.json");
        std::fs::write(&identity, [0x22; 32]).unwrap();
        std::fs::write(&passphrase, b"bootstrap-test-pass\n").unwrap();
        for path in [&identity, &passphrase] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        Fixture {
            dir,
            drive,
            identity,
            passphrase,
            registry,
        }
    }

    fn bootstrap(&self, extra: &[&str]) -> Output {
        let mut argv = vec![
            "bootstrap".to_string(),
            "--name".to_string(),
            "demo".to_string(),
            self.drive.to_str().unwrap().to_string(),
            "--identity-file".to_string(),
            self.identity.to_str().unwrap().to_string(),
            "--passphrase-file".to_string(),
            self.passphrase.to_str().unwrap().to_string(),
            "--registry".to_string(),
            self.registry.to_str().unwrap().to_string(),
            "--no-service".to_string(),
        ];
        argv.extend(extra.iter().map(|s| s.to_string()));
        Command::new(wyrd())
            .args(&argv)
            .output()
            .expect("wyrd bootstrap runs")
    }

    fn registry_entries(&self) -> serde_json::Value {
        let bytes = std::fs::read(&self.registry).expect("registry writes");
        serde_json::from_slice(&bytes).expect("registry is JSON")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Without an owner behind the socket, bootstrap initializes and
/// registers but withholds ready: the registry exists, the state
/// says so explicitly, and the exit status is failure.
#[test]
fn bootstrap_without_an_owner_registers_but_reports_unavailable() {
    let fixture = Fixture::new("no-owner");
    let output = fixture.bootstrap(&[]);
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        !output.status.success(),
        "no verifying socket must never report ready"
    );
    assert!(
        stderr.contains("registered but unavailable"),
        "the state names its stage, got:\n{stderr}"
    );
    let registry = fixture.registry_entries();
    assert_eq!(registry["version"], 1);
    assert_eq!(
        registry["drives"]["demo"]["drive_dir"],
        fixture.drive.to_str().unwrap()
    );
    // A registry entry is a claim, never proof: no socket, no ready.
    assert!(
        !fixture.drive.join("owner.sock").exists(),
        "nothing serves this drive"
    );
}

/// A second run reconciles the existing drive instead of failing on
/// it: create refuses, the flow re-verifies the open and rewrites
/// the same entry.
#[test]
fn bootstrap_rerun_reconciles_instead_of_failing() {
    let fixture = Fixture::new("rerun");
    let first = fixture.bootstrap(&[]);
    assert!(!first.status.success());
    let second = fixture.bootstrap(&[]);
    let stderr = String::from_utf8(second.stderr).expect("stderr is UTF-8");
    assert!(!second.status.success(), "still no owner, still not ready");
    assert!(
        stderr.contains("registered but unavailable"),
        "re-run reconciles to the same state, got:\n{stderr}"
    );
}

/// Validation precedes initialization: a bad name touches nothing.
#[test]
fn bootstrap_rejects_bad_names_before_touching_anything() {
    let fixture = Fixture::new("bad-name");
    let output = Command::new(wyrd())
        .args([
            "bootstrap",
            "--name",
            "BAD NAME!",
            fixture.drive.to_str().unwrap(),
            "--identity-file",
            fixture.identity.to_str().unwrap(),
            "--passphrase-file",
            fixture.passphrase.to_str().unwrap(),
            "--registry",
            fixture.registry.to_str().unwrap(),
            "--no-service",
        ])
        .output()
        .expect("wyrd bootstrap runs");
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(!output.status.success());
    assert!(
        stderr.contains("validate"),
        "the failure names its stage, got:\n{stderr}"
    );
    assert!(
        !fixture.drive.exists(),
        "validation precedes initialization"
    );
    assert!(
        !fixture.registry.exists(),
        "validation precedes registration"
    );
}

/// The registry path in the spec contract resolves per platform;
/// the flag overrides it. The override path is what the installer
/// would never use — but tests must not touch a real home.
#[test]
fn bootstrap_registry_flag_overrides_the_default() {
    let fixture = Fixture::new("registry-flag");
    let elsewhere = fixture.dir.join("elsewhere.json");
    let output = Command::new(wyrd())
        .args([
            "bootstrap",
            "--name",
            "demo",
            fixture.drive.to_str().unwrap(),
            "--identity-file",
            fixture.identity.to_str().unwrap(),
            "--passphrase-file",
            fixture.passphrase.to_str().unwrap(),
            "--registry",
            elsewhere.to_str().unwrap(),
            "--no-service",
        ])
        .output()
        .expect("wyrd bootstrap runs");
    assert!(!output.status.success());
    assert!(elsewhere.exists(), "the override path is honored");
    assert!(
        !fixture.registry.exists(),
        "nothing leaks to the unpassed location"
    );
}
