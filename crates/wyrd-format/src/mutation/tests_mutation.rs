use super::*;
use crate::identity::ObjectKind;
use crate::store::MemoryObjectStore;

fn file(name: &str, body: &[u8]) -> Entry {
    Entry::file(
        name,
        u64::try_from(body.len()).unwrap(),
        false,
        vec![ContentId::derive(ObjectKind::Chunk, body)],
    )
    .unwrap()
}

/// Deterministic mint for tests: every creation draws the same fixed
/// identity, so existing behavior tests stay byte-stable. Tests
/// asserting distinctness mint their own identities explicitly.
fn test_mint() -> impl FnMut() -> Result<EntryIdentity, MutationError<MemoryStoreMarker>> {
    || Ok(EntryIdentity::from_bytes([0xE1; 32]))
}

/// Fixed identity for `mkdir` leaves in behavior tests.
fn test_id() -> EntryIdentity {
    EntryIdentity::from_bytes([0xE1; 32])
}

fn empty_root(store: &mut MemoryObjectStore) -> ContentId {
    Tree::empty().insert_into(store).unwrap()
}

/// Walk `path` from `root` without mutating, for assertions.
fn resolve(
    store: &MemoryObjectStore,
    root: ContentId,
    path: &str,
) -> Result<EntryContent, MutationError<MemoryStoreMarker>> {
    let components = parse_path(path)?;
    let mut tree = load_tree(store, &root)?;
    for (index, component) in components.iter().enumerate() {
        let entry = tree
            .entries()
            .iter()
            .find(|candidate| candidate.name == *component)
            .ok_or_else(|| MutationError::NotFound(component.as_str().to_string()))?;
        if index + 1 == components.len() {
            return Ok(entry.content.clone());
        }
        match &entry.content {
            EntryContent::Dir { subtree } => tree = load_tree(store, subtree)?,
            _ => return Err(MutationError::NotADirectory(component.as_str().to_string())),
        }
    }
    unreachable!("a non-empty path returns from the loop")
}

/// A stand-in error type so `resolve` can reuse the mutation helpers
/// with the in-memory store.
type MemoryStoreMarker = <MemoryObjectStore as ObjectStore>::Error;

#[test]
fn put_creates_nested_directories() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(
        &mut store,
        root,
        "a/b/c.txt",
        file("c.txt", b"hello"),
        &mut test_mint(),
    )
    .unwrap();

    assert!(matches!(
        resolve(&store, root, "a"),
        Ok(EntryContent::Dir { .. })
    ));
    assert!(matches!(
        resolve(&store, root, "a/b"),
        Ok(EntryContent::Dir { .. })
    ));
    match resolve(&store, root, "a/b/c.txt").unwrap() {
        EntryContent::File { size, .. } => assert_eq!(size, 5),
        other => panic!("expected file, got {other:?}"),
    }
}

#[test]
fn put_replaces_keeps_siblings_and_leaves_the_old_root() {
    let mut store = MemoryObjectStore::default();
    let root0 = empty_root(&mut store);
    let root1 = put(
        &mut store,
        root0,
        "a.txt",
        file("a.txt", b"one"),
        &mut test_mint(),
    )
    .unwrap();
    let root2 = put(
        &mut store,
        root1,
        "b.txt",
        file("b.txt", b"bee"),
        &mut test_mint(),
    )
    .unwrap();
    let root3 = put(
        &mut store,
        root2,
        "a.txt",
        file("a.txt", b"new"),
        &mut test_mint(),
    )
    .unwrap();

    assert_ne!(root3, root2, "a replace changes the root");
    assert!(matches!(
        resolve(&store, root3, "b.txt"),
        Ok(EntryContent::File { size: 3, .. })
    ));
    // The old root still resolves to the old bytes.
    match resolve(&store, root2, "a.txt").unwrap() {
        EntryContent::File { size, .. } => assert_eq!(size, 3),
        other => panic!("expected file, got {other:?}"),
    }
}

