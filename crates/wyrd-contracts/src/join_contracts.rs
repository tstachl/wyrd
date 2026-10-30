//! The join contracts: a newly admitted device converges on the
//! authoritative post-admission state through the production admit and
//! catch-up path — invitation, durable obligations, mailbox delivery —
//! composed entirely over public APIs. Delivery order, duplication,
//! gaps, and forgery change what the newcomer observes first, never
//! where it converges: the pusher is transport, never authority.
//!
//! Cross-epoch catch-up (admitted at N, converging past N+1 while
//! offline) converges through rotation delivery: the ECDH-framed
//! capability lands the missing epoch secrets, the carried transition
//! advances the log, and retained epoch-sealed traffic opens on
//! redelivery. Contract 21 pins that convergence; the relay promises
//! retention, not order, so a pre-key offer still skips transiently
//! before the settling drain goes quiet.

use wyrd_format::membership::Admission;
use wyrd_format::{Change, MembershipTransition};
use wyrd_sync::control::{CapabilityPayload, Message, TransitionPayload};
use wyrd_sync::keys::capability::Capability;
use wyrd_sync::keys::EpochSecret;
use wyrd_sync::runtime::Engine;
use wyrd_sync::transport::mailbox::{Disposition, Mailbox, MailboxEnvelope};

use crate::support::{
    device, drive, scratch_dir, sealed_envelope, sign_transition, signed_transition, Device, Relay,
};

/// The owner's epoch-1 secret (test-side root material, mirroring the
/// rig's fixed secrets).
fn epoch1() -> EpochSecret {
    EpochSecret::from_bytes([0x51; 32])
}

/// One owner engine holding epoch 1: genesis drained, self capability
/// committed — the production shape of a drive creator one transition
/// in, built only through public calls.
pub(crate) struct Owner {
    pub(crate) engine: Engine,
    pub(crate) device: Device,
    pub(crate) genesis: MembershipTransition,
}

/// Build the [`Owner`] above. Shared with the owner-interface fork
/// test; see the struct doc for the shape.
pub(crate) fn owner() -> Owner {
    let device = device(0x10);
    let genesis = signed_transition(
        1,
        None,
        vec![],
        vec![
            Change::Admit(Admission {
                device: device.id,
                encryption_key: device.encryption_key,
            }),
            Change::SetOwners(vec![device.id]),
        ],
        &[device.id],
        &[device.id],
        &device,
    );
    let dir = scratch_dir("join-owner");
    let mut engine = Engine::open(
        dir.clone(),
        drive(),
        device.id,
        "contracts",
        device.identity.clone(),
        device.encryption.clone(),
    )
    .unwrap();
    engine.add_epoch_key(
        1,
        zeroize::Zeroizing::new(epoch1().control_key(&drive(), 1)),
    );
    let mut relay = Relay::new();
    relay.queue([sealed_envelope(
        &device.identity,
        device.id,
        &epoch1(),
        1,
        &Message::MembershipTransition(TransitionPayload {
            transition: genesis.canonical_bytes(),
        }),
    )]);
    let report = engine.drain(&mut relay).unwrap();
    assert_eq!(report.accepted, 1, "genesis drains");
    let cap = Capability::new(
        drive(),
        device.id,
        device.encryption_key,
        genesis.transition_id(),
        1,
        vec![epoch1()],
    )
    .unwrap();
    relay.queue([sealed_envelope(
        &device.identity,
        device.id,
        &epoch1(),
        1,
        &Message::Capability(CapabilityPayload {
            device: device.id,
            epoch: 1,
            wrapped: cap.wrap().unwrap().as_bytes().to_vec(),
        }),
    )]);
    let report = engine.drain(&mut relay).unwrap();
    assert_eq!(report.accepted, 1, "self capability installs epoch 1");
    Owner {
        engine,
        device,
        genesis,
    }
}

