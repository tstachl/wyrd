use super::mini_relay::MiniRelay;
use super::tests_harness::{
    keys, wait_for_distinct_deliveries, wait_for_subscription, DELIVERY_TIMEOUT,
};
use super::tests_harness::{live_mailbox, seal_rumor, sender_keys, temp_path, wait_for_delivery};
use super::*;
use std::time::Duration;

/// Park-and-hold wedge (B1): 1024 deliveries settling `Retry` forever
/// must not pin new mail. After the retry budget evicts each parked
/// handover relay-held, the bound drains and a fresh delivery arrives.
#[test]
fn retry_budget_evicts_parked_head_so_new_mail_delivers() {
    const EXTRA: usize = 1;
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

    // Retry each parked handover to the budget: the 100th Retry evicts
    // relay-held instead of staying pinned.
    for id in &ids {
        for _ in 1..MAX_HELD_RETRIES {
            mailbox.settle(*id, Disposition::Retry).unwrap();
        }
    }
    // The last settlement of each id is the evicting one: after the
    // loop every parked handover has been retried MAX_HELD_RETRIES
    // times and evicted, so the bound drains.
    for id in &ids {
        mailbox.settle(*id, Disposition::Retry).unwrap();
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
    // poisoned. (A saturation replay may already be re-driving the
    // evicted wraps back onto the channel here — that redelivery is
    // the recovery path, not a failure — so assert nothing about the
    // next recv; the fresh-mail leg below proves the bound drains.)
    assert_eq!(mailbox.seen_len(), 0, "eviction writes nothing durable");
    assert_eq!(mailbox.poison_len(), 0, "eviction poisons nothing");

    // Fresh mail delivers into the drained bound. A saturation replay
    // may have re-admitted evicted park-... wraps as fresh handovers
    // ahead of it — drain those (Ack, so they converge) until the fresh
    // content arrives. Without the retry budget the parked window would
    // never drain and this loop would time out on the wedged inbox.
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
        mailbox.settle(delivery.id(), Disposition::Ack).unwrap();
        assert!(
            start.elapsed() < DELIVERY_TIMEOUT,
            "fresh mail arrives through replayed evictions"
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
    // through the replay.
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
                replay.settle(delivery.id(), Disposition::Ack).unwrap();
            }
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(fresh_seen, "fresh mail delivers after restart");
    assert_eq!(redelivered, PARKED, "every evicted wrap redelivers");
}
