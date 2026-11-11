use super::mini_relay::MiniRelay;
use super::tests_harness::{
    keys, wait_for_distinct_deliveries, wait_for_subscription, DELIVERY_TIMEOUT,
};
use super::tests_harness::{live_mailbox, seal_rumor, sender_keys, temp_path, wait_for_delivery};
use super::*;
use std::time::Duration;

/// Park-and-hold wedge (B1): 1024 deliveries settling `Retry` forever
/// must not pin new mail. After the retry budget evicts each parked
/// handover relay-held, the bound drains and a fresh delivery arrives —
/// through flood-replayed evictions still settling `Retry`, the
/// production disposition for unknown-epoch mail. Eviction itself
/// requests no replay, so unhealable mail costs no steady-state work.
#[test]
fn retry_budget_evicts_parked_head_so_new_mail_delivers() {
    const EXTRA: usize = 1;
    // Replayed evictions settle Retry like production unknown-epoch
    // mail, so the fresh arrival below converges only after the replayed
    // window re-evicts: bound the whole episode generously.
    const EPISODE: Duration = Duration::from_secs(240);
    let relay = MiniRelay::spawn();
    let url = relay.url().to_string();
    let sender = sender_keys();
    let receiver = keys();
    let receiver_key = receiver.public_key();
    let relays = vec![url];
    let seen_path = temp_path("seen-retry-evict");

    let mut mailbox = live_mailbox(&receiver, &relays, seen_path.clone());
    wait_for_subscription(&relay, DELIVERY_TIMEOUT);

    // Park the full window: 1024 distinct wraps, each settled Retry.
    for index in 0..MAX_UNACKED_DELIVERIES {
        relay.inject(seal_rumor(&sender, receiver_key, format!("park-{index}")));
    }
    let ids = wait_for_distinct_deliveries(&mut mailbox, MAX_UNACKED_DELIVERIES, DELIVERY_TIMEOUT);
    assert_eq!(mailbox.unacked.len(), MAX_UNACKED_DELIVERIES);
    for id in &ids {
        mailbox.settle(*id, Disposition::Retry).unwrap();
    }
    assert_eq!(
        mailbox.unacked.len(),
        MAX_UNACKED_DELIVERIES,
        "one retry keeps the window pinned"
    );

    // Retry each parked handover exactly to the budget: 100 settlements
    // per id, the last of which evicts relay-held instead of staying
    // pinned.
    for id in &ids {
        for _ in 0..MAX_HELD_RETRIES {
            mailbox.settle(*id, Disposition::Retry).unwrap();
        }
    }
    assert!(
        mailbox.unacked.is_empty(),
        "retry budget drains the parked window, got {} held",
        mailbox.unacked.len()
    );
    assert_eq!(
        mailbox.settled_below(),
        MAX_UNACKED_DELIVERIES as u64,
        "watermark advances past evicted ids"
    );
    // Eviction is relay-held, not terminal: nothing durable, nothing
    // poisoned. (The 1024-wrap flood fills the handover channel, so a
    // saturation replay may already be re-driving wraps back onto the
    // channel here — that redelivery is the recovery path, not a
    // failure — so assert nothing about the next recv; the fresh-mail
    // leg below proves the bound drains.)
    assert_eq!(mailbox.seen_len(), 0, "eviction writes nothing durable");
    assert_eq!(mailbox.poison_len(), 0, "eviction poisons nothing");

    // Fresh mail delivers into the drained bound. A flood-triggered
    // saturation replay may have re-admitted evicted park-... wraps as
    // fresh handovers ahead of it — those settle Retry, exactly as
    // production unknown-epoch mail would, until their own budget
    // re-evicts them (eviction itself requests nothing). Without the
    // retry budget the parked window would never drain and this loop
    // would time out on the wedged inbox.
    relay.inject(seal_rumor(
        &sender,
        receiver_key,
        format!("after-evict-{}", EXTRA),
    ));
    let start = std::time::Instant::now();
    loop {
        let delivery =
            wait_for_delivery(&mut mailbox, DELIVERY_TIMEOUT).expect("new mail delivers");
        if delivery.envelope().ciphertext == format!("after-evict-{EXTRA}") {
            mailbox.settle(delivery.id(), Disposition::Ack).unwrap();
            break;
        }
        mailbox.settle(delivery.id(), Disposition::Retry).unwrap();
        assert!(
            start.elapsed() < EPISODE,
            "fresh mail arrives through retry-held replayed evictions"
        );
    }
}

