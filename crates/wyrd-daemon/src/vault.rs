//! The headless vault composer (issue `22a`): the mount's live loop
//! and transport teardown minus the presentation session. A vault has
//! no FUSE surface to unmount and no teardown submitter, so the order
//! it inherits from `lifecycle` is a strict subsequence — loop join,
//! then mailbox, bulk, and serving close — and this module is where
//! that subsequence lives as code rather than prose. The mount keeps
//! its own session-prefixed copy; the order-assertion test the
//! lifecycle module says is missing lands in `22b` against the shared
//! [`shutdown_transport`] below, which is why the transport tail is
//! one function, not two parallel copies.
//!
//! The composer is generic over the loop's view, mailbox, and bulk
//! source exactly like [`Supervisor::spawn_loop`]: production passes
//! the drive view, the live mailbox, and the iroh bulk source, while
//! tests pass fakes. The serving endpoint stays concrete
//! ([`ServingEndpoint`]): its loopback constructor is hermetic enough
//! for unit tests, so no serving fake exists. What tests DO inject is
//! the surface the install step reads through ([`ServeSurface`]): the
//! wiring test pins that installing a never-ready barrier holds the
//! announcement obligation, which a hand-wired barrier test cannot.

use std::io;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use wyrd_core::live::{
    LiveConfig, LiveError, LiveNode, LiveParts, LiveSummary, PassHook, QuotaCheckError,
    ServingBarrier, SyncReport,
};
use wyrd_core::node::{NodeError, WyrdNode};
use wyrd_core::view::{NamespaceView, RuntimeMaterialization};
use wyrd_format::{DiscardRejectedRepresentation, FsObjectStore, ObjectStore};
use wyrd_sync::runtime::{Engine, EngineError, RoutePublishing};
use wyrd_sync::serving::ServingEndpoint;
use wyrd_sync::transport::mailbox::Mailbox;

use crate::lifecycle::{LoopError, Supervisor};
use crate::DriveView;

/// What the install step reads from a bound serving surface: flush
/// the mirror, the address announcements carry, and the barrier that
/// gates their discharge. [`ServingEndpoint`] is the production
/// surface; tests inject a stub to pin the wiring (a never-ready
/// barrier must hold the obligation through the install path, not
/// just through a hand-wired one).
pub trait ServeSurface: Send + Sync {
    /// Wait until every mirror import enqueued so far has landed, so
    /// the first announcement carries a route peers can already dial.
    fn flush_surface(&self) -> io::Result<()>;
    /// The retrieval route announcements discharge with.
    fn node_addr_bytes(&self) -> Vec<u8>;
    /// The discharge barrier for every publish pass.
    fn serving_barrier(&self) -> Arc<dyn ServingBarrier>;
}

impl ServeSurface for ServingEndpoint {
    fn flush_surface(&self) -> io::Result<()> {
        self.flush()
    }

    fn node_addr_bytes(&self) -> Vec<u8> {
        self.node_addr_bytes()
    }

    fn serving_barrier(&self) -> Arc<dyn ServingBarrier> {
        Arc::new(self.handle())
    }
}

/// Install a bound surface on a live node, in the mount's order:
/// flush before announcing the address, so the first seal carries a
/// route peers can already dial; then publish the route and gate
/// every pass's discharge on mirror readiness. Returns the address
/// bytes for the ready line. A backed-up mirror leaves the
/// obligation recorded, never discharged — the wiring test pins
/// exactly that through this function.
pub fn install_serving<V>(live: &mut LiveNode<V>, surface: &dyn ServeSurface) -> io::Result<Vec<u8>>
where
    V: NamespaceView<Materialization = RuntimeMaterialization>,
    <V as NamespaceView>::Store: ObjectStore,
    <<V as NamespaceView>::Store as ObjectStore>::Error: std::fmt::Debug,
{
    surface.flush_surface()?;
    let addr = surface.node_addr_bytes();
    live.set_node_addr(Some(addr.clone()));
    live.set_serving_barrier(surface.serving_barrier());
    Ok(addr)
}

