//! The per-merge partition leg (evidence-gates record in
//! `docs/contracts/sync-and-peers.md`, Conflicts): two admitted
//! peers, control plane withheld on both sides, same path authored on
//! each, reachability restored, then the conflict surface plus the
//! durable-log no-loss assertion. In-process only (MemoryRelay,
//! MemoryMailbox, VaultSource) so it runs in the default gate with no
//! privilege; `partition_leg_needs_no_privilege` pins that.
//!
//! Mechanism fidelity: withholding the send path reproduces
//! the *outcome* of `tap-r down` (peers diverge without hearing each
//! other) while the data plane stays live through the direct serving
//! route. The test pins the outcome half explicitly: both peers'
//! `pending_announcements` are non-empty while partitioned and empty
//! after one heal pass. If a future refactor breaks that accumulation,
//! the leg fails rather than silently testing a heal with nothing to
//! heal.

use super::tests_harness::{drain_side, execute_side, scenario};
use super::*;

use std::collections::BTreeSet;

use crate::authorization::Classification;
use crate::runtime::MaterializationState;
use crate::transport::mailbox::{
    Delivery, DeliveryId, Disposition, Mailbox, MailboxEnvelope, MailboxError, MemoryMailbox,
    SendReport,
};
use wyrd_format::{ContentId, Entry, MemoryObjectStore, ObjectKind, Tree};

/// Withholds the control plane while leaving the data plane live: every
/// send reports zero relay acceptance (so the obligation stays pending
/// per the delivery contract) and nothing is ever offered on receive.
/// The direct serving route (`VaultSource`) stays usable, which is what
/// makes the divergence possible without healing first.
struct PartitionMailbox;

impl Mailbox for PartitionMailbox {
    fn send(&mut self, _envelope: MailboxEnvelope) -> Result<SendReport, MailboxError> {
        Ok(SendReport { accepted: 0 })
    }

    fn recv(&mut self) -> Result<Option<Delivery>, MailboxError> {
        Ok(None)
    }

    fn settle(&mut self, _id: DeliveryId, _d: Disposition) -> Result<(), MailboxError> {
        Ok(())
    }
}

/// One file over one chunk in a scratch store, ready to author.
fn shared_tree(store: &mut MemoryObjectStore, payload: &[u8]) -> (ContentId, ContentId) {
    let chunk = store.insert(ObjectKind::Chunk, payload).unwrap();
    let tree = Tree::from_entries(vec![Entry::file(
        "shared.txt",
        payload.len() as u64,
        false,
        vec![chunk],
    )
    .unwrap()])
    .unwrap()
    .insert_into(store)
    .unwrap();
    (tree, chunk)
}

/// The durable snapshot-id set one engine holds (bodies committed in
/// its fact log).
fn snapshot_set(device: &super::tests_harness::Device) -> BTreeSet<wyrd_format::SnapshotId> {
    device
        .engine
        .store
        .load()
        .unwrap()
        .snapshot_bodies
        .iter()
        .map(|body| body.snapshot_id())
        .collect()
}

