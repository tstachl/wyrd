//! Backend IPC behavioral proofs: the mapping preserves the
//! interface taxonomy end to end, against the real client types on a
//! real owner — a composed view served over a real socket, driven
//! through [`DesktopBackend`]. No mocked owner behavior anywhere:
//! every answer below is produced by the daemon's server over a live
//! loop.
//!
//! What is pinned here (the scaffold's verification list):
//!
//! - every refusal retains its original code through the IPC round
//!   trip;
//! - `Unavailable` and `Unconfirmed` remain distinct at the frontend;
//! - reconnection never replays a mutation;
//! - an unconfirmed mutation never becomes a success through
//!   frontend mapping;
//! - the UI cannot invoke any mutation path except `submit` against
//!   the owner's queue (structural: `submit` is the backend's only
//!   authoring method, and the contract gate pins the crate's
//!   dependency closure).

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use wyrd_core::live::{LiveConfig, LiveParts};
use wyrd_daemon::owner::{OwnerHandles, OwnerServer};
use wyrd_daemon::vault::OwnerView;
use wyrd_desktop::{BackendError, DesktopBackend};
use wyrd_owner_client::{socket_path_for, WireErrorCode, WireMutationKind, WireMutationOutcome};
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::Engine;

/// A mailbox that never delivers and always accepts sends: intake
/// stays empty, the pass still publishes. Same shape as the daemon
/// harness fake, local because that harness is module-private.
struct NoopMailbox;

impl wyrd_sync::transport::mailbox::Mailbox for NoopMailbox {
    fn send(
        &mut self,
        _envelope: wyrd_sync::transport::mailbox::MailboxEnvelope,
    ) -> Result<
        wyrd_sync::transport::mailbox::SendReport,
        wyrd_sync::transport::mailbox::MailboxError,
    > {
        Ok(wyrd_sync::transport::mailbox::SendReport { accepted: 1 })
    }

    fn recv(
        &mut self,
    ) -> Result<
        Option<wyrd_sync::transport::mailbox::Delivery>,
        wyrd_sync::transport::mailbox::MailboxError,
    > {
        Ok(None)
    }

    fn settle(
        &mut self,
        _id: wyrd_sync::transport::mailbox::DeliveryId,
        _disposition: wyrd_sync::transport::mailbox::Disposition,
    ) -> Result<(), wyrd_sync::transport::mailbox::MailboxError> {
        Ok(())
    }
}

