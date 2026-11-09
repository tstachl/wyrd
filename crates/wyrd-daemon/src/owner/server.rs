//! The owner-side socket server: accepts local consumers, checks
//! the peer credential, and dispatches the read operations against
//! the shared serving state. Child 2 adds mutation submission
//! through the single queue on this same transport; child 3 defines
//! the crash answer for requests in flight when the owner dies.
//!
//! Transport (OD-16-B option A): a `UnixListener` on a socket whose
//! path derives from the drive directory
//! ([`socket_path_for`]). One transport for macOS, Linux, and tests.
//!
//! Authentication (OD-16-B caveat): the peer credential's uid must
//! equal the owner's uid at bind. A connected client holds full
//! interface authority — there is no per-operation ACL because every
//! operation is member-position work under the disclosure bound
//! (OD-11, decided): the interface exposes what a member may
//! observe, nothing vault-visible, nothing keyless-replica. The trust
//! root is the same-user peer plus the owner-only drive directory
//! the socket lives in; the sandbox boundary is the path, not a
//! second credential.
//!
//! Framing: each message is a u32 big-endian length prefix followed
//! by that many JSON bytes, capped at [`MAX_FRAME_BYTES`]. Assembly
//! is explicit: the length prefix and the body accumulate byte by
//! byte, an idle connection re-polls on the read deadline, and any
//! stall past the deadline with a partially assembled frame — or a
//! length past the cap — closes the connection without an answer and
//! without dispatching the partial bytes. The client treats any
//! broken connection as unconfirmed (child 3), never as success or
//! failure.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use wyrd_core::live::{LiveParts, SharedObservation};
use wyrd_core::mutation::MutationQueue;
use wyrd_core::policy_channel::{PolicyChannel, PolicyRequest, PolicyResponse};
use wyrd_core::projection::{Projection, SharedFreshness, SharedProjection};
use wyrd_core::quarantine::QuarantineQueue;
use wyrd_core::read::{LiveReader, ReadError};
use wyrd_core::scrub::ScrubQueue;
use wyrd_core::view::{NamespaceView, Node, ViewError};
use wyrd_core::wake::WakeSignal;
use wyrd_core::want::{WantError, WantRegistry};

use crate::vault::OwnerView;

use super::wire::{
    map_policy_error, PathParams, RawRequest, SubmitParams, WireAttr, WireDirEntry, WireError,
    WireErrorCode, WireFreshness, WireLiveness, WireMailboxHealth, WireMutationError,
    WireMutationKind, WireMutationOutcome, WireNode, WireOk, WireParentToken, WirePolicyRow,
    WireResponse, WireResult, WireSyncReport, WireSyncStatus, OP_CAPTURE_PARENT, OP_FRESHNESS,
    OP_LIVENESS, OP_LOOKUP, OP_OBSERVE, OP_PAUSE, OP_POLICY_EVICT, OP_POLICY_PIN, OP_POLICY_STATUS,
    OP_POLICY_UNPIN, OP_READ, OP_READDIR, OP_RESUME, OP_STAT, OP_SUBMIT, WIRE_VERSION,
};