/// Admit `newcomer` through the owner engine and join it from the
/// sealed invitation. Returns the newcomer engine plus every envelope
/// the owner's delivery pass sent, in send order.
/// Admit `newcomer` through the owner engine and join it, shared
/// with the owner-interface fork test.
pub(crate) fn admit_and_collect(
    owner: &mut Owner,
    newcomer: &Device,
    relay: &mut Relay,
) -> (Engine, Vec<MailboxEnvelope>) {
    let outcome = owner
        .engine
        .admit_device(newcomer.id, newcomer.encryption_key)
        .unwrap();
    let mut newcomer_engine = Engine::accept_invitation(
        scratch_dir("join-newcomer"),
        "contracts",
        newcomer.identity.clone(),
        newcomer.encryption.clone(),
        &outcome.invitation,
    )
    .unwrap();
    let sent = owner.engine.deliver_pending(relay).unwrap();
    assert!(sent >= 2, "transition plus capability at minimum");
    // Pull every envelope out of the owner relay in send order.
    let mut envelopes = Vec::new();
    while let Some(delivery) = relay.recv().unwrap() {
        let id = delivery.id();
        envelopes.push(delivery.envelope().clone());
        relay.settle(id, Disposition::Ack).unwrap();
    }
    // The newcomer holds the invitation keys, so nothing stalls on
    // open before the test even starts.
    let _ = &mut newcomer_engine;
    (newcomer_engine, envelopes)
}

/// A newcomer relay holding exactly these envelopes.
pub(crate) fn newcomer_relay(envelopes: Vec<MailboxEnvelope>) -> Relay {
    let mut relay = Relay::new();
    relay.queue(envelopes);
    relay
}

/// Reader convergence is read-only end to end: a reader joins through
/// the same invitation and catch-up path as a member, converges on the
/// admission state, and then cannot author — the refusal arrives over
/// the public engine API, not just the internal gates.
#[test]
fn reader_converges_read_only() {
    use wyrd_format::{Entry, MemoryObjectStore, ObjectKind, ObjectStore, Tree};
    use wyrd_sync::runtime::EngineError;

    let mut owner = owner();
    let newcomer = device(0x20);
    let outcome = owner
        .engine
        .admit_reader(newcomer.id, newcomer.encryption_key)
        .unwrap();
    assert_eq!(outcome.transition.epoch, 2);
    let mut joined = Engine::accept_invitation(
        scratch_dir("join-reader"),
        "contracts",
        newcomer.identity.clone(),
        newcomer.encryption.clone(),
        &outcome.invitation,
    )
    .unwrap();
    let mut outbox = Relay::new();
    let sent = owner.engine.deliver_pending(&mut outbox).unwrap();
    assert!(sent >= 2, "transition plus capability at minimum");
    let mut envelopes = Vec::new();
    while let Some(delivery) = outbox.recv().unwrap() {
        let id = delivery.id();
        envelopes.push(delivery.envelope().clone());
        outbox.settle(id, Disposition::Ack).unwrap();
    }
    let mut relay = newcomer_relay(envelopes);
    let report = joined.drain(&mut relay).unwrap();
    assert_eq!(report.accepted, 2);
    assert_eq!(report.skipped, 0);
    let tip = joined
        .membership_log()
        .known_state()
        .expect("reader converges");
    assert_eq!(tip.epoch, 2);
    assert!(
        joined
            .membership_log()
            .readers_of(&tip.transition_id)
            .expect("post state")
            .contains(&newcomer.id),
        "reader observes its own admission"
    );

    // The converged reader holds secrets but no voice: authoring a
    // well-formed tree refuses with the role named.
    let mut objects = MemoryObjectStore::default();
    let chunk = objects.insert(ObjectKind::Chunk, b"payload").unwrap();
    let tree = Tree::from_entries(vec![Entry::file("file.txt", 7, false, vec![chunk]).unwrap()])
        .unwrap()
        .insert_into(&mut objects)
        .unwrap();
    assert!(matches!(
        joined.author_snapshot(&objects, tree),
        Err(EngineError::ReaderCannotAuthor)
    ));
}

