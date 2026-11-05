use super::prereq_tests::{live_over_fake, FileView};
use super::LiveNode;
use std::collections::VecDeque;
use wyrd_format::{
    DeviceEncryptionKey, DeviceId, Entry, MemoryObjectStore, ObjectKind, ObjectStore, Tree,
};
use wyrd_sync::control::{ControlKind, SealedControl};
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::Engine;
use wyrd_sync::transport::mailbox::{
    open_from_sender, Delivery, DeliveryId, Disposition, Mailbox, MailboxEnvelope, MailboxError,
    MailboxSnapshot, SendReport,
};
use zeroize::Zeroizing;

/// A recording mailbox with a controllable reconnect counter and a
/// scripted transport attachment: the live loop reads `reconnects()`
/// for the trigger edge and `health_snapshot()` for the attachment
/// edge, and the test counts reconciliation requests by opening
/// what was sent. `recv` stays empty — the trigger under test needs
/// no inbound mail, only the edges and the gap signal. Acceptance
/// follows attachment: while detached no relay holds the bytes, so
/// probes mark nothing and obligations stay pending.
struct TriggerMailbox {
    sent: Vec<MailboxEnvelope>,
    inbound: VecDeque<MailboxEnvelope>,
    reconnects: u64,
    connected: usize,
}

impl Mailbox for TriggerMailbox {
    fn send(&mut self, envelope: MailboxEnvelope) -> Result<SendReport, MailboxError> {
        self.sent.push(envelope);
        Ok(SendReport {
            accepted: usize::from(self.connected > 0),
        })
    }
    fn recv(&mut self) -> Result<Option<Delivery>, MailboxError> {
        Ok(self
            .inbound
            .pop_front()
            .map(|envelope| Delivery::new(DeliveryId::new(3), envelope)))
    }
    fn settle(&mut self, _id: DeliveryId, _disposition: Disposition) -> Result<(), MailboxError> {
        Ok(())
    }
    fn reconnects(&self) -> u64 {
        self.reconnects
    }
    fn health_snapshot(&self) -> MailboxSnapshot {
        MailboxSnapshot {
            connected_relays: self.connected,
            total_relays: 1,
            stream_alive: self.connected > 0,
            relay_attached: self.connected > 0,
            ..MailboxSnapshot::unknown()
        }
    }
}

/// Sends in `sends` addressed to `peer` whose inner control kind is
/// the reconciliation request: opens the outer seal with the peer
/// secret the test owns, then reads the kind tag off the sealed
/// control bytes. Anything unopenable counts as not-a-request —
/// the loop's other sends (catch-up, republication) never open as
/// one, so the count isolates the trigger exactly.
fn request_sends(
    sends: &[MailboxEnvelope],
    peer_sk: &DeviceIdentitySecret,
    peer: DeviceId,
) -> usize {
    sends
        .iter()
        .filter(|envelope| {
            open_from_sender(peer_sk, peer, envelope)
                .ok()
                .and_then(|bytes| SealedControl::decode(&bytes).ok())
                .is_some_and(|sealed| sealed.kind == ControlKind::ReconciliationRequest)
        })
        .count()
}

/// A real owner drive with one authored head and one admitted
/// peer, shaped for trigger tests: the engine is created, dropped,
/// and reopened through the keystore path so the membership log
/// hydrates from the store (`create` alone leaves the live log
/// empty and admission has no tip to parent onto), and the peer
/// joins at epoch 2 so the probe names a genuine recipient with
/// production-shaped seals. Returns the node with its temp dir
/// (removed by the caller), the peer secret, and the peer id.
fn trigger_node(
    tag: &str,
) -> (
    LiveNode<FileView>,
    std::path::PathBuf,
    DeviceIdentitySecret,
    DeviceId,
) {
    // Owned secrets throughout.
    let dir = std::env::temp_dir().join(format!(
        "wyrd-core-trigger-{tag}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let identity = DeviceIdentitySecret::from_bytes([0x42; 32]).unwrap();
    let created = Engine::create(dir.clone(), "core-test-pass", identity.clone()).unwrap();
    drop(created);
    let mut engine = Engine::open_keystore(dir.clone(), "core-test-pass", identity).unwrap();
    assert!(
        engine.membership_log().known_state().is_some(),
        "reopen hydrates the log from the store"
    );
    // One authored file, like the scratch fixture: the loop needs a
    // head to serve, and the trigger test needs a quiet drive.
    let mut store = MemoryObjectStore::default();
    let chunk = store.insert(ObjectKind::Chunk, b"remote-base").unwrap();
    let root = Tree::from_entries(vec![Entry::file("f", 11, false, vec![chunk]).unwrap()])
        .unwrap()
        .insert_into(&mut store)
        .unwrap();
    let head = engine.author_snapshot(&store, root).unwrap();
    // A real admission, signed by the authoring path: the peer joins
    // at epoch 2, so the trigger names a genuine recipient and the
    // seals are production-shaped. The peer encryption key is a real
    // curve point (intrinsic validation refuses garbage keys, or the
    // peer could never be capability-able) derived from its own
    // scalar, decoupled from the identity bytes.
    let peer_sk = DeviceIdentitySecret::from_bytes([0x55; 32]).unwrap();
    let peer = peer_sk.device_id();
    let enc_sk = secp256k1::SecretKey::from_slice(&[0x99; 32]).unwrap();
    let enc_kp = secp256k1::Keypair::from_secret_key(secp256k1::SECP256K1, &enc_sk);
    let peer_enc = DeviceEncryptionKey::from_bytes(
        secp256k1::XOnlyPublicKey::from_keypair(&enc_kp)
            .0
            .serialize(),
    );
    engine.add_epoch_key(1, Zeroizing::new([0x77; 32]));
    engine.admit_device(peer, peer_enc).unwrap();
    let node = live_over_fake(engine, store, &[head]);
    (node, dir, peer_sk, peer)
}

/// The loop drives the trigger end to end: session start fires one
/// request, the quiet second pass fires none, and a reconnect edge
/// fires exactly one more. Only the mailbox is fake.
#[test]
fn loop_drives_the_reconciliation_trigger() {
    let (mut node, dir, peer_sk, peer) = trigger_node("session");
    let mut mailbox = TriggerMailbox {
        sent: Vec::new(),
        inbound: VecDeque::new(),
        reconnects: 0,
        connected: 1,
    };
    // Session start counts as a reconnect: one request on the first
    // pass (catch-up traffic may add other sends; the count below
    // isolates the request kind).
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        1,
        "session start probes once"
    );
    // Healthy second pass: no edge, no gap — no new request.
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        1,
        "quiet passes send nothing"
    );
    // A reconnect edge: exactly one more request, then quiet again.
    mailbox.reconnects = 5;
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        2,
        "one probe per edge"
    );
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        2,
        "the edge fires once"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// A late-attaching transport probes on attachment: a follower
