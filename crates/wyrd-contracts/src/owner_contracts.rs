//! Owner-interface behavioral gate that needs a forked drive
//! (child 2): a write through the interface against a two-head
//! drive relays the refusal instead of reinterpreting it. The fork
//! is genuine — two member devices authoring parentless snapshots
//! concurrently, intaken through the public drain path — so the
//! evaluation the submit hits is the same one an in-process submit
//! hits. The join ceremony is shared with the join suite; the fork
//! choreography is this test's own.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use wyrd_daemon::owner::{
    OwnerHandles, OwnerServer, RawRequest, SubmitParams, WireErrorCode, WireMutationKind,
    WireResult, OP_SUBMIT, WIRE_VERSION,
};
use wyrd_format::{
    ContentId, Entry, FsObjectStore, MemoryObjectStore, ObjectKind, ObjectStore, Tree,
};
use wyrd_sync::runtime::{Engine, PublishBudget};
use wyrd_sync::transport::mailbox::{Disposition, Mailbox};

use crate::join_contracts::{admit_and_collect, newcomer_relay, owner};
use crate::support::Relay;
use crate::support::{device, scratch_dir};

/// Insert fork bytes into a store and return the tree: the same
/// bytes in any store hash to the same identities, so the authoring
/// store and the serving view converge on the same closure without
/// sharing memory.
fn seed_fork_bytes(store: &mut MemoryObjectStore, label: &[u8], name: &str) -> ContentId {
    let chunk = store
        .insert(ObjectKind::Chunk, label)
        .expect("the fork chunk inserts");
    Tree::from_entries(vec![Entry::file(
        name,
        label.len() as u64,
        false,
        vec![chunk],
    )
    .expect("the entry builds")])
    .expect("the fork tree builds")
    .insert_into(store)
    .expect("the fork tree inserts")
}

/// Author one parentless snapshot over fresh local objects: with no
/// live heads the author parents onto nothing, so two members doing
/// this concurrently fork by construction.
fn author_parentless(engine: &mut Engine, label: &[u8], name: &str) -> wyrd_format::SnapshotId {
    let mut objects = MemoryObjectStore::default();
    let tree = seed_fork_bytes(&mut objects, label, name);
    engine
        .author_snapshot(&objects, tree)
        .expect("the parentless head authors")
        .snapshot()
        .snapshot_id()
}