/// Contract (OD-15-A downgrade pin): readers receive ongoing snapshot
/// announcements. The behaviour is driven end to end in
/// `wyrd-sync`'s `tests_reader::reader_receives_ongoing_snapshot_announcements`;
/// this is the contract-suite placement, asserted from outside the
/// crate that implements it: after admission catch-up, a member-authored
/// snapshot queues the reader's announcement obligation, the reader's
/// drain accepts it, and redelivery collapses to a duplicate (the
/// announcement committed, not suppressed).
#[test]
fn readers_receive_ongoing_announcements() {
    use wyrd_format::{Entry, MemoryObjectStore, ObjectKind, ObjectStore, Tree};

    let mut owner = owner();
    let newcomer = device(0x20);
    let outcome = owner
        .engine
        .admit_reader(newcomer.id, newcomer.encryption_key)
        .unwrap();
    assert_eq!(outcome.transition.epoch, 2, "admission opens epoch 2");
    let mut joined = Engine::accept_invitation(
        scratch_dir("join-reader-ongoing"),
        "contracts",
        newcomer.identity.clone(),
        newcomer.encryption.clone(),
        &outcome.invitation,
    )
    .unwrap();
    // Catch-up first: transition plus capability converge the reader
    // onto the admission state before any snapshot exists.
    let mut outbox = Relay::new();
    let sent = owner.engine.deliver_pending(&mut outbox).unwrap();
    assert!(sent >= 2, "transition plus capability at minimum");
    let mut catch_up = Vec::new();
    while let Some(delivery) = outbox.recv().unwrap() {
        let id = delivery.id();
        catch_up.push(delivery.envelope().clone());
        outbox.settle(id, Disposition::Ack).unwrap();
    }
    let report = joined.drain(&mut newcomer_relay(catch_up)).unwrap();
    assert_eq!(report.accepted, 2, "reader converges on admission");
    assert_eq!(report.skipped, 0);

    // A member-authored snapshot after admission: the obligation must
    // name the reader, or it stays stale past admission catch-up.
    let mut objects = MemoryObjectStore::default();
    let chunk = objects.insert(ObjectKind::Chunk, b"ongoing").unwrap();
    let tree = Tree::from_entries(vec![Entry::file("on.txt", 7, false, vec![chunk]).unwrap()])
        .unwrap()
        .insert_into(&mut objects)
        .unwrap();
    let authorized = owner.engine.author_snapshot(&objects, tree).unwrap();
    let snapshot_id = authorized.snapshot().snapshot_id();
    let pending = owner.engine.pending_announcements().unwrap();
    assert!(
        pending.contains(&(snapshot_id, newcomer.id)),
        "ongoing authoring queues the announcement for the reader: {pending:?}"
    );

    // The announcement travels the production delivery path and the
    // reader's drain accepts it.
    let mut announcement_outbox = Relay::new();
    let sent = owner
        .engine
        .announce_snapshot(&authorized, &mut announcement_outbox, None)
        .unwrap();
    assert!(sent >= 1, "the announcement discharges");
    let mut envelopes = Vec::new();
    while let Some(delivery) = announcement_outbox.recv().unwrap() {
        let id = delivery.id();
        envelopes.push(delivery.envelope().clone());
        announcement_outbox.settle(id, Disposition::Ack).unwrap();
    }
    assert!(!envelopes.is_empty(), "delivery produced envelopes");
    let report = joined
        .drain(&mut newcomer_relay(envelopes.clone()))
        .unwrap();
    assert!(
        report.accepted >= 1,
        "the reader accepts the ongoing announcement: {report:?}"
    );
    // Redelivery of the same bytes collapses to a duplicate: the
    // announcement committed instead of suppressing.
    let mut redelivery = Relay::new();
    redelivery.queue(envelopes);
    let report = joined.drain(&mut redelivery).unwrap();
    assert!(
        report.duplicates >= 1,
        "the received announcement is durable: {report:?}"
    );
}