/// Bind the drive-directory socket: reclaim a stale entry from an
/// unclean shutdown, then bind. The caller holds the store lock
/// (single owner by kernel enforcement), so no competing binder
/// races the reclaim. Failures refuse the run — an owner that
/// cannot serve its interface never runs deaf.
pub fn bind_listener(socket_path: &Path) -> std::io::Result<UnixListener> {
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// The socket file name inside the drive directory. Beside the
/// store, never inside it: the store reads named files, and a
/// socket entry is not a store input. Removed on clean shutdown;
/// a stale entry from an unclean shutdown is unlinked at bind while
/// the store lock is held, so no competing owner can race the
/// reclaim.
pub const OWNER_SOCKET_NAME: &str = "owner.sock";

/// Derive the interface socket path from the drive directory
/// (OD-16-B): no discovery service, the path is the convention, and
/// the File Provider extension reaches it inside the app-group
/// location the drive directory lives in.
pub fn socket_path_for(drive_dir: &Path) -> PathBuf {
    drive_dir.join(OWNER_SOCKET_NAME)
}

/// Largest whole-file read the interface serves in one request.
/// Bounds one request's memory and the response frame together: the
/// hex encoding doubles the bytes on the wire, so 32 MiB reads stay
/// under the frame ceiling with JSON overhead to spare. Files larger
/// than this are refused with [`WireErrorCode::TooLarge`], never
/// truncated, never streamed past the cap. Paged reads are a future
/// additive operation, not a silent behavior of this one.
pub const DEFAULT_MAX_READ_BYTES: u64 = 32 << 20;

/// Largest single frame (request or response) in bytes: the read
/// bound plus JSON and hex overhead, rounded up. A frame past this
/// closes the connection; the client treats the break as unconfirmed.
pub const MAX_FRAME_BYTES: usize = 96 << 20;

/// Largest file content one submit may carry, in decoded bytes: the
/// commit and append bodies of the submit, summed across fold
/// members. The hex encoding doubles the bytes on the wire, so 32
/// MiB of content stays under the frame ceiling with JSON overhead
/// to spare. A submit past this bound is refused with
/// [`WireErrorCode::TooLarge`] before any decode or queue admission
/// — with retry guidance in the message, never silence. A window
/// saves a larger file through the mount, or by chunked submission
/// (several appends or commits, each within the bound), never by a
/// single over-bound submit.
pub const MAX_SUBMIT_CONTENT_BYTES: usize = 32 << 20;

/// Maximum simultaneous owner-interface connections. One thread
/// serves each connection, so the accept loop enforces a small fixed
/// pool: a connection past the bound is closed immediately — the
/// client treats the break as unconfirmed — and an accept flood
/// cannot grow threads without bound. Idle connections hold their
/// slot; consumers keep few, short-lived connections.
pub const MAX_OWNER_CONNECTIONS: usize = 16;

/// How long the read side blocks registering demand before failing
/// a remote-only read. Matches the backend's bounded open: demand
/// waits, it never hangs the consumer past this.
pub const READ_DEMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// The serving handles the owner interface forwards through. Built
/// by the composer from the split parts — never by opening the
/// store, never by constructing a mailbox. Every handle is shared
/// state the loop already publishes; the server only reads.
pub struct OwnerHandles {
    /// The published serving generations.
    pub projection: SharedProjection<OwnerView>,
    /// Demand the read side registers; the loop admits it each pass.
    pub wants: Arc<WantRegistry>,
    /// The single mutation queue the submit side forwards to. The
    /// interface interprets nothing: the queue owns admission,
    /// ordering, and every refusal.
    pub mutations: Arc<MutationQueue>,
    /// The policy channel the four policy operations forward to. The
    /// interface interprets nothing: the loop-thread executor owns
    /// execution, and every refusal carries through as its own code.
    /// Same engine, same thread as the mutation queue — four engine
    /// operations, not mutations.
    pub policy: Arc<PolicyChannel>,
    /// The freshness judgment over the served generation.
    pub freshness: SharedFreshness,
    /// The latest owner observation, if any pass completed.
    pub observation: SharedObservation,
    /// Repair submission for observed-invalid bytes. `None` reads
    /// rejections untouched, like a standalone backend without a
    /// loop to repair through.
    pub quarantine: Option<Arc<QuarantineQueue>>,
    /// Repair submission for observed losses. `None` leaves losses
    /// surfaced, like a standalone backend.
    pub scrub: Option<Arc<ScrubQueue>>,
    /// The loop's pause switch, shared with the loop like the
    /// projection slot (OD-19-D): the owner surface sets it, the loop
    /// reads it once per pass. Process-local only.
    pub paused: Arc<AtomicBool>,
    /// The loop's pacing signal. `set_paused` pokes it so the loop
    /// re-checks promptly instead of waiting out its idle deadline;
    /// `None` (tests that never run a loop) stores bare, which defers
    /// only, never incorrectly.
    pub waker: Option<Arc<WakeSignal>>,
    /// Largest whole-file read served in one request.
    pub max_read_bytes: u64,
}

impl OwnerHandles {
    /// Build from the split parts, with repair channels unwired and
    /// the default read bound. The composer wires repair from the
    /// loop's queues the way the mount composer wires its backend.
    pub fn from_parts(parts: &LiveParts<OwnerView>) -> Self {
        OwnerHandles {
            projection: Arc::clone(&parts.projection),
            wants: Arc::clone(&parts.wants),
            mutations: Arc::clone(&parts.mutations),
            policy: Arc::clone(&parts.policy),
            freshness: Arc::clone(&parts.freshness),
            observation: Arc::clone(&parts.observation),
            quarantine: None,
            scrub: None,
            paused: Arc::clone(&parts.paused),
            waker: None,
            max_read_bytes: DEFAULT_MAX_READ_BYTES,
        }
    }

    /// Wire the loop's pacing signal, like the mount composer wires
    /// its backend beside `shared_with_wants`. The composer always
    /// wires this in production, so `set_paused` pokes; tests that
    /// never run a loop may skip it and store bare.
    pub fn with_waker(mut self, waker: Arc<WakeSignal>) -> Self {
        self.waker = Some(waker);
        self
    }

    /// Set the pause switch with the pacing-signal poke, mirroring
    /// [`LiveNode::set_paused`](wyrd_core::live::LiveNode::set_paused):
    /// the loop re-checks promptly rather than waiting out its idle
    /// deadline. Idempotent; the last writer wins.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
        if let Some(waker) = &self.waker {
            waker.wake();
        }
    }

    /// Wire the loop's repair queues, like the mount composer wires
    /// its backend beside `shared_with_wants`.
    pub fn with_repair(mut self, quarantine: Arc<QuarantineQueue>, scrub: Arc<ScrubQueue>) -> Self {
        self.quarantine = Some(quarantine);
        self.scrub = Some(scrub);
        self
    }
}

