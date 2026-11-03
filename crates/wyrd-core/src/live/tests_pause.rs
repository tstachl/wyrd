//! Pause and resume (OD-19, A/A/A/A): pause defers network work and
//! obligation discharge only. Local authoring and serving continue,
//! the prerequisite clock keeps its meaning, pause is process-local,
//! and the flag lives on `LiveNode`, gated inside the pass — never
//! in the pacing primitive.

use super::prereq_tests::{live_over_configured, scratch_file_drive};
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use wyrd_format::{Entry, MemoryObjectStore, ObjectKind, ObjectStore, Tree};
use wyrd_sync::keys::{DeviceEncryptionSecret, DeviceIdentitySecret};
use wyrd_sync::runtime::Engine;
use wyrd_sync::transport::mailbox::{
    Delivery, DeliveryId, Disposition, Mailbox, MailboxEnvelope, MailboxError, SendReport,
};

/// Accepts every send and delivers nothing: the relay is healthy, so
/// any discharged obligation lands here and a paused pass sends
/// nothing.
struct AcceptingMailbox {
    sent: Vec<MailboxEnvelope>,
}

impl Mailbox for AcceptingMailbox {
    fn send(&mut self, envelope: MailboxEnvelope) -> Result<SendReport, MailboxError> {
        self.sent.push(envelope);
        Ok(SendReport { accepted: 1 })
    }

    fn recv(&mut self) -> Result<Option<Delivery>, MailboxError> {
        Ok(None)
    }

    fn settle(&mut self, _id: DeliveryId, _disposition: Disposition) -> Result<(), MailboxError> {
        Ok(())
    }
}

/// A drive that owes an announcement before any pass runs: admit a
/// second device, then author past admission so the authoring queues
/// the announcement alongside the body, atomically.
fn engine_owing_announcement(case: &str) -> (Engine, MemoryObjectStore, std::path::PathBuf) {
    let (mut engine, dir, mut store, _chunk, _root, _head) = scratch_file_drive(case);
    let identity_b = DeviceIdentitySecret::generate().unwrap();
    let encryption_b = DeviceEncryptionSecret::generate().unwrap();
    engine
        .admit_device(identity_b.device_id(), encryption_b.encryption_key())
        .unwrap();
    let chunk = store.insert(ObjectKind::Chunk, b"post-admission").unwrap();
    let root = Tree::from_entries(vec![Entry::file("g", 14, false, vec![chunk]).unwrap()])
        .unwrap()
        .insert_into(&mut store)
        .unwrap();
    engine.author_snapshot(&store, root).unwrap();
    assert!(
        !engine.pending_announcements().unwrap().is_empty(),
        "the test owes an announcement before any pass runs"
    );
    (engine, store, dir)
}

/// DG-6 negative invariant, first half: a paused pass leaves the
/// obligation facts byte-identical and sends nothing.
#[test]
fn a_pause_across_a_pass_leaves_the_obligation_facts_intact() {
    let (engine, store, dir) = engine_owing_announcement("pause-obligations-intact");
    let before = engine.pending_announcements().unwrap();
    let heads = engine.live_heads().unwrap();
    let mut node = live_over_configured(engine, store, &heads, &LiveConfig::for_local_sync());
    node.set_paused(true);
    assert!(node.is_paused(), "the switch reads back what was set");
    let mut mailbox = AcceptingMailbox { sent: Vec::new() };
    let report = node
        .sync_once(&mut mailbox, None::<&mut wyrd_sync::serving::VaultSource>)
        .unwrap();
    assert!(mailbox.sent.is_empty(), "a paused pass discharges nothing");
    assert_eq!(report.sent, 0, "the report counts no send while paused");
    assert_eq!(
        node.engine.pending_announcements().unwrap(),
        before,
        "the obligation facts are byte-identical across the paused pass"
    );
    assert!(
        node.engine.has_pending_outbound().unwrap(),
        "the outbox still owes the obligation after the paused pass"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// DG-6 negative invariant, second half: resume is an ordinary pass
/// that discharges what the pause deferred, with no special-cased
/// path.
#[test]
fn a_resume_discharges_them() {
    let (engine, store, dir) = engine_owing_announcement("pause-resume-discharges");
    let heads = engine.live_heads().unwrap();
    let mut node = live_over_configured(engine, store, &heads, &LiveConfig::for_local_sync());
    node.set_paused(true);
    let mut mailbox = AcceptingMailbox { sent: Vec::new() };
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::serving::VaultSource>)
        .unwrap();
    assert!(
        !node.engine.pending_announcements().unwrap().is_empty(),
        "paused: the obligation waits"
    );
    node.set_paused(false);
    assert!(!node.is_paused(), "resume clears the switch");
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::serving::VaultSource>)
        .unwrap();
    assert!(
        node.engine.pending_announcements().unwrap().is_empty(),
        "resumed: the ordinary pass discharges the deferred obligation"
    );
    assert!(!mailbox.sent.is_empty(), "resume sends what pause deferred");
    std::fs::remove_dir_all(dir).unwrap();
}

