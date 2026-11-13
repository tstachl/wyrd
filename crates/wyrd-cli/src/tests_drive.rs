use super::tests_harness::{write_secret, TempDir};
use super::*;
use wyrd_core::lifecycle_surface::DELETION_LOCALITY;
use wyrd_core::member_surface::{HISTORY_NEVER_REENCRYPTED, REMOVAL_BOUNDARY, SOLE_OWNER_TERMINAL};
use wyrd_format::{Change, ContentId, Entry, FsObjectStore, ObjectKind, ObjectStore, Tree};

struct Fixture {
    _temp: TempDir,
    drive: PathBuf,
    identity_file: PathBuf,
    passphrase_file: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new();
        let identity_file = temp.0.join("identity");
        let passphrase_file = temp.0.join("passphrase");
        let drive = temp.0.join("drive");
        write_secret(&identity_file, [0x22; 32]);
        write_secret(&passphrase_file, b"test-pass\n");
        command(vec![
            "init".into(),
            drive.display().to_string(),
            "--identity-file".into(),
            identity_file.display().to_string(),
            "--passphrase-file".into(),
            passphrase_file.display().to_string(),
        ])
        .unwrap();
        Fixture {
            _temp: temp,
            drive,
            identity_file,
            passphrase_file,
        }
    }

    fn args(&self, action: Vec<String>) -> Vec<String> {
        let mut args = vec![
            "drive".into(),
            self.drive.display().to_string(),
            "--identity-file".into(),
            self.identity_file.display().to_string(),
            "--passphrase-file".into(),
            self.passphrase_file.display().to_string(),
        ];
        args.extend(action);
        args
    }

    fn member_args(&self, action: Vec<String>) -> Vec<String> {
        let mut args = vec![
            "member".into(),
            self.drive.display().to_string(),
            "--identity-file".into(),
            self.identity_file.display().to_string(),
            "--passphrase-file".into(),
            self.passphrase_file.display().to_string(),
        ];
        args.extend(action);
        args
    }

    fn open(&self) -> Engine {
        let identity = read_identity(&self.identity_file).unwrap();
        Engine::open_keystore(self.drive.clone(), "test-pass", identity).unwrap()
    }
}

/// The x-only pubkey a 32-byte secret names.
fn device_id_of(secret: &[u8; 32]) -> DeviceId {
    let keys = nostr::key::SecretKey::from_slice(secret).unwrap();
    DeviceId::from_bytes(nostr::key::Keys::new(keys).public_key().to_bytes())
}

/// Author one file snapshot over a drive, returning its tree.
fn write_file(
    drive: &Path,
    identity_file: &Path,
    passphrase: &str,
    name: &str,
    bytes: &[u8],
) -> ContentId {
    let identity = read_identity(identity_file).unwrap();
    let mut engine = Engine::open_keystore(drive.to_path_buf(), passphrase, identity).unwrap();
    let mut store = FsObjectStore::open(drive.to_path_buf()).unwrap();
    let chunk = store.insert(ObjectKind::Chunk, bytes).unwrap();
    let entry = Entry::file(name, bytes.len() as u64, false, vec![chunk]).unwrap();
    let tree = Tree::from_entries(vec![entry])
        .unwrap()
        .insert_into(&mut store)
        .unwrap();
    engine.author_snapshot(&store, tree).unwrap();
    tree
}

/// A joined second device with its own drive dir: pairing, invite,
/// join — the same real surface a person drives.
struct SecondDevice {
    _temp: TempDir,
    drive: PathBuf,
    identity_file: PathBuf,
    passphrase_file: PathBuf,
    device: DeviceId,
}

