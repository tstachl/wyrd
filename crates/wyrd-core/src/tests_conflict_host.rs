//! Host-acquisition tests for the conflict host: the mapping from
//! live engine state to the surface vocabulary runs against real
//! engines through the public authoring API.
//!
//! Reachable states only (a fresh drive, a converged single-head
//! drive): diverged and frozen engines are not constructible through
//! the public API — forking needs two concurrent authors and freezing
//! needs rival transitions — so the diverged comparison, the freeze
//! remedy, and the unavailable copy stay pinned at the surface level
//! (`tests_conflict_surface`), where crafted inputs already cover
//! them. What this file pins is the host seam itself: eligible heads
//! in, screen out, nothing authored.

use wyrd_format::{Entry, FsObjectStore, ObjectKind, ObjectStore, Tree};
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::Engine;

use super::{diverged_report, DEFAULT_INSPECTION_BUDGET};

fn temp_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-core-confhost-{}-{}-{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A fresh drive reports converged with no comparison: zero eligible
/// heads is not divergence, and the screen says the comparison was
/// not requested rather than showing an empty plan.
#[test]
fn diverged_report_on_a_fresh_drive_is_converged_without_inspection() {
    let dir = temp_dir("fresh");
    let identity = DeviceIdentitySecret::generate().unwrap();
    let engine = Engine::create(dir.clone(), "core-test-pass", identity).unwrap();
    let store = FsObjectStore::open(dir).unwrap();

    let revision_before = engine.current();
    let screen = diverged_report(&engine, &store, DEFAULT_INSPECTION_BUDGET).unwrap();

    assert!(
        screen.contains("divergence: none observed (zero or one eligible head)"),
        "fresh drive is converged: {screen}"
    );
    assert!(
        screen.contains("membership: not frozen"),
        "fresh drive carries no freeze: {screen}"
    );
    assert!(
        screen.contains("what differs: not requested in this view"),
        "no comparison without divergence: {screen}"
    );
    assert_eq!(
        engine.current(),
        revision_before,
        "the read-only report authors nothing"
    );
}

/// One authored head stays converged and names no branch: the
/// indicator counts eligible heads, and one is not divergence.
#[test]
fn diverged_report_on_a_single_head_drive_is_converged() {
    let dir = temp_dir("single");
    let identity = DeviceIdentitySecret::generate().unwrap();
    let mut engine = Engine::create(dir.clone(), "core-test-pass", identity).unwrap();
    let mut store = FsObjectStore::open(dir).unwrap();
    let chunk = store.insert(ObjectKind::Chunk, b"kept").unwrap();
    let entry = Entry::file("kept.txt", 4, false, vec![chunk]).unwrap();
    let tree = Tree::from_entries(vec![entry])
        .unwrap()
        .insert_into(&mut store)
        .unwrap();
    let head = engine.author_snapshot(&store, tree).unwrap();
    let head_id = head.snapshot().snapshot_id();

    let revision_before = engine.current();
    let screen = diverged_report(&engine, &store, DEFAULT_INSPECTION_BUDGET).unwrap();

    assert!(
        screen.contains("divergence: none observed (zero or one eligible head)"),
        "one eligible head is converged: {screen}"
    );
    assert!(
        screen.contains("membership: not frozen"),
        "no freeze beside one head: {screen}"
    );
    assert!(
        screen.contains("what differs: not requested in this view"),
        "no comparison for one head: {screen}"
    );
    assert!(
        !screen.contains(&head_id.to_string()),
        "converged screen names no branch: {screen}"
    );
    assert_eq!(
        engine.current(),
        revision_before,
        "the read-only report authors nothing"
    );
}

/// The default budget is the small bounded number the surface pins:
/// the host default and the tested behavior are one constant, not
/// two numbers that can drift.
#[test]
fn default_inspection_budget_is_bounded_and_small() {
    assert_eq!(
        DEFAULT_INSPECTION_BUDGET, 64,
        "the host default matches the surface's exercised budget"
    );
}
