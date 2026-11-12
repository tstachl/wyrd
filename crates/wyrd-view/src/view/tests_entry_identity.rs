//! Entry identity at the view layer: `stat`/`readdir` expose the
//! token, agreement compares it, and divergence carries it per
//! version. Common descent without resolution: two heads sharing an
//! identity still conflict when their contents differ, and identical
//! contents under different identities never merge into agreement.

use super::tests_harness::*;
use super::*;
use wyrd_format::{Entry, EntryIdentity, MemoryObjectStore, ObjectKind, ObjectStore, Tree};

fn token(seed: u8) -> EntryIdentity {
    EntryIdentity::from_bytes([seed; 32])
}

fn file_with(name: &str, size: u64, id: EntryIdentity) -> Entry {
    Entry::file(name, size, false, vec![])
        .unwrap()
        .with_identity(id)
}

/// `stat` and `readdir` expose the token; the root carries none —
/// it is an address, not an entry.
#[test]
fn reads_expose_identity_and_the_root_carries_none() {
    let drive = small_drive();
    let view = view(drive);
    let attr = view.stat("sub").unwrap();
    assert_eq!(
        attr.identity, None,
        "legacy fixture entries predate identity"
    );
    let entries = view.readdir(&view.lookup("").unwrap()).unwrap();
    assert!(
        entries.iter().all(|entry| match &entry.node {
            Node::File { identity, .. }
            | Node::Dir { identity, .. }
            | Node::Symlink { identity, .. } => identity.is_none(),
            Node::MergedDir { .. } | Node::Conflict { .. } => true,
        }),
        "legacy entries expose no token"
    );
    let root = view.lookup("").unwrap();
    assert!(
        matches!(
            root,
            Node::Dir { identity: None, .. } | Node::MergedDir { .. }
        ),
        "the root is not an entry: {root:?}"
    );
}

/// One entry under one identity, read from a tree that carries it:
/// lookup, stat, and readdir agree on the token.
#[test]
fn single_head_reads_carry_the_token() {
    let mut store = MemoryObjectStore::default();
    let id = token(0xA1);
    let root = tree_of(&mut store, vec![file_with("f.txt", 3, id)]);
    let view = DriveView::new(
        store,
        FakeMaterialization::empty(),
        heads(vec![snapshot(root)]),
    );
    assert_eq!(
        view.lookup("f.txt").unwrap(),
        Node::File {
            size: 3,
            executable: false,
            chunks: vec![],
            identity: Some(id),
        }
    );
    assert_eq!(view.stat("f.txt").unwrap().identity, Some(id));
    let entries = view.readdir(&view.lookup("").unwrap()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "f.txt");
    match &entries[0].node {
        Node::File { identity, .. } => assert_eq!(*identity, Some(id)),
        other => panic!("f.txt is a file, saw {other:?}"),
    }
}

/// Same entry, same token, same content on two heads: agreement,
/// not conflict — common descent serves.
#[test]
fn shared_identity_with_shared_content_agrees() {
    let mut store = MemoryObjectStore::default();
    let id = token(0xB2);
    let root = tree_of(&mut store, vec![file_with("f.txt", 3, id)]);
    let view = DriveView::new(
        store,
        FakeMaterialization::empty(),
        heads(vec![snapshot(root), snapshot(root)]),
    );
    assert_eq!(
        view.lookup("f.txt").unwrap(),
        Node::File {
            size: 3,
            executable: false,
            chunks: vec![],
            identity: Some(id),
        },
        "one entry on two heads serves, not conflicts"
    );
}

/// Same identity, divergent content: still two heads — a conflict
/// whose versions both carry the shared token. Identity is common
/// descent, never a resolved state.
#[test]
fn shared_identity_with_divergent_content_conflicts() {
    let mut store = MemoryObjectStore::default();
    let id = token(0xC3);
    let one = tree_of(&mut store, vec![file_with("f.txt", 3, id)]);
    let two = tree_of(&mut store, vec![file_with("f.txt", 4, id)]);
    let view = DriveView::new(
        store,
        FakeMaterialization::empty(),
        heads(vec![snapshot(one), snapshot(two)]),
    );
    match view.lookup("f.txt").unwrap() {
        Node::Conflict { versions } => {
            assert_eq!(versions.len(), 2);
            for version in &versions {
                match &version.node {
                    Node::File { identity, .. } => assert_eq!(*identity, Some(id)),
                    other => panic!("versions stay files, saw {other:?}"),
                }
            }
        }
        other => panic!("divergent contents conflict, saw {other:?}"),
    }
    // The conflicted path itself is synthetic: no token of its own.
    assert_eq!(view.stat("f.txt").unwrap().identity, None);
}

/// Same content under different identities: different entries that
/// merely look alike — they conflict, never agree.
#[test]
fn distinct_identities_with_shared_content_conflict() {
    let mut store = MemoryObjectStore::default();
    let one = tree_of(&mut store, vec![file_with("f.txt", 3, token(0xD4))]);
    let two = tree_of(&mut store, vec![file_with("f.txt", 3, token(0xD5))]);
    let view = DriveView::new(
        store,
        FakeMaterialization::empty(),
        heads(vec![snapshot(one), snapshot(two)]),
    );
    assert!(
        matches!(view.lookup("f.txt").unwrap(), Node::Conflict { .. }),
        "delete-plus-recreate on one head diverges from the untouched entry"
    );
}

/// Transfer fidelity in miniature: tree bytes decoded into a
/// fresh store (what a peer installs verbatim) serve the same
/// token — no inference, no matching, just the bytes.
#[test]
fn transferred_bytes_serve_the_same_token() {
    let mut store = MemoryObjectStore::default();
    let id = token(0xE6);
    let root = tree_of(&mut store, vec![file_with("old.txt", 3, id)]);
    let bytes = store.get(&root).unwrap().unwrap();
    // A fresh store, as a peer would hold after fetching: the
    // encoded transfer, decoded without interpretation.
    let mut peer = MemoryObjectStore::default();
    peer.insert_verified(ObjectKind::Tree, &root, &bytes)
        .unwrap();
    let view = DriveView::new(
        peer,
        FakeMaterialization::empty(),
        heads(vec![snapshot(root)]),
    );
    assert_eq!(view.stat("old.txt").unwrap().identity, Some(id));
    let decoded = Tree::decode(&bytes).unwrap();
    assert_eq!(
        decoded.entries()[0].identity,
        Some(id),
        "the token rode in the bytes"
    );
}
