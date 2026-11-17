//! Read-path replay contracts: the offline listings and dry runs
//! project durable state once per command, not once per helper.
//! Counts pin through `store.rebuild_count()`; the trust boundary
//! (only verified eligible heads carry author/tree detail) is pinned
//! by the `@N` contract test, not here.

use std::collections::BTreeMap;

use super::merge::MergeSelection;
use super::snapshot::author_with_parents;
use super::tests_harness::owner_engine;
use crate::runtime::engine::Engine;
use wyrd_format::{ContentId, Entry, MemoryObjectStore, ObjectKind, ObjectStore, SnapshotId, Tree};

/// Several one-chunk files over one root, inserted into `objects`.
fn tree(objects: &mut MemoryObjectStore, files: &[(&str, &[u8])]) -> ContentId {
    let mut entries = Vec::new();
    for (name, bytes) in files {
        let chunk = objects.insert(ObjectKind::Chunk, bytes).unwrap();
        entries.push(Entry::file(*name, bytes.len() as u64, false, vec![chunk]).unwrap());
    }
    Tree::from_entries(entries)
        .unwrap()
        .insert_into(objects)
        .unwrap()
}

/// Same-epoch forks off one authored base. The base becomes history;
/// the forks are exactly the eligible heads. Authoring never serves
/// the live-heads memo, so it stays cold for the measurement — like
/// the CLI's one-shot process.
fn forks(
    engine: &mut Engine,
    objects: &mut MemoryObjectStore,
    base: ContentId,
    trees: &[ContentId],
) -> Vec<SnapshotId> {
    let first = engine.author_snapshot(objects, base).unwrap();
    let first_id = first.snapshot().snapshot_id();
    trees
        .iter()
        .map(|tree| {
            author_with_parents(engine, objects, *tree, vec![first_id])
                .unwrap()
                .snapshot()
                .snapshot_id()
        })
        .collect()
}

/// Two snapshots: the base holds `a` and `b`, its child holds `c`.
/// The base is history afterwards; the child is the eligible head —
/// a recovery source with rows the live head lacks.
fn base_and_child(
    engine: &mut Engine,
    objects: &mut MemoryObjectStore,
) -> (SnapshotId, SnapshotId) {
    let base = tree(objects, &[("a.txt", b"a"), ("b.txt", b"b")]);
    let base_id = engine
        .author_snapshot(objects, base)
        .unwrap()
        .snapshot()
        .snapshot_id();
    let child = tree(objects, &[("c.txt", b"c")]);
    let child_id = author_with_parents(engine, objects, child, vec![base_id])
        .unwrap()
        .snapshot()
        .snapshot_id();
    (base_id, child_id)
}

/// The `heads` listing projects once: the classified listing and
/// the live-head join share one durable rebuild.
#[test]
fn heads_report_rebuilds_once() {
    let (_dir, mut engine, _) = owner_engine("read-paths-heads");
    let mut objects = MemoryObjectStore::default();
    let base = tree(&mut objects, &[("base", b"base")]);
    let t1 = tree(&mut objects, &[("a", b"a1")]);
    let t2 = tree(&mut objects, &[("a", b"a2")]);
    forks(&mut engine, &mut objects, base, &[t1, t2]);

    let before = engine.store.rebuild_count();
    let listed = engine.snapshot_heads().unwrap();
    let live = engine.live_heads().unwrap();
    assert_eq!(listed.len(), 2, "every tip is listed with its class");
    assert_eq!(live.len(), 2, "both forks stay eligible");
    // The join is by id: the listing's eligible set is exactly the
    // live set, so no row borrows detail from another head.
    let mut listed_eligible: Vec<SnapshotId> = listed
        .iter()
        .filter(|head| {
            matches!(
                head.classification,
                crate::authorization::Classification::Eligible
            )
        })
        .map(|head| head.id)
        .collect();
    listed_eligible.sort();
    let mut live_ids: Vec<SnapshotId> = live
        .iter()
        .map(|head| head.snapshot().snapshot_id())
        .collect();
    live_ids.sort();
    assert_eq!(
        listed_eligible, live_ids,
        "eligible rows join exactly the live heads"
    );
    assert_eq!(
        engine.store.rebuild_count() - before,
        1,
        "listing plus live join share one rebuild"
    );
}

