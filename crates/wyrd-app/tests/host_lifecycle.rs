//! Host lifecycle composition tests: the owner path over the public
//! API, with a real drive per test. Fakes (silent mailbox, loopback
//! serving) live here, never in `src/`.

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wyrd_app::{
    exit_code, finish_running, into_running, open_host, HostError, HostTransport, HostView,
};
use wyrd_core::mutation::{MutationKind, MutationOutcome};
use wyrd_core::node::WyrdNode;
use wyrd_daemon::process::{SHUTDOWN_DEADLINE, TRANSPORT_SHUTDOWN_DEADLINE};
use wyrd_daemon::{TransportDeadlines, VaultEvent};
use wyrd_format::FsObjectStore;
use wyrd_sync::bulk::MemoryBulkSource;
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::Engine;
use wyrd_sync::transport::mailbox::{
    Delivery, DeliveryId, Disposition, Mailbox, MailboxEnvelope, MailboxError, SendReport,
};

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

/// A per-test stop latch: tests run in parallel, so a shared static
/// would let one test observe another's trip.
fn stop_flag() -> &'static std::sync::atomic::AtomicBool {
    Box::leak(Box::new(std::sync::atomic::AtomicBool::new(false)))
}

/// A control plane that never speaks: no announcements, no sends, so
/// the loop idles on pacing alone and every observed change is local.
/// Same shape as the embedder's silent mailbox: the contract needs
/// obligations to drain, not a refusal path.
struct SilentMailbox;

impl Mailbox for SilentMailbox {
    fn send(&mut self, _envelope: MailboxEnvelope) -> Result<SendReport, MailboxError> {
        Ok(SendReport { accepted: 1 })
    }

    fn recv(&mut self) -> Result<Option<Delivery>, MailboxError> {
        Ok(None)
    }

    fn settle(&mut self, _id: DeliveryId, _disposition: Disposition) -> Result<(), MailboxError> {
        Ok(())
    }
}

/// A bulk source that never holds anything: fetches miss, routes
/// publish vacuously. The in-memory fake cannot grow the
/// `RoutePublishing` bound (orphan rule), so this newtype delegates
/// the fetch plane and no-ops the route plane.
struct SilentBulk(MemoryBulkSource);

impl wyrd_sync::bulk::AttemptBudget for SilentBulk {}

impl wyrd_sync::bulk::BulkSource for SilentBulk {
    fn fetch_root_manifest(
        &mut self,
        snapshot: &wyrd_format::SnapshotId,
        max: usize,
    ) -> Result<Option<wyrd_sync::bulk::SealedManifest>, wyrd_sync::bulk::BulkError> {
        self.0.fetch_root_manifest(snapshot, max)
    }

    fn fetch_snapshot(
        &mut self,
        snapshot: &wyrd_format::SnapshotId,
        max: usize,
    ) -> Result<Option<Vec<u8>>, wyrd_sync::bulk::BulkError> {
        self.0.fetch_snapshot(snapshot, max)
    }

    fn fetch_sealed(
        &mut self,
        storage: &wyrd_format::StorageId,
        max: usize,
    ) -> Result<Option<Vec<u8>>, wyrd_sync::bulk::BulkError> {
        self.0.fetch_sealed(storage, max)
    }

    fn fetch_transport(
        &mut self,
        root: &wyrd_format::BaoRoot,
        max: usize,
    ) -> Result<Option<Vec<u8>>, wyrd_sync::bulk::BulkError> {
        self.0.fetch_transport(root, max)
    }
}

impl wyrd_sync::runtime::RoutePublishing for SilentBulk {
    fn publish_routes(
        &mut self,
        _state: &wyrd_sync::runtime::RuntimeState,
    ) -> Result<wyrd_sync::runtime::RouteReport, wyrd_sync::runtime::EngineError> {
        Ok(wyrd_sync::runtime::RouteReport {
            published: 0,
            undecodable: 0,
        })
    }
}

/// Block until a submission is admitted (or fail fast): faster and
/// less flaky than a fixed sleep, and it proves the test raced the
/// teardown instead of the scheduler.
fn await_pending(queue: &wyrd_core::mutation::MutationQueue) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while queue.outstanding() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "submission never landed in pending"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Compose a host ready to run: real drive, opened host, loopback
/// serving bound, node split for live serving. Returns the drive dir
/// (for reopen assertions), the running host, its submission queue,
/// and the serving surface the run installs.
fn compose(
    label: &str,
) -> (
    PathBuf,
    wyrd_app::RunningHost,
    Arc<wyrd_core::mutation::MutationQueue>,
    wyrd_daemon::ServingEndpoint,
    String,
) {
    let dir = scratch(label);
    create_drive(dir.clone());
    let open = open_host(dir.clone(), PASS, identity()).expect("host opens");
    let serving = open
        .node()
        .open_serving(&dir, true)
        .expect("loopback serving binds");
    let serving_id = hex::encode(serving.addr().id.as_bytes());
    let running = into_running(open).expect("composition splits");
    let queue = Arc::clone(&running.parts().mutations);
    (dir, running, queue, serving, serving_id)
}

