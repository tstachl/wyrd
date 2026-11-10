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
/// registered Tauri commands on the mock runtime — no webview, no
/// event loop — against the same real owner. The mock stands in
/// for the transport only: every answer below is produced by the
/// daemon's server over a live loop, and command names are the
/// backend method names verbatim.
///
/// One mock app managing the backend, plus its webview: the pair
/// every command proof invokes through. The app never runs — the
/// mock runtime answers invokes without an event loop.
fn mock_app_with(
    backend: DesktopBackend,
) -> (
    tauri::App<tauri::test::MockRuntime>,
    tauri::WebviewWindow<tauri::test::MockRuntime>,
) {
    let app = tauri::test::mock_builder()
        .manage(wyrd_desktop::AppState::new(backend))
        .invoke_handler(wyrd_desktop::invoke_handler())
        .build(tauri::generate_context!())
        .expect("the mock app builds");
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("the mock webview builds");
    (app, webview)
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
    let (_app, webview) = mock_app_with(backend);

    match invoke(
        &webview,
        "lookup",
        serde_json::json!({"path": "no-such-path.txt"}),
    ) {
        Err(error) => {
            assert_eq!(error["kind"], "refused");
            assert_eq!(error["code"], "NotFound");
        }
        ok => panic!("a missing path refuses through the command: {ok:?}"),
    }

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
    let (_app, webview) = mock_app_with(backend);

    assert!(
        invoke(&webview, "lookup", serde_json::json!({"path": "seed.txt"})).is_ok(),
        "the command serves while the owner is up"
    );

    // Owner alive but unreachable: the command stays unconfirmed —
    // never success, never recast as unavailable.
    owner.stop_serving();
    match invoke(&webview, "lookup", serde_json::json!({"path": "seed.txt"})) {
        Err(error) => assert_eq!(error["kind"], "unconfirmed"),
        ok => panic!("an unreachable owner stays unconfirmed: {ok:?}"),
    }

    // Explicit reconnect while still down reports unavailable,
    // honestly: no connection was minted.
    match invoke(&webview, "reconnect", serde_json::json!({})) {
        Err(error) => assert_eq!(error["kind"], "unavailable"),
        ok => panic!("reconnect with no owner reports unavailable: {ok:?}"),
    }

    // Owner back: the same app's explicit reconnect mints a new
    // instance, and the lookup serves again. No restart anywhere.
    owner.serve_again();
    assert!(
        invoke(&webview, "reconnect", serde_json::json!({})).is_ok(),
        "explicit reconnect mints a new instance"
    );
    assert!(
        invoke(&webview, "lookup", serde_json::json!({"path": "seed.txt"})).is_ok(),
        "availability returns on the same app without restart"
    );

    owner.shutdown();
}