/// One quiet pass projects once: every read-only consumer — live
/// heads, runtime state, reconciliation counters, durability level,
/// pending outbox queries, the epoch-pinned listing, and the
/// classified heads listing — shares the pass-scoped projection
/// keyed by CURRENT instead of replaying history each.
#[test]
fn quiet_read_bundle_rebuilds_once() {
    let (_dir, mut engine, _) = owner_engine("read-paths-quiet-bundle");
    let mut objects = MemoryObjectStore::default();
    let base = tree(&mut objects, &[("base", b"base")]);
    let t1 = tree(&mut objects, &[("a", b"a1")]);
    let t2 = tree(&mut objects, &[("a", b"a2")]);
    forks(&mut engine, &mut objects, base, &[t1, t2]);

    let before = engine.store.rebuild_count();
    let live = engine.live_heads().unwrap();
    assert_eq!(live.len(), 2, "two forks stay eligible");
    let _ = engine.runtime_state().unwrap();
    let _ = engine.reconciliation_counters().unwrap();
    let _ = engine.durability_level().unwrap();
    let _ = engine.pending_announcements().unwrap();
    let _ = engine.pending_carries().unwrap();
    let _ = engine.withdrawal_outstanding().unwrap();
    let _ = engine.live_heads_at_epoch(1).unwrap();
    let listed = engine.snapshot_heads().unwrap();
    assert_eq!(listed.len(), 2, "every tip is listed with its class");
    assert_eq!(
        engine.store.rebuild_count() - before,
        1,
        "one quiet bundle costs one replay no matter how many projections it consults"
    );
}

/// Sequential commits invalidate exactly once each: three authors
/// plus a closing read bundle cost four rebuilds — every commit
/// moves CURRENT past the cached projection, and the bundle
/// replays once. A stale hit would serve pre-commit heads; a
/// missing invalidation would replay per call site.
#[test]
fn sequential_authors_rebuild_once_each() {
    let (_dir, mut engine, _) = owner_engine("read-paths-sequential");
    let mut objects = MemoryObjectStore::default();

    let before = engine.store.rebuild_count();
    for (name, bytes) in [("a", b"a".as_slice()), ("b", b"b"), ("c", b"c")] {
        let root = tree(&mut objects, &[(name, bytes)]);
        engine.author_snapshot(&objects, root).unwrap();
    }
    let live = engine.live_heads().unwrap();
    assert_eq!(live.len(), 1, "each author supersedes the last");
    let _ = engine.runtime_state().unwrap();
    assert_eq!(
        engine.store.rebuild_count() - before,
        4,
        "three commits plus one closing bundle cost four replays"
    );
}

/// The merge dry run over the default selection projects once:
/// selection, rows, and verdict share the pass-scoped projection.
#[test]
fn merge_dry_run_rebuilds_once_over_default_selection() {
    let (_dir, mut engine, _) = owner_engine("read-paths-merge");
    let mut objects = MemoryObjectStore::default();
    let base = tree(&mut objects, &[("base", b"base")]);
    let t1 = tree(&mut objects, &[("a", b"a1")]);
    let t2 = tree(&mut objects, &[("a", b"a2")]);
    forks(&mut engine, &mut objects, base, &[t1, t2]);

    let before = engine.store.rebuild_count();
    // Default selection, like the CLI: every live head, sorted.
    let mut selected: Vec<SnapshotId> = engine
        .live_heads()
        .unwrap()
        .iter()
        .map(|head| head.snapshot().snapshot_id())
        .collect();
    selected.sort();
    // Rows, like the CLI's plan report.
    let _rows = engine.merge_plan(&objects, selected.clone()).unwrap();
    // Verdict, like the CLI's dry run: a closing spec takes one side.
    let spec = BTreeMap::from([("a".to_owned(), MergeSelection::Take(selected[0]))]);
    let _verdict = engine
        .merge_preview(&objects, selected, None, &spec)
        .unwrap();
    assert_eq!(
        engine.store.rebuild_count() - before,
        1,
        "selection, rows, and verdict share one rebuild"
    );
}

/// The recovery dry run projects once: rows and verdict share the
/// pass-scoped projection.
#[test]
fn recover_dry_run_rebuilds_once() {
    let (_dir, mut engine, _) = owner_engine("read-paths-recover");
    let mut objects = MemoryObjectStore::default();
    let (from, _) = base_and_child(&mut engine, &mut objects);

    let before = engine.store.rebuild_count();
    // Rows, like the CLI's plan report.
    let _rows = engine.recovery_plan(&objects, from).unwrap();
    // Verdict, like the CLI's dry run: take the row the live head
    // lacks, whose bytes are local.
    let _verdict = engine
        .recovery_preview(&mut objects, from, &["a.txt".to_owned()], false, &[])
        .unwrap();
    assert_eq!(
        engine.store.rebuild_count() - before,
        1,
        "rows and verdict share one rebuild"
    );
}