/// How long a connection read waits before re-checking the stop
/// flag: bounded shutdown polling, not a client deadline. A slow
/// consumer never stalls shutdown past this plus its in-flight
/// dispatch.
pub const CONN_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long one response write may block: large reads move tens of
/// megabytes, so the writer gets a generous bound rather than the
/// poll interval. A stalled writer still fails closed instead of
/// holding its thread forever.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// A held connection-pool slot, released back to the accept loop
/// on drop: the served-thread count is the live pool size, so the
/// gate in `serve_on` always observes the truth.
struct ConnectionSlot<'a> {
    active: &'a AtomicUsize,
}

impl Drop for ConnectionSlot<'_> {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The owner-side interface server: peer-checked dispatch over the
/// shared serving state. Holds handles, never engines: constructing
/// one names no `DurableStore`, no `RuntimeState`, no `Engine`, and
/// the contract suite pins that.
pub struct OwnerServer {
    handles: OwnerHandles,
    /// The uid that bound the server: the only peer credential ever
    /// accepted. Captured at construction from the owner process.
    owner_uid: u32,
    /// The owner-instance identity minted at construction: parent
    /// tokens carry it, and a token from a previous instance refuses
    /// before reaching the queue. Fresh randomness per server, so a
    /// restart never honors the previous run's captures.
    instance: u64,
}

