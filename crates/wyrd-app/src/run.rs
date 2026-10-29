//! The host's run: split the composed node for live serving, hand
//! the loop to the daemon's vault runner under the teardown order,
//! and fold every outcome into the exit status.
//!
//! The run assembles in two phases so tests can submit through the
//! live channel while the loop is in flight: [`into_running`] splits
//! the node and hands back the backend parts (including the mutation
//! queue), [`finish_running`] supervises the loop and runs the
//! transport tail. Production drops the parts it has no backend for
//! yet, exactly like the headless vault; the order is one copy in
//! `wyrd_daemon::run_vault` either way.
//!
//! The admission rule this ordering protects (OD-06-A, option A):
//! close admission only after the presentation join — here a no-op
//! join of nothing — so the loop's post-return drain executes every
//! admitted mutation instead of resolving it `Shutdown`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use wyrd_core::live::LiveParts;
use wyrd_daemon::posture::PostureRecorder;
use wyrd_daemon::{
    run_vault, LiveConfig, LiveNode, LoopError, Mailbox, ServingEndpoint, TransportDeadlines,
    VaultEvent, VaultLoopEnd, VaultOutcome, VaultRun,
};
use wyrd_sync::runtime::RoutePublishing;

use super::host::{HostError, HostView, OpenHost};

/// A node split for live serving, waiting on its transport: the
/// loop, its backend parts, and the drive the loop reads its
/// materialization role from.
pub struct RunningHost {
    live: LiveNode<HostView>,
    drive_dir: PathBuf,
    parts: LiveParts<HostView>,
}

impl RunningHost {
    /// The backend parts: projection, wants, mutation queue. Tests
    /// submit through `parts.mutations` while the run is in flight;
    /// production holds them for the backend it has not attached yet.
    pub fn parts(&self) -> &LiveParts<HostView> {
        &self.parts
    }

    /// The drive directory the loop serves.
    pub fn drive_dir(&self) -> &std::path::Path {
        &self.drive_dir
    }

    /// The loop's pacing signal, shared with the mailbox: arrivals
    /// short-circuit the idle wait instead of burning the full
    /// window every park.
    pub fn waker(&self) -> Arc<wyrd_daemon::WakeSignal> {
        Arc::clone(self.live.waker())
    }
}

/// The transport the run supervises: a connected mailbox, an
/// optional bound bulk source, and the bound serving endpoint the
/// loop installs. The host never constructs these from transport
/// internals — the developer-run binary wires them through the
/// daemon's shared plumbing, tests through fakes — it only hands
/// them to the runner, which is why this struct names no transport
/// machinery beyond the composer vocabulary.
pub struct HostTransport<M, B> {
    /// The connected control-plane mailbox the loop drains.
    pub mailbox: M,
    /// The bound bulk source, if the composer bound one.
    pub bulk: Option<B>,
    /// The bound serving endpoint, installed on the loop.
    pub serving: ServingEndpoint,
    /// The serving id for the ready line, hex-encoded by the caller.
    pub serving_id: String,
    /// Bounds for the three transport closes.
    pub deadlines: TransportDeadlines,
    /// The host-owned event sink: posture recorder plus operator
    /// lines in production, a recording vec in tests.
    pub emit: Arc<dyn Fn(VaultEvent) + Send + Sync>,
}

/// Split an opened drive for live serving: carry the pending queue,
/// refresh both projections, and record the baseline revision. A
/// refresh failure refuses composition — with no trustworthy
/// projection there is nothing safe to serve.
pub fn into_running(open: OpenHost) -> Result<RunningHost, HostError> {
    let OpenHost { node, drive_dir } = open;
    let config = LiveConfig::for_local_sync();
    let (live, parts) = node.into_live(Duration::from_secs(30), &config)?;
    Ok(RunningHost {
        live,
        drive_dir,
        parts,
    })
}

