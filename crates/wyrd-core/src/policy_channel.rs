//! The policy channel: the owner interface's typed path to the
//! loop-thread policy executor (OD-21, check 2).
//!
//! Four engine operations — pin, unpin, evict, status — run on the
//! same engine, on the same loop thread, as the mutation queue. They
//! are not `MutationKind` members: the queue drags parent-token,
//! fold, and projection machinery that policy must never touch, and
//! the issue frames these as "four engine operations, not
//! mutations". A dedicated typed channel keeps that separation: the
//! interface interprets nothing, the loop executes everything, and
//! there is still exactly one writer (the loop owns `&mut Engine`).
//!
//! Mechanics mirror the mutation queue deliberately: the consumer
//! submits and blocks, the loop drains and completes. No second
//! mutation path, no new authority, no read-side mailbox — status
//! reads run on demand against the loop's engine and its published
//! generation, under a per-request traversal budget, and carry a
//! `complete` flag plus the serving generation/revision.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use wyrd_sync::runtime::EngineError;

use crate::policy::{BoundedCensus, EvictReport, PinReport, PolicyError, UnpinReport};
use crate::wake::WakeSignal;

/// What the loop executes: one of the four policy operations. Paths
/// are drive-relative (`""` addresses the root); the status walk is
/// bounded by the implementer constant at execution time, never by a
/// client-supplied budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyRequest {
    Pin { path: String },
    Unpin { path: String },
    Evict { path: String },
    Status { path: String },
}

/// What the loop answers: the report the operation committed, or the
/// bounded census the status walk produced.
#[derive(Debug)]
pub enum PolicyResponse {
    Pinned(PinReport),
    Unpinned(UnpinReport),
    Evicted(EvictReport),
    Status(BoundedCensus),
}

fn channel_closed() -> PolicyError {
    PolicyError::Engine(EngineError::ObjectStore(
        "policy channel shut down".to_string(),
    ))
}

fn channel_poisoned() -> PolicyError {
    PolicyError::Engine(EngineError::ObjectStore(
        "policy channel lock poisoned".to_string(),
    ))
}

/// The result slot one submitter blocks on. `None` means the loop has
/// not completed it yet.
#[derive(Debug, Default)]
pub struct PolicyReply {
    result: Mutex<Option<Result<PolicyResponse, PolicyError>>>,
    ready: Condvar,
}

impl PolicyReply {
    fn complete(&self, result: Result<PolicyResponse, PolicyError>) {
        let Ok(mut slot) = self.result.lock() else {
            self.ready.notify_all();
            return;
        };
        *slot = Some(result);
        self.ready.notify_all();
    }

    fn wait(&self) -> Result<PolicyResponse, PolicyError> {
        let mut slot = self.result.lock().map_err(|_| channel_poisoned())?;
        loop {
            if let Some(result) = slot.take() {
                return result;
            }
            slot = self.ready.wait(slot).map_err(|_| channel_poisoned())?;
        }
    }
}

#[derive(Debug)]
struct QueuedPolicy {
    request: PolicyRequest,
    reply: Arc<PolicyReply>,
}

#[derive(Debug)]
struct ChannelState {
    pending: VecDeque<QueuedPolicy>,
    closed: bool,
}

/// The channel: the owner server submits and blocks, the loop drains
/// and completes. Its lock is its own, never the projection's or the
/// store's.
#[derive(Debug)]
pub struct PolicyChannel {
    state: Mutex<ChannelState>,
    work: Condvar,
    waker: Mutex<Option<Arc<WakeSignal>>>,
}

impl Default for PolicyChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl PolicyChannel {
    pub fn new() -> Self {
        PolicyChannel {
            state: Mutex::new(ChannelState {
                pending: VecDeque::new(),
                closed: false,
            }),
            work: Condvar::new(),
            waker: Mutex::new(None),
        }
    }

    /// Attach the loop's pacing signal: every admission pokes it, so a
    /// parked loop serves the submission without waiting out its
    /// pacing deadline.
    pub fn attach_waker(&self, waker: Arc<WakeSignal>) {
        *self
            .waker
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(waker);
    }

    fn poke_waker(&self) {
        let guard = self
            .waker
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(waker) = guard.as_ref() {
            waker.wake();
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, ChannelState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Submit one policy operation and block until the loop executes
    /// it. A closed channel fails fast — the request never executed —
    /// so no admitted caller outlives the teardown that owns it.
    pub fn submit(&self, request: PolicyRequest) -> Result<PolicyResponse, PolicyError> {
        let reply = Arc::new(PolicyReply::default());
        {
            let mut state = self.lock_state();
            if state.closed {
                return Err(channel_closed());
            }
            state.pending.push_back(QueuedPolicy {
                request,
                reply: Arc::clone(&reply),
            });
        }
        self.work.notify_one();
        self.poke_waker();
        reply.wait()
    }

    /// Take every currently queued request, preserving submission
    /// order. The loop executes each against its engine and completes
    /// it through [`PolicyChannel::complete`].
    pub fn take_queued(&self) -> Vec<(PolicyRequest, Arc<PolicyReply>)> {
        let mut state = self.lock_state();
        state
            .pending
            .drain(..)
            .map(|queued| (queued.request, queued.reply))
            .collect()
    }

    /// Complete one taken request and wake its submitter.
    pub fn complete(reply: &Arc<PolicyReply>, result: Result<PolicyResponse, PolicyError>) {
        reply.complete(result);
    }

    /// Whether any request is queued.
    pub fn has_queued(&self) -> bool {
        !self.lock_state().pending.is_empty()
    }

    /// Whether admission is closed: after this returns true no new
    /// submission can be admitted. Read by teardown to bound the
    /// drain; it never implies the queued work is done.
    pub fn is_closed(&self) -> bool {
        self.lock_state().closed
    }

    /// Close admission: new submissions fail fast. The loop drains what
    /// remains on its own teardown path; anything left fails via
    /// `shutdown`.
    pub fn close(&self) {
        self.lock_state().closed = true;
        self.work.notify_all();
        self.poke_waker();
    }

    /// Close admission and fail every still-queued request so no
    /// submitter blocks past teardown.
    pub fn shutdown(&self) {
        let queued: Vec<QueuedPolicy> = {
            let mut state = self.lock_state();
            state.closed = true;
            state.pending.drain(..).collect()
        };
        for queued in queued {
            queued.reply.complete(Err(channel_closed()));
        }
        self.work.notify_all();
        self.poke_waker();
    }

    /// Wait until work is queued or admission closes (or the timeout
    /// elapses). Currently unused by the loop (which polls the channel
    /// each pass); kept for teardown symmetry with the mutation queue.
    pub fn wait_until_work_or_closed(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut state = self.lock_state();
        loop {
            if state.closed {
                return true;
            }
            if !state.pending.is_empty() {
                return false;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            state = match self.work.wait_timeout(state, deadline - now) {
                Ok((guard, _)) => guard,
                Err(poison) => poison.into_inner().0,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_channel_refuses_new_work() {
        let channel = PolicyChannel::new();
        channel.close();
        let error = channel
            .submit(PolicyRequest::Status {
                path: String::new(),
            })
            .unwrap_err();
        assert!(
            matches!(error, PolicyError::Engine(_)),
            "shutdown surfaces as an engine refusal, got {error:?}"
        );
    }
}