#[test]
fn remove_drops_the_entry_and_keeps_siblings() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(
        &mut store,
        root,
        "a.txt",
        file("a.txt", b"one"),
        &mut test_mint(),
    )
    .unwrap();
    let root = put(
        &mut store,
        root,
        "keep.txt",
        file("keep.txt", b"two"),
        &mut test_mint(),
    )
    .unwrap();
    let root = remove(&mut store, root, "a.txt").unwrap();

    assert!(matches!(
        resolve(&store, root, "keep.txt"),
        Ok(EntryContent::File { .. })
    ));
    assert!(matches!(
        resolve(&store, root, "a.txt"),
        Err(MutationError::NotFound(_))
    ));
}

#[test]
fn put_is_content_addressed() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let a = put(
        &mut store,
        root,
        "a.txt",
        file("a.txt", b"same"),
        &mut test_mint(),
    )
    .unwrap();
    let b = put(
        &mut store,
        root,
        "a.txt",
        file("a.txt", b"same"),
        &mut test_mint(),
    )
    .unwrap();
    assert_eq!(a, b, "identical content yields the same root");
}

#[test]
fn put_strict_does_not_create_intermediate_directories() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    assert!(matches!(
        put_strict(&mut store, root, "a/b", file("b", b"x")),
        Err(MutationError::NotFound(path)) if path == "a"
    ));
}

#[test]
fn invalid_paths_are_rejected() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    for path in ["", "a/", "/a", "a//b", "a/../b", "a/\0b", "."] {
        assert!(
            matches!(
                put(&mut store, root, path, file("x", b"y"), &mut test_mint()),
                Err(MutationError::Path(_))
            ),
            "path {path:?} must be rejected"
        );
    }
}

#[test]
fn a_mismatched_entry_name_is_rejected() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    assert!(matches!(
        put(
            &mut store,
            root,
            "a/b.txt",
            file("c.txt", b"x"),
            &mut test_mint()
        ),
        Err(MutationError::NameMismatch { .. })
    ));
}

#[test]
fn descending_through_a_file_is_not_a_directory() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(&mut store, root, "a", file("a", b"x"), &mut test_mint()).unwrap();
    assert!(matches!(
        put(&mut store, root, "a/b", file("b", b"y"), &mut test_mint()),
        Err(MutationError::NotADirectory(_))
    ));
}

#[test]
fn removing_a_missing_path_is_not_found() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    assert!(matches!(
        remove(&mut store, root, "nope"),
        Err(MutationError::NotFound(_))
    ));
}

#[test]
fn mutating_a_missing_root_is_missing_tree() {
    let mut store = MemoryObjectStore::default();
    let absent = ContentId::from_bytes([0xAB; 32]);
    assert!(matches!(
        put(&mut store, absent, "a", file("a", b"x"), &mut test_mint()),
        Err(MutationError::MissingTree(_))
    ));
}

#[test]
fn path_depth_is_bounded() {
    let accepted = vec!["a"; MAX_PATH_DEPTH].join("/");
    assert!(parse_path(&accepted).is_ok(), "exactly the maximum is fine");
    let too_deep = vec!["a"; MAX_PATH_DEPTH + 1].join("/");
    assert!(matches!(
        parse_path(&too_deep),
        Err(PathError::TooDeep { depth, max })
            if depth == MAX_PATH_DEPTH + 1 && max == MAX_PATH_DEPTH
    ));
}

#[test]
fn nested_removal_rebuilds_the_parent_and_keeps_the_old_root() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(&mut store, root, "a/b/c", file("c", b"c"), &mut test_mint()).unwrap();
    let with_x = put(&mut store, root, "a/x", file("x", b"x"), &mut test_mint()).unwrap();
    let removed = remove(&mut store, with_x, "a/b/c").unwrap();

    assert_ne!(removed, with_x, "the rebuilt path changes the root");
    assert!(matches!(
        resolve(&store, removed, "a/x"),
        Ok(EntryContent::File { .. })
    ));
    assert!(matches!(
        resolve(&store, removed, "a/b"),
        Ok(EntryContent::Dir { .. })
    ));
    assert!(matches!(
        resolve(&store, removed, "a/b/c"),
        Err(MutationError::NotFound(_))
    ));
    // The old root still resolves the removed leaf.
    assert!(matches!(
        resolve(&store, with_x, "a/b/c"),
        Ok(EntryContent::File { .. })
    ));
}