/// Install the drive directory the loop reads the materialization
/// role from. A composer step of its own, deliberately not a rider
/// on [`install_serving`]: serving-optional compositions (`sync
/// now` without `--serve`) retain like their role too, and a role
/// obligation coupled to serving would silently skip exactly them.
/// Every `into_live` caller in production performs this step;
/// tests that skip it get mirror behavior.
pub fn install_role_dir<V>(live: &mut LiveNode<V>, drive_dir: &std::path::Path)
where
    V: NamespaceView<Materialization = RuntimeMaterialization>,
    <V as NamespaceView>::Store: ObjectStore,
    <<V as NamespaceView>::Store as ObjectStore>::Error: std::fmt::Debug,
{
    live.set_role_dir(drive_dir.to_path_buf());
}

/// One event the vault observer emits to the process host: the host
/// owns diagnostics (stderr, log file, journal), the composer owns
/// the facts. Counts, never health claims — a posture line reports
/// passes and sends, not convergence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultEvent {
    /// The first pass completed with the serving surface installed:
    /// the first true line the operator sees. Carries the serving id
    /// peers dial, exactly as the host hex-encoded it.
    Ready {
        /// The serving id, for the operator's first dial test.
        serving_id: String,
    },
    /// Periodic proof of life while the loop runs.
    Posture {
        /// Completed passes (idle passes included).
        passes: u64,
        /// Transient pass failures absorbed under the error cap.
        errors: u64,
        /// Outbound sends publication committed across all passes.
        sent: u64,
        /// Seconds since the observer started.
        uptime_secs: u64,
    },
    /// One pass failed: the class, its consecutive count, and the
    /// error text. Same facts the mount prints per failure.
    PassFailed {
        class: String,
        consecutive: u32,
        error: String,
    },
}

/// How often the observer emits [`VaultEvent::Posture`] while passes
/// complete: proof of life for a process nobody watches, not a
/// health verdict.
pub const POSTURE_INTERVAL: Duration = Duration::from_secs(60);

/// The vault's loop observer: ready exactly once on the first
/// completed pass, posture on the interval, failures forwarded. The
/// emit sink is host-owned (stderr plus an optional log file in
/// production, a recording vec in tests). Shared between the pass
/// hook and the failure closure through [`VaultObserver::shared`]:
/// both run on the loop thread, so the mutex never contends.
pub struct VaultObserver {
    emit: Arc<dyn Fn(VaultEvent) + Send + Sync>,
    serving_id: String,
    started: Instant,
    ready: bool,
    last_posture: Instant,
    sent: u64,
    errors: u64,
}

impl VaultObserver {
    /// Observe loop passes into `emit`. `started` is a parameter, not
    /// `Instant::now()`, so tests pin uptime without sleeping.
    pub fn new(
        emit: Arc<dyn Fn(VaultEvent) + Send + Sync>,
        serving_id: String,
        started: Instant,
    ) -> Self {
        VaultObserver {
            emit,
            serving_id,
            started,
            ready: false,
            last_posture: started,
            sent: 0,
            errors: 0,
        }
    }

    /// One pass completed: ready on the first, posture on the
    /// interval. Ready means the loop is converging with the serving
    /// surface installed — the install flushed before the address was
    /// announced, so the first completed pass runs with a published
    /// route.
    pub fn pass_completed(&mut self, passes: u64, report: &SyncReport) {
        self.sent += report.sent as u64;
        if !self.ready {
            self.ready = true;
            (self.emit)(VaultEvent::Ready {
                serving_id: self.serving_id.clone(),
            });
        }
        if self.last_posture.elapsed() >= POSTURE_INTERVAL {
            self.last_posture = Instant::now();
            (self.emit)(VaultEvent::Posture {
                passes,
                errors: self.errors,
                sent: self.sent,
                uptime_secs: self.started.elapsed().as_secs(),
            });
        }
    }

    /// One pass failed: forward the facts, and count the error for
    /// the next posture line.
    pub fn pass_failed(&mut self, error: &LiveError, consecutive: u32) {
        self.errors += 1;
        (self.emit)(VaultEvent::PassFailed {
            class: format!("{:?}", crate::core::FailureClass::from(error)),
            consecutive,
            error: error.to_string(),
        });
    }

    /// Share one observer between the node's pass hook and the loop's
    /// failure closure: both run on the loop thread (the hook inside
    /// the pass, the closure on its failure), so they observe one
    /// count each without interleaving.
    pub fn shared(self) -> (Box<PassHook>, impl FnMut(&LiveError, u32) + Send + 'static) {
        let shared = Arc::new(std::sync::Mutex::new(self));
        let for_hook = Arc::clone(&shared);
        let for_errors = shared;
        (
            Box::new(move |passes, report| {
                for_hook
                    .lock()
                    .expect("vault observer lock")
                    .pass_completed(passes, report);
            }),
            move |error, consecutive| {
                for_errors
                    .lock()
                    .expect("vault observer lock")
                    .pass_failed(error, consecutive);
            },
        )
    }
}