/// Run a composed host to completion on the calling thread's terms:
/// the loop runs supervised in the background while the caller
/// submits and trips the stop, then this joins it and returns the
/// outcome for the exit-status fold.
fn finish_with(
    running: wyrd_app::RunningHost,
    stop: &'static std::sync::atomic::AtomicBool,
    serving: wyrd_daemon::ServingEndpoint,
    serving_id: String,
) -> wyrd_daemon::VaultOutcome {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    finish_running(
        running,
        stop,
        HostTransport {
            mailbox: SilentMailbox,
            bulk: Some(SilentBulk(MemoryBulkSource::default())),
            serving,
            serving_id,
            deadlines: TransportDeadlines {
                mailbox: SHUTDOWN_DEADLINE,
                bulk: wyrd_sync::GRACEFUL_CLOSE_DEADLINE,
                serving: TRANSPORT_SHUTDOWN_DEADLINE,
            },
            emit: Arc::new(move |event: VaultEvent| {
                sink.lock().unwrap().push(format!("{event:?}"));
            }),
        },
    )
    .expect("run completes")
}
/// submission made before the stop resolves `Done` (never `Shutdown`),
/// the run exits 0, and the authored snapshot is durable afterwards —
/// the post-return drain executed it against the still-open queue.
#[test]
fn host_quit_preserves_a_dirty_handle() {
    let (dir, running, queue, serving, serving_id) = compose("dirty-quit");
    let stop = stop_flag();
    let run = std::thread::spawn(move || finish_with(running, stop, serving, serving_id));
    // Admit the write, prove it is pending (not yet executed), then
    // quit: whichever pass would have executed it, the drain must.
    let submitter = Arc::clone(&queue);
    let submitted = std::thread::spawn(move || {
        submitter.submit(MutationKind::Mkdir {
            path: "docs".into(),
        })
    });
    await_pending(&queue);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let outcome = run.join().expect("run thread joins");
    assert!(
        matches!(
            submitted.join().expect("submitter joins"),
            Ok(MutationOutcome::Done)
        ),
        "the admitted write executes through shutdown, never Shutdown"
    );
    assert_eq!(exit_code(&outcome), 0, "a clean quit exits 0");
    // Durable afterwards: reopen and read through a fresh node.
    let engine = Engine::open_keystore(dir.clone(), PASS, identity()).expect("reopen works");
    let store = FsObjectStore::open(dir).expect("store reopens");
    let mut node = WyrdNode::<HostView>::new(engine, store).expect("node recomposes");
    node.refresh_live_heads().expect("heads refresh");
    assert!(
        node.view().lookup("docs").is_ok(),
        "the mkdir authored before the stop is durable"
    );
}
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

/// The ordering half of the lifecycle contract, asserted for the
/// host: a submission admitted before the stop executes (the drain
/// runs before admission closes), and admission closes only after
/// the loop joins — a submission after the run resolves `Shutdown`
/// instead of blocking forever.
#[test]
fn host_closes_admission_only_after_its_last_submitter() {
    let (_dir, running, queue, serving, serving_id) = compose("admission-order");
    let stop = stop_flag();
    let run = std::thread::spawn(move || finish_with(running, stop, serving, serving_id));
    // Admit before the stop: the drain must execute this, not the
    // admission close.
    let submitter = Arc::clone(&queue);
    let submitted = std::thread::spawn(move || {
        submitter.submit(MutationKind::Mkdir {
            path: "late".into(),
        })
    });
    await_pending(&queue);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let outcome = run.join().expect("run thread joins");
    assert!(
        matches!(
            submitted.join().expect("submitter joins"),
            Ok(MutationOutcome::Done)
        ),
        "the in-teardown submission executes, never Shutdown"
    );
    assert_eq!(exit_code(&outcome), 0, "a clean quit exits 0");
    // After the joins admission is closed: a late submission resolves
    // instead of blocking, which is what bounds every teardown.
    match queue.submit(MutationKind::Mkdir {
        path: "after".into(),
    }) {
        Err(wyrd_core::mutation::MutationError::Shutdown) => {}
        other => panic!("post-teardown submission must resolve Shutdown, got {other:?}"),
    }
}