fn join_second_device(owner: &Fixture) -> SecondDevice {
    let temp = TempDir::new();
    let drive = temp.0.join("newcomer-drive");
    let identity_file = temp.0.join("newcomer-identity");
    let passphrase_file = temp.0.join("newcomer-passphrase");
    let pairing_file = temp.0.join("pairing");
    let invitation_file = temp.0.join("invitation");
    write_secret(&identity_file, [0x33; 32]);
    write_secret(&passphrase_file, b"newcomer-pass\n");
    let device_args = |action: Vec<String>| {
        let mut args = vec![
            "device".into(),
            drive.display().to_string(),
            "--identity-file".into(),
            identity_file.display().to_string(),
            "--passphrase-file".into(),
            passphrase_file.display().to_string(),
        ];
        args.extend(action);
        args
    };
    command(device_args(vec![
        "pairing-request".into(),
        pairing_file.display().to_string(),
    ]))
    .unwrap();
    let pairing = fs::read_to_string(&pairing_file).unwrap();
    let mut lines = pairing.lines();
    let device = lines
        .next()
        .and_then(|line| line.strip_prefix("device "))
        .expect("pairing names the device")
        .to_owned();
    let key = lines
        .next()
        .and_then(|line| line.strip_prefix("encryption-key "))
        .expect("pairing names the encryption key")
        .to_owned();
    command(owner.member_args(vec![
        "invite".into(),
        device.clone(),
        key,
        invitation_file.display().to_string(),
    ]))
    .unwrap();
    command(device_args(vec![
        "join".into(),
        invitation_file.display().to_string(),
    ]))
    .unwrap();
    SecondDevice {
        _temp: temp,
        drive,
        identity_file,
        passphrase_file,
        device: parse_device_id(&device).unwrap(),
    }
}

fn newcomer_args(newcomer: &SecondDevice, action: Vec<String>) -> Vec<String> {
    let mut args = vec![
        "drive".into(),
        newcomer.drive.display().to_string(),
        "--identity-file".into(),
        newcomer.identity_file.display().to_string(),
        "--passphrase-file".into(),
        newcomer.passphrase_file.display().to_string(),
    ];
    args.extend(action);
    args
}

/// The live head trees a drive serves, sorted: what the drive holds
/// for a person, independent of the epochs the carry stamped on the
/// re-authored snapshots.
fn live_trees(engine: &Engine) -> Vec<ContentId> {
    let mut trees: Vec<ContentId> = engine
        .live_heads()
        .unwrap()
        .iter()
        .map(|head| head.snapshot().tree)
        .collect();
    trees.sort();
    trees
}

/// Deletion is local forget plus this device's own removal: the
/// authored transition holds exactly one change — `Remove(this
/// device)` — and afterwards the local directory is gone.
#[test]
fn drive_deletion_is_local_forget_plus_this_device_removal() {
    let fixture = Fixture::new();
    write_file(
        &fixture.drive,
        &fixture.identity_file,
        "test-pass",
        "kept.txt",
        b"kept",
    );
    let this = device_id_of(&[0x22; 32]);
    let tip_before = fixture
        .open()
        .membership_log()
        .known_state()
        .expect("tip")
        .epoch;

    let identity = read_identity(&fixture.identity_file).unwrap();
    let report = delete_drive(fixture.drive.clone(), "test-pass", identity, true).unwrap();
    assert_eq!(
        report.transition.changes(),
        [Change::Remove(this)],
        "deletion authors only its own removal"
    );
    assert_eq!(
        report.transition.epoch,
        tip_before + 1,
        "deletion commits exactly one new epoch"
    );
    assert!(report.terminal, "owner self-removal empties the owner set");
    assert!(
        !fixture.drive.exists(),
        "after deletion the drive directory is gone"
    );

    // The person-facing path reports the same outcome: a second drive
    // deletes clean through `drive delete --yes` and is gone after.
    let second = Fixture::new();
    command(second.args(vec!["delete".into(), "--yes".into()])).unwrap();
    assert!(
        !second.drive.exists(),
        "the CLI path removes the directory too"
    );
}

/// The negative half: a second peer's head set is byte-identical
/// before and after another device deletes its own drive. Deletion
/// performs no sync — the removal commits into a store that is then
/// removed — so the peer observes no new transition and serves the
/// same heads. The peer is genesis-only (its admission arrives
/// through intake), which is the point, not a gap: whatever the peer
/// holds, deletion must leave alone.
#[test]
fn drive_deletion_leaves_a_peer_head_set_byte_identical() {
    let fixture = Fixture::new();
    let newcomer = join_second_device(&fixture);
    let newcomer_identity = read_identity(&newcomer.identity_file).unwrap();
    let newcomer_engine =
        Engine::open_keystore(newcomer.drive.clone(), "newcomer-pass", newcomer_identity).unwrap();
    let trees_before = live_trees(&newcomer_engine);
    let tip_before = newcomer_engine
        .membership_log()
        .known_state()
        .expect("tip")
        .transition_id;
    drop(newcomer_engine);

    command(fixture.args(vec!["delete".into(), "--yes".into()])).unwrap();
    assert!(!fixture.drive.exists(), "the deleter is gone");

    let newcomer_identity = read_identity(&newcomer.identity_file).unwrap();
    let newcomer_engine =
        Engine::open_keystore(newcomer.drive.clone(), "newcomer-pass", newcomer_identity).unwrap();
    assert_eq!(
        live_trees(&newcomer_engine),
        trees_before,
        "the peer serves byte-identical trees after the deletion"
    );
    assert_eq!(
        newcomer_engine
            .membership_log()
            .known_state()
            .expect("tip")
            .transition_id,
        tip_before,
        "the peer observed no new transition"
    );
}