/// Bounds for the three transport closes, as named fields so a call
/// site cannot silently swap two stages. Mailbox stop and the serving
/// close fail a wedged shutdown; the bulk close is graceful-or-abort
/// and infallible by construction (see the mount's teardown), so it
/// carries a deadline but no outcome.
pub struct TransportDeadlines {
    /// Bound for stopping the mailbox tasks before aborting them.
    pub mailbox: Duration,
    /// Bound for the bulk endpoint's graceful close before aborting it.
    pub bulk: Duration,
    /// Bound for the serving endpoint's graceful close. Much longer
    /// than the mailbox bound: the close drains in-flight transfers
    /// over the same degraded links the drive syncs over, and
    /// mistaking a slow close for a wedged one turns clean shutdowns
    /// into failures. Still bounded, so a peer that never answers
    /// cannot hang teardown forever.
    pub serving: Duration,
}

/// The transport tail's outcome: bulk has no field (infallible by
/// construction), the serving close does. Every stage runs — a failed
/// serving close never skips the closes before it, because the order
/// below closes mailbox and bulk first unconditionally.
pub struct TransportShutdown {
    /// The serving endpoint's graceful close under its deadline.
    pub serving: io::Result<()>,
}

/// The transport tail every composer runs after its loop joins, in
/// the lifecycle order: stop the mailbox tasks under a bounded
/// deadline (a shutdown must never wait on a relay outage that never
/// clears), close bulk graceful-or-abort, release the bulk endpoint
/// (and its runtime) before stopping serving — the abort half of
/// graceful-or-abort — then close serving under its own deadline. The
/// mount runs this same function: the loop-composer tail exists once,
/// so the `22b` order-assertion test has one copy to pin. (`sync now`
/// keeps its split close — bulk closes before the serve phase parks,
/// serving after — because that order brackets residency, not
/// teardown.)
pub fn shutdown_transport<M, B>(
    mailbox: &mut M,
    bulk: Option<B>,
    serving: ServingEndpoint,
    deadlines: &TransportDeadlines,
) -> TransportShutdown
where
    M: Mailbox + Send,
    B: RoutePublishing + Send,
{
    let teardown_start = Instant::now();
    mailbox.shutdown(deadlines.mailbox);
    tracing::info!(
        stage = "teardown",
        elapsed_ms = teardown_start.elapsed().as_millis(),
        "mailbox stopped"
    );
    if let Some(mut bulk) = bulk {
        bulk.shutdown(deadlines.bulk);
        // Drop before the serving close, never earlier: bounds every
        // close path even if the graceful attempt above ever regresses
        // past its deadline.
        drop(bulk);
        tracing::info!(
            stage = "teardown",
            elapsed_ms = teardown_start.elapsed().as_millis(),
            "bulk source stopped"
        );
    }
    let serving = serving.shutdown(deadlines.serving);
    if let Err(error) = &serving {
        tracing::warn!(
            stage = "serving",
            elapsed_ms = teardown_start.elapsed().as_millis(),
            error = %error,
            "serving shutdown failed"
        );
    } else {
        tracing::info!(
            stage = "serving",
            elapsed_ms = teardown_start.elapsed().as_millis(),
            "serving stopped"
        );
    }
    TransportShutdown { serving }
}

/// How the vault's supervised loop ended. Mirrors [`LoopError`] with
/// one addition: the vault has no session join bounding teardown, so
/// a dead supervision (the join itself failing, which the
/// supervision's own panic recovery makes near-impossible) is a
/// distinct outcome instead of a skipped teardown — the serving
/// endpoint still closes below, and the host reports the supervision,
/// not a phantom loop panic. The mailbox and bulk handles moved into
/// the dead supervision drop with its unwinding stack; only the
/// composer-owned serving endpoint closes explicitly on that path.
pub enum VaultLoopEnd {
    /// The loop returned (clean stop or terminal class error).
    Returned(Result<LiveSummary, LoopError>),
    /// The supervision thread itself died.
    SupervisionLost,
}

/// What the vault composer hands back: the loop's end plus the
/// transport tail's outcome. The host folds both into the exit
/// status, first failure wins.
pub struct VaultOutcome {
    /// The supervised loop's end.
    pub loop_end: VaultLoopEnd,
    /// The mailbox/bulk/serving closes, always run.
    pub transport: TransportShutdown,
}