/// A write through the interface against a two-head drive returns
/// `Conflicted`: the interface relays the refusal, it never
/// reinterprets it.
#[test]
fn submit_against_two_heads_relays_conflicted() {
    // The owner holds epoch 1 alone: no snapshots exist, so the
    // join below carries membership only and the newcomer authors
    // parentless too.
    let mut holder_a = owner();
    assert_eq!(
        holder_a.engine.live_heads().unwrap().len(),
        0,
        "no heads before the fork"
    );

    // A second member joins through the invitation and capability
    // path. The admission carries no snapshots, so both members
    // author without seeing the other first.
    let newcomer = device(0x20);
    let mut join_relay = Relay::new();
    let (mut engine_b, envelopes) = admit_and_collect(&mut holder_a, &newcomer, &mut join_relay);
    let mut intake = newcomer_relay(envelopes);
    let report = engine_b.drain(&mut intake).unwrap();
    assert!(report.accepted >= 2, "transition plus capability drain");

    // Both authors are parentless: the drive forks by construction.
    // Only the rival's body travels: canonical snapshot bytes are
    // public plaintext, and eligibility is a DAG judgment over
    // committed bodies — manifests and chunks matter for serving,
    // never for the evaluation a submit hits.
    let second = author_parentless(&mut engine_b, b"second head", "b.txt");
    let first = author_parentless(&mut holder_a.engine, b"first head", "a.txt");
    assert_ne!(first, second, "concurrent authors diverge");
    let rival_body = engine_b
        .live_heads()
        .unwrap()
        .into_iter()
        .find(|head| head.snapshot().snapshot_id() == second)
        .expect("the rival head is live on its engine")
        .snapshot()
        .encode();
    assert_eq!(
        engine_b.live_heads().unwrap().len(),
        1,
        "the rival head stands alone on its engine"
    );
    assert_eq!(
        holder_a.engine.live_heads().unwrap().len(),
        1,
        "the first head stands alone on its engine"
    );

    // The rival announcement leaves through the public outbox
    // path with the relay as its mailbox: exactly the queued
    // obligations, nothing else.
    let mut outbox = Relay::new();
    let sent = engine_b
        .announce_pending(&mut outbox, None, &mut PublishBudget::unbounded())
        .expect("the rival announcement sends");
    assert!(sent >= 1, "the rival announcement leaves");
    let mut envelopes = Vec::new();
    while let Some(delivery) = outbox.recv().unwrap() {
        let id = delivery.id();
        envelopes.push(delivery.envelope().clone());
        outbox.settle(id, Disposition::Ack).unwrap();
    }
    assert_eq!(envelopes.len(), 1, "one announcement, nothing else");

    // Compose the owner over the forked engine and serve submits
    // through the interface with the loop running. The loop's
    // mailbox carries the announcement; its bulk source carries the
    // rival body, so the fetch plan resolves the closure header and
    // the fork evaluates instead of pending forever.
    let mut bulk = wyrd_sync::bulk::MemoryBulkSource::default();
    bulk.publish_snapshot(second, rival_body);
    let mut loop_mailbox = newcomer_relay(envelopes);
    let view_dir = scratch_dir("owner-fork-view");
    let view_store = FsObjectStore::open(view_dir.clone()).unwrap();
    let mut daemon: wyrd_core::node::WyrdNode<
        wyrd_fuse::DriveView<FsObjectStore, wyrd_core::view::RuntimeMaterialization>,
    > = wyrd_core::node::WyrdNode::new(holder_a.engine, view_store).unwrap();
    daemon.refresh_live_heads().unwrap();
    let (mut live, parts) = daemon
        .into_live(
            Duration::from_secs(5),
            &wyrd_core::live::LiveConfig::default(),
        )
        .unwrap();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let loop_thread = std::thread::spawn(move || {
        live.run_loop(
            &mut loop_mailbox,
            Some(&mut bulk),
            stop,
            &wyrd_core::live::LiveConfig::default(),
            &mut |_, _| {},
        )
    });
    // The fork lands through intake and fetch: poll the published
    // observation until both heads evaluate.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let heads = parts
            .observation
            .read()
            .map(|slot| {
                slot.as_ref()
                    .map(|observation| observation.status.live_heads.len())
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        if heads == 2 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the fork never evaluated: still {heads} live head(s)"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let submit = |id: u64| {
        server.dispatch(RawRequest {
            v: WIRE_VERSION,
            id,
            op: OP_SUBMIT.to_string(),
            params: serde_json::to_value(SubmitParams {
                kind: WireMutationKind::Mkdir {
                    path: "nope".to_string(),
                },
            })
            .unwrap(),
        })
    };
    match submit(1).result {
        WireResult::Err(error) => {
            assert_eq!(error.code, WireErrorCode::Conflicted);
        }
        other => panic!("write on a two-head drive unexpectedly succeeded: {other:?}"),
    }
    // The client's own mapping is unchanged by the transport: the
    // same refusal the FUSE boundary maps to EIO arrives here with
    // the two-head count in the message.
    match submit(2).result {
        WireResult::Err(error) => {
            assert!(
                error.message.contains('2'),
                "the message names the head count: {}",
                error.message
            );
        }
        other => panic!("second write unexpectedly succeeded: {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    let summary = loop_thread.join().unwrap();
    assert!(summary.is_ok(), "the loop ends clean on stop");
    drop(parts);
    drop(server);
    std::fs::remove_dir_all(view_dir).unwrap();
}