/// The terminal warning comes before authoring: without `--yes` the
/// surface refuses with the terminal consequence, commits nothing,
/// and leaves the directory alone.
#[test]
fn drive_deletion_warns_before_a_last_owner_removal() {
    let fixture = Fixture::new();
    let error = command(fixture.args(vec!["delete".into()])).unwrap_err();
    assert!(
        matches!(error, CliError::Usage(_)),
        "refusal, not an inline warning: {error:?}"
    );
    assert!(
        error.to_string().contains(SOLE_OWNER_TERMINAL),
        "refusal renders the terminal consequence: {error}"
    );
    let tip = fixture.open().membership_log().known_state().expect("tip");
    assert_eq!(tip.epoch, 1, "the refusal authors nothing");
    assert!(fixture.drive.exists(), "the directory is untouched");

    command(fixture.args(vec!["delete".into(), "--yes".into()])).unwrap();
    assert!(!fixture.drive.exists(), "acknowledged deletion proceeds");
}

/// A non-owner is refused before anything is authored or touched: the
/// refusal names the owner-run command, the shared membership is
/// unchanged, and both directories survive.
#[test]
fn drive_deletion_is_refused_for_a_non_owner() {
    let fixture = Fixture::new();
    let newcomer = join_second_device(&fixture);
    let owner_tip_before = fixture
        .open()
        .membership_log()
        .known_state()
        .expect("tip")
        .transition_id;

    let error = command(newcomer_args(
        &newcomer,
        vec!["delete".into(), "--yes".into()],
    ))
    .unwrap_err();
    assert!(
        matches!(error, CliError::Usage(_)),
        "non-owner refused: {error:?}"
    );
    assert!(
        error.to_string().contains("only an owner can remove"),
        "refusal points at the owner: {error}"
    );
    assert!(
        newcomer.drive.exists(),
        "the refused directory is untouched"
    );
    assert!(fixture.drive.exists(), "the owner is untouched too");
    assert_eq!(
        fixture
            .open()
            .membership_log()
            .known_state()
            .expect("tip")
            .transition_id,
        owner_tip_before,
        "a refused delete authors no membership event"
    );
    // The refused device is still seated owner-side.
    let list = crate::commands::member::member_list_report(&fixture.open()).unwrap();
    assert!(
        list.contains(&format!("member {}", newcomer.device)),
        "the leaver is still seated: {list}"
    );
}

/// The confirmation renders the canonical boundary plus the locality
/// the deletion decision adds, through the same renderer the runner
/// prints — so the wording the person reads is the wording pinned here.
#[test]
fn drive_deletion_confirmation_renders_boundary_and_locality() {
    let fixture = Fixture::new();
    let this = device_id_of(&[0x22; 32]);
    let identity = read_identity(&fixture.identity_file).unwrap();
    let report = delete_drive(fixture.drive.clone(), "test-pass", identity, true).unwrap();
    let confirmation = wyrd_core::lifecycle_surface::render_deletion_confirmation(
        &report.device,
        report.transition.epoch,
        report.carried,
        &fixture._temp.0.join("drive"),
        report.terminal,
    );
    assert_eq!(report.device, this, "deleted self");
    for sentence in [
        REMOVAL_BOUNDARY,
        HISTORY_NEVER_REENCRYPTED,
        DELETION_LOCALITY,
    ] {
        assert!(
            confirmation.contains(sentence),
            "confirmation states {sentence:?}: {confirmation}"
        );
    }
}

/// Registry entry names unique to this process run: the daemon
/// disable path resolves the platform definition for `drive-<name>`
/// when the CLI passes no override, and hermetic CLI tests must
/// never stop or clear a developer's real service of the same name.
fn unique_reconf_name(tag: &str) -> String {
    format!("reconf-{tag}-{}", std::process::id())
}