#[test]
fn removing_a_directory_leaf_keeps_the_directory_empty() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(&mut store, root, "d/f", file("f", b"x"), &mut test_mint()).unwrap();
    let root = remove(&mut store, root, "d/f").unwrap();

    match resolve(&store, root, "d").unwrap() {
        EntryContent::Dir { subtree } => assert!(
            load_tree(&store, &subtree).unwrap().entries().is_empty(),
            "the directory survives, empty"
        ),
        other => panic!("expected a directory, got {other:?}"),
    }
}

#[test]
fn symlinks_are_not_traversed_on_put() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(
        &mut store,
        root,
        "s",
        Entry::symlink("s", "target").unwrap(),
        &mut test_mint(),
    )
    .unwrap();
    assert!(matches!(
        put(&mut store, root, "s/x", file("x", b"y"), &mut test_mint()),
        Err(MutationError::NotADirectory(_))
    ));
    // Replacing the symlink with a file is allowed.
    let root = put(&mut store, root, "s", file("s", b"z"), &mut test_mint()).unwrap();
    assert!(matches!(
        resolve(&store, root, "s"),
        Ok(EntryContent::File { .. })
    ));
}

#[test]
fn mkdir_creates_an_empty_directory() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = mkdir(&mut store, root, "d", test_id()).unwrap();

    match resolve(&store, root, "d").unwrap() {
        EntryContent::Dir { subtree } => assert!(
            load_tree(&store, &subtree).unwrap().entries().is_empty(),
            "mkdir creates an empty directory"
        ),
        other => panic!("expected a directory, got {other:?}"),
    }
}

#[test]
fn mkdir_does_not_create_intermediate_directories() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    assert!(matches!(
        mkdir(&mut store, root, "a/b/c", test_id()),
        Err(MutationError::NotFound(_))
    ));
    let root = mkdir(&mut store, root, "a", test_id()).unwrap();
    let root = mkdir(&mut store, root, "a/b", test_id()).unwrap();
    assert!(matches!(
        resolve(&store, root, "a/b"),
        Ok(EntryContent::Dir { .. })
    ));
}

#[test]
fn mkdir_existing_entry_is_already_exists() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = mkdir(&mut store, root, "d", test_id()).unwrap();
    assert!(matches!(
        mkdir(&mut store, root, "d", test_id()),
        Err(MutationError::AlreadyExists(_))
    ));
    let root = put(
        &mut store,
        root,
        "f.txt",
        file("f.txt", b"x"),
        &mut test_mint(),
    )
    .unwrap();
    assert!(matches!(
        mkdir(&mut store, root, "f.txt", test_id()),
        Err(MutationError::AlreadyExists(_))
    ));
    assert!(matches!(
        mkdir(&mut store, root, "f.txt/sub", test_id()),
        Err(MutationError::NotADirectory(_))
    ));
}

#[test]
fn rmdir_removes_only_empty_directories() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = mkdir(&mut store, root, "d", test_id()).unwrap();
    let root = rmdir(&mut store, root, "d").unwrap();
    assert!(matches!(
        resolve(&store, root, "d"),
        Err(MutationError::NotFound(_))
    ));

    let root = put(&mut store, root, "d/f", file("f", b"x"), &mut test_mint()).unwrap();
    assert!(matches!(
        rmdir(&mut store, root, "d"),
        Err(MutationError::DirectoryNotEmpty(_))
    ));
    // Siblings survive a failed rmdir.
    assert!(matches!(
        resolve(&store, root, "d/f"),
        Ok(EntryContent::File { .. })
    ));
}

#[test]
fn rmdir_rejects_files_and_missing_paths() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(
        &mut store,
        root,
        "f.txt",
        file("f.txt", b"x"),
        &mut test_mint(),
    )
    .unwrap();
    assert!(matches!(
        rmdir(&mut store, root, "f.txt"),
        Err(MutationError::NotADirectory(_))
    ));
    assert!(matches!(
        rmdir(&mut store, root, "nope"),
        Err(MutationError::NotFound(_))
    ));
    assert!(matches!(
        rmdir(&mut store, root, "f.txt/sub"),
        Err(MutationError::NotADirectory(_))
    ));
}

