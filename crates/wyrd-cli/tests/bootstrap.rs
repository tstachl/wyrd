//! The bootstrap handoff against the real binary: initialize,
//! register, withhold ready without a verifying socket. The owner
//! itself is out of scope here (the daemon suite covers the socket
//! half against a stub speaking the framed protocol); this file
//! proves the command's observable contract — states reported,
//! registry written, no silent ready.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

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

/// A real relay beside the binary under test: rust-nostr's
/// in-process `LocalRelay` on its own runtime, which this struct
/// owns so the listener outlives every `block_on` (the sync-config
/// pattern).
struct TestRelay {
    _runtime: tokio::runtime::Runtime,
    relay: nostr_sdk::local_relay::LocalRelay,
    url: String,
}

impl TestRelay {
    fn spawn() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let relay = nostr_sdk::local_relay::LocalRelay::new();
        runtime.block_on(relay.run()).expect("test relay serves");
        let url = runtime.block_on(relay.url()).to_string();
        TestRelay {
            _runtime: runtime,
            relay,
            url,
        }
    }
}

/// The full handoff through the built binaries: bootstrap
/// registers, a separately spawned `wyrd vault` owns and serves,
/// and a non-owner `OwnerClient` gets a terminal answer out of the
/// socket the service path would bind. The OS service manager is
/// deliberately out of scope here (no launchd/systemd in CI);
/// enablement stays the documented manual step.
#[test]
fn bootstrap_then_vault_serves_a_non_owner_client() {
    let fixture = Fixture::new("full-loop");
    let output = fixture.bootstrap(&[]);
    assert!(
        !output.status.success(),
        "no owner runs yet, so ready is withheld"
    );

    let relay = TestRelay::spawn();
    let mut child = Command::new(wyrd())
        .args([
            "vault",
            fixture.drive.to_str().unwrap(),
            "--relay",
            &relay.url,
            "--identity-file",
            fixture.identity.to_str().unwrap(),
            "--passphrase-file",
            fixture.passphrase.to_str().unwrap(),
        ])
        .stderr(Stdio::piped())
        .spawn()
        .expect("wyrd vault spawns");
    let deadline = Instant::now() + Duration::from_secs(90);
    let stderr = child.stderr.take().expect("vault stderr piped");
    let mut ready = false;
    for line in BufReader::new(stderr).lines() {
        let line = line.expect("vault stderr reads");
        if line.contains("vault ready: serving ") {
            ready = true;
            break;
        }
        if Instant::now() > deadline {
            break;
        }
    }
    assert!(ready, "the vault binds the owner socket");
    // Separate the reader from the child: the client below must
    // prove a non-owner process is served, not the test harness.
    drop(child.stderr.take());

    let client = wyrd_daemon::owner::client::OwnerClient::connect_drive(&fixture.drive)
        .expect("the socket accepts a non-owner connection");
    let client = client
        .with_timeout(Duration::from_secs(10))
        .expect("timeout arms");
    // Rebind mutably: with_timeout consumes and returns the client.
    let mut client = client;
    let status = client.observe().expect("observe answers terminally");
    assert!(status.tip.is_some(), "the answer carries durable status");

    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let exit = child.wait().expect("vault reaps");
    assert!(exit.success(), "SIGTERM shuts the owner down clean");
    relay.relay.shutdown();
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