/// Run a split host to completion under `stop`: install the serving
/// surface, supervise the live loop, wait for the first of loop
/// return or stop trip, close admission (no presentation session
/// exists to submit through, so the close only settles
/// close-racers), reap the loop thread, and run the shared transport
/// tail. Returns the loop's end plus the transport outcome; the
/// caller folds both into the exit status with [`exit_code`].
pub fn finish_running<M, B>(
    running: RunningHost,
    stop: &'static AtomicBool,
    transport: HostTransport<M, B>,
) -> Result<VaultOutcome, HostError>
where
    M: Mailbox + Send + 'static,
    B: RoutePublishing + Send + 'static,
{
    let RunningHost {
        live,
        drive_dir,
        parts,
    } = running;
    // No presentation backend is attached yet: the parts are owned
    // but never served, the queue stays open but nobody submits, so
    // passes only ever see intake and fetch work. The queue handle
    // stays alive here (not dropped with the parts) because the
    // runner clones it for supervision; dropping it early would read
    // as a second close.
    let _queue = parts.mutations.clone();
    drop(parts);
    let HostTransport {
        mailbox,
        bulk,
        serving,
        serving_id,
        deadlines,
        emit,
    } = transport;
    let outcome = run_vault(VaultRun {
        live,
        drive_dir,
        mailbox,
        bulk,
        serving,
        serving_id,
        config: LiveConfig::for_local_sync(),
        deadlines,
        stop,
        emit,
    })?;
    Ok(outcome)
}

/// Fold a finished run into a process exit status, the mount's rule:
/// the loop's end first, then the serving close. Exit 0 exactly when
/// the loop stopped clean and every transport closed; anything else
/// is 1. There is no session outcome to fold — a host has no
/// presentation session — so the session slot is unconditionally Ok.
pub fn exit_code(outcome: &VaultOutcome) -> i32 {
    match &outcome.loop_end {
        VaultLoopEnd::Returned(Ok(_)) => match &outcome.transport.serving {
            Ok(()) => 0,
            Err(_) => 1,
        },
        VaultLoopEnd::Returned(Err(_)) | VaultLoopEnd::SupervisionLost => 1,
    }
}

/// The production event sink: posture sidecar generations plus the
/// operator lines a supervisor captures. Failures carry the same
/// shape as the vault's so logs stay comparable across composers.
pub fn telemetry_emit(recorder: Arc<PostureRecorder>) -> Arc<dyn Fn(VaultEvent) + Send + Sync> {
    Arc::new(move |event| match event {
        VaultEvent::Ready { serving_id } => {
            recorder.note_ready();
            eprintln!("host ready: serving {serving_id}");
            tracing::info!(stage = "ready", iroh_id = %serving_id, "host ready");
        }
        VaultEvent::Posture {
            passes,
            errors,
            sent,
            uptime_secs,
        } => {
            recorder.note_posture(passes, errors, sent, uptime_secs);
            eprintln!(
                "host posture: {passes} passes, {errors} errors retried, {sent} sends, uptime {uptime_secs}s"
            );
            tracing::info!(
                stage = "posture",
                passes,
                errors,
                sent,
                uptime_secs,
                "host posture"
            );
        }
        VaultEvent::PassFailed {
            class,
            consecutive,
            error,
        } => {
            recorder.note_failure(class.clone(), consecutive);
            eprintln!("live sync pass failed ({class} class, {consecutive} consecutive): {error}");
            tracing::warn!(
                stage = "sync",
                class = %class,
                consecutive,
                error = %error,
                "live sync pass failed"
            );
        }
    })
}

/// The loop-end half of the fold, as a result: clean return is Ok,
/// every other end carries the reason. Exists so the binary folds
/// without re-matching the runner's vocabulary.
pub fn loop_result(outcome: &VaultOutcome) -> Result<(), String> {
    match &outcome.loop_end {
        VaultLoopEnd::Returned(Ok(_)) => Ok(()),
        VaultLoopEnd::Returned(Err(error)) => Err(match error {
            LoopError::Live(error) => format!("live loop failed: {error}"),
            LoopError::Panicked => "live loop thread panicked".to_string(),
        }),
        VaultLoopEnd::SupervisionLost => Err("supervised loop thread failed".to_string()),
    }
}
