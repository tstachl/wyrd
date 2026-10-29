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
//! by that many JSON bytes, capped at [`MAX_FRAME_BYTES`]. A frame
//! that exceeds the cap closes the connection without an answer —
//! the client treats any broken connection as unconfirmed (child 3),
//! never as success or failure.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use wyrd_core::live::{LiveParts, SharedObservation};
use wyrd_core::mutation::MutationQueue;
use wyrd_core::projection::{Projection, SharedFreshness, SharedProjection};
use wyrd_core::quarantine::QuarantineQueue;
use wyrd_core::read::{LiveReader, ReadError};
use wyrd_core::scrub::ScrubQueue;
use wyrd_core::view::{NamespaceView, Node, ViewError};
use wyrd_core::want::{WantError, WantRegistry};

use crate::vault::OwnerView;

use super::wire::{
    PathParams, RawRequest, SubmitParams, WireAttr, WireDirEntry, WireError, WireErrorCode,
    WireFreshness, WireLiveness, WireMailboxHealth, WireMutationError, WireMutationOutcome,
    WireNode, WireOk, WireResponse, WireResult, WireSyncReport, WireSyncStatus, OP_CAPTURE_PARENT,
    OP_FRESHNESS, OP_LIVENESS, OP_LOOKUP, OP_OBSERVE, OP_READ, OP_READDIR, OP_STAT, OP_SUBMIT,
    WIRE_VERSION,
};

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
/// Bounds one request's memory: files larger than this are refused
/// with [`WireErrorCode::TooLarge`], never truncated, never
/// streamed past the bound. Paged reads are a future additive
/// operation, not a silent behavior of this one.
pub const DEFAULT_MAX_READ_BYTES: u64 = 64 << 20;

/// Largest single frame (request or response) in bytes: the read
/// bound plus JSON and hex overhead, rounded up. A frame past this
/// closes the connection; the client treats the break as unconfirmed.
pub const MAX_FRAME_BYTES: usize = 96 << 20;

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
            freshness: Arc::clone(&parts.freshness),
            observation: Arc::clone(&parts.observation),
            quarantine: None,
            scrub: None,
            max_read_bytes: DEFAULT_MAX_READ_BYTES,
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

/// The owner-side interface server: peer-checked dispatch over the
/// shared serving state. Holds handles, never engines: constructing
/// one names no `DurableStore`, no `RuntimeState`, no `Engine`, and
/// the contract suite pins that.
pub struct OwnerServer {
    handles: OwnerHandles,
    /// The uid that bound the server: the only peer credential ever
    /// accepted. Captured at construction from the owner process.
    owner_uid: u32,
}

impl OwnerServer {
    /// Serve the shared handles. The uid check binds to this
    /// process's effective uid — the owner is the process that
    /// holds the store lock, and only its own user may consume it.
    pub fn new(handles: OwnerHandles) -> Self {
        OwnerServer {
            handles,
            owner_uid: super::auth::owner_uid(),
        }
    }

    /// Bind the drive-directory socket and serve until `stop` trips.
    /// A stale socket entry from an unclean shutdown is reclaimed:
    /// the caller holds the store lock (single owner by kernel
    /// enforcement), so no competing binder exists.
    pub fn bind_and_serve(&self, socket_path: &Path, stop: &AtomicBool) -> std::io::Result<()> {
        let _ = std::fs::remove_file(socket_path);
        let listener = UnixListener::bind(socket_path)?;
        listener.set_nonblocking(true)?;
        self.serve_on(&listener, stop);
        let _ = std::fs::remove_file(socket_path);
        Ok(())
    }

    /// Serve an already-bound listener until `stop` trips. The
    /// accept loop polls so a tripped stop ends serving without a
    /// dummy connection: each connection gets its own thread, and
    /// all threads share the serving handles.
    pub fn serve_on(&self, listener: &UnixListener, stop: &AtomicBool) {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    if !self.check_peer(&stream) {
                        continue;
                    }
                    // Accepted sockets inherit the listener's
                    // non-blocking flag: serve each connection
                    // blocking, one thread per connection, so a slow
                    // consumer never spins the accept loop.
                    match stream.set_nonblocking(false) {
                        Ok(()) => self.serve_connection(stream),
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
    /// answers. Any transport failure ends the connection without a
    /// terminal answer — the client treats the break as unconfirmed.
    fn serve_connection(&self, mut stream: UnixStream) {
        loop {
            let frame = match read_frame(&mut stream) {
                Ok(frame) => frame,
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
        match slot.as_ref() {
            Some(observation) => Ok(WireOk::Liveness {
                liveness: WireLiveness {
                    health: WireMailboxHealth::from(&observation.health),
                    report: Some(WireSyncReport::from(&observation.report)),
                    passes: observation.passes,
                },
            }),
            // No pass yet: unknown health, no report, zero passes.
            // "Owner reachable, no recent sync activity" reads
            // exactly like this — distinct from "owner unreachable",
            // which is a refused connection, not a response.
            None => Ok(WireOk::Liveness {
                liveness: WireLiveness {
                    health: WireMailboxHealth::unknown(),
                    report: None,
                    passes: 0,
                },
            }),
        }
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
            Some(token) => Ok(WireOk::ParentToken {
                token: token.wire_value(),
            }),
            None => Err((
                WireErrorCode::NotFound,
                format!("no parent to capture for {:?}", params.path),
            )),
        }
    }

    /// Forward one mutation to the single queue and block for its
    /// terminal answer. The interface interprets nothing: decode
    /// failures refuse at the wire, queue refusals carry through as
    /// distinct outcomes, and a success means exactly what an
    /// in-process submit's success means. A broken connection while
    /// blocked is unconfirmed, never success and never failure
    /// (child 3 owns that answer).
    fn op_submit(&self, params: serde_json::Value) -> Result<WireOk, (WireErrorCode, String)> {
        let params: SubmitParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad submit params: {error}"),
            )
        })?;
        let kind = params
            .kind
            .to_kind()
            .map_err(|message| (WireErrorCode::Malformed, message))?;
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

/// Read one length-delimited frame: u32 big-endian length, then that
/// many JSON bytes. Anything overlong, short, or unreadable ends
/// the connection — the client treats the break as unconfirmed.
fn read_frame(stream: &mut UnixStream) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame exceeds interface bound",
        ));
    }
    let mut frame = vec![0u8; len];
    stream.read_exact(&mut frame)?;
    Ok(frame)
}

/// Write one length-delimited frame.
fn write_frame(stream: &mut UnixStream, bytes: &[u8]) -> std::io::Result<()> {
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