/// Everything [`run_vault`] needs, as named fields: nine positional
/// arguments would let a call site silently swap two stages worth of
/// handles.
pub struct VaultRun<V: NamespaceView, M, B> {
    /// The composed live node, serving not yet installed.
    pub live: LiveNode<V>,
    /// The drive directory the loop reads the materialization role
    /// from: installed on the loop with the serving surface, so the
    /// vault process retains like its role.
    pub drive_dir: std::path::PathBuf,
    /// The connected control-plane mailbox.
    pub mailbox: M,
    /// The bound bulk source, if the composer bound one.
    pub bulk: Option<B>,
    /// The bound serving endpoint: installed on the loop, closed in
    /// teardown.
    pub serving: ServingEndpoint,
    /// The serving id for the ready line, hex-encoded by the host.
    pub serving_id: String,
    /// The loop's budgets, shared with every other composer through
    /// `for_local_sync`.
    pub config: LiveConfig,
    /// Bounds for the three transport closes.
    pub deadlines: TransportDeadlines,
    /// The process-wide stop latch the loop polls.
    pub stop: &'static AtomicBool,
    /// The bound owner socket, served through the run. The run shuts
    /// it down after the transport tail while the live node (and its
    /// store lock) is still held — the removal must precede the lock
    /// release, or a successor owner binding the same path races this
    /// unlink (F1). Owned here rather than by the composer so the
    /// ordering is structural, not a call-site discipline.
    pub owner_service: crate::owner::OwnerService,
    /// The host-owned event sink (stderr plus log file in
    /// production, a recording vec in tests).
    pub emit: Arc<dyn Fn(VaultEvent) + Send + Sync>,
}