/// Relay partition produces two eligible heads and neither branch is
/// lost: the per-merge evidence for the conflict guarantee (ROADMAP
/// conflict promise; "partition and heal" in the v0.2 Definition of
/// Done). Verdict lives in CI/PR evidence only, per the evidence-gates
/// record in `docs/contracts/sync-and-peers.md`.
#[test]
fn relay_partition_produces_two_heads_and_both_survive() {
    let (mut pair, _, _) = scenario();
    assert_eq!(drain_side(&mut pair.relay, &mut pair.a).accepted, 7);
    assert_eq!(drain_side(&mut pair.relay, &mut pair.b).accepted, 6);
    // Converge the scenario bodies so the partition parents onto one
    // head instead of the scenario's two parentless roots.
    assert_eq!(execute_side(&mut pair.bulk, &mut pair.a).snapshot_bodies, 2);
    assert_eq!(execute_side(&mut pair.bulk, &mut pair.b).snapshot_bodies, 2);

    // Baseline: A authors one snapshot, B converges onto it. Single
    // head on both sides before the partition opens.
    let mut base_objects = MemoryObjectStore::default();
    let (base_tree, _) = shared_tree(&mut base_objects, b"partition-baseline");
    let baseline = pair
        .a
        .engine
        .author_snapshot(&base_objects, base_tree)
        .unwrap();
    let baseline_id = baseline.snapshot().snapshot_id();
    {
        let mut mailbox = MemoryMailbox {
            relay: &mut pair.relay,
            owner: pair.a.device,
        };
        pair.a
            .engine
            .announce_snapshot(&baseline, &mut mailbox, None)
            .unwrap();
    }
    assert_eq!(drain_side(&mut pair.relay, &mut pair.b).accepted, 1);
    {
        let serving = crate::serving::VaultSource::from_state(
            &pair.a.engine.runtime_state().unwrap(),
            pair.a.engine.vault(),
        )
        .unwrap();
        let mut serving = serving;
        pair.b
            .engine
            .execute_plan(&mut serving, &mut pair.b.objects)
            .unwrap();
    }
    for side in [&pair.a, &pair.b] {
        let heads = side.engine.live_heads().unwrap();
        assert_eq!(heads.len(), 1, "single head before the partition");
        assert_eq!(heads[0].snapshot().snapshot_id(), baseline_id);
    }

    // Durable baseline for the no-loss assertion (the log must only
    // grow: conflicts preserve both branches):
    // the union of snapshot ids plus the announcement count. Both must
    // only grow across the partition and heal.
    let mut before: BTreeSet<wyrd_format::SnapshotId> = BTreeSet::new();
    before.extend(snapshot_set(&pair.a));
    before.extend(snapshot_set(&pair.b));
    assert!(before.contains(&baseline_id));
    let announcements_before: usize = pair.a.engine.store.load().unwrap().announcements.len()
        + pair.b.engine.store.load().unwrap().announcements.len();

    // Partition: neither side can reach the control plane. Each
    // authors the same path with different bytes; the sends withhold
    // (zero acceptance) so both obligations stay pending.
    let mut objects_a = MemoryObjectStore::default();
    let (tree_a, chunk_a) = shared_tree(&mut objects_a, b"partition-a-side");
    let authored_a = pair.a.engine.author_snapshot(&objects_a, tree_a).unwrap();
    let id_a = authored_a.snapshot().snapshot_id();
    let mut objects_b = MemoryObjectStore::default();
    let (tree_b, chunk_b) = shared_tree(&mut objects_b, b"partition-b-side");
    let authored_b = pair.b.engine.author_snapshot(&objects_b, tree_b).unwrap();
    let id_b = authored_b.snapshot().snapshot_id();
    assert_ne!(id_a, id_b, "same path, different bytes: two snapshots");
    assert_eq!(
        pair.a
            .engine
            .announce_snapshot(&authored_a, &mut PartitionMailbox, None)
            .unwrap(),
        0,
        "partitioned send withholds"
    );
    assert_eq!(
        pair.b
            .engine
            .announce_snapshot(&authored_b, &mut PartitionMailbox, None)
            .unwrap(),
        0,
        "partitioned send withholds"
    );
    assert!(
        !pair.a.engine.pending_announcements().unwrap().is_empty(),
        "A accumulates durable outbox obligations while partitioned"
    );
    assert!(
        !pair.b.engine.pending_announcements().unwrap().is_empty(),
        "B accumulates durable outbox obligations while partitioned"
    );

    // Heal: reachability returns, one pass discharges everything.
    {
        let mut mailbox = MemoryMailbox {
            relay: &mut pair.relay,
            owner: pair.a.device,
        };
        pair.a
            .engine
            .announce_pending(&mut mailbox, None, &mut PublishBudget::unbounded())
            .unwrap();
    }
    {
        let mut mailbox = MemoryMailbox {
            relay: &mut pair.relay,
            owner: pair.b.device,
        };
        pair.b
            .engine
            .announce_pending(&mut mailbox, None, &mut PublishBudget::unbounded())
            .unwrap();
    }
    assert_eq!(drain_side(&mut pair.relay, &mut pair.a).accepted, 1);
    assert_eq!(drain_side(&mut pair.relay, &mut pair.b).accepted, 1);
    assert!(
        pair.a.engine.pending_announcements().unwrap().is_empty(),
        "one heal pass discharges A"
    );
    assert!(
        pair.b.engine.pending_announcements().unwrap().is_empty(),
        "one heal pass discharges B"
    );

    // Bodies cross on the serving route that stayed live through the
    // partition; each side demands the other's chunk so the object
    // leg proves both versions readable, not just announced.
    pair.a
        .engine
        .set_materialization(chunk_b, MaterializationState::Pinned)
        .unwrap();
    pair.b
        .engine
        .set_materialization(chunk_a, MaterializationState::Pinned)
        .unwrap();
    {
        let serving = crate::serving::VaultSource::from_state(
            &pair.a.engine.runtime_state().unwrap(),
            pair.a.engine.vault(),
        )
        .unwrap();
        let mut serving = serving;
        let report = pair
            .b
            .engine
            .execute_plan(&mut serving, &mut pair.b.objects)
            .unwrap();
        assert_eq!(report.snapshot_bodies, 1, "B fetches A's branch");
    }
    {
        let serving = crate::serving::VaultSource::from_state(
            &pair.b.engine.runtime_state().unwrap(),
            pair.b.engine.vault(),
        )
        .unwrap();
        let mut serving = serving;
        let report = pair
            .a
            .engine
            .execute_plan(&mut serving, &mut pair.a.objects)
            .unwrap();
        assert_eq!(report.snapshot_bodies, 1, "A fetches B's branch");
    }
    assert_eq!(
        pair.a.objects.get(&chunk_b).unwrap().as_deref(),
        Some(b"partition-b-side".as_slice()),
        "A reads B's version bytes"
    );
    assert_eq!(
        pair.b.objects.get(&chunk_a).unwrap().as_deref(),
        Some(b"partition-a-side".as_slice()),
        "B reads A's version bytes"
    );

    // Both heads classify eligible on both sides.
    for side in [&pair.a, &pair.b] {
        let heads = side.engine.live_heads().unwrap();
        let mut ids: Vec<_> = heads.iter().map(|h| h.snapshot().snapshot_id()).collect();
        ids.sort();
        let mut expected = vec![id_a, id_b];
        expected.sort();
        assert_eq!(ids, expected, "two live heads after the heal");
        let listed = side.engine.snapshot_heads().unwrap();
        for id in [id_a, id_b] {
            let row = listed.iter().find(|row| row.id == id).unwrap_or_else(|| {
                panic!("healed head {id:?} is listed");
            });
            assert!(
                matches!(row.classification, Classification::Eligible),
                "healed head {id:?} classifies eligible, got {:?}",
                row.classification
            );
        }
    }

    // A write fails closed on the conflicted drive: no single tree to
    // mutate, nothing authored.
    let pending = pair.a.engine.begin_write().unwrap();
    assert_eq!(pending.head_count(), 2, "two rival heads");
    assert_eq!(pending.base_tree(), None, "no base under conflict");
    let mut objects = MemoryObjectStore::default();
    let (tree, _) = shared_tree(&mut objects, b"conflicted-write");
    let error = pair
        .a
        .engine
        .author_pending_write(&objects, tree, pending)
        .unwrap_err();
    assert!(
        matches!(error, super::EngineError::WriteConflicted { heads: 2 }),
        "authoring onto a conflict fails closed, got {error:?}"
    );
    assert_eq!(
        pair.a.engine.live_heads().unwrap().len(),
        2,
        "the refused authoring commits nothing"
    );

    // No-loss over the durable log: every
    // pre-partition snapshot id is still present, both branches are
    // pre-partition snapshot id is still present, both branches are
    // present, and the announcement fact count never shrinks.
    let mut after: BTreeSet<wyrd_format::SnapshotId> = BTreeSet::new();
    after.extend(snapshot_set(&pair.a));
    after.extend(snapshot_set(&pair.b));
    assert!(after.is_superset(&before), "no snapshot id lost");
    assert!(
        after.contains(&id_a) && after.contains(&id_b),
        "both branches kept"
    );
    let announcements_after: usize = pair.a.engine.store.load().unwrap().announcements.len()
        + pair.b.engine.store.load().unwrap().announcements.len();
    assert!(
        announcements_after >= announcements_before,
        "announcement facts never shrink"
    );
}

/// The leg stays runnable in the default gate: its own source must
/// never name a privileged dependency, so a future edit cannot quietly
/// turn the per-merge gate into one that needs the KVM host. This is
/// what keeps the leg in the default nextest profile rather than the
/// slow profile or the manual suite. Tokens are
/// spelled fragmented so this check does not match itself.
#[test]
fn partition_leg_needs_no_privilege() {
    let src = include_str!("tests_partition.rs");
    let forbidden = [
        concat!("su", "do"),
        concat!("/dev/", "kvm"),
        concat!("ip ", "link"),
        concat!("lima", "ctl"),
        concat!("Mini", "Relay"),
        concat!("Live", "Mailbox"),
    ];
    for token in forbidden {
        assert!(
            !src.contains(token),
            "partition leg must stay privilege-free",
        );
    }
}
