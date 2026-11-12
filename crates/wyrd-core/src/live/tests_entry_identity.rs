//! Stable entry identity across the live mutation path: every
//! creation mints a distinct token, every modification and rename
//! preserves it, deletion retires it, and recreation mints anew.
//! The peer-originated rename case closes the file: identity
//! travels inside snapshot-visible tree bytes, so a device that
//! never observed the entry still preserves it.
//!
//! Observation reads the authored head's tree straight from the
//! store (the same bytes a peer would fetch), plus `current_node`
//! where the loop's own view is the assertion.

use super::parent_mutation_tests::{live_over_tree, TreeView};
use super::*;
use wyrd_format::{MemoryObjectStore, ObjectKind, ObjectStore, Tree};
use wyrd_sync::keys::{DeviceEncryptionSecret, DeviceIdentitySecret};
use wyrd_sync::transport::mailbox::{
    Delivery, DeliveryId, Disposition, Mailbox, MailboxEnvelope, MailboxError, SendReport,
};

/// A scratch engine with no authored heads, ready for live-loop
/// authoring from empty, over the real tree-backed view (so the
/// loop observes exactly what it authors, identities included).
fn empty_live(tag: &str) -> (LiveNode<TreeView>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-core-identity-{tag}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let identity = DeviceIdentitySecret::generate().unwrap();
    let engine = Engine::create(dir.clone(), "core-test-pass", identity).unwrap();
    let node = live_over_tree(engine, MemoryObjectStore::default(), &[]);
    (node, dir)
}

/// The entry identity the loop's own view reports at `path`:
/// `None` where the path does not resolve or the node is synthetic
/// (root, merged, conflict), `Some(token)` — possibly `None` for
/// pre-identity entries — where a real entry resolves.
fn observed(node: &LiveNode<TreeView>, path: &str) -> Option<Option<EntryIdentity>> {
    let heads = node.live_heads_traced().unwrap();
    match node.current_node(&heads, path).unwrap() {
        Some(Node::File { identity, .. })
        | Some(Node::Dir { identity, .. })
        | Some(Node::Symlink { identity, .. }) => Some(identity),
        Some(Node::MergedDir { .. }) | Some(Node::Conflict { .. }) => None,
        None => None,
    }
}

/// Require the resolved token at `path`: the entry exists and
/// carries a minted identity.
fn require_token(node: &LiveNode<TreeView>, path: &str) -> EntryIdentity {
    observed(node, path)
        .unwrap_or_else(|| panic!("{path} resolves"))
        .unwrap_or_else(|| panic!("{path} carries an identity"))
}

/// Create one file through the loop and return its minted identity.
fn create(node: &mut LiveNode<TreeView>, path: &str) -> EntryIdentity {
    // Mkdir is strict (no intermediate creation), so walk down
    // level by level; each level is its own entry.
    if let Some(dir) = path.rsplit_once('/').map(|(dir, _)| dir) {
        let mut prefix = String::new();
        for component in dir.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            match node.apply_mutation(
                &MutationKind::Mkdir {
                    path: prefix.clone(),
                },
                None,
            ) {
                Ok(_) => {}
                Err(MutationError::AlreadyExists(_)) => {}
                Err(error) => panic!("parent mkdir commits: {error:?}"),
            }
        }
        // The levels blocked parent captures until publication; run
        // the publication half the sync pass would run, then capture.
        node.mutations().publish_parent_tokens();
        let token = node
            .mutations()
            .capture_parent(dir)
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
        return require_token(node, path);
    }
    node.mutations().publish_parent_tokens();
    let token = node
        .mutations()
        .capture_parent("")
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
    require_token(node, path)
}