/// Run a headless vault to completion: install the serving surface,
/// supervise the live loop on its own thread under `stop`, wait for
/// the first of loop return or stop trip, close admission for the
/// drain (a vault has no presentation session and therefore no
/// teardown submitter — no session join to bound one), reap the loop
/// thread (its post-return drain has executed everything queued), mop
/// up with `close_admission`, and run the shared transport tail.
/// Returns the loop's end plus the transport outcome; the host maps
/// both to its exit status.
///
/// Install failure returns before the loop starts: the endpoint is
/// dropped unclosed, the mount's behavior on the same failure, since
/// a mirror that never flushed has no residency to drain.
pub fn run_vault<V, M, B>(run: VaultRun<V, M, B>) -> io::Result<VaultOutcome>
where
    V: NamespaceView<Materialization = RuntimeMaterialization> + Send + Sync + 'static,
    V::Store: ObjectStore + DiscardRejectedRepresentation + Send + Sync + 'static,
    <V::Store as ObjectStore>::Error: std::fmt::Debug,
    <V::Store as DiscardRejectedRepresentation>::Error: std::fmt::Debug,
    M: Mailbox + Send + 'static,
    B: RoutePublishing + Send + 'static,
{
    let VaultRun {
        mut live,
        drive_dir,
        mailbox,
        bulk,
        serving,
        serving_id,
        config,
        deadlines,
        stop,
        mut owner_service,
        emit,
    } = run;
    // The surface the loop's barrier reads through is the bound
    // endpoint itself: flush before announcing the address, then
    // publish the route and gate discharge on mirror readiness.
    // The role directory installs beside serving, never through
    // it, so serving-optional compositions cannot forget it.
    install_role_dir(&mut live, &drive_dir);
    install_serving(&mut live, &serving)?;
    // The observer's two ends: completed passes report through the
    // node's hook, failures through the loop's closure. Both ends
    // share one observer, so readiness, posture, and error counts
    // stay one story.
    let (hook, on_error) = VaultObserver::new(emit, serving_id, Instant::now()).shared();
    live.set_pass_hook(hook);
    let supervisor = Supervisor::new(Arc::clone(live.mutations()), stop, Arc::clone(live.waker()));
    // The policy channel shares the loop thread with the mutation
    // queue, so it shares the teardown: close admission with the
    // drain (admitted-before-stop resolves through it), fail
    // close-racers with the mop-up. The owner interface serves until
    // after this run returns, so without this a pin admitted at the
    // stop would hang its connection past the server join.
    let policy = Arc::clone(live.policy_channel());
    let (trigger_tx, trigger_rx) = std::sync::mpsc::channel::<()>();
    let drive = supervisor.spawn_loop(live, mailbox, bulk, config, trigger_tx, on_error);
    // Teardown trigger: the loop's return or the stop trip. The wait
    // polls the process latch in slices because a signal-handler trip
    // cannot notify the channel — the mount's slow-path guarantee.
    loop {
        if stop.load(Ordering::Relaxed) {
            tracing::info!(stage = "teardown", "shutdown latch tripped");
            break;
        }
        match trigger_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(()) => {
                tracing::info!(stage = "teardown", "teardown trigger received");
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                tracing::info!(stage = "teardown", "trigger senders gone");
                break;
            }
        }
    }
    // No session to join: close admission at once, but leave queued
    // requests for the loop thread's post-return drain — an admission
    // made before the stop resolves `Done` through the drain instead
    // of racing `shutdown`'s fail-all for the batch (see
    // `Supervisor::close_for_drain`). The drain terminates on the
    // close (empty plus closed returns promptly), so the join below
    // cannot hang on it.
    supervisor.close_for_drain();
    policy.close();
    tracing::info!(stage = "teardown", "admission closed");
    match drive.join() {
        Ok(returned) => {
            // Mop-up: the drain executed everything queued, so this
            // finds nothing pending and changes nothing. Anything
            // left here would be a close-racer the drain never saw;
            // failing it closed keeps the post-teardown invariant
            // (closed and empty) that `close_admission` documents.
            supervisor.close_admission();
            policy.shutdown();
            let mut mailbox = returned.mailbox;
            let bulk = returned.bulk;
            let _live = returned.live;
            tracing::info!(stage = "teardown", "loop thread joined");
            let transport = shutdown_transport(&mut mailbox, bulk, serving, &deadlines);
            // The interface stops after the tail while the live node
            // (and its store lock) is still held: `_live` drops at
            // the arm's end, after this shutdown removes the socket.
            owner_service.shutdown();
            tracing::info!(stage = "teardown", "owner interface stopped");
            Ok(VaultOutcome {
                loop_end: VaultLoopEnd::Returned(returned.result),
                transport,
            })
        }
        Err(_) => {
            tracing::error!(stage = "sync", "supervised loop thread failed");
            // The drain died with the loop thread, so nothing will ever
            // run the queued requests: fail them now rather than strand
            // their submitters (same mop-up as the join-Ok path).
            supervisor.close_admission();
            policy.shutdown();
            let serving = serving.shutdown(deadlines.serving);
            if let Err(error) = &serving {
                tracing::warn!(stage = "serving", error = %error, "serving shutdown failed");
            }
            owner_service.shutdown();
            tracing::info!(stage = "teardown", "owner interface stopped");
            Ok(VaultOutcome {
                loop_end: VaultLoopEnd::SupervisionLost,
                transport: TransportShutdown { serving },
            })
        }
    }
}

/// The owner view every headless composer builds its node over: the
/// same view the daemon builds its backends from, so the host serves
/// what the backends would serve. Concrete (not generic) on purpose —
/// every owner-mode entry point composes the same node, and the
/// composition check that pins the host's vocabulary can name it.
pub type OwnerView = DriveView<FsObjectStore, RuntimeMaterialization>;