/// Contract (OD-15-A downgrade pin): a reader-authored announcement
/// commits no fact. Readers hold epoch secrets and materialize, but
/// author nothing: intake suppresses a reader-signed announcement
/// before the compatibility gate, before staging, and before any fact
/// commits — the verdict is memory-only and bounded. The test drives it
/// over the public delivery path with a hand-signed reader announcement:
/// the drain processes it, the announcement store, pending obligations,
/// and live heads are unchanged, and redelivery suppresses again
/// (no duplicate — nothing durable committed).
#[test]
fn reader_authored_announcements_commit_no_fact() {
    use wyrd_format::membership::Admission;
    use wyrd_format::membership::{
        set_root, MEMBER_SET_CONTEXT, OWNER_SET_CONTEXT, READER_SET_CONTEXT,
    };
    use wyrd_format::{Change, MembershipTransition, SnapshotId};
    use wyrd_sync::control::{
        sign_announcement, CapabilityPayload, Message, SnapshotAnnouncement, TransitionPayload,
    };
    use wyrd_sync::keys::capability::Capability;
    use wyrd_sync::keys::EpochSecret;

    let owner_device = device(0x10);
    let reader = device(0x22);
    let epoch1 = EpochSecret::from_bytes([0x51; 32]);
    let epoch2 = EpochSecret::from_bytes([0x52; 32]);
    let genesis = signed_transition(
        1,
        None,
        vec![],
        vec![
            Change::Admit(Admission {
                device: owner_device.id,
                encryption_key: owner_device.encryption_key,
            }),
            Change::SetOwners(vec![owner_device.id]),
        ],
        &[owner_device.id],
        &[owner_device.id],
        &owner_device,
    );
    // Manual epoch-2 reader admission with known secrets: the helper
    // `signed_transition` hardcodes an empty reader set, so the
    // transition is built directly with the reader root.
    let mut admit = MembershipTransition::new(
        2,
        Some(genesis.transition_id()),
        vec![],
        vec![Change::AdmitReader(Admission {
            device: reader.id,
            encryption_key: reader.encryption_key,
        })],
        set_root(MEMBER_SET_CONTEXT, &[owner_device.id]).unwrap(),
        set_root(OWNER_SET_CONTEXT, &[owner_device.id]).unwrap(),
        set_root(READER_SET_CONTEXT, &[reader.id]).unwrap(),
        owner_device.id,
    )
    .unwrap();
    crate::support::sign_transition(&mut admit, &owner_device.signing, &drive());

    let dir = scratch_dir("join-reader-suppress");
    let mut engine = Engine::open(
        dir,
        drive(),
        owner_device.id,
        "contracts",
        owner_device.identity.clone(),
        owner_device.encryption.clone(),
    )
    .unwrap();
    engine.add_epoch_key(1, zeroize::Zeroizing::new(epoch1.control_key(&drive(), 1)));
    engine.add_epoch_key(2, zeroize::Zeroizing::new(epoch2.control_key(&drive(), 2)));
    let mut relay = Relay::new();
    relay.queue([sealed_envelope(
        &owner_device.identity,
        owner_device.id,
        &epoch1,
        1,
        &Message::MembershipTransition(TransitionPayload {
            transition: genesis.canonical_bytes(),
        }),
    )]);
    let report = engine.drain(&mut relay).unwrap();
    assert_eq!(report.accepted, 1, "genesis drains");
    let cap = Capability::new(
        drive(),
        owner_device.id,
        owner_device.encryption_key,
        genesis.transition_id(),
        1,
        vec![epoch1.clone()],
    )
    .unwrap();
    relay.queue([sealed_envelope(
        &owner_device.identity,
        owner_device.id,
        &epoch1,
        1,
        &Message::Capability(CapabilityPayload {
            device: owner_device.id,
            epoch: 1,
            wrapped: cap.wrap().unwrap().as_bytes().to_vec(),
        }),
    )]);
    let report = engine.drain(&mut relay).unwrap();
    assert_eq!(report.accepted, 1, "self capability installs epoch 1");
    // The epoch-2 reader admission travels under epoch 1, like the
    // rig's own admit transition.
    relay.queue([sealed_envelope(
        &owner_device.identity,
        owner_device.id,
        &epoch1,
        1,
        &Message::MembershipTransition(TransitionPayload {
            transition: admit.canonical_bytes(),
        }),
    )]);
    let report = engine.drain(&mut relay).unwrap();
    assert_eq!(report.accepted, 1, "reader admission drains");
    let tip = engine.membership_log().known_state().expect("tip");
    assert_eq!(tip.epoch, 2);
    assert!(
        engine
            .membership_log()
            .readers_of(&tip.transition_id)
            .expect("post state")
            .contains(&reader.id),
        "reader is a reader of the admission state"
    );

    // Baseline: no heads beyond genesis, no pending announcements.
    let heads_before = engine.live_heads().unwrap();
    let pending_before = engine.pending_announcements().unwrap();
    assert!(
        pending_before.is_empty(),
        "no pending announcements before the forged send: {pending_before:?}"
    );

    // A reader-signed announcement for a snapshot the reader could
    // never author, sealed under the epoch the reader holds.
    let snapshot = SnapshotId::from_bytes([0xAB; 32]);
    let mut announcement = SnapshotAnnouncement {
        snapshot,
        author: reader.id,
        epoch: 2,
        membership: tip.transition_id,
        body_root: wyrd_format::BaoRoot::from_bytes([0x44; 32]),
        root_manifest: wyrd_format::ContentId::from_bytes([0x55; 32]),
        root_manifest_transport: wyrd_format::BaoRoot::from_bytes([0x66; 32]),
        node_addr: None,
        signature: [0; 64],
    };
    sign_announcement(&mut announcement, &reader.identity, &drive());
    relay.queue([sealed_envelope(
        &reader.identity,
        owner_device.id,
        &epoch2,
        2,
        &Message::SnapshotAnnouncement(announcement.clone()),
    )]);
    let report = engine.drain(&mut relay).unwrap();
    assert!(
        report.accepted >= 1,
        "the forged announcement is processed: {report:?}"
    );
    // Nothing committed: heads, pending obligations, and the durable
    // outbox are unchanged afterwards.
    assert_eq!(
        engine.live_heads().unwrap(),
        heads_before,
        "suppression leaves heads untouched"
    );
    assert_eq!(
        engine.pending_announcements().unwrap(),
        pending_before,
        "suppression queues no fetch and holds nothing pending"
    );
    // A fresh seal of the same reader-signed bytes suppresses again
    // instead of collapsing to a duplicate: the verdict was
    // memory-only, so no announcement fact committed. (Identical
    // bytes would dedupe on the message id without reprocessing;
    // a fresh seal forces intake to judge the content again, and
    // the reader check fires before the known-announcement gate.)
    relay.queue([sealed_envelope(
        &reader.identity,
        owner_device.id,
        &epoch2,
        2,
        &Message::SnapshotAnnouncement(announcement),
    )]);
    let report = engine.drain(&mut relay).unwrap();
    assert!(
        report.accepted >= 1,
        "a suppressed announcement re-suppresses on fresh seal: {report:?}"
    );
    assert_eq!(
        report.duplicates, 0,
        "a suppressed announcement commits nothing to redeliver as duplicate: {report:?}"
    );
    assert_eq!(
        engine.live_heads().unwrap(),
        heads_before,
        "redelivery still leaves heads untouched"
    );
}