/// Hand-write one registry entry for the fixture drive: the
/// registration flow itself would touch a real service manager, so
/// hermetic CLI tests stage the file directly — the daemon suite
/// owns the provisioning handoff.
fn write_registry_entry(registry: &Path, name: &str, drive: &Path, relays: &[&str]) {
    let relays: Vec<String> = relays.iter().map(|relay| relay.to_string()).collect();
    let raw = serde_json::json!({
        "version": 1,
        "drives": {
            name: {
                "name": name,
                "drive_dir": drive.to_string_lossy(),
                "relays": relays,
                "service": format!("drive-{name}"),
                "mode": "vault",
            }
        },
    });
    fs::write(registry, serde_json::to_vec(&raw).unwrap()).unwrap();
}

/// A stub owner socket answering every request with the given
/// refusal code: the real framed protocol over a Unix socket, so the
/// command exercises its real client and refusal mapping — only the
/// owner is fake. The accept loop is deadline-bounded so the thread
/// always returns for `join`. Mirrors the daemon suite's stub; this
/// binary cannot import it.
fn stub_owner(
    sock: PathBuf,
    code: &'static str,
    max_requests: usize,
) -> std::thread::JoinHandle<()> {
    if sock.exists() {
        fs::remove_file(&sock).unwrap();
    }
    std::thread::spawn(move || {
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
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
            if std::io::Read::read_exact(&mut stream, &mut len).is_err() {
                continue;
            }
            let len = u32::from_be_bytes(len) as usize;
            let mut body = vec![0u8; len];
            if std::io::Read::read_exact(&mut stream, &mut body).is_err() {
                continue;
            }
            let id: u64 = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value.get("id")?.as_u64())
                .unwrap_or(0);
            let reply = serde_json::json!({
                "v": wyrd_daemon::owner::WIRE_VERSION,
                "id": id,
                "result": {"err": {"code": code, "message": "stub refusal"}},
            });
            let body = serde_json::to_vec(&reply).unwrap();
            let _ = std::io::Write::write_all(&mut stream, &(body.len() as u32).to_be_bytes());
            let _ = std::io::Write::write_all(&mut stream, &body);
        }
    })
}

fn registry_relays(registry: &Path, name: &str) -> Vec<String> {
    let raw: serde_json::Value = serde_json::from_slice(&fs::read(registry).unwrap()).unwrap();
    raw["drives"][name]["relays"]
        .as_array()
        .unwrap()
        .iter()
        .map(|relay| relay.as_str().unwrap().to_string())
        .collect()
}

/// Reconfigure replaces the relay set through the CLI surface: the
/// entry is rewritten and the run reports reconfigured-and-verified
/// (a stub owner answers the pre-first-pass refusal this session).
#[test]
fn drive_reconfigure_replaces_relays_and_verifies() {
    let fixture = Fixture::new();
    let name = unique_reconf_name("replace");
    let registry = fixture._temp.0.join("drives.json");
    write_registry_entry(&registry, &name, &fixture.drive, &["relay://old"]);
    // The stub answers the pre-first-pass refusal, so verification
    // is fast and deterministic — no socket wait, no real manager.
    let sock = fixture.drive.join("owner.sock");
    let server = stub_owner(sock, "Unobserved", 1);
    command(fixture.args(vec![
        "reconfigure".into(),
        "--mode".into(),
        "vault".into(),
        "--relay".into(),
        "relay://new".into(),
        "--registry".into(),
        registry.display().to_string(),
        "--no-service".into(),
    ]))
    .unwrap();
    server.join().unwrap();
    assert_eq!(
        registry_relays(&registry, &name),
        vec!["relay://new".to_string()],
        "the entry carries the new set"
    );
}

/// A terminal owner refusal still verifies nothing: the entry is
/// rewritten and the run reports reconfigured-but-unavailable with
/// the reason, fast and without a real manager.
#[test]
fn drive_reconfigure_reports_unavailable_on_owner_refusal() {
    let fixture = Fixture::new();
    let name = unique_reconf_name("refused");
    let registry = fixture._temp.0.join("drives.json");
    write_registry_entry(&registry, &name, &fixture.drive, &["relay://old"]);
    let sock = fixture.drive.join("owner.sock");
    let server = stub_owner(sock, "Store", 1);
    let error = command(fixture.args(vec![
        "reconfigure".into(),
        "--mode".into(),
        "vault".into(),
        "--relay".into(),
        "relay://new".into(),
        "--registry".into(),
        registry.display().to_string(),
        "--no-service".into(),
    ]))
    .unwrap_err();
    server.join().unwrap();
    assert!(
        matches!(error, CliError::Usage(_)),
        "a refusing owner verifies nothing: {error:?}"
    );
    assert!(
        error.to_string().contains("reconfigured but unavailable"),
        "run reports the stage: {error}"
    );
    assert_eq!(
        registry_relays(&registry, &name),
        vec!["relay://new".to_string()],
        "the entry carries the new set"
    );
}

