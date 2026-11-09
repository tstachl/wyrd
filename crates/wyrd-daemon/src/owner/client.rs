//! The shared Rust client for the drive-owner interface: a typed
//! transport over the socket [`OwnerServer`](super::server::OwnerServer)
//! serves, speaking the same framed JSON the server dispatches.
//! Every consumer in this language uses this client — Tauri first —
//! so framing, request ids, refusal mapping, and the unconfirmed
//! answer live in exactly one place instead of one hand-roll per
//! consumer. A consumer in another language implements the same
//! framing against the golden vectors (`vectors.json`), which pin
//! the bytes both sides must agree on.
//!
//! The client forwards and never interprets: it exposes one typed
//! method per interface operation, carries refusals through as data,
//! and treats every broken connection as unconfirmed — never success,
//! never failure. It owns no mutation path of its own: `submit`
//! forwards to the owner's single queue, and reconciliation re-reads
//! through the owner rather than deciding outcomes locally. See
//! `docs/contracts/owner-interface.md` for the protocol contract;
//! this module is its Rust-side transport.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use super::server::{socket_path_for, MAX_FRAME_BYTES};
use super::wire::{
    PathParams, RawRequest, SubmitParams, WireAttr, WireDirEntry, WireError, WireErrorCode,
    WireFreshness, WireLiveness, WireMutationKind, WireMutationOutcome, WireNode, WireOk,
    WireParentToken, WireResponse, WireResult, WireSyncStatus, OP_CAPTURE_PARENT, OP_FRESHNESS,
    OP_LIVENESS, OP_LOOKUP, OP_OBSERVE, OP_PAUSE, OP_READ, OP_READDIR, OP_RESUME, OP_STAT,
    OP_SUBMIT, WIRE_VERSION,
};

/// How long one request waits for the owner's terminal answer. The
/// server's demand-bounded reads and paced dispatches resolve well
/// inside this; a submit blocked behind a draining queue may take
/// longer, in which case the wait expires into [`ClientError::Unconfirmed`]
/// — the outcome is unknown, exactly as a broken connection leaves
/// it — never into an invented success or failure.
pub const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);

/// The client-side answer to one interface request. `Refused` is the
/// only terminal failure: the owner parsed the request and answered
/// with a defined code. Everything transport-shaped is `Unconfirmed`
/// or `Unavailable`, and everything protocol-shaped is
/// `Incompatible` — the four never collapse into each other, because
/// a retry decision depends on which one came back.
#[derive(Debug)]
pub enum ClientError {
    /// No transport: the socket is absent or the connection refused.
    /// The owner is unavailable. Never an empty drive, never a
    /// success, never evidence about any request — no request was
    /// sent.
    Unavailable(String),
    /// The request never got a terminal answer: the connection
    /// broke, a frame tore, the response overran the bound, or the
    /// wait expired. The outcome is unknown. Reconnect — the broken
    /// instance poisons itself and sends nothing more — then
    /// reconcile (re-read through the owner) before deciding whether
    /// retry is safe; a transport failure alone never authorizes
    /// retry and never establishes failure.
    Unconfirmed { op: String, detail: String },
    /// The owner answered with a defined refusal. Terminal: the
    /// request did not commit and will not commit on retry unless the
    /// caller changes what it asked for.
    Refused(WireError),
    /// The peer does not speak this wire version, echoed the wrong
    /// request id, or answered an operation with a result shape that
    /// operation never produces. An explicit incompatibility, never
    /// a generic unavailability.
    Incompatible(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Unavailable(detail) => write!(f, "owner unavailable: {detail}"),
            ClientError::Unconfirmed { op, detail } => {
                write!(f, "unconfirmed {op}: {detail}")
            }
            ClientError::Refused(error) => {
                write!(f, "owner refused ({:?}): {}", error.code, error.message)
            }
            ClientError::Incompatible(detail) => write!(f, "incompatible owner: {detail}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// A typed connection to the independently installed drive owner.
/// One request at a time per connection — the server serves each
/// connection on its own thread, and sequential ids make every
/// answer matchable to its request. Connections are cheap and
/// short-lived; the pool bound counts concurrent connections, so a
/// consumer that pipelines keeps few.
///
/// A connection that reports `Unconfirmed` is broken: framing past
/// that point is unknowable (an unread body, a late answer, a torn
/// frame), so the instance poisons itself and every further call
/// fails without touching the socket. Reconnect with
/// [`OwnerClient::connect`]: recovery is a new connection, never a
/// resumed stream.
pub struct OwnerClient {
    stream: UnixStream,
    next_id: u64,
    timeout: Duration,
    broken: bool,
}

impl std::fmt::Debug for OwnerClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnerClient")
            .field("next_id", &self.next_id)
            .field("timeout", &self.timeout)
            .field("broken", &self.broken)
            .finish()
    }
}

impl OwnerClient {
    /// Connect to the interface socket at an explicit path. A refused
    /// or absent socket is [`ClientError::Unavailable`]: the owner is
    /// unreachable, which says nothing about any drive's contents.
    pub fn connect(socket_path: &Path) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(socket_path).map_err(|error| {
            ClientError::Unavailable(format!("connect {socket_path:?}: {error}"))
        })?;
        stream
            .set_read_timeout(Some(DEFAULT_RESPONSE_TIMEOUT))
            .map_err(|error| ClientError::Unavailable(format!("arm read timeout: {error}")))?;
        stream
            .set_write_timeout(Some(DEFAULT_RESPONSE_TIMEOUT))
            .map_err(|error| ClientError::Unavailable(format!("arm write timeout: {error}")))?;
        Ok(OwnerClient {
            stream,
            next_id: 1,
            timeout: DEFAULT_RESPONSE_TIMEOUT,
            broken: false,
        })
    }

