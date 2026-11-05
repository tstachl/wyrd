//! Per-pass publish budget (issue `publish-phase-budget`): a relay
//! that accepts connections but never returns `OK` must not wedge
//! the loop behind N_obligations x per-send timeout. Two pending
//! obligations, one hanging relay, one bounded budget: the first
//! send is attempted, the second stays pending, the pass returns
//! under the stated bound with the clip reported — and the next
//! pass with a healthy relay carries the deferred obligation over
//! instead of having retired it.
use super::prereq_tests::{live_over_configured, scratch_file_drive};
use super::*;
use std::time::{Duration, Instant};
use wyrd_format::{Entry, MemoryObjectStore, ObjectKind, ObjectStore, Tree};
use wyrd_sync::keys::{DeviceEncryptionSecret, DeviceIdentitySecret};
use wyrd_sync::runtime::Engine;
use wyrd_sync::transport::mailbox::{
    Delivery, DeliveryId, Disposition, Mailbox, MailboxEnvelope, MailboxError, SendReport,
};

/// Accepts the connection but never returns `OK`: every send hangs
/// for the configured stall, then resolves unaccepted — the
/// transport ran, no relay holds the bytes, the obligation stays
/// pending. Models the SDK resolving `send_event` unaccepted after
/// its pinned `OK` timeout without paying that timeout per case.
struct NeverOkMailbox {
    stall: Duration,
    sends: usize,
}

impl Mailbox for NeverOkMailbox {
    fn send(&mut self, _envelope: MailboxEnvelope) -> Result<SendReport, MailboxError> {
        std::thread::sleep(self.stall);
        self.sends += 1;
        Ok(SendReport { accepted: 0 })
    }

    fn recv(&mut self) -> Result<Option<Delivery>, MailboxError> {
        Ok(None)
    }

    fn settle(&mut self, _id: DeliveryId, _disposition: Disposition) -> Result<(), MailboxError> {
        Ok(())
    }
}

/// Accepts every send: the healthy relay the carry-over pass uses.
struct AcceptingMailbox {
    sent: usize,
}

impl Mailbox for AcceptingMailbox {
    fn send(&mut self, _envelope: MailboxEnvelope) -> Result<SendReport, MailboxError> {
        self.sent += 1;
        Ok(SendReport { accepted: 1 })
    }

    fn recv(&mut self) -> Result<Option<Delivery>, MailboxError> {
        Ok(None)
    }

    fn settle(&mut self, _id: DeliveryId, _disposition: Disposition) -> Result<(), MailboxError> {
        Ok(())
    }
}

/// Admit B and author one snapshot past admission: the admit
/// queues B's capability, transition, and the genesis announcement;
/// the authoring queues the new announcement. The drain below
/// clears all of it, so two snapshots authored after the drain owe
/// exactly two announcement obligations — no transitions (no
/// membership change since), no capabilities (epoch unchanged).
fn engine_admitted(case: &str) -> (Engine, MemoryObjectStore, std::path::PathBuf) {
    let (mut engine, dir, mut store, _chunk, _root, _head) = scratch_file_drive(case);
    let identity_b = DeviceIdentitySecret::generate().unwrap();
    let encryption_b = DeviceEncryptionSecret::generate().unwrap();
    engine
        .admit_device(identity_b.device_id(), encryption_b.encryption_key())
        .unwrap();
    author_snapshot(&mut engine, &mut store, b"post-admission");
    (engine, store, dir)
}

fn author_snapshot(engine: &mut Engine, store: &mut MemoryObjectStore, bytes: &[u8]) {
    let chunk = store.insert(ObjectKind::Chunk, bytes).unwrap();
    let root = Tree::from_entries(vec![Entry::file("g", 14, false, vec![chunk]).unwrap()])
        .unwrap()
        .insert_into(store)
        .unwrap();
    engine.author_snapshot(store, root).unwrap();
}

/// A hanging relay clips the publish phase instead of wedging the
/// pass: one send attempted (and unaccepted), the second obligation
/// still pending, the pass back under the stated bound with the
/// clip and the deferral in the report. The carry-over pass then
/// discharges both — the clip retired nothing.
#[test]
fn publish_budget_clips_a_hanging_relay_with_carryover() {
    let (mut engine, mut store, dir) = engine_admitted("publish-budget-clip");
    // Drain everything the admit queued with a healthy relay, so the
    // two snapshots below owe exactly two announcements.
    let mut drain = AcceptingMailbox { sent: 0 };
    let unbounded = &mut PublishBudget::unbounded();
    engine.deliver_pending(&mut drain, unbounded).unwrap();
    engine
        .announce_pending(&mut drain, None, unbounded)
        .unwrap();
    assert!(
        !engine.has_pending_outbound().unwrap(),
        "the drain clears every admit-time obligation"
    );
    author_snapshot(&mut engine, &mut store, b"marker-one");
    author_snapshot(&mut engine, &mut store, b"marker-two");
    let heads = engine.live_heads().unwrap();
    let mut config = LiveConfig::for_local_sync();
    // Far below one hanging send: the first attempt runs (the
    // deadline is checked between obligations, never mid-send),
    // the second trips the clip.
    config.publish_pass_budget = Duration::from_millis(100);
    let mut node = live_over_configured(engine, store, &heads, &config);
    assert_eq!(
        node.pending_obligations().unwrap().len(),
        2,
        "two snapshots past a drained admit owe exactly two announcements"
    );
    let mut hanging = NeverOkMailbox {
        stall: Duration::from_millis(500),
        sends: 0,
    };
    let started = Instant::now();
    let report = node
        .sync_once(&mut hanging, None::<&mut wyrd_sync::serving::VaultSource>)
        .unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(10),
        "a clipped pass returns under the stated bound, took {elapsed:?}"
    );
    assert!(
        hanging.sends >= 1,
        "the first obligation is attempted (the pass's reconciliation probe may send first, so this counts transport attempts, not obligations)"
    );
    assert_eq!(report.sent, 0, "nothing accepted, nothing counted");
    assert!(
        report.publish_clipped,
        "the clip is in the pass report, not just the debug log"
    );
    assert_eq!(
        report.publish_deferred, 1,
        "the unattempted obligation is reported deferred"
    );
    assert_eq!(
        node.pending_obligations().unwrap().len(),
        2,
        "both obligations stay pending: attempted-but-unaccepted and clipped alike"
    );
    // Carry-over: the same node with a healthy relay discharges
    // both obligations on the next pass.
    let mut healthy = AcceptingMailbox { sent: 0 };
    let report = node
        .sync_once(&mut healthy, None::<&mut wyrd_sync::serving::VaultSource>)
        .unwrap();
    assert!(!report.publish_clipped, "no clip on a healthy relay");
    assert_eq!(report.sent, 2, "the clip retired nothing: both discharge");
    assert_eq!(
        node.pending_obligations().unwrap().len(),
        0,
        "the outbox drains once the relay answers"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