impl OwnerServer {
    /// Serve the shared handles. The uid check binds to this
    /// process's effective uid — the owner is the process that
    /// holds the store lock, and only its own user may consume it.
    pub fn new(handles: OwnerHandles) -> Self {
        let mut bytes = [0u8; 8];
        let instance = getrandom::getrandom(&mut bytes)
            .map(|()| u64::from_ne_bytes(bytes))
            .unwrap_or_else(|_| {
                // Entropy unavailable: pid plus clock still separates
                // runs in practice, and a collision only risks a
                // stale-token refusal, never a wrong-directory
                // create — the queue re-checks every token.
                std::process::id() as u64
                    ^ std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|age| age.as_nanos() as u64)
                        .unwrap_or(0)
            });
        OwnerServer {
            handles,
            owner_uid: super::auth::owner_uid(),
            instance,
        }
    }

    /// The owner-instance identity this server mints into tokens.
    /// Test and diagnostic use: a consumer never needs it, since a
    /// token from any other instance refuses on use.
    pub fn instance(&self) -> u64 {
        self.instance
    }

    /// Bind the drive-directory socket and serve until `stop` trips.
    /// A stale socket entry from an unclean shutdown is reclaimed:
    /// the caller holds the store lock (single owner by kernel
    /// enforcement), so no competing binder exists.
    pub fn bind_and_serve(&self, socket_path: &Path, stop: &AtomicBool) -> std::io::Result<()> {
        let listener = bind_listener(socket_path)?;
        self.serve_on(&listener, stop);
        let _ = std::fs::remove_file(socket_path);
        Ok(())
    }

    /// Serve an already-bound listener until `stop` trips. The
    /// accept loop polls so a tripped stop ends serving without a
    /// dummy connection. Each connection gets its own thread under
    /// a scoped join, bounded by [`MAX_OWNER_CONNECTIONS`]: a
    /// connection past the bound is closed immediately instead of
    /// spawning, so an accept flood cannot grow threads without
    /// bound. All served threads share the serving handles, every
    /// read re-checks the stop flag inside the poll interval, and
    /// the scope joins every connection before returning — no
    /// shutdown blocks past one poll behind an in-flight dispatch.
    /// A dispatch blocked in the queue resolves through the run's
    /// own teardown (teardown drain or admission close), so the
    /// join is bounded by the run, never by a silent waiter.
    pub fn serve_on(&self, listener: &UnixListener, stop: &AtomicBool) {
        let active = AtomicUsize::new(0);
        // Shared by the gate and every spawned guard: a shared
        // reference, so each `move` closure copies it instead of
        // moving the counter.
        let slots = &active;
        std::thread::scope(|scope| {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if !self.check_peer(&stream) {
                            continue;
                        }
                        // The pool gate, before any thread exists: an
                        // excess connection closes on drop,
                        // unconfirmed like every other broken
                        // connection. The accept loop is the only
                        // spawner, so a check-then-count here never
                        // overshoots — concurrent exits only free
                        // slots.
                        if slots.load(Ordering::Relaxed) >= MAX_OWNER_CONNECTIONS {
                            tracing::warn!("owner interface connection refused: pool saturated");
                            continue;
                        }
                        // Accepted sockets inherit the listener's
                        // non-blocking flag: serve each connection
                        // blocking with timeouts, one thread per
                        // connection, so a slow consumer never spins
                        // the accept loop and never stalls shutdown
                        // past the poll interval.
                        match stream.set_nonblocking(false) {
                            Ok(()) => {
                                let _ = stream.set_read_timeout(Some(CONN_POLL_INTERVAL));
                                let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
                                slots.fetch_add(1, Ordering::Relaxed);
                                scope.spawn(move || {
                                    let _slot = ConnectionSlot { active: slots };
                                    self.serve_connection(stream, stop)
                                });
                            }
                            Err(error) => {
                                tracing::warn!(error = %error, "owner interface connection setup failed");
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "owner interface accept failed");
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        })
    }

    /// The whole authentication check (OD-16-B caveat): the peer's
    /// uid must equal the owner's uid at bind. Anything else is
    /// dropped and logged — never answered, never probed further.
    /// Same-user is the complete credential: the drive directory is
    /// owner-only, and a connected client holds full member-position
    /// authority, so the check answers "is this our owner process's
    /// user", not "which operation may this identity invoke".
    fn check_peer(&self, stream: &UnixStream) -> bool {
        match super::auth::peer_uid(stream) {
            Ok(uid) if uid == self.owner_uid => true,
            Ok(uid) => {
                tracing::warn!(
                    peer_uid = uid,
                    owner_uid = self.owner_uid,
                    "owner interface refused foreign-uid peer"
                );
                false
            }
            Err(error) => {
                tracing::warn!(error = %error, "owner interface refused peer without credentials");
                false
            }
        }
    }

    /// Serve one connection to close: read frames, dispatch, write
    /// answers. Idle reads re-check the stop flag inside the poll
    /// interval, so a tripped stop ends an idle connection without
    /// waiting out a dispatch. A torn frame, an overlong length, or
    /// any transport failure ends the connection without a terminal
    /// answer — the client treats the break as unconfirmed, and a
    /// half-read frame is never dispatched.
    fn serve_connection(&self, mut stream: UnixStream, stop: &AtomicBool) {
        loop {
            let frame = match read_frame(&mut stream) {
                Ok(Some(frame)) => frame,
                // An idle read deadline is the stop poll, not a
                // failure: idle while stopping ends the connection,
                // idle while serving waits for the next request.
                Ok(None) => {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    continue;
                }
                // A partially assembled frame at the deadline, an
                // overlong length, or any transport failure: close
                // deterministically, still unconfirmed, never a
                // half-read dispatch.
                Err(_) => return,
            };
            let response = self.dispatch_frame(&frame);
            let bytes = match serde_json::to_vec(&response) {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(error = %error, "owner interface response encode failed");
                    return;
                }
            };
            if write_frame(&mut stream, &bytes).is_err() {
                return;
            }
        }
    }

    /// Parse one frame and dispatch it. An unparseable frame has no
    /// id to echo, so the connection closes without an answer — the
    /// one case that produces no refusal, because there is no
    /// request to refuse.
    fn dispatch_frame(&self, frame: &[u8]) -> WireResponse {
        let request: RawRequest = match serde_json::from_slice(frame) {
            Ok(request) => request,
            Err(_) => {
                return WireResponse {
                    v: WIRE_VERSION,
                    id: 0,
                    result: WireResult::Err(WireError {
                        code: WireErrorCode::Malformed,
                        message: "request is not a valid interface request".to_string(),
                    }),
                };
            }
        };
        self.dispatch(request)
    }

    /// Dispatch a parsed request. Public so tests drive dispatch
    /// without a socket, and so the contract suite names the exact
    /// surface under test.
    pub fn dispatch(&self, request: RawRequest) -> WireResponse {
        let RawRequest { v, id, op, params } = request;
        if v != WIRE_VERSION {
            return refuse(
                id,
                WireErrorCode::UnknownVersion,
                format!("unsupported wire version {v}, want {}", WIRE_VERSION),
            );
        }
        let result = match op.as_str() {
            OP_LOOKUP => self.op_lookup(params),
            OP_STAT => self.op_stat(params),
            OP_READDIR => self.op_readdir(params),
            OP_READ => self.op_read(params),
            OP_OBSERVE => self.op_observe(),
            OP_LIVENESS => self.op_liveness(),
            OP_FRESHNESS => self.op_freshness(),
            OP_SUBMIT => self.op_submit(params),
            OP_CAPTURE_PARENT => self.op_capture_parent(params),
            OP_PAUSE => self.op_pause(),
            OP_RESUME => self.op_resume(),
            OP_POLICY_PIN => self.op_policy_pin(params),
            OP_POLICY_UNPIN => self.op_policy_unpin(params),
            OP_POLICY_EVICT => self.op_policy_evict(params),
            OP_POLICY_STATUS => self.op_policy_status(params),
            unknown => Err((
                WireErrorCode::UnsupportedOperation,
                format!("unsupported operation {unknown:?}"),
            )),
        };
        WireResponse {
            v: WIRE_VERSION,
            id,
            result: match result {
                Ok(ok) => WireResult::Ok(Box::new(ok)),
                Err((code, message)) => WireResult::Err(WireError { code, message }),
            },
        }
    }

    /// The current published generation, cloned under a short read
    /// lock. Lock-free after the clone: the generation is immutable,
    /// so a slow consumer pins its own complete snapshot without
    /// blocking publication — including bounded demand waits, which
    /// must never hold the publication lock.
    fn current_projection(&self) -> Result<Arc<Projection<OwnerView>>, (WireErrorCode, String)> {
        self.handles
            .projection
            .read()
            .map(|slot| Arc::clone(&slot))
            .map_err(|_| {
                (
                    WireErrorCode::Poisoned,
                    "serving projection lock poisoned".to_string(),
                )
            })
    }

    fn op_lookup(&self, params: serde_json::Value) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad lookup params: {error}"),
            )
        })?;
        let projection = self.current_projection()?;
        let node = projection
            .view()
            .lookup(&params.path)
            .map_err(map_view_error)?;
        Ok(WireOk::Lookup {
            node: WireNode::from(&node),
            generation: projection.generation(),
            revision: projection.revision(),
        })
    }

    fn op_stat(&self, params: serde_json::Value) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad stat params: {error}"),
            )
        })?;
        let projection = self.current_projection()?;
        let attr = projection
            .view()
            .stat(&params.path)
            .map_err(map_view_error)?;
        Ok(WireOk::Stat {
            attr: WireAttr::from(&attr),
            generation: projection.generation(),
            revision: projection.revision(),
        })
    }

    fn op_readdir(&self, params: serde_json::Value) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad readdir params: {error}"),
            )
        })?;
        let projection = self.current_projection()?;
        let node = projection
            .view()
            .lookup(&params.path)
            .map_err(map_view_error)?;
        let entries = projection.view().readdir(&node).map_err(map_view_error)?;
        Ok(WireOk::Readdir {
            entries: entries.iter().map(WireDirEntry::from).collect(),
            generation: projection.generation(),
            revision: projection.revision(),
        })
    }

    fn op_read(&self, params: serde_json::Value) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad read params: {error}"),
            )
        })?;
        let projection = self.current_projection()?;
        // Bound one request's memory before touching the bytes: a
        // file past the bound is refused explicitly, never
        // truncated and never streamed past the cap.
        let size = match projection
            .view()
            .lookup(&params.path)
            .map_err(map_view_error)?
        {
            Node::File { size, .. } => size,
            // Symlinks resolve at the presentation boundary and
            // conflicts address through version-qualified paths: a
            // non-file never reads, and names the real failure. Same
            // shape as the demand reader's own guard.
            node => match projection.view().open_file(&node) {
                Err(error) => return Err(map_view_error(error)),
                Ok(_) => {
                    return Err((
                        WireErrorCode::NotAFile,
                        format!("{} is not a file", params.path),
                    ));
                }
            },
        };
        if size > self.handles.max_read_bytes {
            return Err((
                WireErrorCode::TooLarge,
                format!(
                    "file is {size} bytes, interface read bound is {}",
                    self.handles.max_read_bytes
                ),
            ));
        }
        let mut reader =
            LiveReader::new(READ_DEMAND_TIMEOUT).with_wants(Arc::clone(&self.handles.wants));
        match (&self.handles.quarantine, &self.handles.scrub) {
            (Some(quarantine), Some(scrub)) => {
                reader = reader.with_repair(Arc::clone(quarantine), Arc::clone(scrub));
            }
            (Some(quarantine), None) => {
                reader = reader.with_quarantine(Arc::clone(quarantine));
            }
            (None, Some(scrub)) => {
                reader = reader.with_scrub(Arc::clone(scrub));
            }
            (None, None) => {}
        }
        let bytes = reader
            .read_file(projection.view(), &params.path)
            .map_err(map_read_error)?;
        Ok(WireOk::Read {
            bytes_hex: hex::encode(&bytes),
            generation: projection.generation(),
            revision: projection.revision(),
        })
    }

    fn op_observe(&self) -> Result<WireOk, (WireErrorCode, String)> {
        let slot = self.handles.observation.read().map_err(|_| {
            (
                WireErrorCode::Poisoned,
                "owner observation lock poisoned".to_string(),
            )
        })?;
        match slot.as_ref() {
            Some(observation) => Ok(WireOk::Observe {
                status: WireSyncStatus::from(&observation.status),
            }),
            // Reachable owner, no completed pass: explicit, never a
            // healthy-looking default.
            None => Err((
                WireErrorCode::Unobserved,
                "owner reachable, no sync pass completed yet".to_string(),
            )),
        }
    }

    fn op_liveness(&self) -> Result<WireOk, (WireErrorCode, String)> {
        let slot = self.handles.observation.read().map_err(|_| {
            (
                WireErrorCode::Poisoned,
                "owner observation lock poisoned".to_string(),
            )
        })?;
        let paused = self.handles.paused.load(Ordering::Relaxed);
        match slot.as_ref() {
            Some(observation) => Ok(WireOk::Liveness {
                liveness: WireLiveness {
                    health: WireMailboxHealth::from(&observation.health),
                    report: Some(WireSyncReport::from(&observation.report)),
                    passes: observation.passes,
                    paused,
                },
            }),
            // No pass yet: unknown health, no report, zero passes.
            // "Owner reachable, no recent sync activity" reads
            // exactly like this — distinct from "owner unreachable",
            // which is a refused connection, not a response, and
            // from paused, which sets the flag below.
            None => Ok(WireOk::Liveness {
                liveness: WireLiveness {
                    health: WireMailboxHealth::unknown(),
                    report: None,
                    passes: 0,
                    paused,
                },
            }),
        }
    }

    /// Set the pause switch: network work and obligation discharge
    /// defer from the next pass; local authoring and serving continue.
    /// Idempotent — pausing twice is one pause — and the answer echoes
    /// the switch as it reads back, so the consumer never infers state
    /// from silence.
    fn op_pause(&self) -> Result<WireOk, (WireErrorCode, String)> {
        self.handles.set_paused(true);
        Ok(WireOk::Paused {
            paused: self.handles.paused.load(Ordering::Relaxed),
        })
    }

    /// Clear the pause switch: the next pass is ordinary. Idempotent —
    /// resuming a running drive changes nothing — and the answer echoes
    /// the switch as it reads back.
    fn op_resume(&self) -> Result<WireOk, (WireErrorCode, String)> {
        self.handles.set_paused(false);
        Ok(WireOk::Paused {
            paused: self.handles.paused.load(Ordering::Relaxed),
        })
    }

    fn op_freshness(&self) -> Result<WireOk, (WireErrorCode, String)> {
        let slot = self.handles.freshness.read().map_err(|_| {
            (
                WireErrorCode::Poisoned,
                "freshness lock poisoned".to_string(),
            )
        })?;
        Ok(WireOk::Freshness {
            freshness: WireFreshness::from(&*slot),
        })
    }

    /// Capture the current parent-directory incarnation for a later
    /// create: the consumer sends the token back with the operation,
    /// and the owner re-checks it before authoring. A path with no
    /// token refuses here — creates address paths through tokens,
    /// never bare.
    fn op_capture_parent(
        &self,
        params: serde_json::Value,
    ) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad capture-parent params: {error}"),
            )
        })?;
        match self.handles.mutations.capture_parent(&params.path) {
            Ok(Some(token)) => Ok(WireOk::ParentToken {
                token: WireParentToken {
                    instance: self.instance,
                    counter: token.wire_value(),
                },
            }),
            Ok(None) => Err((
                WireErrorCode::NotFound,
                format!("no parent to capture for {:?}", params.path),
            )),
            // A poisoned token table is not a missing parent: name it
            // so the desktop surfaces a fault instead of retrying a
            // stale-parent loop forever.
            Err(error) => {
                let mapped = WireMutationError::from(&error);
                Err((mapped.code, mapped.message))
            }
        }
    }

    /// Forward one mutation to the single queue and block for its
    /// terminal answer. The interface interprets nothing: decode
    /// failures refuse at the wire, queue refusals carry through as
    /// distinct outcomes, and a success means exactly what an
    /// in-process submit's success means. A broken connection while
    /// blocked is unconfirmed, never success and never failure
    /// (child 3 owns that answer). A submit past the content bound
    /// refuses here with `TooLarge` — legibly, with retry guidance —
    /// before any decode or queue admission, so an over-limit submit
    /// never meets the framing silence of an overlong frame.
    fn op_submit(&self, params: serde_json::Value) -> Result<WireOk, (WireErrorCode, String)> {
        let params: SubmitParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad submit params: {error}"),
            )
        })?;
        let carried = submit_content_bytes(&params.kind);
        if carried > MAX_SUBMIT_CONTENT_BYTES as u64 {
            return Err((
                WireErrorCode::TooLarge,
                format!(
                    "submit carries {carried} content bytes, over the {} interface submit bound: split into smaller submits or save through the mount",
                    MAX_SUBMIT_CONTENT_BYTES
                ),
            ));
        }
        let kind = params.kind.to_kind(self.instance)?;
        match self.handles.mutations.submit(kind) {
            Ok(outcome) => Ok(WireOk::Submitted {
                outcome: WireMutationOutcome::from(&outcome),
            }),
            Err(error) => {
                let mapped = WireMutationError::from(&error);
                Err((mapped.code, mapped.message))
            }
        }
    }

    /// Forward one pin to the loop-thread policy executor and block
    /// for its terminal answer. The interface interprets nothing:
    /// decode failures refuse at the wire, policy refusals carry
    /// through as distinct codes, and a success means exactly what an
    /// in-process pin's success means.
    fn op_policy_pin(&self, params: serde_json::Value) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad policy-pin params: {error}"),
            )
        })?;
        match self
            .handles
            .policy
            .submit(PolicyRequest::Pin { path: params.path })
        {
            Ok(PolicyResponse::Pinned(report)) => Ok(WireOk::PolicyPinned {
                pinned: report.pinned,
                already_pinned: report.already_pinned,
                files: report.files,
                dirs: report.dirs,
            }),
            Ok(_) => Err((
                WireErrorCode::Engine,
                "policy channel answered pin with the wrong outcome".to_string(),
            )),
            Err(error) => Err(map_policy_error(error)),
        }
    }

    /// Forward one unpin to the loop-thread policy executor and block
    /// for its terminal answer. Releasing a promise never refuses —
    /// the executor owns that invariant, the interface only carries
    /// it.
    fn op_policy_unpin(
        &self,
        params: serde_json::Value,
    ) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad policy-unpin params: {error}"),
            )
        })?;
        match self
            .handles
            .policy
            .submit(PolicyRequest::Unpin { path: params.path })
        {
            Ok(PolicyResponse::Unpinned(report)) => Ok(WireOk::PolicyUnpinned {
                released: report.released,
                already_unpinned: report.already_unpinned,
                files: report.files,
                dirs: report.dirs,
            }),
            Ok(_) => Err((
                WireErrorCode::Engine,
                "policy channel answered unpin with the wrong outcome".to_string(),
            )),
            Err(error) => Err(map_policy_error(error)),
        }
    }

    /// Forward one evict to the loop-thread policy executor and block
    /// for its terminal answer. A pinned subtree refuses with
    /// `PinnedPresent` before committing anything — narrowing an
    /// explicit promise piecemeal never happens silently.
    fn op_policy_evict(
        &self,
        params: serde_json::Value,
    ) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad policy-evict params: {error}"),
            )
        })?;
        match self
            .handles
            .policy
            .submit(PolicyRequest::Evict { path: params.path })
        {
            Ok(PolicyResponse::Evicted(report)) => Ok(WireOk::PolicyEvicted {
                released: report.released,
                already_remote: report.already_remote,
                files: report.files,
                dirs: report.dirs,
            }),
            Ok(_) => Err((
                WireErrorCode::Engine,
                "policy channel answered evict with the wrong outcome".to_string(),
            )),
            Err(error) => Err(map_policy_error(error)),
        }
    }

    /// Answer one on-demand status read from the loop-thread executor.
    /// No periodic census exists: every status is a bounded walk at
    /// request time, with a `complete` flag naming whether the
    /// traversal budget covered the subtree. The serving generation
    /// and durable revision ride the answer so the consumer correlates
    /// it with the freshness judgment instead of assuming recency.
    fn op_policy_status(
        &self,
        params: serde_json::Value,
    ) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad policy-status params: {error}"),
            )
        })?;
        let projection = self.current_projection()?;
        let generation = projection.generation();
        let revision = projection.revision();
        match self
            .handles
            .policy
            .submit(PolicyRequest::Status { path: params.path })
        {
            Ok(PolicyResponse::Status(bounded)) => Ok(WireOk::PolicyStatus {
                files: bounded
                    .census
                    .files
                    .iter()
                    .map(WirePolicyRow::from)
                    .collect(),
                conflicts: bounded.census.conflicts.clone(),
                dirs: bounded.census.dirs,
                symlinks_skipped: bounded.census.symlinks_skipped,
                complete: bounded.complete,
                generation,
                revision,
            }),
            Ok(_) => Err((
                WireErrorCode::Engine,
                "policy channel answered status with the wrong outcome".to_string(),
            )),
            Err(error) => Err(map_policy_error(error)),
        }
    }
}