/// Eviction alone triggers no replay while the mail stays unhealed:
/// a small parked set evicts, a full test cooldown passes with no
/// replay episode, and fresh mail still delivers through the drained
/// window. Unhealable mail must cost no steady-state work — the replay
/// comes only from the engine's key-install signal (pinned by the
/// catchup heal test), never from eviction itself.
#[test]
fn eviction_requests_no_replay_while_unhealed() {
    const PARKED: usize = 4;
    let relay = MiniRelay::spawn();
    let url = relay.url().to_string();
    let sender = sender_keys();
    let receiver = keys();
    let receiver_key = receiver.public_key();
    let relays = vec![url];
    let seen_path = temp_path("seen-retry-no-replay");

    let mut mailbox = live_mailbox(&receiver, &relays, seen_path.clone());
    wait_for_subscription(&relay, DELIVERY_TIMEOUT);
    // Small enough to stay clear of the channel-full saturation flag:
    // no replay fires before the eviction below.
    assert_eq!(mailbox.health().saturation_recoveries, 0);

    for index in 0..PARKED {
        relay.inject(seal_rumor(
            &sender,
            receiver_key,
            format!("unhealed-park-{index}"),
        ));
    }
    let ids = wait_for_distinct_deliveries(&mut mailbox, PARKED, DELIVERY_TIMEOUT);
    for id in &ids {
        for _ in 0..MAX_HELD_RETRIES {
            mailbox.settle(*id, Disposition::Retry).unwrap();
        }
    }
    assert!(mailbox.unacked.is_empty(), "parked mail evicts");
    assert_eq!(mailbox.settled_below(), PARKED as u64);

    // Past one full test cooldown with no heal and no other trigger:
    // no replay episode may fire on eviction alone.
    std::thread::sleep(Duration::from_secs(25));
    assert_eq!(
        mailbox.health().saturation_recoveries,
        0,
        "no replay without a heal signal"
    );

    // Fresh mail delivers into the drained window. A duplicate
    // notification already queued behind the eviction may re-admit one
    // evicted wrap under a fresh id ahead of it — a one-time leftover,
    // not a replay: it settles Retry and re-evicts on its own budget
    // while fresh mail arrives behind it.
    relay.inject(seal_rumor(
        &sender,
        receiver_key,
        "unhealed-fresh".to_string(),
    ));
    let start = std::time::Instant::now();
    loop {
        let delivery =
            wait_for_delivery(&mut mailbox, DELIVERY_TIMEOUT).expect("fresh mail delivers");
        if delivery.envelope().ciphertext == "unhealed-fresh" {
            mailbox.settle(delivery.id(), Disposition::Ack).unwrap();
            break;
        }
        mailbox.settle(delivery.id(), Disposition::Retry).unwrap();
        assert!(
            start.elapsed() < DELIVERY_TIMEOUT,
            "fresh mail arrives behind a leftover duplicate"
        );
    }
}

/// Restart redelivery after eviction: evicted wraps were never acked,
/// so the relay replays them onto a fresh mailbox over the same
/// durable path — and fresh mail still delivers through the replay.
#[test]
fn evicted_retry_mail_redelivers_after_restart() {
    let relay = MiniRelay::spawn();
    let url = relay.url().to_string();
    let sender = sender_keys();
    let receiver = keys();
    let receiver_key = receiver.public_key();
    let relays = vec![url.clone()];
    let seen_path = temp_path("seen-retry-evict-restart");

    let mut mailbox = live_mailbox(&receiver, &relays, seen_path.clone());
    wait_for_subscription(&relay, DELIVERY_TIMEOUT);

    const PARKED: usize = 8;
    for index in 0..PARKED {
        relay.inject(seal_rumor(
            &sender,
            receiver_key,
            format!("restart-park-{index}"),
        ));
    }
    let ids = wait_for_distinct_deliveries(&mut mailbox, PARKED, DELIVERY_TIMEOUT);
    for id in &ids {
        for _ in 0..MAX_HELD_RETRIES {
            mailbox.settle(*id, Disposition::Retry).unwrap();
        }
    }
    assert!(
        mailbox.unacked.is_empty(),
        "parked mail evicts before restart"
    );
    drop(mailbox);

    // Restart on the same durable path: the poison cache is gone and
    // the seen log holds nothing, so every evicted wrap is offered
    // again — and a fresh wrap injected after the restart delivers
    // through the replay. Redelivered park mail settles Retry, the
    // production disposition for still-unopenable mail; the window has
    // room for all of it plus the fresh wrap, so no second eviction is
    // needed to prove the arrival.
    let mut replay = live_mailbox(&receiver, &relays, seen_path.clone());
    relay.inject(seal_rumor(
        &sender,
        receiver_key,
        "restart-fresh".to_string(),
    ));
    let deadline = Duration::from_secs(60);
    let start = std::time::Instant::now();
    let mut fresh_seen = false;
    let mut redelivered = 0;
    while !fresh_seen || redelivered < PARKED {
        assert!(
            start.elapsed() < deadline,
            "evicted mail redelivers and fresh mail arrives"
        );
        if let Some(delivery) = replay.recv().unwrap() {
            if delivery.envelope().ciphertext == "restart-fresh" {
                replay.settle(delivery.id(), Disposition::Ack).unwrap();
                fresh_seen = true;
            } else {
                redelivered += 1;
                replay.settle(delivery.id(), Disposition::Retry).unwrap();
            }
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(fresh_seen, "fresh mail delivers after restart");
    assert_eq!(redelivered, PARKED, "every evicted wrap redelivers");
}
