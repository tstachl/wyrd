use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};

/// A relay-less client still establishes a drainer: the handshake
/// resolves and hands back an open channel. Pins the `Result` return
/// through the real function — no relays, no network, just the
/// spawn-and-readiness ordering the supervisor depends on.
#[test]
fn drainer_establishes_over_a_relay_less_client() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime builds");
    let client = Arc::new(Client::default());
    let health = Arc::new(SupervisorState {
        stream_alive: AtomicBool::new(true),
        connected_relays: AtomicUsize::new(0),
        saturated: AtomicBool::new(false),
        saturation_recoveries: AtomicU64::new(0),
        ticks: AtomicU64::new(0),
        stream_recovery_attempts: AtomicU64::new(0),
        relay_recovery_attempts: AtomicU64::new(0),
        closed_subscriptions: AtomicU64::new(0),
        stream_episode: AtomicBool::new(false),
        relay_episode: AtomicBool::new(false),
        saturation_episode: AtomicBool::new(false),
        relay_attached: AtomicBool::new(false),
        saturation_replay_at: std::sync::Mutex::new(None),
    });
    let intake_waker: Arc<std::sync::Mutex<Option<Arc<WakeSignal>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let mut receiver = runtime
        .block_on(establish_drainer(
            &client,
            &intake_waker,
            &health,
            &SubscriptionId::generate(),
        ))
        .expect("drainer establishes");
    // Empty, not disconnected: the spawned task is alive and holding
    // the sender, so the handshake ordered listen-before-subscribe.
    assert!(
        matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "established drainer holds an open channel"
    );
}

/// A dropped readiness sender (the drainer task died before
/// subscribing) reports a transport error instead of panicking.
/// Drives the production handshake mapping directly: killing the real
/// spawned task before its first poll is not deterministically
/// triggerable, so this pins the exact error the supervisor retries
/// on.
#[test]
fn dropped_readiness_reports_a_transport_error_not_a_panic() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime builds");
    // Delivered readiness resolves: the handshake is still exact, not
    // timed.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    ready_tx.send(()).expect("receiver alive");
    runtime
        .block_on(await_drainer_ready(ready_rx))
        .expect("delivered readiness resolves");
    // Sender dropped with nothing sent: the drainer died before its
    // subscription, and that is a reportable error.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    drop(ready_tx);
    let error = runtime
        .block_on(await_drainer_ready(ready_rx))
        .expect_err("dropped readiness fails");
    assert_eq!(
        error,
        MailboxError::Transport("drainer task died before signaling readiness".into()),
        "drainer death surfaces as a reportable error"
    );
}

/// The replay-claim ordering: the episode is claimed before the
/// request flag is consumed, so a trigger arriving while an episode
/// runs is preserved for the next due tick instead of being cleared.
/// Drives the extracted production predicate directly — the interleaving
/// with a live episode is timing, but the ordering it must obey is not.
#[test]
fn replay_claim_preserves_a_trigger_while_an_episode_runs() {
    use super::supervision::replay_claim;
    use std::sync::atomic::Ordering;

    // Idle tick: no request, no claim, nothing consumed.
    let saturated = AtomicBool::new(false);
    let episode = AtomicBool::new(false);
    assert!(!replay_claim(true, &saturated, &episode));
    assert!(!saturated.load(Ordering::Relaxed));

    // Not due: the request survives for a later due tick.
    saturated.store(true, Ordering::Relaxed);
    assert!(!replay_claim(false, &saturated, &episode));
    assert!(saturated.load(Ordering::Relaxed));

    // Due with no episode in flight: claim and consume.
    assert!(replay_claim(true, &saturated, &episode));
    assert!(!saturated.load(Ordering::Relaxed));
    assert!(episode.load(Ordering::Relaxed));

    // Trigger while the episode runs: no claim, flag preserved.
    saturated.store(true, Ordering::Relaxed);
    assert!(!replay_claim(true, &saturated, &episode));
    assert!(
        saturated.load(Ordering::Relaxed),
        "the in-flight episode must not swallow the request"
    );

    // Episode done, still due: the preserved request claims now.
    episode.store(false, Ordering::Relaxed);
    assert!(replay_claim(true, &saturated, &episode));
    assert!(!saturated.load(Ordering::Relaxed));
}
