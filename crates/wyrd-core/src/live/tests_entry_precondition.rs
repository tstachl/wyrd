//! Entry-identity preconditions on the owner mutation path: an
//! entry-addressed mutation carries the caller's expected token, and
//! the serialized apply checks it before touching the tree. Same
//! token proceeds (the arm still applies its own content and kind
//! checks); a different token — including a legacy entry that
//! never had one — is `StaleEntry`; a missing path is `NotFound`
//! (not the legacy `Stale`); a conflicted drive is `Conflicted`.
//! `None` expected keeps exactly the legacy behavior, so FUSE and
//! old in-process callers are byte-for-byte unaffected.

use super::parent_mutation_tests::{live_over_tree, TreeView};
use super::*;
use wyrd_format::{EntryIdentity, MemoryObjectStore, Tree};
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::Engine;

/// A scratch engine over a memory store, ready for live-loop
/// authoring from empty.
fn scratch_node(tag: &str) -> (LiveNode<TreeView>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-core-precondition-{tag}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let engine = Engine::create(
        dir.clone(),
        "core-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let node = live_over_tree(engine, MemoryObjectStore::default(), &[]);
    (node, dir)
}

/// The token the live view reports at `path`, or `None` where the
/// path does not resolve.
fn observed_token(node: &LiveNode<TreeView>, path: &str) -> Option<EntryIdentity> {
    let heads = node.live_heads_traced().unwrap();
    match node.current_node(&heads, path).unwrap() {
        Some(Node::File { identity, .. })
        | Some(Node::Dir { identity, .. })
        | Some(Node::Symlink { identity, .. }) => identity,
        _ => None,
    }
}

/// Create one empty file through the loop and return its minted
/// identity.
fn create_file(node: &mut LiveNode<TreeView>, path: &str) -> EntryIdentity {
    node.mutations().publish_parent_tokens();
    let parent = path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let token = node
        .mutations()
        .capture_parent(parent)
        .unwrap()
        .expect("capture succeeds after publish");
    node.apply_mutation(
        &MutationKind::CreateFile {
            path: path.into(),
            parent: token,
        },
        None,
    )
    .expect("create commits");
    observed_token(node, path).expect("the created entry carries an identity")
}

/// Commit bytes against an explicit expected identity.
fn commit(
    node: &mut LiveNode<TreeView>,
    path: &str,
    content: &[u8],
    expected: Option<EntryIdentity>,
) -> Result<MutationOutcome, MutationError> {
    let base = match observed_token(node, path) {
        Some(_) => {
            let heads = node.live_heads_traced().unwrap();
            match node.current_node(&heads, path).unwrap() {
                Some(Node::File {
                    size,
                    executable,
                    chunks,
                    ..
                }) => FileIdentity::new(size, executable, chunks),
                _ => FileIdentity::new(0, false, Vec::new()),
            }
        }
        None => FileIdentity::new(0, false, Vec::new()),
    };
    node.apply_mutation(
        &MutationKind::CommitFile {
            path: path.into(),
            base,
            executable: false,
            content: content.to_vec(),
            expected,
        },
        None,
    )
}

#[test]
fn commit_with_matching_identity_commits() {
    let (mut node, dir) = scratch_node("commit-match");
    let token = create_file(&mut node, "f.txt");
    match commit(&mut node, "f.txt", b"hello", Some(token)) {
        Ok(MutationOutcome::Committed(_)) => {}
        other => panic!("a matching precondition commits: {other:?}"),
    }
    assert_eq!(
        observed_token(&node, "f.txt"),
        Some(token),
        "content changes preserve the entry"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn commit_with_stale_base_but_matching_identity_stays_stale() {
    let (mut node, dir) = scratch_node("commit-stale-base");
    let token = create_file(&mut node, "f.txt");
    // A base that matches nothing committed: same entry, changed
    // content view — still `Stale`, not `StaleEntry`.
    let wrong = FileIdentity::new(999, false, Vec::new());
    match node.apply_mutation(
        &MutationKind::CommitFile {
            path: "f.txt".into(),
            base: wrong,
            executable: false,
            content: b"hello".to_vec(),
            expected: Some(token),
        },
        None,
    ) {
        Err(MutationError::Stale(path)) => assert_eq!(path, "f.txt"),
        other => panic!("a stale base stays Stale: {other:?}"),
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn commit_with_replaced_identity_refuses_stale_entry() {
    let (mut node, dir) = scratch_node("commit-replaced");
    let first = create_file(&mut node, "f.txt");
    // Delete plus recreate at the same path: the token retires and
    // a new one mints.
    node.apply_mutation(
        &MutationKind::Unlink {
            path: "f.txt".into(),
            expected: None,
        },
        None,
    )
    .expect("unlink commits");
    let second = create_file(&mut node, "f.txt");
    assert_ne!(first, second, "recreation mints a new identity");
    // The old token no longer names this path's entry.
    match commit(&mut node, "f.txt", b"hello", Some(first)) {
        Err(MutationError::StaleEntry {
            path,
            expected,
            found,
        }) => {
            assert_eq!(path, "f.txt");
            assert_eq!(expected, first);
            assert_eq!(found, Some(second));
        }
        other => panic!("a replaced entry refuses as StaleEntry: {other:?}"),
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn commit_on_missing_path_with_identity_is_not_found() {
    let (mut node, dir) = scratch_node("commit-missing");
    let missing = EntryIdentity::from_bytes([0x42; 32]);
    match commit(&mut node, "gone.txt", b"hello", Some(missing)) {
        Err(MutationError::NotFound(path)) => assert_eq!(path, "gone.txt"),
        other => panic!("a missing path refuses NotFound, never Stale: {other:?}"),
    }
    // Without a precondition the legacy refusal stands unchanged.
    match commit(&mut node, "gone.txt", b"hello", None) {
        Err(MutationError::Stale(_)) => {}
        other => panic!("the legacy path keeps its Stale refusal: {other:?}"),
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unlink_checks_the_observed_entry() {
    let (mut node, dir) = scratch_node("unlink-expected");
    let token = create_file(&mut node, "f.txt");
    let other = EntryIdentity::from_bytes([0x77; 32]);
    match node.apply_mutation(
        &MutationKind::Unlink {
            path: "f.txt".to_string(),
            expected: Some(other),
        },
        None,
    ) {
        Err(MutationError::StaleEntry { found, .. }) => {
            assert_eq!(found, Some(token), "the refusal names the entry in the way");
        }
        other => panic!("a wrong token refuses StaleEntry: {other:?}"),
    }
    node.apply_mutation(
        &MutationKind::Unlink {
            path: "f.txt".to_string(),
            expected: Some(token),
        },
        None,
    )
    .expect("the matching token unlinks");
    assert!(
        observed_token(&node, "f.txt").is_none(),
        "the entry is gone"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rename_with_trailing_slash_dir_source_commits() {
    let (mut node, dir) = scratch_node("rename-trailing-slash");
    node.apply_mutation(
        &MutationKind::Mkdir {
            path: "d".to_string(),
        },
        None,
    )
    .expect("mkdir commits");
    let token = observed_token(&node, "d").expect("the dir carries an identity");
    // The format layer accepts one trailing slash on a directory
    // source; the identity lookup must see the same entry, not fail
    // the view grammar first.
    node.apply_mutation(
        &MutationKind::Rename {
            from: "d/".to_string(),
            to: "moved".to_string(),
            no_replace: false,
            expected: Some(token),
        },
        None,
    )
    .expect("a trailing-slash rename of a dir commits with its token");
    assert_eq!(
        observed_token(&node, "moved"),
        Some(token),
        "the move preserves the entry"
    );
    // Without a precondition the call behaves exactly as before the
    // identity work: the format rule still applies.
    node.apply_mutation(
        &MutationKind::Mkdir {
            path: "e".to_string(),
        },
        None,
    )
    .expect("mkdir commits");
    node.apply_mutation(
        &MutationKind::Rename {
            from: "e/".to_string(),
            to: "moved-e".to_string(),
            no_replace: false,
            expected: None,
        },
        None,
    )
    .expect("a trailing-slash rename of a dir commits path-addressed");
    let moved_token = observed_token(&node, "moved-e").expect("the moved dir carries an identity");
    // A wrong token through the same trailing-slash path still
    // refuses instead of landing on whatever the normalized path
    // resolves to: the normalization must never become a bypass.
    let other = EntryIdentity::from_bytes([0x77; 32]);
    match node.apply_mutation(
        &MutationKind::Rename {
            from: "moved-e/".to_string(),
            to: "moved-e2".to_string(),
            no_replace: true,
            expected: Some(other),
        },
        None,
    ) {
        Err(MutationError::StaleEntry { .. }) => {}
        other => panic!("a wrong token refuses StaleEntry: {other:?}"),
    }
    assert_eq!(
        observed_token(&node, "moved-e"),
        Some(moved_token),
        "the refused rename commits nothing"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rename_checks_the_observed_source_entry() {
    let (mut node, dir) = scratch_node("rename-expected");
    let token = create_file(&mut node, "a.txt");
    let other = create_file(&mut node, "b.txt");
    match node.apply_mutation(
        &MutationKind::Rename {
            from: "a.txt".to_string(),
            to: "moved.txt".to_string(),
            no_replace: true,
            expected: Some(other),
        },
        None,
    ) {
        Err(MutationError::StaleEntry { .. }) => {}
        other => panic!("a wrong source token refuses StaleEntry: {other:?}"),
    }
    node.apply_mutation(
        &MutationKind::Rename {
            from: "a.txt".to_string(),
            to: "moved.txt".to_string(),
            no_replace: true,
            expected: Some(token),
        },
        None,
    )
    .expect("the matching token renames");
    assert_eq!(
        observed_token(&node, "moved.txt"),
        Some(token),
        "a rename preserves the entry"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rmdir_and_setattrs_check_the_observed_entry() {
    let (mut node, dir) = scratch_node("rmdir-setattrs-expected");
    node.apply_mutation(
        &MutationKind::Mkdir {
            path: "d".to_string(),
        },
        None,
    )
    .expect("mkdir commits");
    let token = observed_token(&node, "d").expect("the dir carries an identity");
    let other = EntryIdentity::from_bytes([0x77; 32]);
    match node.apply_mutation(
        &MutationKind::Rmdir {
            path: "d".to_string(),
            expected: Some(other),
        },
        None,
    ) {
        Err(MutationError::StaleEntry { .. }) => {}
        other => panic!("a wrong dir token refuses StaleEntry: {other:?}"),
    }
    let file = create_file(&mut node, "f.txt");
    match node.apply_mutation(
        &MutationKind::SetAttrs {
            path: "f.txt".to_string(),
            size: None,
            executable: Some(true),
            base: None,
            expected: Some(other),
        },
        None,
    ) {
        Err(MutationError::StaleEntry { .. }) => {}
        other => panic!("a wrong file token refuses StaleEntry: {other:?}"),
    }
    node.apply_mutation(
        &MutationKind::SetAttrs {
            path: "f.txt".to_string(),
            size: None,
            executable: Some(true),
            base: None,
            expected: Some(file),
        },
        None,
    )
    .expect("the matching token sets attrs");
    node.apply_mutation(
        &MutationKind::Rmdir {
            path: "d".to_string(),
            expected: Some(token),
        },
        None,
    )
    .expect("the matching token removes the dir");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn append_checks_the_observed_entry() {
    let (mut node, dir) = scratch_node("append-expected");
    let token = create_file(&mut node, "f.txt");
    let other = EntryIdentity::from_bytes([0x77; 32]);
    match node.apply_mutation(
        &MutationKind::AppendFile {
            path: "f.txt".to_string(),
            content: b"more".to_vec(),
            expected: Some(other),
        },
        None,
    ) {
        Err(MutationError::StaleEntry { .. }) => {}
        other => panic!("a wrong append token refuses StaleEntry: {other:?}"),
    }
    node.apply_mutation(
        &MutationKind::AppendFile {
            path: "f.txt".to_string(),
            content: b"more".to_vec(),
            expected: Some(token),
        },
        None,
    )
    .expect("the matching token appends");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn legacy_entry_without_identity_refuses_stale_entry() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-core-precondition-legacy-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let mut engine = Engine::create(
        dir.clone(),
        "core-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let mut store = MemoryObjectStore::default();
    // A pre-identity tree: the encoder writes the legacy kinds, so
    // the entry carries no token — exactly what an old snapshot
    // replays as.
    let root = Tree::from_entries(vec![Entry::file("old.txt", 3, false, vec![]).unwrap()])
        .unwrap()
        .insert_into(&mut store)
        .unwrap();
    let head = engine.author_snapshot(&store, root).unwrap();
    let mut node = live_over_tree(engine, store, &[head]);
    let wanted = EntryIdentity::from_bytes([0x42; 32]);
    match node.apply_mutation(
        &MutationKind::Unlink {
            path: "old.txt".to_string(),
            expected: Some(wanted),
        },
        None,
    ) {
        Err(MutationError::StaleEntry {
            expected, found, ..
        }) => {
            assert_eq!(expected, wanted);
            assert_eq!(found, None, "a legacy entry reports no token");
        }
        other => panic!("a legacy entry refuses StaleEntry: {other:?}"),
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// The four-way decision, pinned without a live loop: the
/// multi-head gate in `apply_mutation` fires before `build_root`,
/// so the conflicted branch is defensive there and covered here.
#[test]
fn check_expected_identity_classifies_four_ways() {
    let wanted = EntryIdentity::from_bytes([0x42; 32]);
    let other = EntryIdentity::from_bytes([0x77; 32]);
    let file = |identity: Option<EntryIdentity>| Node::File {
        size: 1,
        executable: false,
        chunks: Vec::new(),
        identity,
    };
    let heads: &[AuthorizedSnapshot] = &[];
    // No precondition: everything passes through.
    assert!(LiveNode::<TreeView>::check_expected_identity(
        &None,
        Some(&file(Some(wanted))),
        "f",
        heads
    )
    .is_ok());
    assert!(LiveNode::<TreeView>::check_expected_identity(&None, None, "f", heads).is_ok());
    // Same token: proceed; the arm still applies its own checks.
    assert!(LiveNode::<TreeView>::check_expected_identity(
        &Some(wanted),
        Some(&file(Some(wanted))),
        "f",
        heads
    )
    .is_ok());
    // Different token: stale entry, naming both sides.
    match LiveNode::<TreeView>::check_expected_identity(
        &Some(wanted),
        Some(&file(Some(other))),
        "f",
        heads,
    ) {
        Err(MutationError::StaleEntry {
            path,
            expected,
            found,
        }) => {
            assert_eq!(path, "f");
            assert_eq!(expected, wanted);
            assert_eq!(found, Some(other));
        }
        other => panic!("a replaced entry refuses StaleEntry: {other:?}"),
    }
    // Legacy entry without identity: stale entry with no found token.
    match LiveNode::<TreeView>::check_expected_identity(
        &Some(wanted),
        Some(&file(None)),
        "f",
        heads,
    ) {
        Err(MutationError::StaleEntry { found, .. }) => assert_eq!(found, None),
        other => panic!("a legacy entry refuses StaleEntry: {other:?}"),
    }
    // Missing path: NotFound, never Stale.
    match LiveNode::<TreeView>::check_expected_identity(&Some(wanted), None, "gone", heads) {
        Err(MutationError::NotFound(path)) => assert_eq!(path, "gone"),
        other => panic!("a missing path refuses NotFound: {other:?}"),
    }
    // Conflicted path: refused as Conflicted.
    match LiveNode::<TreeView>::check_expected_identity(
        &Some(wanted),
        Some(&Node::Conflict { versions: vec![] }),
        "f",
        heads,
    ) {
        Err(MutationError::Conflicted { .. }) => {}
        other => panic!("a conflicted path refuses Conflicted: {other:?}"),
    }
}

#[test]
fn fold_member_with_wrong_identity_refuses_stale_entry() {
    let (mut node, dir) = scratch_node("fold-expected");
    let token = create_file(&mut node, "f.txt");
    let other = EntryIdentity::from_bytes([0x77; 32]);
    match node.apply_mutation(
        &MutationKind::Fold {
            members: vec![FoldMember {
                kind: MutationKind::CommitFile {
                    path: "f.txt".to_string(),
                    base: FileIdentity::new(0, false, Vec::new()),
                    executable: false,
                    content: b"folded".to_vec(),
                    expected: Some(other),
                },
                forcer: true,
            }],
        },
        None,
    ) {
        // Folds report per-member outcomes: a failed forcer aborts
        // with nothing committed, but the fold itself stays Ok.
        Ok(MutationOutcome::Fold {
            forcer:
                FoldForcerOutcome::Failed(MutationError::StaleEntry {
                    expected, found, ..
                }),
            ..
        }) => {
            assert_eq!(expected, other);
            assert_eq!(found, Some(token));
        }
        other => panic!("a folded mismatch refuses StaleEntry: {other:?}"),
    }
    assert_eq!(
        observed_token(&node, "f.txt"),
        Some(token),
        "the refused fold commits nothing"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