/// What headless owner composition can report. The engine arrives
/// already opened (credentials live in the binaries, never here);
/// everything downstream of the lock is this function's one copy.
#[derive(Debug, thiserror::Error)]
pub enum OwnerError {
    /// The object store beside the drive failed to open.
    #[error("object store failed: {0}")]
    Store(String),
    /// The durable engine half of composition failed.
    #[error(transparent)]
    Engine(#[from] EngineError),
    /// Node composition over the opened store failed.
    #[error(transparent)]
    Node(#[from] NodeError),
    /// A configured retention quota the store already exceeds.
    #[error("retention quota refused at startup: {0}")]
    Retention(#[from] QuotaCheckError),
    /// The application settings file refused to load or apply.
    #[error("application settings refused: {0}")]
    Settings(String),
    /// Serving bind or the supervised run failed at the io boundary.
    #[error("owner run failed: {0}")]
    Io(#[from] io::Error),
}

/// A drive split for live serving with its surface bound: the loop,
/// its backend parts, the drive the loop reads its materialization
/// role from, and the installed serving endpoint with the ready-line
/// id. The parts stay exposed (not dropped) so backends — and tests
/// that submit through the live channel — can hold the queue while
/// the run is in flight.
pub struct ComposedHeadless {
    /// The supervised live loop, serving not yet installed.
    pub live: LiveNode<OwnerView>,
    /// The backend parts: projection, wants, mutation queue.
    pub parts: LiveParts<OwnerView>,
    /// The drive directory the loop reads its materialization role
    /// from.
    pub drive_dir: PathBuf,
    /// The bound serving endpoint, installed on the loop by the run.
    pub serving: ServingEndpoint,
    /// The serving id for the ready line, hex-encoded here so every
    /// composer prints the same rendering.
    pub serving_id: String,
}

/// Compose an opened drive for headless ownership: open the object
/// store, refuse a quota the store already exceeds, compose the node
/// over the daemon view with live heads refreshed, bind the serving
/// surface (loopback for developer runs, real otherwise), and split
/// for live serving. The ONLY headless composition path — `wyrd
/// vault`, `wyrd-host`, and every future owner-mode entry point call
/// this instead of assembling the steps ad hoc. Mailbox connection,
/// bulk binding, and the emit sink stay caller-side: they are
/// transport binding and diagnostics, not composition.
pub fn compose_headless_owner(
    engine: Engine,
    drive_dir: PathBuf,
    loopback: bool,
) -> Result<ComposedHeadless, OwnerError> {
    compose_headless_owner_with_quota(engine, drive_dir, loopback, None)
}

/// Compose an opened drive for headless ownership with a retention
/// ceiling: the same number gates the authoring path and
/// receive-path admission (DG-4 A), with the store accountant wired
/// so the comparison is against a maintained tally. `None` is
/// unlimited (the dogfood default).
///
/// The application settings file supplies what the flag does not:
/// its quota fills a `None` here (the flag wins), and its other
/// budget bounds plus the open timeout apply onto the composition.
/// A corrupt settings file refuses the composition loudly.
pub fn compose_headless_owner_with_quota(
    engine: Engine,
    drive_dir: PathBuf,
    loopback: bool,
    retained_quota: Option<u64>,
) -> Result<ComposedHeadless, OwnerError> {
    let (file_quota, rest) =
        crate::settings::split_file_overrides(&drive_dir).map_err(OwnerError::Settings)?;
    let quota = retained_quota.or(file_quota);
    let (store, quota_config) = match quota {
        Some(limit) => {
            let (config, retained) = LiveConfig::with_retained_quota(limit);
            let store =
                FsObjectStore::open_with(drive_dir.clone(), Some(std::sync::Arc::clone(&retained)))
                    .map_err(|error| OwnerError::Store(error.to_string()))?;
            (store, config)
        }
        None => {
            let store = FsObjectStore::open(drive_dir.clone())
                .map_err(|error| OwnerError::Store(error.to_string()))?;
            (store, LiveConfig::for_local_sync())
        }
    };
    // The quota above already wired its accountant; the remaining
    // file bounds (admission, pass budgets, open timeout) apply
    // here. `split_file_overrides` strips the quota from `rest`,
    // so this cannot rebuild the mismatch.
    let applied = crate::core::apply_budget_settings(
        &quota_config,
        crate::settings::DEFAULT_OPEN_TIMEOUT,
        &rest,
    )
    .map_err(OwnerError::Settings)?;
    let config = applied.config;
    crate::process::check_startup_retention(&config, &store, engine.vault().seen_bytes())?;
    let mut node = WyrdNode::new(engine, store)?;
    node.refresh_live_heads()?;
    let serving = node.open_serving(&drive_dir, loopback)?;
    let serving_id = hex::encode(serving.addr().id.as_bytes());
    let (live, parts) = node.into_live(applied.open_timeout, &config)?;
    Ok(ComposedHeadless {
        live,
        parts,
        drive_dir,
        serving,
        serving_id,
    })
}

/// Run a composed headless owner to completion under `stop`: install
/// the serving surface, serve the drive-owner interface beside the
/// drive, supervise the live loop, wait for the first of loop return
/// or stop trip, close admission for the post-return drain (no
/// presentation session exists to submit through, so nothing new can
/// queue behind the drain), reap the loop thread, mop up with
/// `close_admission`, stop the interface, and run the shared transport
/// tail under the mount's bounds. The ONLY headless run path, next
/// to the ONLY headless composition path above.
///
/// `configured_relays` is the relay count the durable status
/// observation reports: the composer owns the relay configuration,
/// and the loop never constructs a mailbox from it. The interface
/// serves at the drive-directory socket until the run returns; a
/// bind failure refuses the run rather than running deaf.
pub fn run_composed_owner<M, B>(
    composed: ComposedHeadless,
    stop: &'static AtomicBool,
    mailbox: M,
    bulk: Option<B>,
    emit: Arc<dyn Fn(VaultEvent) + Send + Sync>,
    configured_relays: usize,
) -> Result<VaultOutcome, OwnerError>
where
    M: Mailbox + Send + 'static,
    B: RoutePublishing + Send + 'static,
{
    let ComposedHeadless {
        mut live,
        parts,
        drive_dir,
        serving,
        serving_id,
    } = composed;
    live.set_configured_relays(configured_relays);
    // The drive-owner interface serves from here: the shared
    // projection, the loop-published slots, and the single queue,
    // peer-checked at the drive-directory socket. The composer
    // wires the loop's repair queues the way the mount composer
    // wires its backend — through the shared `OwnerService`, so
    // headless and mount bind the same surface. Bind before
    // spawning: an owner that cannot serve its interface refuses
    // the run instead of running deaf. The store lock is held
    // throughout the run, so no competing owner can race the
    // stale-socket reclaim.
    let owner_service = crate::owner::OwnerService::bind(
        crate::owner::OwnerHandles::for_live(
            &parts,
            Arc::clone(live.quarantine_queue()),
            Arc::clone(live.scrub_queue()),
            Arc::clone(live.waker()),
        ),
        &drive_dir,
    )
    .map_err(OwnerError::Io)?;
    // No presentation backend is attached yet: the parts are owned
    // but never served, the queue stays open but nobody submits, so
    // passes only ever see intake and fetch work. The queue handle
    // stays alive here (not dropped with the parts) because the
    // runner clones it for supervision; the caller-held clone (a
    // backend, or a test submitting through the live channel) keeps
    // working through the run.
    let _queue = parts.mutations.clone();
    drop(parts);
    // The service travels into the run so the shutdown order is
    // structural: `run_vault` stops the interface after the tail
    // while the live node (and its store lock) is still held, and
    // the removal precedes the lock release.
    let outcome = run_vault(VaultRun {
        live,
        drive_dir,
        mailbox,
        bulk,
        serving,
        serving_id,
        config: LiveConfig::for_local_sync(),
        deadlines: TransportDeadlines {
            mailbox: crate::process::SHUTDOWN_DEADLINE,
            bulk: wyrd_sync::GRACEFUL_CLOSE_DEADLINE,
            serving: crate::process::TRANSPORT_SHUTDOWN_DEADLINE,
        },
        stop,
        owner_service,
        emit,
    });
    // The loop is reaped; the interface stopped with it, on success
    // and on failure alike — an early run error never detaches a
    // still-serving socket. A submission admitted before the stop
    // resolves through the teardown drain or refuses `Shutdown` at
    // admission close: no consumer waits past this join without a
    // terminal answer. The socket belongs to the run: `run_vault`
    // shut it down after the tail while the live node (and its store
    // lock) was still held, so no later owner inherits a stale entry
    // and no client mistakes an absent owner for a listening one.
    // The store lock releases with the engine drop after this return.
    outcome.map_err(OwnerError::from)
}

#[cfg(test)]
mod tests_shutdown_order {
    //! The quit half of the lifecycle: the transport tail shuts down
    //! in the documented teardown order with every stage bounded.
    //! The session → admission → loop half lives in `lifecycle` (and
    //! its queue-half tests); this pins the tail every composer runs
    //! after its loop joins: mailbox first, bulk second, serving
    //! last — and a failed serving close never skips the closes
    //! before it, because the order below closes them first
    //! unconditionally.
    use super::*;
    use std::sync::Mutex;
    use wyrd_format::{BaoRoot, SnapshotId, StorageId};
    use wyrd_sync::bulk::{BulkError, BulkSource, SealedManifest};
    use wyrd_sync::runtime::{RouteReport, RuntimeState};
    use wyrd_sync::serving::Vault;
    use wyrd_sync::transport::mailbox::{
        Delivery, DeliveryId, Disposition, MailboxEnvelope, MailboxError, SendReport,
    };

    struct OrderMailbox {
        order: Arc<Mutex<Vec<&'static str>>>,
        deadlines: Arc<Mutex<Vec<Duration>>>,
    }

    impl Mailbox for OrderMailbox {
        fn send(&mut self, _envelope: MailboxEnvelope) -> Result<SendReport, MailboxError> {
            unimplemented!("shutdown-order fake never sends")
        }
        fn recv(&mut self) -> Result<Option<Delivery>, MailboxError> {
            unimplemented!("shutdown-order fake never receives")
        }
        fn settle(
            &mut self,
            _id: DeliveryId,
            _disposition: Disposition,
        ) -> Result<(), MailboxError> {
            unimplemented!("shutdown-order fake never settles")
        }
        fn shutdown(&mut self, deadline: Duration) {
            self.order.lock().unwrap().push("mailbox");
            self.deadlines.lock().unwrap().push(deadline);
        }
    }

    struct OrderBulk {
        order: Arc<Mutex<Vec<&'static str>>>,
        deadlines: Arc<Mutex<Vec<Duration>>>,
    }

    impl wyrd_sync::bulk::AttemptBudget for OrderBulk {}

    impl BulkSource for OrderBulk {
        fn fetch_root_manifest(
            &mut self,
            _snapshot: &SnapshotId,
            _max: usize,
        ) -> Result<Option<SealedManifest>, BulkError> {
            unimplemented!("shutdown-order fake never fetches")
        }
        fn fetch_snapshot(
            &mut self,
            _snapshot: &SnapshotId,
            _max: usize,
        ) -> Result<Option<Vec<u8>>, BulkError> {
            unimplemented!("shutdown-order fake never fetches")
        }
        fn fetch_sealed(
            &mut self,
            _storage: &StorageId,
            _max: usize,
        ) -> Result<Option<Vec<u8>>, BulkError> {
            unimplemented!("shutdown-order fake never fetches")
        }
        fn fetch_transport(
            &mut self,
            _root: &BaoRoot,
            _max: usize,
        ) -> Result<Option<Vec<u8>>, BulkError> {
            unimplemented!("shutdown-order fake never fetches")
        }
    }

    impl RoutePublishing for OrderBulk {
        fn publish_routes(&mut self, _state: &RuntimeState) -> Result<RouteReport, EngineError> {
            unimplemented!("shutdown-order fake never publishes")
        }
        fn shutdown(&mut self, deadline: Duration) {
            self.order.lock().unwrap().push("bulk");
            self.deadlines.lock().unwrap().push(deadline);
        }
    }

    /// Quit tears down in the documented order: the transport tail
    /// stops the mailbox under its deadline first, closes bulk
    /// graceful-or-abort second, and closes serving last — every
    /// stage bounded. Serving is a real loopback endpoint (no fake
    /// exists by design), so its position is pinned by recording its
    /// completion after the shared tail returns: the call returns
    /// only after the serving close reports, which follows the
    /// mailbox and bulk closes in the function body. The
    /// session → admission → loop half of the documented order lives
    /// in `lifecycle` (queue-half unit tests) and the Lima mount
    /// legs; this test pins the tail every composer runs after its
    /// loop joins.
    #[test]
    fn quit_tears_down_in_documented_order() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-quit-order-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let vault = Vault::open(&dir).unwrap();
        let serving = ServingEndpoint::open_loopback(&vault, &dir).unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let deadlines = Arc::new(Mutex::new(Vec::new()));
        let mut mailbox = OrderMailbox {
            order: Arc::clone(&order),
            deadlines: Arc::clone(&deadlines),
        };
        let bulk = OrderBulk {
            order: Arc::clone(&order),
            deadlines: Arc::clone(&deadlines),
        };
        // Distinct deadlines per stage: the recording tells which
        // bound each stage ran under, not just that it ran.
        let bounds = TransportDeadlines {
            mailbox: Duration::from_secs(5),
            bulk: Duration::from_secs(7),
            serving: Duration::from_secs(60),
        };
        let outcome = shutdown_transport(&mut mailbox, Some(bulk), serving, &bounds);
        assert!(
            outcome.serving.is_ok(),
            "the serving close runs last and reports"
        );
        order.lock().unwrap().push("serving");
        assert_eq!(
            *order.lock().unwrap(),
            vec!["mailbox", "bulk", "serving"],
            "mailbox stops before bulk closes, and the tail returns only after the serving close"
        );
        assert_eq!(
            *deadlines.lock().unwrap(),
            vec![Duration::from_secs(5), Duration::from_secs(7)],
            "every stage runs under its own bounded deadline"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