/// Ordered catch-up converges: transition plus capability drain to
/// acceptance with nothing held, skipped, or duplicated.
#[test]
fn invited_device_converges_on_ordered_catch_up() {
    let mut owner = owner();
    let newcomer = device(0x20);
    let mut outbox = Relay::new();
    let (mut joined, envelopes) = admit_and_collect(&mut owner, &newcomer, &mut outbox);
    assert_eq!(envelopes.len(), 2, "one transition, one wrap");

    let mut relay = newcomer_relay(envelopes);
    let report = joined.drain(&mut relay).unwrap();
    assert_eq!(report.accepted, 2);
    assert_eq!(report.deferred, 0);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.duplicates, 0);
    assert_eq!(joined.pending_count(), 0, "nothing held");
}

/// Reversed catch-up converges identically: the rotation delivery
/// carries its own transition, so arrival order is irrelevant — the
/// grant commits with the history it needs attached.
#[test]
fn invited_device_converges_on_reversed_catch_up() {
    let mut owner = owner();
    let newcomer = device(0x20);
    let mut outbox = Relay::new();
    let (mut joined, envelopes) = admit_and_collect(&mut owner, &newcomer, &mut outbox);
    assert_eq!(envelopes.len(), 2);
    let reversed = vec![envelopes[1].clone(), envelopes[0].clone()];

    let mut relay = newcomer_relay(reversed);
    let report = joined.drain(&mut relay).unwrap();
    assert_eq!(
        report.accepted, 1,
        "rotation commits; the transition duplicates"
    );
    assert_eq!(report.duplicates, 1, "already recorded via rotation");
    assert_eq!(report.skipped, 0);
    assert_eq!(report.deferred, 0);
    assert_eq!(joined.pending_count(), 0, "nothing held");
}