    /// Connect to the interface socket derived from a drive
    /// directory — the same derivation the owner binds. No discovery
    /// service; the path is the convention.
    pub fn connect_drive(drive_dir: &Path) -> Result<Self, ClientError> {
        Self::connect(&socket_path_for(drive_dir))
    }

    /// Wait at most this long for one terminal answer instead of the
    /// default. An expired wait is `Unconfirmed`, never success. The
    /// socket timeouts are armed together with the stored value, and
    /// a failure to arm them fails the builder — the stored timeout
    /// never diverges from the socket.
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self, ClientError> {
        self.timeout = timeout;
        self.stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| ClientError::Unavailable(format!("arm read timeout: {error}")))?;
        self.stream
            .set_write_timeout(Some(timeout))
            .map_err(|error| ClientError::Unavailable(format!("arm write timeout: {error}")))?;
        Ok(self)
    }

    /// Look up one path: the node plus the serving generation and
    /// durable revision it was served from, for the freshness
    /// correlation.
    pub fn lookup(&mut self, path: &str) -> Result<(WireNode, u64, u64), ClientError> {
        match self.transact(
            OP_LOOKUP,
            PathParams {
                path: path.to_string(),
            },
        )? {
            WireOk::Lookup {
                node,
                generation,
                revision,
            } => Ok((node, generation, revision)),
            other => Err(unexpected(OP_LOOKUP, other)),
        }
    }

    /// Stat one path: attributes plus serving generation and durable
    /// revision.
    pub fn stat(&mut self, path: &str) -> Result<(WireAttr, u64, u64), ClientError> {
        match self.transact(
            OP_STAT,
            PathParams {
                path: path.to_string(),
            },
        )? {
            WireOk::Stat {
                attr,
                generation,
                revision,
            } => Ok((attr, generation, revision)),
            other => Err(unexpected(OP_STAT, other)),
        }
    }

    /// List one directory: entries plus serving generation and
    /// durable revision.
    pub fn readdir(&mut self, path: &str) -> Result<(Vec<WireDirEntry>, u64, u64), ClientError> {
        match self.transact(
            OP_READDIR,
            PathParams {
                path: path.to_string(),
            },
        )? {
            WireOk::Readdir {
                entries,
                generation,
                revision,
            } => Ok((entries, generation, revision)),
            other => Err(unexpected(OP_READDIR, other)),
        }
    }

    /// Read one whole file, hex-decoded, plus serving generation and
    /// durable revision. Files past the interface read bound refuse
    /// with `TooLarge` — never truncated, never silently streamed.
    pub fn read(&mut self, path: &str) -> Result<(Vec<u8>, u64, u64), ClientError> {
        match self.transact(
            OP_READ,
            PathParams {
                path: path.to_string(),
            },
        )? {
            WireOk::Read {
                bytes_hex,
                generation,
                revision,
            } => {
                let bytes = decode_read_bytes(&bytes_hex)?;
                Ok((bytes, generation, revision))
            }
            other => Err(unexpected(OP_READ, other)),
        }
    }

    /// The owner's durable observation: what a member may observe,
    /// never liveness. `Unobserved` (no pass completed yet) arrives
    /// as `Refused`, explicitly, never as a healthy-looking default.
    pub fn observe(&mut self) -> Result<WireSyncStatus, ClientError> {
        match self.transact(OP_OBSERVE, serde_json::Value::Null)? {
            WireOk::Observe { status } => Ok(status),
            other => Err(unexpected(OP_OBSERVE, other)),
        }
    }

    /// Transport health plus the last pass report, pinned to the pass
    /// count. A separate object from the durable observation by
    /// contract: a live socket is not a durable fact.
    pub fn liveness(&mut self) -> Result<WireLiveness, ClientError> {
        match self.transact(OP_LIVENESS, serde_json::Value::Null)? {
            WireOk::Liveness { liveness } => Ok(liveness),
            other => Err(unexpected(OP_LIVENESS, other)),
        }
    }

    /// The projection freshness judgment: fresh empty, fresh
    /// non-empty, retained last-good with its refresh failure, or no
    /// trustworthy projection. The consumer tells the four apart
    /// here instead of inferring them from silence.
    pub fn freshness(&mut self) -> Result<WireFreshness, ClientError> {
        match self.transact(OP_FRESHNESS, serde_json::Value::Null)? {
            WireOk::Freshness { freshness } => Ok(freshness),
            other => Err(unexpected(OP_FRESHNESS, other)),
        }
    }

    /// Capture the parent-directory incarnation a create must send
    /// against. `path` is the parent directory that will contain the
    /// create — `""` for the drive root, `"data"` for
    /// `"data/nested.txt"` — not the file path itself. The token is
    /// single-attempt by construction: a pass that replaces the
    /// namespace between capture and submit invalidates it, and the
    /// create refuses `StaleParent`. The consumer discipline is
    /// re-capture and retry. The token binds
    /// the queue counter to the owner instance that minted it:
    /// never reused across a restart, never invented client-side.
    pub fn capture_parent(&mut self, path: &str) -> Result<WireParentToken, ClientError> {
        match self.transact(
            OP_CAPTURE_PARENT,
            PathParams {
                path: path.to_string(),
            },
        )? {
            WireOk::ParentToken { token } => Ok(token),
            other => Err(unexpected(OP_CAPTURE_PARENT, other)),
        }
    }

    /// Forward one mutation to the owner's single queue and block for
    /// its terminal answer. Refusals arrive as `Refused` with their
    /// own codes, never collapsed; a success means exactly what an
    /// in-process submit's success means. A sent request is not a
    /// committed write: only the terminal answer says what happened.
    pub fn submit(&mut self, kind: WireMutationKind) -> Result<WireMutationOutcome, ClientError> {
        match self.transact(OP_SUBMIT, SubmitParams { kind })? {
            WireOk::Submitted { outcome } => Ok(outcome),
            other => Err(unexpected(OP_SUBMIT, other)),
        }
    }

    /// Defer network work and obligation discharge from the next pass.
    /// Idempotent; answers with the switch as it reads back.
    pub fn pause(&mut self) -> Result<bool, ClientError> {
        match self.transact(OP_PAUSE, serde_json::Value::Null)? {
            WireOk::Paused { paused } => Ok(paused),
            other => Err(unexpected(OP_PAUSE, other)),
        }
    }

    /// Return to an ordinary pass. Idempotent; answers with the
    /// switch as it reads back.
    pub fn resume(&mut self) -> Result<bool, ClientError> {
        match self.transact(OP_RESUME, serde_json::Value::Null)? {
            WireOk::Paused { paused } => Ok(paused),
            other => Err(unexpected(OP_RESUME, other)),
        }
    }

    /// Re-read one path after an unconfirmed request touched it. A
    /// missing path is `Ok(None)` — absence as data, not as refusal —
    /// so the caller can reach a definite verdict; any other refusal
    /// propagates, and a transport break stays `Unconfirmed`. What
    /// "committed" means is the caller's to decide from the
    /// re-read — namespace facts for existence and kind, file
    /// identities for content — because only the caller knows what
    /// it sent. A failed re-read never resolves the original
    /// request: it stays unconfirmed. A fold touches many paths:
    /// re-read every one of them, and never treat absence on one
    /// path as proof the fold did not commit.
    pub fn reconcile_lookup(
        &mut self,
        path: &str,
    ) -> Result<Option<(WireNode, u64, u64)>, ClientError> {
        match self.lookup(path) {
            Ok(found) => Ok(Some(found)),
            Err(ClientError::Refused(error)) if error.code == WireErrorCode::NotFound => Ok(None),
            Err(other) => Err(other),
        }
    }

    /// One framed exchange: length-prefixed JSON out, length-prefixed
    /// JSON back, id echoed. Any transport break — refused write,
    /// torn frame, overlong length, expired wait, unparseable answer —
    /// is `Unconfirmed` for this op, never success and never failure,
    /// and poisons the instance: the stream position past a break is
    /// unknowable (an unread body, a late answer, a torn frame), so
    /// every further call fails without touching the socket.
    /// Reconnect for the recovery re-read.
    fn transact<P: serde::Serialize>(
        &mut self,
        op: &str,
        params: P,
    ) -> Result<WireOk, ClientError> {
        if self.broken {
            return Err(ClientError::Unconfirmed {
                op: op.to_string(),
                detail: "the connection broke on an earlier request; reconnect before retrying"
                    .to_string(),
            });
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let params = serde_json::to_value(params)
            .map_err(|error| self.fail(op, format!("encode {op} params: {error}")))?;
        let request = serde_json::to_vec(&RawRequest {
            v: WIRE_VERSION,
            id,
            op: op.to_string(),
            params,
        })
        .map_err(|error| self.fail(op, format!("encode {op} request: {error}")))?;
        if request.len() > MAX_FRAME_BYTES {
            // Nothing went out, so the outcome is certainly absent —
            // but the shape stays Unconfirmed, and the instance still
            // poisons: one error taxonomy, no special cases.
            return Err(self.fail(op, format!("{op} request exceeds frame bound")));
        }
        let len = u32::try_from(request.len())
            .map_err(|error| self.fail(op, format!("frame {op} request: {error}")))?;
        if let Err(error) = self
            .stream
            .write_all(&len.to_be_bytes())
            .and_then(|()| self.stream.write_all(&request))
            .and_then(|()| self.stream.flush())
        {
            return Err(self.fail(op, format!("send {op}: {error}")));
        }
        let mut len_bytes = [0u8; 4];
        if let Err(error) = self.stream.read_exact(&mut len_bytes) {
            return Err(self.fail(op, format!("read {op} response length: {error}")));
        }
        let len = u32::from_be_bytes(len_bytes) as usize;
        if len > MAX_FRAME_BYTES {
            // The body stays unread by design: never allocate what the
            // bound refuses, and never let a later call misread it as
            // a new length.
            return Err(self.fail(
                op,
                format!("{op} response length {len} exceeds frame bound"),
            ));
        }
        let mut frame = vec![0u8; len];
        if let Err(error) = self.stream.read_exact(&mut frame) {
            return Err(self.fail(op, format!("read {op} response body: {error}")));
        }
        let response: WireResponse = serde_json::from_slice(&frame)
            .map_err(|error| self.fail(op, format!("parse {op} response: {error}")))?;
        if response.v != WIRE_VERSION {
            return Err(ClientError::Incompatible(format!(
                "owner answered wire version {}, want {WIRE_VERSION}",
                response.v
            )));
        }
        if response.id != id {
            return Err(ClientError::Incompatible(format!(
                "owner echoed id {}, want {id} for {op}",
                response.id
            )));
        }
        match response.result {
            WireResult::Ok(ok) => Ok(*ok),
            WireResult::Err(error) => Err(ClientError::Refused(error)),
        }
    }

    /// Record a transport break: the outcome is `Unconfirmed` and the
    /// instance is poisoned, so no later call can misread the stream
    /// past this point.
    fn fail(&mut self, op: &str, detail: String) -> ClientError {
        self.broken = true;
        ClientError::Unconfirmed {
            op: op.to_string(),
            detail,
        }
    }
}

/// An answer shape the operation never produces: the peer is not
/// speaking this interface, so the failure is incompatibility, not a
/// refusal of the request.
fn unexpected(op: &str, ok: WireOk) -> ClientError {
    ClientError::Incompatible(format!(
        "owner answered {op} with an unrelated result: {ok:?}"
    ))
}

/// Decode read bytes the way [`OwnerClient::read`] does, for the
/// conformance vectors and any consumer that receives a raw
/// [`WireOk::Read`] through another path. One decoder, not one per
/// consumer.
pub fn decode_read_bytes(bytes_hex: &str) -> Result<Vec<u8>, ClientError> {
    hex::decode(bytes_hex)
        .map_err(|error| ClientError::Incompatible(format!("non-hex read bytes: {error}")))
}

/// Re-exported for consumers that build requests without the client
/// (golden-vector authors, protocol tests): the version every
/// request and response carries.
pub use super::wire::WIRE_VERSION as CLIENT_WIRE_VERSION;
