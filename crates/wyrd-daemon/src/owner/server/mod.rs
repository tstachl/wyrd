//! The owner-side socket server: accepts local consumers, checks
//! the peer credential, and dispatches the read operations against
//! the shared serving state. Child 2 adds mutation submission
//! through the single queue on this same transport; child 3 defines
//! the crash answer for requests in flight when the owner dies.
//!
//! Transport (OD-16-B option A): a `UnixListener` on a socket whose
//! path derives from the drive directory
//! ([`super::socket_path_for`]). One transport for macOS, Linux, and tests.
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
//! by that many JSON bytes, capped at [`wyrd_owner_client::transport::MAX_FRAME_BYTES`]. Assembly
//! is explicit: the length prefix and the body accumulate byte by
//! byte, an idle connection re-polls on the read deadline, and any
//! stall past the deadline with a partially assembled frame — or a
//! length past the cap — closes the connection without an answer and
//! without dispatching the partial bytes. The client treats any
//! broken connection as unconfirmed (child 3), never as success or
//! failure.

mod framing;
mod ops;
mod service;
mod socket;

use framing::{read_frame, refuse, write_frame};
pub use service::{OwnerHandles, OwnerService};
pub use socket::{
    bind_listener, default_group_container_with_home, group_container_from_env, group_label_path,
    group_socket_path, group_symlink_path, production_group_container,
    production_group_container_with_home, GroupContainerConfig, DEFAULT_MAX_READ_BYTES,
    FILEPROVIDER_DRIVE_ID_ENV, FILEPROVIDER_GROUP_CONTAINER_ENV, MAX_OWNER_CONNECTIONS,
    MAX_SUBMIT_CONTENT_BYTES, READ_DEMAND_TIMEOUT,
};

// `tests_owner` reaches this helper through the old server path.
#[cfg(test)]
pub(in crate::owner) use framing::bound_read_refusal;

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use super::wire::{
    RawRequest, WireError, WireErrorCode, WireResponse, WireResult, OP_CAPTURE_PARENT,
    OP_DIAGNOSTICS_SET, OP_DIAGNOSTICS_STATUS, OP_FRESHNESS, OP_LIVENESS, OP_LOOKUP, OP_OBSERVE,
    OP_PAUSE, OP_POLICY_EVICT, OP_POLICY_PIN, OP_POLICY_STATUS, OP_POLICY_UNPIN, OP_READ,
    OP_READDIR, OP_RESUME, OP_STAT, OP_SUBMIT, WIRE_VERSION,
};

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
pub(crate) struct ConnectionSlot<'a> {
    active: &'a AtomicUsize,
}

impl Drop for ConnectionSlot<'_> {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Reserve one connection-pool slot atomically: the increment
/// happens inside the same compare-exchange as the bound check, so
/// two accept loops racing on the last slot cannot both win.
/// Returns the guard on success; the guard's drop frees the slot.
pub(crate) fn try_acquire_slot(counter: &AtomicUsize) -> Option<ConnectionSlot<'_>> {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |held| {
            (held < MAX_OWNER_CONNECTIONS).then_some(held + 1)
        })
        .ok()
        .map(|_| ConnectionSlot { active: counter })
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
    /// The shared connection-slot counter: every accept loop on this
    /// server — primary and group — draws from one pool, so the
    /// second listener never doubles the ceiling. A plain counter,
    /// not a shared pointer: the server itself travels behind an
    /// `Arc` where sharing is needed.
    slots: AtomicUsize,
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
            slots: AtomicUsize::new(0),
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
        // The pool counter lives on the server, so every accept loop
        // — primary and group — draws from one budget.
        let slots: &AtomicUsize = &self.slots;
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
                        // connection. The reservation is atomic
                        // (compare-exchange on the shared counter),
                        // so two accept loops can never both observe
                        // a free slot and overshoot the bound.
                        let slot = match try_acquire_slot(slots) {
                            Some(slot) => slot,
                            None => {
                                tracing::warn!(
                                    "owner interface connection refused: pool saturated"
                                );
                                continue;
                            }
                        };
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
                                scope.spawn(move || {
                                    let _slot = slot;
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
            OP_DIAGNOSTICS_SET => self.op_diagnostics_set(params),
            OP_DIAGNOSTICS_STATUS => self.op_diagnostics_status(),
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
}