/// Duplicated catch-up converges: redelivery is dedupe, not state.
#[test]
fn invited_device_converges_on_duplicated_catch_up() {
    let mut owner = owner();
    let newcomer = device(0x20);
    let mut outbox = Relay::new();
    let (mut joined, envelopes) = admit_and_collect(&mut owner, &newcomer, &mut outbox);
    assert_eq!(envelopes.len(), 2);
    let doubled = vec![
        envelopes[0].clone(),
        envelopes[1].clone(),
        envelopes[0].clone(),
        envelopes[1].clone(),
    ];

    let mut relay = newcomer_relay(doubled);
    let report = joined.drain(&mut relay).unwrap();
    assert_eq!(report.skipped, 0);
    assert!(report.duplicates >= 2, "redelivery collapses");
    assert_eq!(joined.pending_count(), 0, "nothing held");
}

/// A missing transition does not block the rotation: the delivery
/// carries the transition it needs, so the grant commits while the
/// epoch-sealed transition is still in flight — and the late
/// transition acks as a duplicate, which the set-based machines
/// absorb.
#[test]
fn invited_device_converges_after_gap_then_redelivery() {
    let mut owner = owner();
    let newcomer = device(0x20);
    let mut outbox = Relay::new();
    let (mut joined, envelopes) = admit_and_collect(&mut owner, &newcomer, &mut outbox);
    assert_eq!(envelopes.len(), 2);

    // The rotation first, its transition unseen: self-contained, so
    // it commits without holding.
    let mut relay = newcomer_relay(vec![envelopes[1].clone()]);
    let report = joined.drain(&mut relay).unwrap();
    assert_eq!(report.accepted, 1);
    assert_eq!(report.skipped, 0);
    assert_eq!(joined.pending_count(), 0, "nothing held");

    // The transition lands later: already recorded via the
    // rotation, so it duplicates — nothing lost.
    let mut relay = newcomer_relay(vec![envelopes[0].clone()]);
    let report = joined.drain(&mut relay).unwrap();
    assert_eq!(report.accepted, 0);
    assert_eq!(report.duplicates, 1);
    assert_eq!(report.skipped, 0);
    assert_eq!(joined.pending_count(), 0, "gap healed");
}