/// No `--relay` flags keep the registered set: a
/// credential-path-only reconfigure never clears relays by accident.
#[test]
fn drive_reconfigure_keeps_relays_without_relay_flags() {
    let fixture = Fixture::new();
    let name = unique_reconf_name("keep");
    let registry = fixture._temp.0.join("drives.json");
    write_registry_entry(&registry, &name, &fixture.drive, &["relay://kept"]);
    let sock = fixture.drive.join("owner.sock");
    let server = stub_owner(sock, "Unobserved", 1);
    command(fixture.args(vec![
        "reconfigure".into(),
        "--mode".into(),
        "vault".into(),
        "--registry".into(),
        registry.display().to_string(),
        "--no-service".into(),
    ]))
    .unwrap();
    server.join().unwrap();
    assert_eq!(
        registry_relays(&registry, &name),
        vec!["relay://kept".to_string()],
        "the registered set survives"
    );
}

/// A mount target refuses before anything stops: the refusal names
/// the installer follow-up and the registry keeps its bytes exactly.
#[test]
fn drive_reconfigure_refuses_non_vault_modes_before_stopping() {
    let fixture = Fixture::new();
    let name = unique_reconf_name("mount");
    let registry = fixture._temp.0.join("drives.json");
    write_registry_entry(&registry, &name, &fixture.drive, &["relay://old"]);
    let before = fs::read(&registry).unwrap();
    let error = command(fixture.args(vec![
        "reconfigure".into(),
        "--mode".into(),
        "mount".into(),
        "--registry".into(),
        registry.display().to_string(),
        "--no-service".into(),
    ]))
    .unwrap_err();
    assert!(
        matches!(error, CliError::Usage(_)),
        "refusal, not an inline warning: {error:?}"
    );
    assert!(
        error.to_string().contains("installer follow-up"),
        "refusal points at the follow-up: {error}"
    );
    assert_eq!(fs::read(&registry).unwrap(), before);
}

/// An unregistered directory refuses without writing: the drive
/// opens (the keystore is real) but no entry names it.
#[test]
fn drive_reconfigure_refuses_an_unregistered_directory() {
    let fixture = Fixture::new();
    let registry = fixture._temp.0.join("absent.json");
    let error = command(fixture.args(vec![
        "reconfigure".into(),
        "--mode".into(),
        "vault".into(),
        "--relay".into(),
        "relay://new".into(),
        "--registry".into(),
        registry.display().to_string(),
        "--no-service".into(),
    ]))
    .unwrap_err();
    assert!(
        matches!(error, CliError::Usage(_)),
        "refusal names the missing registration: {error:?}"
    );
    assert!(
        error.to_string().contains("no registry entry"),
        "refusal says what to do: {error}"
    );
    assert!(!registry.exists(), "a refusal writes nothing");
}

/// A live owner holding the store fails closed with the terminal
/// held-drive exit (78, never retried by the supervisor): the
/// registry keeps its bytes exactly, like every other offline
/// writer's lock refusal.
#[test]
fn drive_reconfigure_refuses_a_held_drive_with_78() {
    let fixture = Fixture::new();
    let name = unique_reconf_name("held");
    let registry = fixture._temp.0.join("drives.json");
    write_registry_entry(&registry, &name, &fixture.drive, &["relay://old"]);
    let before = fs::read(&registry).unwrap();
    // Hold the store open: a second opener fails the lock, the same
    // contention the sync lock test pins.
    let _held = fixture.open();
    let error = command(fixture.args(vec![
        "reconfigure".into(),
        "--mode".into(),
        "vault".into(),
        "--registry".into(),
        registry.display().to_string(),
        "--no-service".into(),
    ]))
    .unwrap_err();
    assert_eq!(
        error.exit_code(),
        78,
        "a held drive exits held-drive: {error:?}"
    );
    assert_eq!(fs::read(&registry).unwrap(), before);
}