#[test]
fn rename_moves_a_file_and_replaces_a_file() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(
        &mut store,
        root,
        "a.txt",
        file("a.txt", b"one"),
        &mut test_mint(),
    )
    .unwrap();
    let root = put(
        &mut store,
        root,
        "b.txt",
        file("b.txt", b"two"),
        &mut test_mint(),
    )
    .unwrap();
    let root = rename(&mut store, root, "a.txt", "b.txt").unwrap();

    assert!(matches!(
        resolve(&store, root, "a.txt"),
        Err(MutationError::NotFound(_))
    ));
    match resolve(&store, root, "b.txt").unwrap() {
        EntryContent::File { size, .. } => assert_eq!(size, 3),
        other => panic!("expected file, got {other:?}"),
    }
}

#[test]
fn rename_moves_a_subtree_across_directories() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(&mut store, root, "a/b/c", file("c", b"c"), &mut test_mint()).unwrap();
    let root = mkdir(&mut store, root, "dst", test_id()).unwrap();
    let root = rename(&mut store, root, "a/b", "dst/b").unwrap();

    assert!(matches!(
        resolve(&store, root, "a/b"),
        Err(MutationError::NotFound(_))
    ));
    match resolve(&store, root, "dst/b/c").unwrap() {
        EntryContent::File { size, .. } => assert_eq!(size, 1),
        other => panic!("expected file, got {other:?}"),
    }
    // The emptied parent survives; only the moved entry leaves.
    assert!(matches!(
        resolve(&store, root, "a"),
        Ok(EntryContent::Dir { .. })
    ));
}

#[test]
fn rename_enforces_the_replacement_matrix() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(&mut store, root, "f", file("f", b"x"), &mut test_mint()).unwrap();
    let root = mkdir(&mut store, root, "d", test_id()).unwrap();
    // File onto a directory is refused even when the directory is empty.
    assert!(matches!(
        rename(&mut store, root, "f", "d"),
        Err(MutationError::IsDirectory(_))
    ));

    // Directory onto a file is refused.
    assert!(matches!(
        rename(&mut store, root, "d", "f"),
        Err(MutationError::NotADirectory(_))
    ));

    // Directory onto an empty directory replaces.
    let root = mkdir(&mut store, root, "e", test_id()).unwrap();
    let root = rename(&mut store, root, "d", "e").unwrap();
    assert!(matches!(
        resolve(&store, root, "d"),
        Err(MutationError::NotFound(_))
    ));
    assert!(matches!(
        resolve(&store, root, "e"),
        Ok(EntryContent::Dir { .. })
    ));

    // Directory onto a non-empty directory is refused.
    let root = mkdir(&mut store, root, "full", test_id()).unwrap();
    let root = put(
        &mut store,
        root,
        "full/k",
        file("k", b"k"),
        &mut test_mint(),
    )
    .unwrap();
    assert!(matches!(
        rename(&mut store, root, "e", "full"),
        Err(MutationError::DirectoryNotEmpty(_))
    ));
}

#[test]
fn rename_rejects_descendant_moves_and_missing_parents() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = mkdir(&mut store, root, "d", test_id()).unwrap();
    let root = put(&mut store, root, "d/f", file("f", b"x"), &mut test_mint()).unwrap();

    assert!(matches!(
        rename(&mut store, root, "d", "d/sub"),
        Err(MutationError::InvalidRename(_))
    ));
    assert!(matches!(
        rename(&mut store, root, "d/f", "absent/g"),
        Err(MutationError::NotFound(_))
    ));
    assert!(matches!(
        rename(&mut store, root, "absent", "d/g"),
        Err(MutationError::NotFound(_))
    ));
}

#[test]
fn rename_same_path_is_a_no_op() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(
        &mut store,
        root,
        "a.txt",
        file("a.txt", b"x"),
        &mut test_mint(),
    )
    .unwrap();
    assert_eq!(rename(&mut store, root, "a.txt", "a.txt").unwrap(), root);
}

