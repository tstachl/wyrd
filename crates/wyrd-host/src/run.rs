//! The host's run vocabulary: compose an opened drive through the
//! daemon's shared owner implementation, then fold the finished run
//! into the exit status and operator telemetry.
//!
//! The composition itself is one copy in
//! `wyrd_daemon::compose_headless_owner` / `run_composed_owner`,
//! shared with `wyrd vault`: this module holds only the host's
//! phrasing of it — the drive-owner interface the tests submit
//! through, the ready/posture/failure sink, and the mount's fold
//! rule. A second owner implementation here would be a competing
//! owner path, which is exactly what the layering forbids.
//!
//! The admission rule this ordering protects (OD-06-A, option A):
//! close admission only after the presentation join — here a no-op
//! join of nothing — so the loop's post-return drain executes every
//! admitted mutation instead of resolving it `Shutdown`.

use std::sync::Arc;

use wyrd_daemon::posture::PostureRecorder;
use wyrd_daemon::{ComposedHeadless, LoopError, VaultEvent, VaultLoopEnd, VaultOutcome};

use super::host::{HostError, OpenHost};

/// Compose an opened drive for live serving through the daemon's
/// shared owner implementation: store, retention, node, serving
/// surface, and live split in one copy with every other owner-mode
/// entry point. The returned parts stay exposed (not dropped) so
/// backends — and tests submitting through the live channel — can
/// hold the mutation queue while the run is in flight.
///
/// The desktop host composes through the same application
/// settings file as every other composer: `compose_headless_owner`
/// delegates to `compose_headless_owner_with_quota(..., None)`,
/// where a missing flag falls through to the file's quota and the
/// file's other bounds and open timeout apply. There is no
/// unlimited-only host mode — only an unset file section composes
/// stock.
pub fn compose(open: OpenHost, loopback: bool) -> Result<ComposedHeadless, HostError> {
    let OpenHost { engine, drive_dir } = open;
    let composed = wyrd_daemon::compose_headless_owner(engine, drive_dir, loopback)
        .map_err(HostError::Owner)?;
    Ok(composed)
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