/// OD-19-A outcome: with the drive paused, a `Mkdir` submitted
/// through the owner surface commits and the submitter hears success,
/// never `TimedOut`. The pause never stops a local write.
#[test]
fn a_pause_never_stops_a_local_write() {
    let (engine, dir, store, _chunk, _root, head) = scratch_file_drive("pause-local-write");
    let mut node = live_over_configured(engine, store, &[head], &LiveConfig::for_local_sync());
    let generation_before = node.generation();
    node.set_paused(true);
    let queue = Arc::clone(node.mutations());
    let submitter = std::thread::spawn(move || {
        queue.submit(crate::mutation::MutationKind::Mkdir {
            path: "paused-dir".into(),
        })
    });
    let mut mailbox = AcceptingMailbox { sent: Vec::new() };
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::serving::VaultSource>)
        .unwrap();
    assert_eq!(
        submitter.join().unwrap(),
        Ok(crate::mutation::MutationOutcome::Done),
        "a local write commits while paused"
    );
    assert!(
        node.generation() > generation_before,
        "the commit publishes a new serving generation while paused"
    );
    assert!(
        mailbox.sent.is_empty(),
        "but the paused pass still discharges nothing"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// The `max_mutation_wait` finding, pinned: a submitted mutation
/// that the paused passes keep evaluating succeeds — the pause adds
/// no fourth refusal and the clock keeps its existing meaning.
#[test]
fn a_pause_never_drops_a_pending_mutation() {
    let (engine, dir, store, _chunk, _root, head) = scratch_file_drive("pause-pending-mutation");
    let mut config = LiveConfig::for_local_sync();
    config.max_mutation_wait = Duration::from_secs(30);
    let mut node = live_over_configured(engine, store, &[head], &config);
    node.set_paused(true);
    let queue = Arc::clone(node.mutations());
    let submitter = std::thread::spawn(move || {
        queue.submit(crate::mutation::MutationKind::Mkdir {
            path: "pending-dir".into(),
        })
    });
    let mut mailbox = AcceptingMailbox { sent: Vec::new() };
    // Several paused passes in a row: every one of them evaluates
    // admitted work instead of letting it wait out the clock.
    for _ in 0..3 {
        node.sync_once(&mut mailbox, None::<&mut wyrd_sync::serving::VaultSource>)
            .unwrap();
    }
    assert_eq!(
        submitter.join().unwrap(),
        Ok(crate::mutation::MutationOutcome::Done),
        "paused passes evaluate admitted work; nothing times out"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// OD-19-D negative invariant: pause is not process lifecycle. The
/// switch never trips the stop flag, and a paused loop still exits
/// promptly on stop.
#[test]
fn pause_is_not_process_lifecycle() {
    let (engine, dir, store, _chunk, _root, head) = scratch_file_drive("pause-not-lifecycle");
    let mut node = live_over_configured(engine, store, &[head], &LiveConfig::for_local_sync());
    node.set_paused(true);
    let stop = AtomicBool::new(true);
    let config = LiveConfig::for_local_sync();
    let mut mailbox = AcceptingMailbox { sent: Vec::new() };
    let mut observed = 0u32;
    let summary = node
        .run_loop(
            &mut mailbox,
            None::<&mut wyrd_sync::serving::VaultSource>,
            &stop,
            &config,
            &mut |_, _| observed += 1,
        )
        .unwrap();
    assert_eq!(summary.passes, 0, "a tripped stop exits without passing");
    assert!(
        node.is_paused(),
        "exiting on stop leaves the pause switch alone"
    );
    assert!(stop.load(Ordering::Relaxed), "the stop flag is still set");
    drop(node);
    std::fs::remove_dir_all(dir).unwrap();
}

/// The coalescing dependency: pause then resume in quick succession
/// cannot swallow the resume — the next pass runs and re-checks the
/// world.
#[test]
fn a_resume_is_never_missed_by_coalescing() {
    let (engine, dir, store, _chunk, _root, head) = scratch_file_drive("pause-coalescing");
    let mut node = live_over_configured(engine, store, &[head], &LiveConfig::for_local_sync());
    node.set_paused(true);
    node.set_paused(false);
    assert!(!node.is_paused(), "a quick pause/resume pair lands resumed");
    let mut mailbox = AcceptingMailbox { sent: Vec::new() };
    let report = node
        .sync_once(&mut mailbox, None::<&mut wyrd_sync::serving::VaultSource>)
        .unwrap();
    // The pass ran (no swallow, no stall): an idle drive reports an
    // idle pass rather than hanging or erroring.
    assert_eq!(
        report.sent, 0,
        "nothing owed, nothing sent — but the pass ran"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