/// An offline device converges past its invitation through rotation
/// delivery. B is admitted (epoch 2) and stays offline while C is
/// admitted (epoch 3): the owner's queue holds B's contiguous 2..3
/// sequence, and B converges fully — the rotation lands epoch 3's
/// secrets with its transition attached, and the epoch-sealed epoch-3
/// transition retained from before the keys landed opens on redelivery.
/// The relay promises retention, not order, so the pre-key offer still
/// skips transiently; the settling drain goes quiet.
#[test]
fn offline_device_catch_up_accumulates_contiguously() {
    let mut owner = owner();
    let device_b = device(0x20);
    let device_c = device(0x30);
    let outcome_b = owner
        .engine
        .admit_device(device_b.id, device_b.encryption_key)
        .unwrap();
    owner
        .engine
        .admit_device(device_c.id, device_c.encryption_key)
        .unwrap();

    // Owner-side contiguity: every epoch from B's admission to the
    // tip is queued, nothing skipped, nothing doubled.
    let state = owner.engine.runtime_state().unwrap();
    let mut b_epochs: Vec<u64> = state
        .pending_capabilities()
        .into_iter()
        .filter(|(_, recipient)| *recipient == device_b.id)
        .map(|(epoch, _)| epoch)
        .collect();
    b_epochs.sort();
    assert_eq!(b_epochs, vec![2, 3], "contiguous admission..current");

    let mut outbox = Relay::new();
    let sent = owner.engine.deliver_pending(&mut outbox).unwrap();
    assert!(sent >= 4, "two transitions plus two wraps for B");
    let mut envelopes = Vec::new();
    while let Some(delivery) = outbox.recv().unwrap() {
        let id = delivery.id();
        if delivery.envelope().recipient == device_b.id {
            envelopes.push(delivery.envelope().clone());
        }
        outbox.settle(id, Disposition::Ack).unwrap();
    }
    assert_eq!(envelopes.len(), 4, "B's full catch-up set");

    let mut joined = Engine::accept_invitation(
        scratch_dir("join-offline-b"),
        "contracts",
        device_b.identity.clone(),
        device_b.encryption.clone(),
        &outcome_b.invitation,
    )
    .unwrap();
    let mut relay = newcomer_relay(envelopes);
    let report = joined.drain(&mut relay).unwrap();
    // Epoch 2 commits; epoch 3 converges through its rotation — the
    // epoch-sealed epoch-3 offer from before the keys landed skips
    // transiently, then opens on redelivery once rotation installs
    // the key.
    assert_eq!(report.accepted, 3, "admission epoch plus rotation converge");
    assert_eq!(report.skipped, 1, "pre-key offer retained, not lost");
    assert_eq!(joined.pending_count(), 0, "nothing held");
    // The retained transition opens under the rotation-installed key:
    // opening it is the public proof the epoch-3 secret landed — but
    // its document already committed via the rotation, so it
    // duplicates instead of committing redundantly.
    let report = joined.drain(&mut relay).unwrap();
    assert_eq!(report.accepted, 0);
    assert_eq!(report.duplicates, 1);
    assert_eq!(report.skipped, 0);
    assert_eq!(joined.pending_count(), 0, "nothing held");
    // Settled: the third drain goes quiet.
    let report = joined.drain(&mut relay).unwrap();
    assert_eq!(report.accepted, 0);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.duplicates, 0);
}