#[test]
fn rename_moves_a_symlink_without_following_it() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(
        &mut store,
        root,
        "s",
        Entry::symlink("s", "target").unwrap(),
        &mut test_mint(),
    )
    .unwrap();
    let root = rename(&mut store, root, "s", "t").unwrap();

    match resolve(&store, root, "t").unwrap() {
        EntryContent::Symlink { target } => assert_eq!(target, "target"),
        other => panic!("expected symlink, got {other:?}"),
    }
    assert!(matches!(
        resolve(&store, root, "s"),
        Err(MutationError::NotFound(_))
    ));
}

#[test]
fn rename_trailing_slash_source_requires_a_directory() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = mkdir(&mut store, root, "d", test_id()).unwrap();
    let root = put(&mut store, root, "f", file("f", b"x"), &mut test_mint()).unwrap();
    let root = put(
        &mut store,
        root,
        "s",
        Entry::symlink("s", "target").unwrap(),
        &mut test_mint(),
    )
    .unwrap();

    // A directory source moves identically with or without the slash.
    let moved = rename(&mut store, root, "d/", "e").unwrap();
    assert!(matches!(
        resolve(&store, moved, "e"),
        Ok(EntryContent::Dir { .. })
    ));

    // A trailing slash on a file or symlink is refused, not resolved.
    assert!(matches!(
        rename(&mut store, root, "f/", "g"),
        Err(MutationError::InvalidRename(_))
    ));
    assert!(matches!(
        rename(&mut store, root, "s/", "g"),
        Err(MutationError::InvalidRename(_))
    ));
    // Even a self-rename with a slash validates the source kind.
    assert!(matches!(
        rename(&mut store, root, "f/", "f"),
        Err(MutationError::InvalidRename(_))
    ));
}

#[test]
fn rename_trailing_slash_destination_requires_a_directory() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = mkdir(&mut store, root, "d", test_id()).unwrap();
    let root = mkdir(&mut store, root, "e", test_id()).unwrap();
    let root = put(&mut store, root, "f", file("f", b"x"), &mut test_mint()).unwrap();

    // Directory onto an empty directory with a slash replaces.
    let root = rename(&mut store, root, "d", "e/").unwrap();
    assert!(matches!(
        resolve(&store, root, "e"),
        Ok(EntryContent::Dir { .. })
    ));

    // A slashed destination that is absent or not a directory fails.
    assert!(matches!(
        rename(&mut store, root, "e", "absent/"),
        Err(MutationError::NotFound(_))
    ));
    assert!(matches!(
        rename(&mut store, root, "e", "f/"),
        Err(MutationError::NotADirectory(_))
    ));
    // A file source onto a slashed directory is still a directory clash.
    assert!(matches!(
        rename(&mut store, root, "f", "e/"),
        Err(MutationError::IsDirectory(_))
    ));
    // Doubled separators are not a trailing slash; they stay invalid.
    assert!(matches!(
        rename(&mut store, root, "e", "e//"),
        Err(MutationError::Path(_))
    ));
}

#[test]
fn rename_rejects_non_canonical_paths_on_either_operand() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(&mut store, root, "a", file("a", b"x"), &mut test_mint()).unwrap();
    for (from, to) in [
        ("", "b"),
        ("a", ""),
        ("/a", "b"),
        ("a", "/b"),
        ("a//b", "c"),
        ("a", "b//c"),
        ("a/./b", "c"),
        ("a", "."),
        ("a/../b", "c"),
        ("a", ".."),
    ] {
        assert!(
            matches!(
                rename(&mut store, root, from, to),
                Err(MutationError::Path(_))
            ),
            "{from:?} -> {to:?} must be rejected"
        );
    }
}

#[test]
fn rename_through_a_file_parent_is_not_a_directory() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(&mut store, root, "f", file("f", b"x"), &mut test_mint()).unwrap();
    let root = put(&mut store, root, "g", file("g", b"y"), &mut test_mint()).unwrap();
    assert!(matches!(
        rename(&mut store, root, "g", "f/deep"),
        Err(MutationError::NotADirectory(_))
    ));
    assert!(matches!(
        rename(&mut store, root, "f/deep", "g"),
        Err(MutationError::NotADirectory(_))
    ));
}