/// Commit content through the loop against the current file identity.
fn commit(node: &mut LiveNode<TreeView>, path: &str, content: &[u8]) {
    let heads = node.live_heads_traced().unwrap();
    let base = match node.current_node(&heads, path).unwrap() {
        Some(Node::File {
            size,
            executable,
            chunks,
            ..
        }) => FileIdentity::new(size, executable, chunks),
        other => panic!("{path} is a committable file, saw {other:?}"),
    };
    node.apply_mutation(
        &MutationKind::CommitFile {
            expected: None,
            path: path.into(),
            base,
            executable: false,
            content: content.to_vec(),
        },
        None,
    )
    .expect("commit applies");
}

/// Two creations mint distinct identities, even with identical content.
#[test]
fn creations_mint_distinct_identities() {
    let (mut node, dir) = empty_live("create-distinct");
    let first = create(&mut node, "a.txt");
    commit(&mut node, "a.txt", b"same bytes");
    let second = create(&mut node, "b.txt");
    commit(&mut node, "b.txt", b"same bytes");
    assert_ne!(first, second, "identical content, distinct entries");
    std::fs::remove_dir_all(dir).unwrap();
}

/// Content modification preserves the entry: commit, append, and
/// setattr all keep the token the creation minted.
#[test]
fn content_modification_preserves_identity() {
    let (mut node, dir) = empty_live("modify-preserves");
    let created = create(&mut node, "f.txt");
    commit(&mut node, "f.txt", b"version one");
    let committed = require_token(&node, "f.txt");
    assert_eq!(created, committed, "commit preserves");
    node.apply_mutation(
        &MutationKind::AppendFile {
            expected: None,
            path: "f.txt".into(),
            content: b" plus more".to_vec(),
        },
        None,
    )
    .expect("append applies");
    let appended = require_token(&node, "f.txt");
    assert_eq!(created, appended, "append preserves");
    node.apply_mutation(
        &MutationKind::SetAttrs {
            expected: None,
            path: "f.txt".into(),
            size: None,
            executable: Some(true),
            base: None,
        },
        None,
    )
    .expect("setattr applies");
    let flagged = require_token(&node, "f.txt");
    assert_eq!(created, flagged, "setattr preserves");
    assert_eq!(
        node.live_heads_traced().unwrap().len(),
        1,
        "one head throughout"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// Rename preserves the moved token, retires nothing else, and the
/// source path stops resolving.
#[test]
fn rename_preserves_identity() {
    let (mut node, dir) = empty_live("rename-preserves");
    let created = create(&mut node, "from.txt");
    commit(&mut node, "from.txt", b"data");
    node.apply_mutation(&MutationKind::Mkdir { path: "sub".into() }, None)
        .expect("destination parent exists");
    node.apply_mutation(
        &MutationKind::Rename {
            expected: None,
            from: "from.txt".into(),
            to: "sub/to.txt".into(),
            no_replace: false,
        },
        None,
    )
    .expect("rename applies");
    assert_eq!(
        observed(&node, "sub/to.txt"),
        Some(Some(created)),
        "the moved entry keeps its token"
    );
    assert_eq!(
        observed(&node, "from.txt"),
        None,
        "the source path stops resolving"
    );
    let parent = require_token(&node, "sub");
    assert_ne!(parent, created, "the intermediate is its own entry");
    std::fs::remove_dir_all(dir).unwrap();
}

/// Delete retires the token: recreating at the same path mints a
/// new one, and the old token names no entry anymore.
#[test]
fn delete_retires_and_recreate_mints() {
    let (mut node, dir) = empty_live("delete-recreate");
    let created = create(&mut node, "f.txt");
    commit(&mut node, "f.txt", b"data");
    node.apply_mutation(
        &MutationKind::Unlink {
            expected: None,
            path: "f.txt".into(),
        },
        None,
    )
    .expect("unlink applies");
    assert_eq!(observed(&node, "f.txt"), None);
    let recreated = create(&mut node, "f.txt");
    assert_ne!(created, recreated, "recreation mints anew");
    // The retired token names nothing: every entry in the head's
    // tree carries a different token.
    let heads = node.live_heads_traced().unwrap();
    let tree_id = heads[0].snapshot().tree;
    let store = node.store.read().unwrap();
    let bytes = store.get(&tree_id).unwrap().unwrap();
    for entry in Tree::decode(&bytes).unwrap().entries() {
        assert_ne!(
            entry.identity,
            Some(created),
            "the retired token is gone from the tree"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// Directory lifecycle matches: mkdir mints, rmdir retires,
/// re-mkdir mints anew.
#[test]
fn directory_lifecycle_mints_and_retires() {
    let (mut node, dir) = empty_live("dir-lifecycle");
    node.apply_mutation(&MutationKind::Mkdir { path: "d".into() }, None)
        .expect("mkdir applies");
    let created = require_token(&node, "d");
    node.apply_mutation(
        &MutationKind::Rmdir {
            expected: None,
            path: "d".into(),
        },
        None,
    )
    .expect("rmdir applies");
    assert_eq!(observed(&node, "d"), None);
    node.apply_mutation(&MutationKind::Mkdir { path: "d".into() }, None)
        .expect("re-mkdir applies");
    let recreated = require_token(&node, "d");
    assert_ne!(created, recreated);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A copy is a creation, not a move: same bytes elsewhere mint a
/// new token.
#[test]
fn copy_mints_a_new_identity() {
    let (mut node, dir) = empty_live("copy-mints");
    let original = create(&mut node, "original.txt");
    commit(&mut node, "original.txt", b"shared bytes");
    let copy = create(&mut node, "copy.txt");
    commit(&mut node, "copy.txt", b"shared bytes");
    assert_ne!(original, copy, "same content, new entry");
    std::fs::remove_dir_all(dir).unwrap();
}

/// Nested creation mints every level: the leaf and each created
/// intermediate are distinct entries.
#[test]
fn nested_creation_mints_every_level() {
    let (mut node, dir) = empty_live("nested-mints");
    let leaf = create(&mut node, "a/b/c.txt");
    let a = require_token(&node, "a");
    let b = require_token(&node, "a/b");
    assert_ne!(leaf, a);
    assert_ne!(leaf, b);
    assert_ne!(a, b);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A stale commit that lands after a same-path replacement fails
/// closed and mints nothing: the replacement keeps its token.
#[test]
fn stale_commit_preserves_the_replacement_identity() {
    let (mut node, dir) = empty_live("stale-replacement");
    let first = create(&mut node, "f.txt");
    commit(&mut node, "f.txt", b"first");
    let heads = node.live_heads_traced().unwrap();
    let stale_base = match node.current_node(&heads, "f.txt").unwrap() {
        Some(Node::File {
            size,
            executable,
            chunks,
            ..
        }) => FileIdentity::new(size, executable, chunks),
        other => panic!("f.txt is a file, saw {other:?}"),
    };
    // Same-path replacement: unlink plus create mints anew.
    node.apply_mutation(
        &MutationKind::Unlink {
            expected: None,
            path: "f.txt".into(),
        },
        None,
    )
    .expect("unlink applies");
    let replacement = create(&mut node, "f.txt");
    assert_ne!(first, replacement);
    // The stale handle (bound to the retired entry) fails closed.
    let error = node
        .apply_mutation(
            &MutationKind::CommitFile {
                expected: None,
                path: "f.txt".into(),
                base: stale_base,
                executable: false,
                content: b"stale write".to_vec(),
            },
            None,
        )
        .unwrap_err();
    assert_eq!(error, MutationError::Stale("f.txt".to_string()));
    assert_eq!(
        observed(&node, "f.txt"),
        Some(Some(replacement)),
        "the replacement keeps its token"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// Inbound test mailbox: feeds recorded envelopes, collects what the
/// node publishes. Same shape as the convergence harness's replay
/// mailbox, local so this file stands alone.
struct ShuttleMailbox {
    queue: std::collections::VecDeque<(DeliveryId, MailboxEnvelope)>,
    next: u64,
}

impl ShuttleMailbox {
    fn new(envelopes: Vec<MailboxEnvelope>) -> Self {
        let mut mailbox = ShuttleMailbox {
            queue: std::collections::VecDeque::new(),
            next: 1,
        };
        for envelope in envelopes {
            mailbox.push(envelope);
        }
        mailbox
    }

    fn push(&mut self, envelope: MailboxEnvelope) {
        self.queue.push_back((DeliveryId::new(self.next), envelope));
        self.next += 1;
    }

    fn take_sent(&mut self) -> Vec<MailboxEnvelope> {
        // Sends during `sync_once` re-enter the same queue the test
        // later replays, so sent items are drained explicitly.
        std::mem::take(&mut self.queue)
            .into_iter()
            .map(|(_, envelope)| envelope)
            .collect()
    }
}

impl Mailbox for ShuttleMailbox {
    fn send(&mut self, envelope: MailboxEnvelope) -> Result<SendReport, MailboxError> {
        self.push(envelope);
        Ok(SendReport { accepted: 1 })
    }

    fn recv(&mut self) -> Result<Option<Delivery>, MailboxError> {
        Ok(self
            .queue
            .front()
            .map(|(id, envelope)| Delivery::new(*id, envelope.clone())))
    }

    fn settle(&mut self, id: DeliveryId, disposition: Disposition) -> Result<(), MailboxError> {
        let Some(pos) = self.queue.iter().position(|(held, _)| *held == id) else {
            return Err(MailboxError::Transport("unknown delivery".into()));
        };
        match disposition {
            Disposition::Ack | Disposition::Poison => {
                self.queue.remove(pos);
            }
            Disposition::Retry => {
                let held = self.queue.remove(pos).expect("position is valid");
                self.queue.push_back(held);
            }
        }
        Ok(())
    }
}

/// Send-only test mailbox: collects what the node publishes
/// without ever feeding anything back. A loopback mailbox would
/// let the node consume (and discharge) its own announcement —
/// real relays route to other devices — so the publish pass runs
/// against this sink and the recorded envelopes travel to the
/// peer explicitly.
struct BlastMailbox {
    sent: Vec<MailboxEnvelope>,
}

impl Mailbox for BlastMailbox {
    fn send(&mut self, envelope: MailboxEnvelope) -> Result<SendReport, MailboxError> {
        self.sent.push(envelope);
        Ok(SendReport { accepted: 1 })
    }

    fn recv(&mut self) -> Result<Option<Delivery>, MailboxError> {
        Ok(None)
    }

    fn settle(&mut self, _id: DeliveryId, _disposition: Disposition) -> Result<(), MailboxError> {
        Ok(())
    }
}

/// Run passes until the node reports quiet, like the convergence
/// harness: intake, fetch, and publication each take the passes
/// they take, and quiet means done.
fn drive_quiet(
    node: &mut LiveNode<TreeView>,
    mailbox: &mut impl Mailbox,
    mut bulk: Option<&mut wyrd_sync::serving::VaultSource>,
) {
    let mut passes = 0u32;
    loop {
        let pass = node.sync_once(mailbox, bulk.as_deref_mut()).unwrap();
        passes += 1;
        if node.is_quiet(&pass).unwrap() {
            break;
        }
        assert!(passes < 24, "convergence must terminate");
    }
}

/// The load-bearing row: a peer-originated rename preserves the
/// token. A authors a file (identity X); B converges through the
/// real intake/fetch path; B renames through its own live loop; A
/// converges back and observes X at the new path — transferred in
/// snapshot bytes, never inferred.
#[test]
fn peer_originated_rename_preserves_identity() {
    let tag = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir_a = std::env::temp_dir().join(format!("wyrd-core-peer-a-{tag}"));
    let dir_b = std::env::temp_dir().join(format!("wyrd-core-peer-b-{tag}"));
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();

    // Device A: one file under a fixed token — the token the whole
    // test tracks. Fixed, not minted, so the assertion names the
    // exact bytes that must survive the round trip.
    let identity_x = EntryIdentity::from_bytes([0x58; 32]);
    let identity_a = DeviceIdentitySecret::generate().unwrap();
    let mut engine_a = Engine::create(dir_a.clone(), "core-test-pass", identity_a).unwrap();
    let mut store_a = MemoryObjectStore::default();
    let chunk = store_a.insert(ObjectKind::Chunk, b"peer-base").unwrap();
    let root_a = Tree::from_entries(vec![Entry::file("f", 9, false, vec![chunk])
        .unwrap()
        .with_identity(identity_x)])
    .unwrap()
    .insert_into(&mut store_a)
    .unwrap();
    let head_a = engine_a.author_snapshot(&store_a, root_a).unwrap();

    // Admit B and collect A's outbound envelopes for B.
    let identity_b = DeviceIdentitySecret::generate().unwrap();
    let encryption_b = DeviceEncryptionSecret::generate().unwrap();
    engine_a.stage_carry_heads().unwrap();
    let outcome = engine_a
        .admit_device(identity_b.device_id(), encryption_b.encryption_key())
        .unwrap();
    engine_a.carry_pending(&store_a).unwrap();
    let mut recorder = ShuttleMailbox::new(Vec::new());
    engine_a
        .deliver_pending(&mut recorder, &mut PublishBudget::unbounded())
        .unwrap();
    engine_a
        .announce_pending(&mut recorder, None, &mut PublishBudget::unbounded())
        .unwrap();
    let outbound = recorder.take_sent();
    assert!(!outbound.is_empty(), "admission owes B catch-up");

    // Device B joins, then converges: intake plus fetch off A's
    // serving vault, quiet when the heads land.
    let engine_b = Engine::accept_invitation(
        dir_b.clone(),
        "core-test-pass",
        identity_b,
        encryption_b,
        &outcome.invitation,
    )
    .unwrap();
    let state_a = engine_a.runtime_state().unwrap();
    let mut bulk_a =
        wyrd_sync::serving::VaultSource::from_state(&state_a, engine_a.vault()).unwrap();
    drop(state_a);
    let mut node_b = live_over_tree(engine_b, MemoryObjectStore::default(), &[]);
    let mut inbound_b = ShuttleMailbox::new(outbound);
    drive_quiet(&mut node_b, &mut inbound_b, Some(&mut bulk_a));
    let heads_b = node_b.engine.live_heads().unwrap();
    assert_eq!(heads_b.len(), 1, "B projects one head");

    // B renames through its own live loop — the same path a Finder
    // rename would travel — and publishes. Convergence left file
    // chunks remote-only; authoring needs the closure locally, so
    // stage the hydration B would have done on open (fetch itself
    // is orthogonal machinery, pinned elsewhere).
    node_b
        .store
        .write()
        .unwrap()
        .insert(ObjectKind::Chunk, b"peer-base")
        .unwrap();
    node_b
        .apply_mutation(
            &MutationKind::Rename {
                expected: None,
                from: "f".into(),
                to: "renamed.txt".into(),
                no_replace: false,
            },
            None,
        )
        .expect("B renames");
    let mut to_a = BlastMailbox { sent: Vec::new() };
    drive_quiet(&mut node_b, &mut to_a, None);
    let published = to_a.sent;
    assert!(!published.is_empty(), "B's rename travels to A");

    // A converges back off B's serving vault and observes the
    // original token at the new path.
    let mut node_a = live_over_tree(engine_a, store_a, &[head_a]);
    let state_b = node_b.engine.runtime_state().unwrap();
    let mut bulk_b =
        wyrd_sync::serving::VaultSource::from_state(&state_b, node_b.engine.vault()).unwrap();
    drop(state_b);
    let mut inbound_a = ShuttleMailbox::new(published);
    drive_quiet(&mut node_a, &mut inbound_a, Some(&mut bulk_b));
    let heads_a = node_a.engine.live_heads().unwrap();
    assert_eq!(heads_a.len(), 1, "A projects one head after intake");
    let tree_id = heads_a[0].snapshot().tree;
    let store = node_a.store.read().unwrap();
    let bytes = store.get(&tree_id).unwrap().unwrap();
    let tree = Tree::decode(&bytes).unwrap();
    let renamed = tree
        .entries()
        .iter()
        .find(|entry| entry.name.as_str() == "renamed.txt")
        .expect("the renamed path resolves on A");
    assert_eq!(
        renamed.identity,
        Some(identity_x),
        "the peer rename preserved the token through intake"
    );
    assert!(
        tree.entries()
            .iter()
            .all(|entry| entry.name.as_str() != "f"),
        "the source path is gone on A"
    );
    drop(store);
    drop(node_a);
    drop(node_b);
    std::fs::remove_dir_all(dir_a).unwrap();
    std::fs::remove_dir_all(dir_b).unwrap();
}

/// Owner restart preserves identity: after dropping the node and
/// reopening the same durable directory, `stat` reports the same
/// token — the identity lives in the persisted tree bytes, not in
/// process memory.
#[test]
fn owner_restart_preserves_identity() {
    let tag = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("wyrd-core-identity-restart-{tag}"));
    std::fs::create_dir_all(&dir).unwrap();
    let identity = DeviceIdentitySecret::generate().unwrap();
    let store = MemoryObjectStore::default();
    let before = {
        let engine = Engine::create(dir.clone(), "core-test-pass", identity.clone()).unwrap();
        let mut node = live_over_tree(engine, store, &[]);
        let created = create(&mut node, "f.txt");
        commit(&mut node, "f.txt", b"restart me");
        let heads = node.live_heads_traced().unwrap();
        let stat = match node.current_node(&heads, "f.txt").unwrap() {
            Some(Node::File { identity, .. }) => identity,
            other => panic!("f.txt is a file, saw {other:?}"),
        };
        assert_eq!(stat, Some(created));
        (node.store.clone(), created)
    };
    // Restart: the node is gone; the durable directory and the
    // object store (the daemon's persisted vault stand-in) survive.
    let engine = Engine::open_keystore(dir.clone(), "core-test-pass", identity).unwrap();
    let heads: Vec<AuthorizedSnapshot> = engine.live_heads().unwrap();
    assert_eq!(heads.len(), 1, "the authored head survives reopen");
    let node = live_over_tree(engine, (*before.0.read().unwrap()).clone(), &heads);
    assert_eq!(
        require_token(&node, "f.txt"),
        before.1,
        "reopen serves the same token"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// FUSE and owner views agree across a rename: the content-derived
/// handle tuple (`FileIdentity`, what FUSE opens against) and the
/// entry token (what the owner reports) both survive together —
/// neither layer invents a new identity for the moved entry.
#[test]
fn fuse_and_owner_views_agree_across_rename() {
    let (mut node, dir) = empty_live("fuse-owner-agreement");
    let created = create(&mut node, "from.txt");
    commit(&mut node, "from.txt", b"agreement bytes");
    let heads = node.live_heads_traced().unwrap();
    let before_handle = match node.current_node(&heads, "from.txt").unwrap() {
        Some(Node::File {
            size,
            executable,
            chunks,
            ..
        }) => FileIdentity::new(size, executable, chunks),
        other => panic!("from.txt is a file, saw {other:?}"),
    };
    node.apply_mutation(
        &MutationKind::Rename {
            expected: None,
            from: "from.txt".into(),
            to: "to.txt".into(),
            no_replace: false,
        },
        None,
    )
    .expect("rename applies");
    let heads = node.live_heads_traced().unwrap();
    let (after_handle, after_token) = match node.current_node(&heads, "to.txt").unwrap() {
        Some(Node::File {
            size,
            executable,
            chunks,
            identity,
        }) => (FileIdentity::new(size, executable, chunks), identity),
        other => panic!("to.txt is a file, saw {other:?}"),
    };
    assert_eq!(
        before_handle, after_handle,
        "the FUSE handle tuple is rename-stable"
    );
    assert_eq!(
        after_token,
        Some(created),
        "the owner-reported token is the creation token"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
