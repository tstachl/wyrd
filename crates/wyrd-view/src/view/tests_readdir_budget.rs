//! Bounded enumeration: the listing budget is charged before entries
//! are collected, so an over-limit tree is refused instead of
//! materialized and handed to a mapping layer.

use super::*;

use wyrd_format::{ContentId, Entry, MemoryObjectStore, ObjectStore};

use crate::view::tests_harness::{
    chunk, heads, small_drive, snapshot, snapshot_at, tree_of, view as make_view, CountingStore,
    FakeMaterialization,
};

/// A lawful directory well under the serving bound serves exactly as
/// the unbounded surface does: the budget does not truncate or alter a
/// directory that fits.
#[test]
fn bounded_listing_serves_a_directory_under_budget() {
    let drive = small_drive();
    let view = make_view(drive);
    let node = view.lookup("").unwrap();
    let listing = view.readdir_budgeted(&node, MAX_READDIR_WORK).unwrap();
    assert!(!listing.limit_exceeded);
    assert!(listing.work > 0);
    let names: Vec<_> = listing
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(names, ["hello.txt", "link", "sub"]);
    assert_eq!(listing.entries, view.readdir(&node).unwrap());
}

/// A directory whose estimated served size exceeds the budget refuses
/// with an empty result. The refusal happens while collecting, not
/// after a full vector has been built.
#[test]
fn bounded_listing_refuses_an_over_budget_directory() {
    let mut store = MemoryObjectStore::default();
    let subtree = tree_of(
        &mut store,
        vec![Entry::dir("child", ContentId::from_bytes([1; 32])).unwrap()],
    );
    let entries = (0..200)
        .map(|index| Entry::dir(format!("name-{index:04}-{}", "x".repeat(200)), subtree).unwrap())
        .collect();
    let root = tree_of(&mut store, entries);
    let view = DriveView::new(
        store,
        FakeMaterialization::empty(),
        heads(vec![snapshot(root)]),
    );
    let node = view.lookup("").unwrap();
    let budget = 16 * 1024;
    let listing = view.readdir_budgeted(&node, budget).unwrap();
    assert!(listing.limit_exceeded);
    assert!(listing.work > budget);
    assert!(
        listing.entries.is_empty(),
        "a refusal is never a partial listing"
    );
}

/// A flat directory far below `Limits::V0.max_tree_entries` is still
/// lawful to author, but its listing is past the live serving bound.
/// The production ceiling refuses it with an empty result rather than
/// materializing a million-entry response.
#[test]
fn lawful_flat_directory_over_serving_bound_refuses() {
    let mut store = MemoryObjectStore::default();
    let subtree = tree_of(
        &mut store,
        vec![Entry::dir("child", ContentId::from_bytes([1; 32])).unwrap()],
    );
    let entries: Vec<_> = (0..12_000)
        .map(|index| Entry::dir(format!("name-{index:05}-{}", "x".repeat(200)), subtree).unwrap())
        .collect();
    let root = tree_of(&mut store, entries);
    let view = DriveView::new(
        store,
        FakeMaterialization::empty(),
        heads(vec![snapshot(root)]),
    );
    let node = view.lookup("").unwrap();
    let listing = view.readdir_budgeted(&node, MAX_READDIR_WORK).unwrap();
    assert!(listing.limit_exceeded);
    assert!(listing.work > MAX_READDIR_WORK);
    assert!(listing.entries.is_empty());
}

/// Tree bytes are charged before decoding, so a single very wide tree
/// is refused without paying for the decode at all.
#[test]
fn bounded_listing_charges_tree_bytes_before_decoding() {
    let mut store = MemoryObjectStore::default();
    let root = tree_of(
        &mut store,
        vec![Entry::symlink("s", "t".repeat(1024)).unwrap()],
    );
    // The charge is the whole encoded buffer plus one byte: the store
    // exposes bytes, not a length hint, so this allocation precedes the
    // verdict and is documented as the residual per-request spike.
    let encoded = store.get(&root).unwrap().unwrap().len() as u64;
    let view = DriveView::new(
        store,
        FakeMaterialization::empty(),
        heads(vec![snapshot(root)]),
    );
    let node = view.lookup("").unwrap();
    let listing = view.readdir_budgeted(&node, 16).unwrap();
    assert!(listing.limit_exceeded);
    assert_eq!(listing.work, encoded + 1);
    assert!(listing.entries.is_empty());
}

/// One entry can carry its own wire amplification: a file with a huge
/// chunk list is refused by the estimated-byte charge even with few
/// directory entries.
#[test]
fn bounded_listing_counts_chunk_lists_in_the_entry_estimate() {
    let mut store = MemoryObjectStore::default();
    let id = chunk(&mut store, b"x");
    let root = tree_of(
        &mut store,
        vec![Entry::file("big", 0, false, vec![id; 20_000]).unwrap()],
    );
    let view = DriveView::new(
        store,
        FakeMaterialization::empty(),
        heads(vec![snapshot(root)]),
    );
    let node = view.lookup("").unwrap();
    let budget = 1024 * 1024;
    let listing = view.readdir_budgeted(&node, budget).unwrap();
    assert!(listing.limit_exceeded);
    assert!(listing.work > budget);
    assert!(listing.entries.is_empty());
}

/// A merged directory shares the budget across its per-head trees: the
/// first over-limit tree stops the walk before the next store read.
#[test]
fn bounded_union_stops_before_loading_the_next_head() {
    let mut inner = MemoryObjectStore::default();
    let subtree = tree_of(
        &mut inner,
        vec![Entry::dir("child", ContentId::from_bytes([1; 32])).unwrap()],
    );
    // Distinct tree bytes so the heads disagree at the root and the
    // union path — the one under test — is actually taken.
    let tree_with = |pad: &str| -> Vec<Entry> {
        (0..200)
            .map(|index| Entry::dir(format!("n{index}-{}", pad.repeat(200)), subtree).unwrap())
            .collect()
    };
    let first = tree_of(&mut inner, tree_with("y"));
    let second = tree_of(&mut inner, tree_with("z"));
    let store = CountingStore {
        inner,
        gets: std::cell::Cell::new(0),
    };
    let view = DriveView::new(
        store,
        FakeMaterialization::empty(),
        heads(vec![snapshot_at(first, 1), snapshot_at(second, 2)]),
    );
    let node = view.lookup("").unwrap();
    assert!(matches!(node, Node::MergedDir { .. }));
    view.store_read().unwrap().gets.set(0);
    let listing = view.readdir_budgeted(&node, 16 * 1024).unwrap();
    assert!(listing.limit_exceeded);
    assert!(listing.entries.is_empty());
    assert_eq!(
        view.store_read().unwrap().gets.get(),
        1,
        "the second head's tree is never read once the budget trips"
    );
}

/// The estimate grows with the two payload dimensions that a single
/// entry can amplify: its name and its chunk list.
#[test]
fn entry_estimate_grows_with_name_and_chunk_count() {
    let dir = Node::Dir {
        subtree: ContentId::from_bytes([2; 32]),
        identity: None,
    };
    assert!(dir_entry_work("a-longer-name", &dir) > dir_entry_work("a", &dir));
    let id = ContentId::from_bytes([3; 32]);
    let no_chunks = Node::File {
        size: 0,
        executable: false,
        chunks: Vec::new(),
        identity: None,
    };
    let with_chunks = Node::File {
        size: 0,
        executable: false,
        chunks: vec![id],
        identity: None,
    };
    assert!(dir_entry_work("x", &with_chunks) > dir_entry_work("x", &no_chunks));
}