#[test]
fn rename_replaces_symlinks_in_both_directions() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(&mut store, root, "f", file("f", b"x"), &mut test_mint()).unwrap();
    let root = put(
        &mut store,
        root,
        "s",
        Entry::symlink("s", "target").unwrap(),
        &mut test_mint(),
    )
    .unwrap();
    let root = mkdir(&mut store, root, "d", test_id()).unwrap();

    // File onto symlink and symlink onto file both replace.
    let root = rename(&mut store, root, "f", "s").unwrap();
    assert!(matches!(
        resolve(&store, root, "s"),
        Ok(EntryContent::File { .. })
    ));
    let root = put(
        &mut store,
        root,
        "t",
        Entry::symlink("t", "other").unwrap(),
        &mut test_mint(),
    )
    .unwrap();
    let root = rename(&mut store, root, "t", "s").unwrap();
    match resolve(&store, root, "s").unwrap() {
        EntryContent::Symlink { target } => assert_eq!(target, "other"),
        other => panic!("expected symlink, got {other:?}"),
    }

    // Symlink onto a directory is a directory clash either way.
    let root = put(
        &mut store,
        root,
        "u",
        Entry::symlink("u", "target").unwrap(),
        &mut test_mint(),
    )
    .unwrap();
    assert!(matches!(
        rename(&mut store, root, "u", "d"),
        Err(MutationError::IsDirectory(_))
    ));
    assert!(matches!(
        rename(&mut store, root, "d", "u"),
        Err(MutationError::NotADirectory(_))
    ));
}

#[test]
fn failed_renames_leave_the_root_untouched() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(&mut store, root, "f", file("f", b"x"), &mut test_mint()).unwrap();
    let root = mkdir(&mut store, root, "d", test_id()).unwrap();
    let root = put(&mut store, root, "d/k", file("k", b"k"), &mut test_mint()).unwrap();

    // Every failure returns no root; the input root keeps resolving.
    for (from, to) in [
        ("absent", "f"),
        ("f", "d"),
        ("d", "f"),
        ("d", "d/sub"),
        ("f/", "g"),
        ("f", "absent/deep"),
    ] {
        assert!(
            rename(&mut store, root, from, to).is_err(),
            "{from:?} -> {to:?} must fail"
        );
    }
    assert!(matches!(
        resolve(&store, root, "f"),
        Ok(EntryContent::File { .. })
    ));
    assert!(matches!(
        resolve(&store, root, "d/k"),
        Ok(EntryContent::File { .. })
    ));
    let bytes = store.get(&root).unwrap().unwrap();
    assert_eq!(Tree::decode(&bytes).unwrap().content_id(), root);
}

#[test]
fn namespace_mutations_keep_canonical_order_and_history() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(
        &mut store,
        root,
        "z.txt",
        file("z.txt", b"z"),
        &mut test_mint(),
    )
    .unwrap();
    let root = mkdir(&mut store, root, "m", test_id()).unwrap();
    let root = rename(&mut store, root, "z.txt", "a.txt").unwrap();

    let bytes = store.get(&root).unwrap().unwrap();
    let decoded = Tree::decode(&bytes).unwrap();
    let names: Vec<&str> = decoded
        .entries()
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(names, ["a.txt", "m"], "canonical order survives mutations");
    assert_eq!(decoded.content_id(), root);
}

/// Read one entry whole (identity included) for assertions.
fn entry_at(store: &MemoryObjectStore, root: ContentId, path: &str) -> Entry {
    let components = parse_path(path).unwrap();
    let mut tree = load_tree(store, &root).unwrap();
    for (index, component) in components.iter().enumerate() {
        let entry = tree
            .entries()
            .iter()
            .find(|candidate| candidate.name == *component)
            .unwrap_or_else(|| panic!("{path} resolves"))
            .clone();
        if index + 1 == components.len() {
            return entry;
        }
        match &entry.content {
            EntryContent::Dir { subtree } => tree = load_tree(store, subtree).unwrap(),
            _ => panic!("{path} walks through a directory"),
        }
    }
    unreachable!("a non-empty path returns from the loop")
}

fn distinct_id(n: u8) -> EntryIdentity {
    EntryIdentity::from_bytes([n; 32])
}