/// File content one submit carries, in decoded bytes: the commit
/// and append bodies, summed across fold members (recursively — a
/// nested fold is refused later as malformed, but its bytes still
/// count here). Measured on the hex length so an over-bound submit
/// refuses before any decode allocates. Saturating: the sum names a
/// magnitude for the refusal message, never an exact ledger.
fn submit_content_bytes(kind: &WireMutationKind) -> u64 {
    match kind {
        WireMutationKind::CommitFile { content_hex, .. }
        | WireMutationKind::AppendFile { content_hex, .. } => (content_hex.len() / 2) as u64,
        WireMutationKind::Fold { members } => members
            .iter()
            .map(|member| submit_content_bytes(&member.kind))
            .fold(0u64, |total, carried| total.saturating_add(carried)),
        _ => 0,
    }
}

/// Build a refusal response to a parsed request.
fn refuse(id: u64, code: WireErrorCode, message: String) -> WireResponse {
    WireResponse {
        v: WIRE_VERSION,
        id,
        result: WireResult::Err(WireError { code, message }),
    }
}

/// Map a view failure to a wire refusal. Every variant names its
/// own code — the transport never reinterprets a failure, it carries
/// it.
fn map_view_error(error: ViewError) -> (WireErrorCode, String) {
    let message = error.to_string();
    let code = match &error {
        ViewError::NotFound => WireErrorCode::NotFound,
        ViewError::InvalidPath => WireErrorCode::Malformed,
        ViewError::NotADirectory => WireErrorCode::NotADirectory,
        ViewError::NotAFile => WireErrorCode::NotAFile,
        ViewError::Conflict => WireErrorCode::Conflicted,
        ViewError::NotMaterialized { .. } => WireErrorCode::NotMaterialized,
        ViewError::Unavailable { .. } => WireErrorCode::Unavailable,
        ViewError::Corrupt => WireErrorCode::Corrupt,
        ViewError::RejectedRepresentation { .. } => WireErrorCode::RejectedRepresentation,
        ViewError::LostRepresentation { .. } => WireErrorCode::LostRepresentation,
        ViewError::Store(..) => WireErrorCode::Store,
    };
    (code, message)
}