static SCRATCH_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A scratch single-device drive with seeded content, split for
/// serving with the loop running: the live baseline every proof
/// drives through the backend.
fn scratch_drive() -> (Engine, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-ipc-{}-{}",
        std::process::id(),
        SCRATCH_N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let engine = Engine::create(
        dir.clone(),
        "ipc-proof-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    (engine, dir)
}

/// A running owner: the composed parts, the loop on a thread, and
/// the server bound on the drive socket on a thread. The caller owns
/// teardown: trip both stops, join both threads, remove the
/// directory. Two stops, not one: killing the socket server must not
/// stop the loop (the Unconfirmed proof needs the owner alive but
/// unreachable), and stopping the loop must not close the socket.
struct RunningOwner {
    parts: LiveParts<OwnerView>,
    dir: PathBuf,
    loop_stop: &'static AtomicBool,
    serve_stop: &'static AtomicBool,
    loop_thread: Option<
        std::thread::JoinHandle<Result<wyrd_core::live::LiveSummary, wyrd_core::live::LiveError>>,
    >,
    serve_thread: Option<std::thread::JoinHandle<()>>,
}

fn running_owner() -> RunningOwner {
    let (engine, dir) = scratch_drive();
    let store = wyrd_format::FsObjectStore::open(dir.clone()).unwrap();
    let mut daemon = wyrd_core::node::WyrdNode::new(engine, store).unwrap();
    daemon.put_file("seed.txt", b"seed").unwrap();
    daemon.refresh_live_heads().unwrap();
    let (mut live, parts) = daemon
        .into_live(Duration::from_secs(5), &LiveConfig::default())
        .unwrap();
    let loop_stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let loop_thread = std::thread::spawn(move || {
        let mut mailbox = NoopMailbox;
        live.run_loop(
            &mut mailbox,
            None::<&mut wyrd_sync::bulk::MemoryBulkSource>,
            loop_stop,
            &LiveConfig::default(),
            &mut |_, _| {},
        )
    });
    // Wait for the first pass to publish: reads serve from the
    // published generation, and the observation proof needs it.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if parts
            .observation
            .read()
            .map(|slot| slot.is_some())
            .unwrap_or(false)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the loop published no observation"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    let socket_path = socket_path_for(&dir);
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let serve_stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let serve_thread = std::thread::spawn(move || server.serve_on(&listener, serve_stop));
    RunningOwner {
        parts,
        dir,
        loop_stop,
        serve_stop,
        loop_thread: Some(loop_thread),
        serve_thread: Some(serve_thread),
    }
}

impl RunningOwner {
    /// Stop the socket server and join it. The loop keeps running:
    /// the owner is alive but unreachable.
    fn stop_serving(&mut self) {
        self.serve_stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.serve_thread.take() {
            thread.join().unwrap();
        }
        let _ = std::fs::remove_file(socket_path_for(&self.dir));
    }

    /// Serve the current handles again on the drive socket (a new
    /// server instance, as after a restart: parent tokens from the
    /// previous instance refuse stale).
    fn serve_again(&mut self) {
        let server = OwnerServer::new(OwnerHandles::from_parts(&self.parts));
        let socket_path = socket_path_for(&self.dir);
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = self.serve_stop;
        stop.store(false, Ordering::Relaxed);
        self.serve_thread = Some(std::thread::spawn(move || server.serve_on(&listener, stop)));
    }

    fn shutdown(mut self) {
        self.serve_stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.serve_thread.take() {
            thread.join().unwrap();
        }
        self.loop_stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.loop_thread.take() {
            let summary = thread.join().unwrap();
            assert!(summary.is_ok(), "the loop ends clean on stop");
        }
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

fn backend_for(owner: &RunningOwner) -> DesktopBackend {
    DesktopBackend::connect(&owner.dir).expect("the backend connects to the serving owner")
}

/// Every refusal retains its original code through the IPC round
/// trip: a missing path refuses `NotFound` on reads and on submits,
/// each with its own code intact, never collapsed into one.
#[test]
fn refusals_keep_their_codes_through_the_backend() {
    let owner = running_owner();
    let backend = backend_for(&owner);

    match backend.lookup("no-such-path.txt") {
        Err(BackendError::Refused { error }) => assert_eq!(error.code, WireErrorCode::NotFound),
        other => panic!("a missing path refuses NotFound: {other:?}"),
    }
    match backend.submit(WireMutationKind::Unlink {
        path: "no-such-path.txt".to_string(),
    }) {
        Err(BackendError::Refused { error }) => assert_eq!(error.code, WireErrorCode::NotFound),
        other => panic!("an unlink of a missing path refuses NotFound: {other:?}"),
    }
    // The refusal names the cause: the code is data, not a boolean.
    match backend.lookup("no-such-path.txt") {
        Err(BackendError::Refused { error }) => assert!(
            !error.message.is_empty(),
            "a refusal carries its message with its code"
        ),
        other => panic!("a missing path refuses: {other:?}"),
    }

    owner.shutdown();
}

/// `Unavailable` and `Unconfirmed` remain distinct at the frontend: a
/// drive with no owner to connect to reports `Unavailable`, while an
/// owner that dies mid-connection reports `Unconfirmed` — never one
/// recast as the other.
#[test]
fn unavailable_and_unconfirmed_stay_distinct() {
    // No owner at all: connecting reports Unavailable, which says
    // nothing about any drive.
    let absent = std::env::temp_dir().join(format!(
        "wyrd-desktop-ipc-absent-{}-{}",
        std::process::id(),
        SCRATCH_N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&absent).unwrap();
    match DesktopBackend::connect(&absent) {
        Err(BackendError::Unavailable { .. }) => {}
        other => panic!("no owner means Unavailable: {other:?}"),
    }
    std::fs::remove_dir_all(&absent).unwrap();

    // A live owner that goes away mid-connection: the established
    // connection breaks, and the outcome is Unconfirmed — unknown,
    // never recast as failure or as absence.
    let mut owner = running_owner();
    let backend = backend_for(&owner);
    backend
        .lookup("seed.txt")
        .expect("the seed serves while the owner runs");
    owner.stop_serving();
    match backend.lookup("seed.txt") {
        Err(BackendError::Unconfirmed { op, .. }) => assert_eq!(op, "lookup"),
        other => panic!("a dead owner means Unconfirmed, never Unavailable: {other:?}"),
    }

    owner.shutdown();
}

/// Reconnection never replays a mutation and recovers the drive: a
/// poisoned instance sends nothing further (even a valid request
/// stays `Unconfirmed` after the server restarts — had it touched
/// the socket it would have succeeded), and the mint-new reconnect
/// serves again. A fresh submit after reconnecting commits exactly
/// once.
#[test]
fn reconnect_never_replays_and_recovers() {
    let mut owner = running_owner();
    let backend = backend_for(&owner);
    backend
        .lookup("seed.txt")
        .expect("the seed serves while the owner runs");
    owner.stop_serving();
    match backend.lookup("seed.txt") {
        Err(BackendError::Unconfirmed { .. }) => {}
        other => panic!("the break poisons the instance: {other:?}"),
    }

    // The server is back, but the poisoned instance still sends
    // nothing: a valid request stays Unconfirmed instead of
    // succeeding, which proves no new bytes touched the socket and
    // no mutation could have replayed through the old instance.
    owner.serve_again();
    match backend.lookup("seed.txt") {
        Err(BackendError::Unconfirmed { .. }) => {}
        other => panic!("a poisoned instance sends nothing, even to a live server: {other:?}"),
    }

    backend
        .reconnect()
        .expect("reconnect mints a new connection");
    backend
        .lookup("seed.txt")
        .expect("the new connection serves again");
    let outcome = backend
        .submit(WireMutationKind::Mkdir {
            path: "reconnected-dir".to_string(),
        })
        .expect("a fresh submit commits after reconnect");
    assert_eq!(outcome, WireMutationOutcome::Done);
    match backend.lookup("reconnected-dir") {
        Ok(_) => {}
        other => panic!("the reconnected submit committed once: {other:?}"),
    }

    owner.shutdown();
}

/// An unconfirmed mutation never becomes a success through frontend
/// mapping: on a poisoned instance the reconciling re-read stays
/// `Unconfirmed` — never `Ok(None)` absence, never a verdict — while
/// a healthy instance reconciles to definite answers.
#[test]
fn unconfirmed_never_becomes_success_through_mapping() {
    let mut owner = running_owner();
    let backend = backend_for(&owner);

    // Healthy controls: absence is data, presence is a verdict.
    match backend.reconcile_lookup("never-created.txt") {
        Ok(None) => {}
        other => panic!("a missing path reconciles to absence: {other:?}"),
    }
    match backend.reconcile_lookup("seed.txt") {
        Ok(Some(_)) => {}
        other => panic!("a served path reconciles to presence: {other:?}"),
    }

    // Poison the instance, then reconcile: the re-read cannot reach
    // the owner, so it stays Unconfirmed — it must never resolve the
    // original request into absence or presence.
    owner.stop_serving();
    match backend.lookup("seed.txt") {
        Err(BackendError::Unconfirmed { .. }) => {}
        other => panic!("the break poisons the instance: {other:?}"),
    }
    match backend.reconcile_lookup("never-created.txt") {
        Err(BackendError::Unconfirmed { op, .. }) => assert_eq!(op, "lookup"),
        other => panic!("a failed re-read never resolves the request: {other:?}"),
    }

    owner.shutdown();
}

/// `submit` is the backend's only authoring method, and it commits
/// through the owner's single queue: capture-then-create reports the
/// created identity, and the lookup serves the same node. The
/// structural half of the single-mutation-path invariant is the
/// crate's dependency closure (no `wyrd-core`, no `wyrd-sync`, no
/// second queue reachable); this is its behavioral half.
#[test]
fn submit_forwards_through_the_single_queue_and_commits() {
    let owner = running_owner();
    let backend = backend_for(&owner);

    let token = backend
        .capture_parent("")
        .expect("the parent captures against the drive root");
    let outcome = backend
        .submit(WireMutationKind::CreateFile {
            path: "from-ui.txt".to_string(),
            parent: token,
        })
        .expect("create submits through the single queue");
    let created = match outcome {
        WireMutationOutcome::Created(identity) => identity,
        other => panic!("create reports its identity: {other:?}"),
    };
    match backend.lookup("from-ui.txt") {
        Ok((node, _, _)) => match node {
            wyrd_owner_client::WireNode::File { size, .. } => {
                assert_eq!(size, 0, "the created file starts empty")
            }
            other => panic!("the created path serves a file: {other:?}"),
        },
        other => panic!("the created path serves: {other:?}"),
    };
    // The round trip preserves content: what was committed reads back.
    let content_hex: String = b"ui bytes".iter().map(|b| format!("{b:02x}")).collect();
    backend
        .submit(WireMutationKind::CommitFile {
            path: "from-ui.txt".to_string(),
            base: created,
            executable: false,
            content_hex,
        })
        .expect("commit submits through the single queue");
    match backend.read("from-ui.txt") {
        Ok((bytes, _, _)) => assert_eq!(bytes, b"ui bytes"),
        other => panic!("the committed content reads back: {other:?}"),
    }

    owner.shutdown();
}

/// Command-layer proofs: the same taxonomy, driven through the
/// registered Tauri commands on the mock runtime — no event loop,
/// no real webview — against the same real owner. The mock stands
/// in for the transport only: every answer below is produced by the
/// daemon's server over a live loop, and the invoked command names
/// and argument shapes are the registered ones verbatim, so a
/// name/argument drift between the handler and the frontend fails
/// here.
///
/// Sandbox note: building the mock webview resolves a data
/// directory, and on Linux an unset one falls back to the app
/// local data dir plus `create_dir_all` — which the CI sandbox
/// denies. Each proof sets an explicit per-test data directory
/// under the temp dir (already writable: the owner sockets live
/// there), skipping the home-directory resolution entirely.
///
/// One mock app managing the backend, plus its webview: the pair
/// every command proof invokes through. The app never runs — the
/// mock runtime answers invokes without an event loop.
fn mock_app_with_state(
    state: wyrd_desktop::AppState,
    tag: &str,
) -> (
    tauri::App<tauri::test::MockRuntime>,
    tauri::WebviewWindow<tauri::test::MockRuntime>,
    std::path::PathBuf,
) {
    let app = tauri::test::mock_builder()
        .manage(state)
        .manage(wyrd_desktop::FlightTracker::new())
        .invoke_handler(wyrd_desktop::invoke_handler())
        .build(tauri::generate_context!())
        .expect("the mock app builds");
    let data_dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-webview-{}-{}",
        std::process::id(),
        tag
    ));
    std::fs::create_dir_all(&data_dir).expect("the webview data dir builds");
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .data_directory(data_dir.clone())
        .build()
        .expect("the mock webview builds");
    (app, webview, data_dir)
}

/// One mock app managing a pre-connected backend: the pair every
/// single-drive command proof invokes through.
fn mock_app_with(
    backend: DesktopBackend,
    tag: &str,
) -> (
    tauri::App<tauri::test::MockRuntime>,
    tauri::WebviewWindow<tauri::test::MockRuntime>,
    std::path::PathBuf,
) {
    let state = wyrd_desktop::AppState::new();
    state
        .insert(backend)
        .expect("test backends connect to valid scratch drives");
    mock_app_with_state(state, tag)
}

/// Invoke one command on the mock webview: the JSON the command
/// layer answers with, success or taxonomy error alike.
fn invoke(
    webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
    cmd: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    let request = tauri::webview::InvokeRequest {
        cmd: cmd.into(),
        callback: tauri::ipc::CallbackFn(0),
        error: tauri::ipc::CallbackFn(1),
        url: "tauri://localhost"
            .parse()
            .expect("the local origin parses"),
        body: tauri::ipc::InvokeBody::Json(args),
        headers: Default::default(),
        invoke_key: tauri::test::INVOKE_KEY.into(),
    };
    tauri::test::get_ipc_response(webview, request).map(|body| {
        body.deserialize::<serde_json::Value>()
            .expect("response deserializes")
    })
}

/// Refusals keep their codes through the command boundary: a
/// missing path refuses `NotFound` from `lookup`, with the
/// taxonomy tag intact in the rejection JSON.
#[test]
fn commands_preserve_refusal_codes() {
    let owner = running_owner();
    let backend = backend_for(&owner);
    let (_app, webview, data_dir) = mock_app_with(backend, "refusals");

    match invoke(
        &webview,
        "lookup",
        serde_json::json!({"driveDir": owner.dir.to_string_lossy(), "path": "no-such-path.txt"}),
    ) {
        Err(error) => {
            assert_eq!(error["kind"], "refused");
            assert_eq!(error["code"], "NotFound");
        }
        ok => panic!("a missing path refuses through the command: {ok:?}"),
    }

    std::fs::remove_dir_all(&data_dir).expect("the webview data dir cleans up");
    owner.shutdown();
}

/// Availability returns without restart, through the commands: the
/// lookup serves while the owner is up, stays unconfirmed while it
/// is unreachable, and serves again after an explicit `reconnect`
/// — all on the one mock app, which never restarts.
#[test]
fn commands_recover_availability_without_restart() {
    let mut owner = running_owner();
    let backend = backend_for(&owner);
    let (_app, webview, data_dir) = mock_app_with(backend, "recovery");

    let drive = owner.dir.to_string_lossy().into_owned();
    let lookup = |path: &str| {
        invoke(
            &webview,
            "lookup",
            serde_json::json!({"driveDir": drive, "path": path}),
        )
    };
    assert!(
        lookup("seed.txt").is_ok(),
        "the command serves while the owner is up"
    );

    // Owner alive but unreachable: the command stays unconfirmed —
    // never success, never recast as unavailable.
    owner.stop_serving();
    match lookup("seed.txt") {
        Err(error) => assert_eq!(error["kind"], "unconfirmed"),
        ok => panic!("an unreachable owner stays unconfirmed: {ok:?}"),
    }

    // Explicit reconnect while still down reports unavailable,
    // honestly: no connection was minted.
    match invoke(
        &webview,
        "reconnect",
        serde_json::json!({"driveDir": drive}),
    ) {
        Err(error) => assert_eq!(error["kind"], "unavailable"),
        ok => panic!("reconnect with no owner reports unavailable: {ok:?}"),
    }

    // Owner back: the same app's explicit reconnect mints a new
    // instance, and the lookup serves again. No restart anywhere.
    owner.serve_again();
    assert!(
        invoke(
            &webview,
            "reconnect",
            serde_json::json!({"driveDir": drive}),
        )
        .is_ok(),
        "explicit reconnect mints a new instance"
    );
    assert!(
        lookup("seed.txt").is_ok(),
        "availability returns on the same app without restart"
    );

    std::fs::remove_dir_all(&data_dir).expect("the webview data dir cleans up");
    owner.shutdown();
}

/// Registry compatibility: entries the daemon's own writer stores
/// read back through the desktop mirror byte-exact — names, dirs,
/// relays, service, mode — so the lifecycle view agrees with the
/// provisioning flow about which drives exist.
#[test]
fn registry_mirror_agrees_with_the_daemon_writer() {
    use std::collections::BTreeMap;
    use wyrd_daemon::bootstrap::{write_registry, RegistryEntry, ServiceMode};

    let dir = std::env::temp_dir().join(format!("wyrd-desktop-compat-reg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let drive = dir.join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    let path = dir.join("drives.json");
    let mut drives = BTreeMap::new();
    drives.insert(
        "alpha".to_string(),
        RegistryEntry {
            name: "alpha".to_string(),
            drive_dir: drive.clone(),
            relays: vec!["wss://relay.example".to_string()],
            service: "drive-alpha".to_string(),
            mode: ServiceMode::Vault,
        },
    );
    write_registry(&path, &drives).expect("the daemon writer stores");

    let state = wyrd_desktop::registry::list_drives_at(&path);
    assert_eq!(
        state.status,
        wyrd_desktop::RegistryStatus::Ready,
        "a daemon-written registry reads ready"
    );
    assert_eq!(state.drives.len(), 1);
    let view = &state.drives[0];
    assert_eq!(view.name, "alpha");
    assert_eq!(view.drive_dir, drive.to_string_lossy());
    assert_eq!(view.relays, vec!["wss://relay.example".to_string()]);
    assert_eq!(view.service, "drive-alpha");
    assert_eq!(view.mode, "vault");
    // No owner serves the scratch drive: the probe reports no
    // transport, which says nothing about any drive.
    assert!(
        matches!(
            view.availability,
            wyrd_desktop::DriveAvailability::Unavailable { .. }
        ),
        "no owner serves the scratch drive: {:?}",
        view.availability
    );
    assert!(state.skipped.is_empty());

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Settings compatibility: a section the daemon's own writer
/// stores — notification records included — reads back through the
/// desktop mirror with the bounds intact and the records excluded.
#[test]
fn settings_mirror_agrees_with_the_daemon_writer() {
    use std::collections::BTreeMap;
    use wyrd_daemon::settings::{
        write_settings, AppSettingsFile, DriveSettingsSection, APP_SETTINGS_VERSION,
    };

    let dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-compat-settings-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("settings.json");
    let mut notified = BTreeMap::new();
    notified.insert("sync.quiet".to_string(), 1700000000u64);
    let mut drives = BTreeMap::new();
    drives.insert(
        "/drives/alpha".to_string(),
        DriveSettingsSection {
            retained_bytes_quota: Some(0),
            max_admit_per_pass: Some(50),
            fetch_pass_budget_secs: None,
            max_mutation_wait_secs: Some(120),
            open_timeout_secs: Some(45),
            notified,
        },
    );
    write_settings(
        &path,
        &AppSettingsFile {
            version: APP_SETTINGS_VERSION,
            drives,
        },
    )
    .expect("the daemon writer stores");

    match wyrd_desktop::settings_view::drive_settings_at(
        &path,
        std::path::Path::new("/drives/alpha"),
    ) {
        wyrd_desktop::DriveSettings::Configured(view) => {
            assert_eq!(view.retained_bytes_quota, Some(0));
            assert_eq!(view.max_admit_per_pass, Some(50));
            assert_eq!(view.fetch_pass_budget_secs, None);
            assert_eq!(view.max_mutation_wait_secs, Some(120));
            assert_eq!(view.open_timeout_secs, Some(45));
        }
        state => panic!("a daemon-written section configures: {state:?}"),
    }
    assert_eq!(
        wyrd_desktop::settings_view::drive_settings_at(&path, std::path::Path::new("/drives/beta")),
        wyrd_desktop::DriveSettings::Nosection
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Reachability verifies this session against the live owner: the
/// registry probe reports `Available` while the server answers and
/// `Unavailable` once the socket is gone — never from cached state.
#[test]
fn registry_probe_verifies_against_the_live_owner() {
    use std::collections::BTreeMap;
    use wyrd_daemon::bootstrap::{write_registry, RegistryEntry, ServiceMode};

    let mut owner = running_owner();
    let dir = std::env::temp_dir().join(format!("wyrd-desktop-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("drives.json");
    let mut drives = BTreeMap::new();
    drives.insert(
        "live".to_string(),
        RegistryEntry {
            name: "live".to_string(),
            drive_dir: owner.dir.clone(),
            relays: Vec::new(),
            service: "drive-live".to_string(),
            mode: ServiceMode::Vault,
        },
    );
    write_registry(&path, &drives).expect("the daemon writer stores");

    let state = wyrd_desktop::registry::list_drives_at(&path);
    assert_eq!(state.drives.len(), 1);
    assert_eq!(
        state.drives[0].availability,
        wyrd_desktop::DriveAvailability::Available,
        "the serving owner verifies this session"
    );

    owner.stop_serving();
    let state = wyrd_desktop::registry::list_drives_at(&path);
    assert!(
        matches!(
            state.drives[0].availability,
            wyrd_desktop::DriveAvailability::Unavailable { .. }
        ),
        "a gone socket probes unavailable: {:?}",
        state.drives[0].availability
    );

    std::fs::remove_dir_all(&dir).unwrap();
    owner.shutdown();
}

/// The new commands answer through the registered surface with
/// their names and empty argument shapes: the frontend invokes
/// exactly these, and the payloads carry the state structs — never
/// the owner taxonomy.
#[test]
fn local_queries_answer_through_the_commands() {
    let owner = running_owner();
    let backend = backend_for(&owner);
    let (_app, webview, data_dir) = mock_app_with(backend, "queries");

    match invoke(&webview, "list_drives", serde_json::json!({})) {
        Ok(value) => {
            assert!(
                value.get("status").is_some(),
                "a status rides along: {value}"
            );
            assert!(value.get("drives").is_some(), "entries ride along: {value}");
            assert!(value.get("skipped").is_some(), "skips ride along: {value}");
        }
        Err(error) => panic!("list_drives answers its state struct: {error}"),
    }
    match invoke(
        &webview,
        "drive_settings",
        serde_json::json!({"driveDir": owner.dir.to_string_lossy()}),
    ) {
        Ok(value) => {
            assert!(value.get("state").is_some(), "a state rides along: {value}");
        }
        Err(error) => panic!("drive_settings answers its state struct: {error}"),
    }

    // An invalid drive identity answers the explicit unreadable
    // state — never the taxonomy, never a silent resolution
    // against the process directory.
    match invoke(
        &webview,
        "drive_settings",
        serde_json::json!({"driveDir": "relative/path"}),
    ) {
        Ok(value) => {
            assert_eq!(
                value.get("state"),
                Some(&serde_json::Value::String("unreadable".to_string())),
                "a relative drive reads unreadable: {value}"
            );
            assert!(
                value.get("reason").is_some(),
                "the refusal names its reason: {value}"
            );
        }
        Err(error) => panic!("drive_settings never rejects with the taxonomy: {error}"),
    }

    std::fs::remove_dir_all(&data_dir).expect("the webview data dir cleans up");
    owner.shutdown();
}

/// Per-drive failure isolation, through the commands: two live
/// owners behind one connection map, no pre-connected backend — the
/// map connects lazily from the explicit drive identity, so there
/// is no implicit active drive. One socket dying invalidates
/// nothing else: the other drive keeps serving, and an explicit
/// reconnect of the healthy drive touches only that drive.
#[test]
fn per_drive_failure_isolation_through_the_commands() {
    let mut first = running_owner();
    let second = running_owner();
    assert_ne!(first.dir, second.dir, "two drives, two sockets");

    let state = wyrd_desktop::AppState::new();
    let (_app, webview, data_dir) = mock_app_with_state(state, "isolation");

    let lookup = |drive: &std::path::Path| {
        invoke(
            &webview,
            "lookup",
            serde_json::json!({"driveDir": drive.to_string_lossy(), "path": "seed.txt"}),
        )
    };
    assert!(
        lookup(&first.dir).is_ok(),
        "the first drive serves with no pre-connected backend"
    );
    assert!(
        lookup(&second.dir).is_ok(),
        "the second drive serves on the same app"
    );

    // One socket dies: that drive stays unconfirmed while the other
    // keeps serving on the same map.
    first.stop_serving();
    match lookup(&first.dir) {
        Err(error) => assert_eq!(error["kind"], "unconfirmed"),
        ok => panic!("the downed drive stays unconfirmed: {ok:?}"),
    }
    assert!(
        lookup(&second.dir).is_ok(),
        "the surviving drive is unaffected by the other's dead socket"
    );

    // Reconnecting the healthy drive touches only that drive: it
    // still serves, and the downed drive stays unconfirmed.
    assert!(
        invoke(
            &webview,
            "reconnect",
            serde_json::json!({"driveDir": second.dir.to_string_lossy()}),
        )
        .is_ok(),
        "explicit reconnect of the healthy drive mints its own instance"
    );
    assert!(
        lookup(&second.dir).is_ok(),
        "the healthy drive serves after its own reconnect"
    );
    match lookup(&first.dir) {
        Err(error) => assert_eq!(error["kind"], "unconfirmed"),
        ok => panic!("the downed drive is still unconfirmed: {ok:?}"),
    }

    std::fs::remove_dir_all(&data_dir).expect("the webview data dir cleans up");
    first.shutdown();
    second.shutdown();
}

/// The map is a pinned chokepoint, not a convention: a backend
/// connected through a `..` alias of a live drive reaches the
/// owner (the OS resolves the alias), but inserting it refuses
/// with the rule reason — the alias can never key a second entry
/// for the same socket.
#[test]
fn insert_refuses_a_dotdot_alias_for_a_live_drive() {
    let owner = running_owner();
    std::fs::create_dir_all(owner.dir.join("sub")).expect("the alias parent exists");
    let alias = owner.dir.join("sub").join("..");
    let backend =
        DesktopBackend::connect(&alias).expect("the OS resolves the alias to the live socket");
    backend
        .lookup("seed.txt")
        .expect("the alias reaches the live owner");
    let state = wyrd_desktop::AppState::new();
    match state.insert(backend) {
        Err(reason) => assert!(
            reason.contains("must not contain .."),
            "the refusal names the rule: {reason}"
        ),
        Ok(()) => panic!("a `..`-carrying path must never key the connection map"),
    }
    owner.shutdown();
}

/// The encoding refusal is pinned too: a backend connected through
/// a non-UTF-8 alias of a live drive reaches the owner (the OS
/// resolves the alias), but inserting it refuses with the encoding
/// reason — no uncheckable spelling can key the map.
///
/// Linux-only: macOS (APFS) cannot represent the non-UTF-8 input at
/// all, so the refusal is unreachable there by construction; CI on
/// Linux runs this case.
#[cfg(target_os = "linux")]
#[test]
fn insert_refuses_a_non_utf8_alias_for_a_live_drive() {
    use std::os::unix::ffi::OsStrExt;
    let owner = running_owner();
    let odd = owner.dir.join(std::ffi::OsStr::from_bytes(b"odd-\xff-dir"));
    std::fs::create_dir_all(&odd).expect("the non-UTF-8 parent exists");
    let alias = odd.join("..");
    let backend =
        DesktopBackend::connect(&alias).expect("the OS resolves the alias to the live socket");
    backend
        .lookup("seed.txt")
        .expect("the alias reaches the live owner");
    let state = wyrd_desktop::AppState::new();
    match state.insert(backend) {
        Err(reason) => assert!(
            reason.contains("is not valid UTF-8"),
            "the refusal names the encoding: {reason}"
        ),
        Ok(()) => panic!("a non-UTF-8 path must never key the connection map"),
    }
    owner.shutdown();
}

/// Provisioning refuses a non-canonical drive identity before any
/// process spawns: the report is `incomplete` at stage `validate`,
/// and the observed registry still re-reads — verification runs on
/// every path, including argument refusals.
#[test]
fn provision_commands_refuse_a_non_canonical_drive_identity() {
    let (_app, webview, data_dir) = mock_app_with_state(wyrd_desktop::AppState::new(), "prov-bad");
    let report = invoke(
        &webview,
        "provision_create",
        serde_json::json!({
            "args": {
                "name": "alpha",
                "driveDir": "drives/alpha",
                "relays": [],
                "identityFile": "/keys/identity",
                "passphraseFile": "/keys/passphrase",
                "mode": "vault",
                "noService": true,
            }
        }),
    )
    .expect("provisioning reports infallibly");
    assert_eq!(report["result"]["result"], "expectedfailure");
    assert_eq!(report["membership"], serde_json::Value::Null);
    assert!(
        report["observed"]["status"].is_string(),
        "the registry still re-reads on refusal: {report}"
    );
    std::fs::remove_dir_all(&data_dir).expect("the webview data dir cleans up");
}

/// A second lifecycle operation on the same drive refuses `busy`:
/// the flight guard serializes per drive, never queues behind a
/// service restart. The first flight is held open by the test (no
/// process runs); the observed registry still re-reads.
#[test]
fn provision_commands_refuse_busy_while_a_flight_is_held() {
    let state = wyrd_desktop::AppState::new();
    let drive = std::path::PathBuf::from("/drives/alpha");
    assert!(state.claim_flight(&drive), "the test holds the flight");
    let (_app, webview, data_dir) = mock_app_with_state(state, "prov-busy");
    let report = invoke(
        &webview,
        "provision_leave",
        serde_json::json!({
            "args": {
                "driveDir": "/drives/alpha",
                "identityFile": "/keys/identity",
                "passphraseFile": "/keys/passphrase",
                "acknowledged": false,
            }
        }),
    )
    .expect("provisioning reports infallibly");
    assert_eq!(report["result"]["result"], "busy");
    assert_eq!(report["result"]["drive_dir"], "/drives/alpha");
    assert_eq!(report["membership"], serde_json::Value::Null);
    assert!(
        report["observed"]["status"].is_string(),
        "the registry still re-reads on busy: {report}"
    );
    std::fs::remove_dir_all(&data_dir).expect("the webview data dir cleans up");
}

/// The full create path through a fake CLI: version check passes,
/// the canned line exits 0, and the report pairs the completion
/// with the post-run registry observation — the same
/// verification-via-reread the shipped command performs, with no
/// mock. What the text says is display detail; the code decides.
/// The fake answers `__build_info` with the workspace version
/// the adapter requires.
#[test]
fn provision_create_completes_through_a_fake_cli() {
    let dir = std::env::temp_dir().join(format!("wyrd-desktop-ipc-fakecli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cli = dir.join("wyrd");
    let version = env!("CARGO_PKG_VERSION");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\necho \"drive alpha verified available (socket /drives/alpha/owner.sock)\"\nexit 0\n"
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
    let (_app, webview, data_dir) = mock_app_with_state(state, "prov-ready");
    let report = invoke(
        &webview,
        "provision_create",
        serde_json::json!({
            "args": {
                "name": "alpha",
                "driveDir": "/drives/alpha",
                "relays": ["wss://relay.example"],
                "identityFile": "/keys/identity",
                "passphraseFile": "/keys/passphrase",
                "mode": "vault",
                "noService": true,
            }
        }),
    )
    .expect("provisioning reports infallibly");
    assert_eq!(
        report["result"]["result"], "completed",
        "fake exit 0 completes without parsing its text: {report}"
    );
    assert_eq!(report["membership"], serde_json::Value::Null);
    assert!(
        report["observed"]["status"].is_string(),
        "the post-run observation rides along: {report}"
    );
    std::fs::remove_dir_all(&data_dir).expect("the webview data dir cleans up");
    std::fs::remove_dir_all(&dir).unwrap_or(());
}

/// The flight releases after a normal operation: a second
/// lifecycle call on the same drive runs instead of refusing busy.
/// The first call drives the fake CLI to exit 0 (the fake ignores
/// the nonexistent drive); the second proves the set is empty.
#[test]
fn provision_flight_releases_after_a_normal_operation() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-ipc-fakecli-{}-release",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let cli = dir.join("wyrd");
    let version = env!("CARGO_PKG_VERSION");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nexit 0\n"
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
    let (_app, webview, data_dir) = mock_app_with_state(state, "prov-release");
    let create = serde_json::json!({
        "args": {
            "name": "alpha",
            "driveDir": "/drives/alpha",
            "relays": [],
            "identityFile": "/keys/identity",
            "passphraseFile": "/keys/passphrase",
            "mode": "vault",
            "noService": true,
        }
    });
    let first = invoke(&webview, "provision_create", create.clone())
        .expect("provisioning reports infallibly");
    assert_eq!(first["result"]["result"], "completed");
    let second =
        invoke(&webview, "provision_create", create).expect("provisioning reports infallibly");
    assert_ne!(
        second["result"]["result"], "busy",
        "the flight released after the first operation: {second}"
    );
    std::fs::remove_dir_all(&data_dir).expect("the webview data dir cleans up");
    std::fs::remove_dir_all(&dir).unwrap_or(());
}

/// Leave re-observes membership through the owner: a fake exit-0
/// leave against a live drive reports `completed`, and the report
/// carries the membership tip the owner actually serves — the
/// leave contract's independent check, never CLI text.
#[test]
fn provision_leave_reobserves_membership_through_the_owner() {
    let owner = running_owner();
    let dir = std::env::temp_dir().join(format!(
        "wyrd-desktop-ipc-fakecli-{}-leave",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let cli = dir.join("wyrd");
    let version = env!("CARGO_PKG_VERSION");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nexit 0\n"
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
    let (_app, webview, data_dir) = mock_app_with_state(state, "prov-leave-member");
    let drive = owner.dir.to_string_lossy().into_owned();
    let report = invoke(
        &webview,
        "provision_leave",
        serde_json::json!({
            "args": {
                "driveDir": drive,
                "identityFile": "/keys/identity",
                "passphraseFile": "/keys/passphrase",
                "acknowledged": false,
            }
        }),
    )
    .expect("provisioning reports infallibly");
    assert_eq!(
        report["result"]["result"], "completed",
        "fake exit 0 completes: {report}"
    );
    let membership = &report["membership"];
    assert!(
        membership.is_object(),
        "the live owner answers the membership re-observation: {report}"
    );
    assert!(
        membership["known_members"].as_u64().unwrap_or(0) >= 1,
        "the single-device drive names its member: {report}"
    );
    std::fs::remove_dir_all(&data_dir).expect("the webview data dir cleans up");
    std::fs::remove_dir_all(&dir).unwrap_or(());
    owner.shutdown();
}