/// `put` keeps the caller's leaf identity and mints fresh
/// identities for intermediates it creates: the leaf is the
/// caller's decision, the intermediates are new entries.
#[test]
fn put_preserves_the_leaf_and_mints_intermediates() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let mut counter = 0u8;
    let mut mint = || {
        counter += 1;
        Ok(distinct_id(counter))
    };
    let leaf = file("c.txt", b"hello").with_identity(distinct_id(7));
    let root = put(&mut store, root, "a/b/c.txt", leaf, &mut mint).unwrap();
    assert_eq!(
        entry_at(&store, root, "a/b/c.txt").identity,
        Some(distinct_id(7))
    );
    let a = entry_at(&store, root, "a").identity;
    let b = entry_at(&store, root, "a/b").identity;
    assert!(a.is_some() && b.is_some(), "intermediates mint");
    assert_ne!(a, b, "intermediates are distinct entries");
    // Rebinding the same intermediates preserves them: a second
    // leaf under the same parents keeps every existing identity.
    let before_a = a;
    let before_b = b;
    let mut counter2 = 100u8;
    let mut mint2 = || {
        counter2 += 1;
        Ok(distinct_id(counter2))
    };
    let root = put(
        &mut store,
        root,
        "a/b/d.txt",
        file("d.txt", b"bye").with_identity(distinct_id(8)),
        &mut mint2,
    )
    .unwrap();
    assert_eq!(entry_at(&store, root, "a").identity, before_a);
    assert_eq!(entry_at(&store, root, "a/b").identity, before_b);
    assert_eq!(
        entry_at(&store, root, "a/b/d.txt").identity,
        Some(distinct_id(8))
    );
}

/// `mkdir` stamps the caller's identity on the new directory and
/// preserves every rebound parent on the way down.
#[test]
fn mkdir_stamps_the_leaf_and_preserves_parents() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(
        &mut store,
        root,
        "p/q.txt",
        file("q.txt", b"x").with_identity(distinct_id(3)),
        &mut test_mint(),
    )
    .unwrap();
    let parent_before = entry_at(&store, root, "p").identity;
    let root = mkdir(&mut store, root, "p/new", distinct_id(9)).unwrap();
    assert_eq!(
        entry_at(&store, root, "p/new").identity,
        Some(distinct_id(9))
    );
    assert_eq!(entry_at(&store, root, "p").identity, parent_before);
}

/// Rename preserves the moved identity and retires the replaced
/// destination: the destination entry is deleted, not merged.
#[test]
fn rename_preserves_the_moved_identity_and_retires_the_replaced() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(
        &mut store,
        root,
        "from.txt",
        file("from.txt", b"data").with_identity(distinct_id(5)),
        &mut test_mint(),
    )
    .unwrap();
    let root = put(
        &mut store,
        root,
        "to.txt",
        file("to.txt", b"old").with_identity(distinct_id(6)),
        &mut test_mint(),
    )
    .unwrap();
    let root = rename(&mut store, root, "from.txt", "to.txt").unwrap();
    assert_eq!(
        entry_at(&store, root, "to.txt").identity,
        Some(distinct_id(5)),
        "the moved entry keeps its identity over the replacement"
    );
    // The retired identity names no entry anymore.
    for entry in Tree::decode(&store.get(&root).unwrap().unwrap())
        .unwrap()
        .entries()
    {
        assert_ne!(
            entry.identity,
            Some(distinct_id(6)),
            "retired identity is gone"
        );
    }
}

/// A same-kind replace through `put` is the caller's decision:
/// the format keeps whatever identity the caller supplies.
#[test]
fn put_replace_keeps_the_caller_supplied_identity() {
    let mut store = MemoryObjectStore::default();
    let root = empty_root(&mut store);
    let root = put(
        &mut store,
        root,
        "a.txt",
        file("a.txt", b"one").with_identity(distinct_id(1)),
        &mut test_mint(),
    )
    .unwrap();
    let root = put(
        &mut store,
        root,
        "a.txt",
        file("a.txt", b"two").with_identity(distinct_id(2)),
        &mut test_mint(),
    )
    .unwrap();
    assert_eq!(
        entry_at(&store, root, "a.txt").identity,
        Some(distinct_id(2))
    );
}