/// Map a demand-driven read failure to a wire refusal. Timeouts and
/// saturated demand are transient — the message says retry — while
/// terminal verdicts carry the view's own code.
fn map_read_error(error: ReadError) -> (WireErrorCode, String) {
    match error {
        ReadError::View(error) => map_view_error(error),
        ReadError::Timeout { content } => (
            WireErrorCode::Timeout,
            format!("fetch demand expired for {content}, retry"),
        ),
        ReadError::Terminal(error) => map_view_error(error),
        ReadError::Demand(WantError::Saturated) => (
            WireErrorCode::Saturated,
            "demand registry saturated, retry".to_string(),
        ),
        ReadError::Demand(WantError::TimedOut) => (
            WireErrorCode::Timeout,
            "demand wait expired, retry".to_string(),
        ),
        ReadError::Demand(WantError::Lock) => (
            WireErrorCode::Poisoned,
            "demand registry lock poisoned".to_string(),
        ),
    }
}

/// Read one length-delimited frame: u32 big-endian length, then
/// that many JSON bytes. Returns `Ok(None)` on an idle read deadline
/// at a frame boundary — the caller re-checks its stop flag and
/// polls again. Anything short of a complete frame ends the
/// connection with `Err`: a peer that goes quiet mid-frame (a short
/// length prefix or a short body at the deadline), a vanished peer,
/// an overlong length, or any transport error. The connection closes
/// without an answer and without dispatching the partial bytes — the
/// client treats the break as unconfirmed, and the next connection
/// starts at a fresh frame boundary, never mid-stream.
fn read_frame(stream: &mut UnixStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    let mut prefix_read = 0;
    while prefix_read < len.len() {
        match stream.read(&mut len[prefix_read..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "owner interface peer closed mid-frame",
                ));
            }
            Ok(n) => prefix_read += n,
            Err(error)
                if error.kind() == std::io::ErrorKind::TimedOut
                    || error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                if prefix_read == 0 {
                    return Ok(None);
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "owner interface frame stalled mid-length; closing",
                ));
            }
            Err(error) => return Err(error),
        }
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame exceeds interface bound",
        ));
    }
    let mut frame = vec![0u8; len];
    let mut body_read = 0;
    while body_read < len {
        match stream.read(&mut frame[body_read..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "owner interface peer closed mid-frame",
                ));
            }
            Ok(n) => body_read += n,
            // The length prefix is already consumed: a stall here is
            // a torn frame however many body bytes arrived, including
            // none — idleness only polls at a frame boundary, never
            // mid-frame.
            Err(error)
                if error.kind() == std::io::ErrorKind::TimedOut
                    || error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "owner interface frame stalled mid-body; closing",
                ));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(Some(frame))
}

/// Write one length-delimited frame.
/// Write one length-delimited frame. A response past the frame
/// ceiling never goes out: the connection closes instead, and the
/// client treats the break as unconfirmed. The read bound keeps
/// legitimate responses under the ceiling; this check is the
/// backstop that makes the bound load-bearing rather than
/// aspirational.
fn write_frame(stream: &mut UnixStream, bytes: &[u8]) -> std::io::Result<()> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "response exceeds frame bound",
        ));
    }
    let len = u32::try_from(bytes.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "response exceeds frame bound",
        )
    })?;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()
}