/// whose transport was down at session start fires no
/// reconnect-counter edge when it attaches — recovery attempts
/// only fire post-attachment, so initial attachment moves no
/// counter — and it observes no durable gap, because it never
/// learned what it missed. Without an attachment edge the trigger
/// stays quiet. The from-zero connected-relays transition latches
/// the probe; later passes stay quiet. What this proves is the
/// emit side only: the follower now solicits where it was
/// previously mute. A statement against a sender whose obligation
/// is still pending (or never queued, in the late-joiner case)
/// retransmits through the existing answer path; a
/// delivered-but-never-ingested pair stays outside the missing
/// set by contract, and recovering that shape is the sender half,
/// not this probe.
#[test]
fn loop_probes_on_late_transport_attach() {
    let (mut node, dir, peer_sk, peer) = trigger_node("attach");
    // Transport down from the start: the session-start probe
    // fires, but no relay holds the bytes so nothing is marked.
    let mut mailbox = TriggerMailbox {
        sent: Vec::new(),
        inbound: VecDeque::new(),
        reconnects: 0,
        connected: 0,
    };
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        1,
        "session start probes once even with no transport"
    );
    // Still down, no edge, no gap: quiet.
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        1,
        "a relayless follower sends no second probe on its own"
    );
    // Attachment with no counter movement: the probe must fire
    // anyway — this is the transport session start the recovery
    // counter cannot see.
    mailbox.connected = 1;
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        2,
        "late attachment probes once"
    );
    // Attached and quiet: no further probes without a new edge.
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        2,
        "a steady attachment sends nothing more"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// A full loss and return probes once per return: the from-zero
/// transition is the edge, so the loss itself sends nothing and
/// each re-attachment solicits exactly once. Post-first-attachment
/// flaps also move the recovery counter, which is the backstop;
/// this test pins the attachment edge half with the counter held
/// still.
#[test]
fn reattach_after_full_loss_probes_once_per_return() {
    let (mut node, dir, peer_sk, peer) = trigger_node("flap");
    let mut mailbox = TriggerMailbox {
        sent: Vec::new(),
        inbound: VecDeque::new(),
        reconnects: 0,
        connected: 0,
    };
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        1,
        "session start probes once even with no transport"
    );
    mailbox.connected = 1;
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        2,
        "first attachment probes once"
    );
    // The loss itself is not an edge: from-one-to-zero solicits
    // nothing.
    mailbox.connected = 0;
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        2,
        "losing the transport sends no probe"
    );
    // The return is: exactly one more probe, then quiet again.
    mailbox.connected = 1;
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        3,
        "re-attachment probes once"
    );
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        3,
        "a steady re-attachment sends nothing more"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// Attachment while paused defers the edge: paused passes skip the
/// edge and the probe without advancing the observed count, so the
/// first unpaused pass after the attach fires it. An attach before
/// the very first pass is the exception that proves the rule: with
/// no observed count yet there is no from-zero transition, and the
/// session-start counter edge owns that probe instead.
#[test]
fn attach_while_paused_fires_on_first_unpaused_pass() {
    let (mut node, dir, peer_sk, peer) = trigger_node("pause");
    let mut mailbox = TriggerMailbox {
        sent: Vec::new(),
        inbound: VecDeque::new(),
        reconnects: 0,
        connected: 0,
    };
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        1,
        "session start probes once even with no transport"
    );
    node.set_paused(true);
    mailbox.connected = 1;
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        1,
        "paused passes neither edge nor probe"
    );
    node.set_paused(false);
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        2,
        "the deferred attach edge fires on resume"
    );
    node.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        request_sends(&mailbox.sent, &peer_sk, peer),
        2,
        "the resumed attachment then stays quiet"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