/// Delivery fans out to several newcomers from one pass: the shared
/// transition bytes serve every recipient under per-recipient outer
/// seals, and each per-pair rotation opens only for its own
/// device. B (admitted at 2) converges past its invitation through
/// rotation; C (admitted at 3, holding 1..=3) converges fully from the
/// same pass.
#[test]
fn two_newcomers_converge_from_one_delivery_pass() {
    let mut owner = owner();
    let device_b = device(0x20);
    let device_c = device(0x30);
    let outcome_b = owner
        .engine
        .admit_device(device_b.id, device_b.encryption_key)
        .unwrap();
    let outcome_c = owner
        .engine
        .admit_device(device_c.id, device_c.encryption_key)
        .unwrap();

    let mut outbox = Relay::new();
    let sent = owner.engine.deliver_pending(&mut outbox).unwrap();
    assert!(sent >= 4, "two transitions plus two wraps at minimum");
    let mut b_envelopes = Vec::new();
    let mut c_envelopes = Vec::new();
    while let Some(delivery) = outbox.recv().unwrap() {
        let id = delivery.id();
        if delivery.envelope().recipient == device_b.id {
            b_envelopes.push(delivery.envelope().clone());
        } else if delivery.envelope().recipient == device_c.id {
            c_envelopes.push(delivery.envelope().clone());
        }
        outbox.settle(id, Disposition::Ack).unwrap();
    }
    assert!(
        !b_envelopes.is_empty() && !c_envelopes.is_empty(),
        "one pass serves both newcomers"
    );

    // The owner's obligations to both newcomers discharge on send.
    let state = owner.engine.runtime_state().unwrap();
    assert!(
        state
            .pending_capabilities()
            .into_iter()
            .all(|(_, recipient)| recipient != device_b.id && recipient != device_c.id),
        "no capability obligation retained"
    );
    assert!(
        state
            .pending_transitions()
            .into_iter()
            .all(|(_, recipient)| recipient != device_b.id && recipient != device_c.id),
        "no transition obligation retained"
    );

    let mut joined_b = Engine::accept_invitation(
        scratch_dir("join-fanout-b"),
        "contracts",
        device_b.identity.clone(),
        device_b.encryption.clone(),
        &outcome_b.invitation,
    )
    .unwrap();
    let mut relay_b = newcomer_relay(b_envelopes);
    let report_b = joined_b.drain(&mut relay_b).unwrap();
    assert_eq!(
        report_b.accepted, 3,
        "B converges through epoch 2 plus rotation"
    );
    assert_eq!(report_b.skipped, 1, "pre-key offer retained, not lost");
    // The retained epoch-3 transition opens under the
    // rotation-installed key — but its document already committed
    // via the rotation, so it duplicates; then the drain goes quiet.
    let report_b = joined_b.drain(&mut relay_b).unwrap();
    assert_eq!(report_b.accepted, 0);
    assert_eq!(report_b.duplicates, 1);
    assert_eq!(report_b.skipped, 0);
    let report_b = joined_b.drain(&mut relay_b).unwrap();
    assert_eq!(report_b.accepted, 0);
    assert_eq!(report_b.skipped, 0);

    let mut joined_c = Engine::accept_invitation(
        scratch_dir("join-fanout-c"),
        "contracts",
        device_c.identity.clone(),
        device_c.encryption.clone(),
        &outcome_c.invitation,
    )
    .unwrap();
    let c_len = c_envelopes.len();
    let mut relay_c = newcomer_relay(c_envelopes);
    let report_c = joined_c.drain(&mut relay_c).unwrap();
    assert_eq!(report_c.accepted, c_len, "C opens everything it was sent");
    assert_eq!(report_c.skipped, 0);
    assert_eq!(report_c.deferred, 0);
    assert_eq!(joined_c.pending_count(), 0, "nothing held");
}

/// A forged transition from a legitimate member cannot extend
/// newcomer state: the pusher is transport, never authority. The
/// owner-signed but invalid epoch-2 sibling is suppressed while the
/// genuine admission converges around it.
#[test]
fn forged_transition_from_member_cannot_extend_newcomer_state() {
    let mut owner = owner();
    let newcomer = device(0x20);
    let mut outbox = Relay::new();
    let (mut joined, envelopes) = admit_and_collect(&mut owner, &newcomer, &mut outbox);
    assert_eq!(envelopes.len(), 2);

    // A sibling of the admission at epoch 2 with garbage roots,
    // signed by the owner's own key: envelope-authentic, content-
    // invalid. Transitions carry no envelope-bound epoch of their
    // own, so it seals under the held epoch-1 key.
    let mut forged = MembershipTransition::new(
        2,
        Some(owner.genesis.transition_id()),
        Vec::new(),
        vec![Change::Rotate],
        [0xFF; 32],
        [0xFF; 32],
        [0xFF; 32],
        owner.device.id,
    )
    .unwrap();
    sign_transition(&mut forged, &owner.device.signing, &drive());
    let forged_envelope = sealed_envelope(
        &owner.device.identity,
        newcomer.id,
        &epoch1(),
        1,
        &Message::MembershipTransition(TransitionPayload {
            transition: forged.canonical_bytes(),
        }),
    );
    let stream = vec![envelopes[0].clone(), forged_envelope, envelopes[1].clone()];

    let mut relay = newcomer_relay(stream);
    let report = joined.drain(&mut relay).unwrap();
    assert_eq!(report.accepted, 3, "genuine pair plus suppressed forgery");
    assert_eq!(report.skipped, 0);
    assert_eq!(joined.pending_count(), 0, "forgery held nothing");
    // The forgery settled without a fact: a second drain finds nothing
    // retained and nothing to re-resolve.
    let report = joined.drain(&mut relay).unwrap();
    assert_eq!(report.accepted, 0);
    assert_eq!(report.duplicates, 0);
}
