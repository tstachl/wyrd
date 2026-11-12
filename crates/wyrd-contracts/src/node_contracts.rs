//! The node-without-a-backend contract: the composed node (engine,
//! view, loop, channels) operates end to end with no presentation
//! backend in the path — no `FuseBackend`, no fuser, no mount. This
//! is the architectural proof that the provider edge is inverted
//! (backends build from the node's live parts), so driving the node
//! headless exercises the same composition production mounts.
//!
//! Namespace assertions run through a [`NamespaceView`] bound, not the
//! concrete view type, so the proof holds against the neutral surface
//! rather than one implementation of it.
//!
//! Contract 36 goes one further: the loop, parts, and projection
//! compose over a view type defined outside `wyrd-view` entirely,
//! proving the genericity claim (any provider implements the
//! trait; the node never names one).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use wyrd_core::live::LiveConfig;
use wyrd_core::mutation::{
    FileIdentity, FoldDisposition, FoldForcerOutcome, FoldMember, MutationError, MutationKind,
    MutationOutcome,
};
use wyrd_core::node::{NodeError, WyrdNode};
use wyrd_core::view::{
    Attr, DirEntry, Head, NamespaceView, Node, OpenFile, RuntimeMaterialization, ViewError,
    ViewLockError,
};
use wyrd_format::{
    ContentId, Entry, FetchStatus, FsObjectStore, MemoryObjectStore, ObjectKind, ObjectStore,
    StoreFailure, Tree,
};
use wyrd_sync::bulk::MemoryBulkSource;
use wyrd_sync::durable::DurableError;
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::{Engine, EngineError};
use wyrd_sync::transport::mailbox::{
    Delivery, DeliveryId, Disposition, Mailbox, MailboxEnvelope, MailboxError, SendReport,
};
use wyrd_view::{DriveView, ViewHead};

/// A control plane that never speaks: no announcements, no sends, so
/// the loop idles on pacing alone and every observed change is local.
struct SilentMailbox;

impl Mailbox for SilentMailbox {
    fn send(&mut self, _envelope: MailboxEnvelope) -> Result<SendReport, MailboxError> {
        // Deliberately claims acceptance the fake never observed: the
        // contracts here need obligations to drain, not a refusal path.
        Ok(SendReport { accepted: 1 })
    }

    fn recv(&mut self) -> Result<Option<Delivery>, MailboxError> {
        Ok(None)
    }

    fn settle(&mut self, _id: DeliveryId, _disposition: Disposition) -> Result<(), MailboxError> {
        Ok(())
    }
}

/// The file a headless node serves, asserted through the neutral view
/// surface: resolve, open, read back the authored bytes.
fn assert_serves_hello<V: NamespaceView>(view: &V) {
    let node = view.lookup("hello.txt").expect("authored file resolves");
    let file = view.open_file(&node).expect("a file opens");
    assert_eq!(
        view.read(&file, 0, 64).unwrap(),
        b"hello",
        "the node serves authored bytes with no backend involved"
    );
}

/// Contract 35: the node composes, runs, mutates, and serves without
/// any presentation backend. The direct write path (`put_file`) and
/// the loop-driven mutation channel (`Mkdir` through the queue) both
/// converge into generations the test reads through the publication
/// slot — the same parts a backend would build from, but no backend
/// is ever constructed.
#[test]
fn node_composes_and_serves_without_a_presentation_backend() {
    let dir = headless_dir("fuse-view");
    let engine = Engine::create(
        dir.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();

    let mut node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(engine, MemoryObjectStore::default()).unwrap();
    publish_hello(&mut node);
    drive_headless_node(node);

    std::fs::remove_dir_all(dir).unwrap();
}

/// A second namespace-view provider, defined outside `wyrd-view`: a
/// newtype over [`DriveView`] implementing [`NamespaceView`] by
/// delegation. It proves the trait is implementable out-of-crate —
/// heads cross through the public [`ViewHead`] admission ticket, and
/// every read method delegates to the inner view's inherent surface.
///
/// The delegation is deliberately thin: the point is the composition
/// boundary (the loop, parts, and projection monomorphize over this
/// type), not independent namespace semantics.
///
/// The store handle rides alongside the inner view (both address the
/// same bytes) so the lock-borrowing accessors resolve against state
/// the provider owns rather than a temporary clone.
struct ContractView {
    view: DriveView<MemoryObjectStore, RuntimeMaterialization>,
    store: Arc<RwLock<MemoryObjectStore>>,
}

impl ContractView {
    fn wrap(
        store: Arc<RwLock<MemoryObjectStore>>,
        materialization: RuntimeMaterialization,
        heads: Vec<Head>,
    ) -> Self {
        let view = DriveView::shared(
            Arc::clone(&store),
            materialization,
            heads.into_iter().map(ViewHead::new).collect(),
        );
        ContractView { view, store }
    }
}

impl NamespaceView for ContractView {
    type Store = MemoryObjectStore;
    type Materialization = RuntimeMaterialization;

    fn open(
        store: MemoryObjectStore,
        materialization: RuntimeMaterialization,
        heads: Vec<Head>,
    ) -> Self {
        Self::wrap(Arc::new(RwLock::new(store)), materialization, heads)
    }

    fn open_shared(
        store: Arc<RwLock<MemoryObjectStore>>,
        materialization: RuntimeMaterialization,
        heads: Vec<Head>,
    ) -> Self {
        Self::wrap(store, materialization, heads)
    }

    fn store_handle(&self) -> Arc<RwLock<MemoryObjectStore>> {
        Arc::clone(&self.store)
    }

    fn store_read(&self) -> Result<RwLockReadGuard<'_, MemoryObjectStore>, ViewLockError> {
        self.store.read().map_err(|_| ViewLockError)
    }

    fn store_write(&self) -> Result<RwLockWriteGuard<'_, MemoryObjectStore>, ViewLockError> {
        self.store.write().map_err(|_| ViewLockError)
    }

    fn set_heads(&mut self, heads: Vec<Head>) {
        self.view
            .set_heads(heads.into_iter().map(ViewHead::new).collect())
    }

    fn set_materialization(&mut self, materialization: RuntimeMaterialization) {
        self.view.set_materialization(materialization)
    }

    fn status(&self, id: &ContentId) -> FetchStatus {
        self.view.status(id)
    }

    fn lookup(&self, path: &str) -> Result<Node, ViewError> {
        self.view.lookup(path)
    }

    fn stat(&self, path: &str) -> Result<Attr, ViewError> {
        self.view.stat(path)
    }

    fn readdir(&self, node: &Node) -> Result<Vec<DirEntry>, ViewError> {
        self.view.readdir(node)
    }

    fn open_file(&self, node: &Node) -> Result<OpenFile, ViewError> {
        self.view.open(node)
    }

    fn read(&self, file: &OpenFile, offset: u64, len: usize) -> Result<Vec<u8>, ViewError> {
        self.view.read(file, offset, len)
    }
}

/// Contract 36: the node composes, runs, mutates, and serves over a
/// provider defined outside `wyrd-view`. Same driver as contract 35,
/// monomorphized over [`ContractView`] — the loop, the split parts,
/// and the publication slot never name the FUSE view type.
#[test]
fn node_serves_over_a_view_defined_outside_the_fuse_crate() {
    let dir = headless_dir("contract-view");
    let engine = Engine::create(
        dir.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();

    let mut node: WyrdNode<ContractView> =
        WyrdNode::new(engine, MemoryObjectStore::default()).unwrap();
    publish_hello(&mut node);
    drive_headless_node(node);

    std::fs::remove_dir_all(dir).unwrap();
}

/// A commit refused at the retention ceiling commits nothing at all: no
/// snapshot, and therefore no announcement obligation for a snapshot the
/// drive never took.
///
/// The obligation is the half that matters and the half that is easy to
/// get wrong. It is written in the same single fact-commit as the
/// snapshot body, so a refusal landing after that call would leave a
/// durable `AnnouncementQueued` for a snapshot that does not exist —
/// unreclaimable, and something a later pass would announce to peers. A
/// refusal before the commit's first write never reaches the fact log.
///
/// The witness is the durable fact log itself: its committed segments on
/// disk, counted before and after. An unchanged count is positive
/// evidence the log was not appended to, obligation included. (The
/// pending-announcement backlog would be the direct witness, but a
/// single-member drive has no recipients, so it is empty either way and
/// proves nothing.) The errno half of the same refusal is pinned at the
/// mount boundary in `wyrd-daemon`, which owns the backend composition.
///
/// The loop runs on a scope thread because `submit` is synchronous: it
/// waits for the loop to apply the mutation and reply. Nothing inside
/// the scope may panic — a panic there would skip the stop flag and the
/// scope would block forever joining a loop with no reason to exit — so
/// the outcome is carried out and asserted after the join.
#[test]
fn a_quota_refused_commit_commits_nothing_at_all() {
    const PASS: &str = "headless-test-pass";
    let dir = headless_dir("contract-retained-quota");
    let engine =
        Engine::create(dir.clone(), PASS, DeviceIdentitySecret::generate().unwrap()).unwrap();

    // One ceiling and one accountant, so the count the check reads is
    // the count the store keeps.
    let (config, retained) = LiveConfig::with_retained_quota(0);
    let mut node: WyrdNode<ContractView> =
        WyrdNode::new(engine, MemoryObjectStore::default().with_retained(retained)).unwrap();
    publish_hello(&mut node);

    // The drive's own content puts it over a zero ceiling, so the
    // refusal is genuinely reached rather than an empty store.
    let facts_before = committed_fact_segments(&dir);
    assert!(facts_before > 0, "the drive has committed something");

    let (mut live, parts) = node.into_live(Duration::from_secs(5), &config).unwrap();
    let slot = Arc::clone(&parts.projection);
    let queue = Arc::clone(&parts.mutations);
    let stop = Arc::new(AtomicBool::new(false));
    // The loop paces from the same budgets; the quota is read from the
    // node's own accountant, not from the loop's copy of the config.
    let loop_config = LiveConfig {
        budgets: config.budgets,
        interval: Duration::from_millis(10),
        ..LiveConfig::default()
    };

    let outcome = std::thread::scope(|scope| {
        let loop_stop = Arc::clone(&stop);
        scope.spawn(move || {
            let mut mailbox = SilentMailbox;
            live.run_loop(
                &mut mailbox,
                None::<&mut MemoryBulkSource>,
                &loop_stop,
                &loop_config,
                &mut |_, _| {},
            )
        });

        let outcome = queue.submit(MutationKind::Mkdir {
            path: "denied".to_string(),
        });
        // Idle passes, so the final segment count is attributable to
        // the refused commit: this shows fifty ordinary passes append
        // nothing of their own. It is deliberately not the witness for
        // the obligation — an obligation whose snapshot body was never
        // committed would not be announced either, because
        // `announce_pending` finds no body and `reannounce_one` commits
        // nothing when no announcement matches. The count below is the
        // witness, and it works because the obligation is written in the
        // same fact-commit as the body.
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(10));
        }
        stop.store(true, Ordering::Relaxed);
        outcome
    });

    let error = outcome.expect_err("a commit at the ceiling is refused, not queued");
    assert!(
        matches!(error, MutationError::Store(StoreFailure::StorageFull)),
        "the refusal is the store-full classification, which reaches the \
         mount as ENOSPC: {error}"
    );
    assert_eq!(
        committed_fact_segments(&dir),
        facts_before,
        "a quota-refused commit commits no facts: no snapshot, and no \
         announcement obligation for a snapshot that does not exist"
    );
    // The namespace never saw the refused mutation, and the drive still
    // serves what it had before.
    let projection = slot.read().unwrap();
    assert!(
        projection.view().lookup("denied").is_err(),
        "the refused mkdir left no directory"
    );
    assert_serves_hello(projection.view());
    drop(projection);

    std::fs::remove_dir_all(dir).unwrap();
}

/// Committed fact-log segments on disk. The durable store keeps one file
/// per fact-commit, so this is the count a refusal would have to leave
/// alone. Read from the filesystem rather than the engine because
/// `Engine::open` needs the drive and encryption secrets that `create`
/// generates internally, and re-deriving them is not worth a witness the
/// directory layout states directly.
///
/// Decision: the `return 0` on an unreadable `commits/` is fail-closed,
/// not fail-open, because of how the sole calling test consumes it. The
/// before-read asserts `facts_before > 0`, so an unreadable tree fails
/// there; the after-read asserts equality against that bound positive
/// count, so an unreadable tree fails there too. There is no path on
/// which both sides read 0 and the comparison passes vacuously.
fn committed_fact_segments(dir: &std::path::Path) -> usize {
    let commits = dir.join("commits");
    let Ok(entries) = std::fs::read_dir(&commits) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let path = entry.path();
            // Skip temps: the durable store writes `commits/<name>.tmp`
            // and renames it into place, so counting them would make this
            // witness one in-flight commit away from a spurious failure.
            // The same skip the read-only object-store walk uses.
            path.is_file() && path.extension().is_none_or(|ext| ext != "tmp")
        })
        .count()
}

/// A fresh engine directory for one headless composition.
fn headless_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-node-headless-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The direct write path, asserted through the neutral view surface.
fn publish_hello<V>(node: &mut WyrdNode<V>)
where
    V: NamespaceView<Materialization = RuntimeMaterialization>,
    V::Store: ObjectStore,
    <V::Store as ObjectStore>::Error: std::fmt::Debug,
{
    node.put_file("hello.txt", b"hello").unwrap();
    assert_serves_hello(node.view());
}

/// The loop-driven half, generic over the view: split the node, run
/// the loop on a scope thread, submit a `Mkdir` through the queue,
/// and read both generations through the publication slot — the same
/// parts a backend would build from, but no backend is ever
/// constructed.
fn drive_headless_node<V>(node: WyrdNode<V>)
where
    V: NamespaceView<Materialization = RuntimeMaterialization> + Send + Sync,
    V::Store: ObjectStore + Send + Sync,
    <V::Store as ObjectStore>::Error: std::fmt::Debug,
    V::Store: wyrd_format::DiscardRejectedRepresentation,
    <V::Store as wyrd_format::DiscardRejectedRepresentation>::Error: std::fmt::Debug,
{
    let (mut live, parts) = node
        .into_live(Duration::from_secs(5), &LiveConfig::default())
        .unwrap();
    // The test thread never touches the loop owner: it reads through
    // the shared publication slot from the node's own parts — the
    // same slot a backend would build from, but no backend exists.
    let slot = Arc::clone(&parts.projection);
    let queue = Arc::clone(&parts.mutations);
    let stop = Arc::new(AtomicBool::new(false));
    let config = LiveConfig {
        interval: Duration::from_millis(10),
        ..LiveConfig::default()
    };

    std::thread::scope(|scope| {
        let loop_stop = Arc::clone(&stop);
        let handle = scope.spawn(move || {
            let mut mailbox = SilentMailbox;
            live.run_loop(
                &mut mailbox,
                None::<&mut MemoryBulkSource>,
                &loop_stop,
                &config,
                &mut |_, _| {},
            )
        });

        // The mounted mutation channel works with no backend
        // submitting: the loop drains the queue and publishes.
        let outcome = queue
            .submit(MutationKind::Mkdir {
                path: "dir".to_string(),
            })
            .expect("mkdir submits");
        assert!(
            matches!(outcome, MutationOutcome::Done),
            "loop-driven mutation commits headless: {outcome:?}"
        );

        // The new generation serves through the publication slot.
        let mut seen_dir = false;
        for _ in 0..500 {
            let projection = slot.read().unwrap();
            assert_serves_hello(projection.view());
            if projection.view().lookup("dir").is_ok() {
                seen_dir = true;
                break;
            }
            drop(projection);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(seen_dir, "the mkdir generation publishes headless");

        stop.store(true, Ordering::Relaxed);
        handle
            .join()
            .unwrap()
            .expect("loop shuts down cleanly headless");
    });

    drop(parts);
}

/// Contract 45: one commit-forcing event's whole pending set plus
/// itself authors a single snapshot (`docs/contracts/write-path.md`, DG-1). Two
/// content members across two paths plus a namespace forcer fold into
/// one snapshot whose tree equals the final coherent state, and the
/// served generation advances by exactly one. Submitted straight
/// through the queue — no presentation backend builds the fold — so
/// this pins the loop's fold contract; the daemon's gathering (which
/// handles fold into submissions) is pinned at the mount boundary
/// in `wyrd-daemon`.
#[test]
fn fold_commits_one_snapshot_for_many_members() {
    let dir = headless_dir("contract-fold");
    let engine = Engine::create(
        dir.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();

    let mut node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(engine, MemoryObjectStore::default()).unwrap();
    node.put_file("a.txt", b"a").unwrap();
    node.put_file("b.txt", b"b").unwrap();
    let base_a = match node.view().lookup("a.txt").expect("a.txt resolves") {
        Node::File {
            size,
            executable,
            chunks,
            ..
        } => FileIdentity::new(size, executable, chunks),
        other => panic!("a.txt is a file, saw {other:?}"),
    };

    let (mut live, parts) = node
        .into_live(Duration::from_secs(5), &LiveConfig::default())
        .unwrap();
    let slot = Arc::clone(&parts.projection);
    let queue = Arc::clone(&parts.mutations);
    let before = slot.read().unwrap().generation();
    let stop = Arc::new(AtomicBool::new(false));
    let config = LiveConfig {
        interval: Duration::from_millis(10),
        ..LiveConfig::default()
    };

    std::thread::scope(|scope| {
        let loop_stop = Arc::clone(&stop);
        let handle = scope.spawn(move || {
            let mut mailbox = SilentMailbox;
            live.run_loop(
                &mut mailbox,
                None::<&mut MemoryBulkSource>,
                &loop_stop,
                &config,
                &mut |_, _| {},
            )
        });

        let outcome = queue
            .submit(MutationKind::Fold {
                members: vec![
                    FoldMember {
                        kind: MutationKind::CommitFile {
                            path: "a.txt".to_string(),
                            base: base_a,
                            executable: false,
                            content: b"A2".to_vec(),
                        },
                        forcer: false,
                    },
                    FoldMember {
                        kind: MutationKind::AppendFile {
                            path: "b.txt".to_string(),
                            content: b"+b".to_vec(),
                        },
                        forcer: false,
                    },
                    FoldMember {
                        kind: MutationKind::Mkdir {
                            path: "c-dir".to_string(),
                        },
                        forcer: true,
                    },
                ],
            })
            .expect("a fold submits");
        match outcome {
            MutationOutcome::Fold {
                forcer: FoldForcerOutcome::Applied(forcer),
                members,
            } => {
                assert!(
                    matches!(*forcer, MutationOutcome::Done),
                    "the namespace forcer reports its own outcome: {forcer:?}"
                );
                assert_eq!(
                    members.len(),
                    2,
                    "one disposition per non-forcing member: {members:?}"
                );
                assert!(
                    members
                        .iter()
                        .all(|member| matches!(member, FoldDisposition::Committed(_))),
                    "both content members commit into the one snapshot: {members:?}"
                );
            }
            other => panic!("a fold answers with a fold outcome, saw {other:?}"),
        }

        // The fold authors one snapshot: the served generation advances
        // by exactly one, and the tree equals the final coherent state.
        let mut published = false;
        for _ in 0..500 {
            if slot.read().unwrap().generation() == before + 1 {
                published = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(published, "one forcing event publishes one generation");
        let projection = slot.read().unwrap();
        let view = projection.view();
        let a = view.lookup("a.txt").expect("a.txt resolves");
        let file_a = view.open_file(&a).expect("a.txt opens");
        assert_eq!(view.read(&file_a, 0, 64).unwrap(), b"A2");
        let b = view.lookup("b.txt").expect("b.txt resolves");
        let file_b = view.open_file(&b).expect("b.txt opens");
        assert_eq!(view.read(&file_b, 0, 64).unwrap(), b"b+b");
        assert!(
            view.lookup("c-dir").is_ok(),
            "the namespace member lands in the same snapshot"
        );
        drop(projection);

        stop.store(true, Ordering::Relaxed);
        handle
            .join()
            .unwrap()
            .expect("loop shuts down cleanly headless");
    });

    drop(parts);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Contract 49 (`quarantine_drain_repairs_through_the_loop`): a
/// submitted verification failure is diagnosed, unlinked, and
/// unclaimed by one loop pass — bytes gone, claim gone, `Cached`
/// policy intact — and the repair is reported in the pass report,
/// not repeated on the next pass. The observation half (which bytes
/// may be named) is pinned at the view boundary in `wyrd-view` and
/// the discard half in `wyrd-core`; this pins the loop's half over
/// public APIs: submit, pass, repaired.
#[test]
fn quarantine_drain_repairs_through_the_loop() {
    use wyrd_core::quarantine::VerificationFailure;
    use wyrd_format::ObjectKind;

    let dir = headless_dir("contract-quarantine");
    let engine = Engine::create(
        dir.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let mut node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(engine, MemoryObjectStore::default()).unwrap();
    node.put_file("f.txt", b"eleven bytes").unwrap();
    let chunk = match node.view().lookup("f.txt").expect("f.txt resolves") {
        Node::File { chunks, .. } => chunks[0],
        other => panic!("f.txt is a file, saw {other:?}"),
    };
    let (mut live, parts) = node
        .into_live(Duration::from_secs(5), &LiveConfig::default())
        .unwrap();
    live.quarantine_queue()
        .submit(VerificationFailure::content_hash_mismatch(
            chunk,
            ObjectKind::Chunk,
        ));
    let mut mailbox = SilentMailbox;
    let report = live
        .sync_once(&mut mailbox, None::<&mut MemoryBulkSource>)
        .unwrap();
    assert_eq!(report.quarantined.observed, 1);
    assert_eq!(report.quarantined.claims_cleared, 1);
    assert_eq!(
        report.quarantined.bytes_discarded,
        b"eleven bytes".len() as u64
    );
    assert_eq!(report.quarantined.failures, 0);
    // Nothing requeued, nothing repaired twice.
    let idle = live
        .sync_once(&mut mailbox, None::<&mut MemoryBulkSource>)
        .unwrap();
    assert_eq!(idle.quarantined.observed, 0);
    drop(parts);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Contract 50 (`quarantined_identity_redemands_on_next_waiter`):
/// after the drain, the cleared claim with its intact `Cached`
/// policy reconciles back to pending, so the next waiter's pass
/// plans a fresh fetch instead of settling remote-only. No waiter
/// is synthesized: without one the identity stays unplanned.
/// (`RemoteOnly` content never pends, so the attempt itself proves
/// the policy survived the claim-clearing.)
#[test]
fn quarantined_identity_redemands_on_next_waiter() {
    use wyrd_core::quarantine::VerificationFailure;
    use wyrd_format::ObjectKind;

    let dir = headless_dir("contract-redemand");
    let engine = Engine::create(
        dir.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let mut node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(engine, MemoryObjectStore::default()).unwrap();
    node.put_file("f.txt", b"eleven bytes").unwrap();
    let chunk = match node.view().lookup("f.txt").expect("f.txt resolves") {
        Node::File { chunks, .. } => chunks[0],
        other => panic!("f.txt is a file, saw {other:?}"),
    };
    let (mut live, parts) = node
        .into_live(Duration::from_secs(5), &LiveConfig::default())
        .unwrap();
    live.quarantine_queue()
        .submit(VerificationFailure::content_hash_mismatch(
            chunk,
            ObjectKind::Chunk,
        ));
    let mut mailbox = SilentMailbox;
    live.sync_once(&mut mailbox, None::<&mut MemoryBulkSource>)
        .unwrap();
    // The waiter returns: admission carries the demand and the
    // pass plans a fresh fetch for the unclaimed identity, even
    // with an empty bulk source to attempt it against.
    parts.wants.register(chunk).unwrap();
    let bulk = &mut MemoryBulkSource::default();
    let redemand = live.sync_once(&mut mailbox, Some(bulk)).unwrap();
    // The empty bulk has no representation to serve, so the
    // re-planned identity lands as unfulfilled + missing — the
    // point is that it was planned at all: a `RemoteOnly`
    // identity with no claim and no policy would never appear.
    assert_eq!(redemand.fetched.unfulfilled, 1);
    assert_eq!(redemand.fetched.missing, 1);
    drop(parts);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Contract 51 (`quarantine_discards_real_bitrotted_bytes`):
/// contracts 49 and 50 run over `MemoryObjectStore`, which cannot
/// observe a verification failure and discards unconditionally —
/// so this one runs the loop over a directory store with genuinely
/// bitrotted bytes: the drain must verify-then-delete (a
/// `NowValid` would keep them), and the live file must be gone
/// from disk afterwards, not just unclaimed.
#[test]
fn quarantine_discards_real_bitrotted_bytes_through_the_loop() {
    use wyrd_core::quarantine::VerificationFailure;
    use wyrd_format::{FsObjectStore, ObjectKind};

    let dir = headless_dir("contract-bitrot");
    let engine = Engine::create(
        dir.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let mut node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(engine, FsObjectStore::open(dir.join("objects")).unwrap()).unwrap();
    node.put_file("f.txt", b"eleven bytes").unwrap();
    let chunk = match node.view().lookup("f.txt").expect("f.txt resolves") {
        Node::File { chunks, .. } => chunks[0],
        other => panic!("f.txt is a file, saw {other:?}"),
    };
    // Bitrot under the live name, at the documented layout.
    let hex = chunk.to_string();
    let bitrotted = dir
        .join("objects")
        .join("objects")
        .join(format!("{:02x}", ObjectKind::Chunk.byte()))
        .join(&hex[..2])
        .join(&hex[2..]);
    std::fs::write(&bitrotted, b"tampered!!!!").unwrap();
    let (mut live, parts) = node
        .into_live(Duration::from_secs(5), &LiveConfig::default())
        .unwrap();
    live.quarantine_queue()
        .submit(VerificationFailure::content_hash_mismatch(
            chunk,
            ObjectKind::Chunk,
        ));
    let mut mailbox = SilentMailbox;
    let report = live
        .sync_once(&mut mailbox, None::<&mut MemoryBulkSource>)
        .unwrap();
    assert_eq!(report.quarantined.observed, 1);
    assert_eq!(report.quarantined.claims_cleared, 1);
    assert_eq!(
        report.quarantined.bytes_discarded,
        b"eleven bytes".len() as u64
    );
    assert_eq!(report.quarantined.failures, 0);
    assert!(
        !bitrotted.exists(),
        "the bitrotted file is unlinked from disk"
    );
    drop(parts);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Author a snapshot through the engine behind the view's back: the
/// bytes land in the view's store (so the closure verifies), the
/// durable heads advance, and the installed projection stays exactly
/// what it was. The composition boundary must heal this gap.
fn author_behind_the_view(
    node: &mut WyrdNode<DriveView<MemoryObjectStore, RuntimeMaterialization>>,
    name: &str,
    data: &[u8],
) {
    let (engine, view) = node.parts_mut();
    let mut store = view.store_write().expect("the view store writes");
    let chunk = store
        .insert(ObjectKind::Chunk, data)
        .expect("the chunk inserts");
    let tree = Tree::from_entries(vec![Entry::file(
        name,
        data.len() as u64,
        false,
        vec![chunk],
    )
    .expect("the entry builds")])
    .expect("the tree builds")
    .insert_into(&mut *store)
    .expect("the tree inserts");
    drop(store);
    let store = view.store_read().expect("the view store reads");
    engine
        .author_snapshot(&*store, tree)
        .expect("the engine authors behind the view");
}

/// Issue 32: a reopened drive serves its namespace at composition,
/// with no explicit refresh anywhere. The view opens empty while the
/// engine holds durable heads; the composition boundary is where the
/// cache is made consistent, so the first generation already serves
/// the committed heads.
#[test]
fn reopened_drive_serves_its_namespace_at_composition() {
    const PASS: &str = "composition-freshness-pass";
    let dir = headless_dir("composition-reopen");
    let objects = dir.join("objects");
    let identity = DeviceIdentitySecret::generate().unwrap();
    let engine = Engine::create(dir.clone(), PASS, identity.clone()).unwrap();
    let mut node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(engine, FsObjectStore::open(objects.clone()).unwrap()).unwrap();
    node.put_file("hello.txt", b"hello").unwrap();
    drop(node);

    // Reopen from custody with no explicit refresh: composition alone
    // must project the durable heads.
    let reopened = Engine::open_keystore(dir.clone(), PASS, identity).unwrap();
    let node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(reopened, FsObjectStore::open(objects).unwrap()).unwrap();
    let (_live, parts) = node
        .into_live(Duration::from_secs(5), &LiveConfig::default())
        .expect("composition projects the durable heads");
    let projection = parts.projection.read().unwrap();
    assert_serves_hello(projection.view());
    drop(projection);
    drop(parts);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Issue 32: one otherwise-idle pass after composition does not
/// regress the namespace. The baseline revision is recorded after the
/// composition refresh, so the pass sees no change past the split and
/// leaves the served generation — still correct — alone.
#[test]
fn idle_pass_after_composition_does_not_regress() {
    const PASS: &str = "composition-freshness-pass";
    let dir = headless_dir("composition-idle");
    let objects = dir.join("objects");
    let identity = DeviceIdentitySecret::generate().unwrap();
    let engine = Engine::create(dir.clone(), PASS, identity.clone()).unwrap();
    let mut node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(engine, FsObjectStore::open(objects.clone()).unwrap()).unwrap();
    node.put_file("hello.txt", b"hello").unwrap();
    drop(node);

    let reopened = Engine::open_keystore(dir.clone(), PASS, identity).unwrap();
    let node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(reopened, FsObjectStore::open(objects).unwrap()).unwrap();
    let (mut live, parts) = node
        .into_live(Duration::from_secs(5), &LiveConfig::default())
        .expect("composition projects the durable heads");
    {
        let projection = parts.projection.read().unwrap();
        assert_serves_hello(projection.view());
    }
    let mut mailbox = SilentMailbox;
    live.sync_once(&mut mailbox, None::<&mut MemoryBulkSource>)
        .expect("the idle pass runs");
    let projection = parts.projection.read().unwrap();
    assert_serves_hello(projection.view());
    drop(projection);
    drop(parts);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Issue 32: an engine mutation through `parts_mut` is visible at
/// composition. The unrestricted engine handle may move the revision
/// past the installed namespace; the composition refresh runs after
/// any such mutation, so the live view never claims the new revision
/// while holding the old namespace.
#[test]
fn engine_mutation_through_parts_mut_is_visible() {
    let dir = headless_dir("composition-parts-mut");
    let engine = Engine::create(
        dir.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let mut node: WyrdNode<DriveView<MemoryObjectStore, RuntimeMaterialization>> =
        WyrdNode::new(engine, MemoryObjectStore::default()).unwrap();
    author_behind_the_view(&mut node, "behind.txt", b"behind");

    let (_live, parts) = node
        .into_live(Duration::from_secs(5), &LiveConfig::default())
        .expect("composition heals the parts_mut gap");
    let projection = parts.projection.read().unwrap();
    let opened = projection
        .view()
        .lookup("behind.txt")
        .expect("the engine-authored head serves at composition");
    let file = projection.view().open_file(&opened).unwrap();
    assert_eq!(
        projection.view().read(&file, 0, 64).unwrap(),
        b"behind",
        "the live view serves the engine's revision, not the old namespace"
    );
    drop(projection);
    drop(parts);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Issue 32: all-pending heads at composition stay unprojected but
/// do not fail it. Bytes the store does not hold cannot be served,
/// and a head mid-transfer must never blank an installed view — and
/// a first composition has no prior projection, so the baseline is
/// empty and `Ok`. The pending head stays observable (the next pass
/// reports it) and installs once its closure lands.
#[test]
fn all_pending_heads_at_composition_stay_unprojected() {
    let dir = headless_dir("composition-pending");
    let engine = Engine::create(
        dir.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let mut node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(engine, MemoryObjectStore::default()).unwrap();
    node.put_file("hello.txt", b"hello").unwrap();

    // A second head authored from a scratch store: eligible, but its
    // closure is still fetching from this view's perspective.
    let mut scratch = MemoryObjectStore::default();
    let chunk = scratch
        .insert(ObjectKind::Chunk, b"second")
        .expect("the chunk inserts");
    let tree = Tree::from_entries(vec![
        Entry::file("second.txt", 6, false, vec![chunk]).expect("the entry builds")
    ])
    .expect("the tree builds")
    .insert_into(&mut scratch)
    .expect("the tree inserts");
    {
        let (engine, _view) = node.parts_mut();
        engine
            .author_snapshot(&scratch, tree)
            .expect("the engine authors a pending head");
    }

    let (mut live, parts) = node
        .into_live(Duration::from_secs(5), &LiveConfig::default())
        .expect("pending heads defer publication, they do not refuse composition");
    {
        let projection = parts.projection.read().unwrap();
        assert_serves_hello(projection.view());
        assert!(
            projection.view().lookup("second.txt").is_err(),
            "the unfetched head mounts nothing yet"
        );
    }
    let mut mailbox = SilentMailbox;
    let report = live
        .sync_once(&mut mailbox, None::<&mut MemoryBulkSource>)
        .expect("the pass runs");
    assert_eq!(
        report.pending_heads, 1,
        "the unfetched head is reported, not silently converged"
    );
    drop(parts);
    std::fs::remove_dir_all(dir).unwrap();

    // The true all-pending case: zero publishable heads, so the
    // baseline is empty — and still `Ok`, with the pending head
    // reported rather than hidden.
    let dir = headless_dir("composition-pending-all");
    let engine = Engine::create(
        dir.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let mut node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(engine, MemoryObjectStore::default()).unwrap();
    {
        let mut scratch = MemoryObjectStore::default();
        let chunk = scratch
            .insert(ObjectKind::Chunk, b"second")
            .expect("the chunk inserts");
        let tree = Tree::from_entries(vec![
            Entry::file("second.txt", 6, false, vec![chunk]).expect("the entry builds")
        ])
        .expect("the tree builds")
        .insert_into(&mut scratch)
        .expect("the tree inserts");
        let (engine, _view) = node.parts_mut();
        engine
            .author_snapshot(&scratch, tree)
            .expect("the engine authors a pending head");
    }
    let (mut live, parts) = node
        .into_live(Duration::from_secs(5), &LiveConfig::default())
        .expect("all-pending heads defer publication, they do not refuse composition");
    {
        let projection = parts.projection.read().unwrap();
        assert!(
            projection.view().lookup("second.txt").is_err(),
            "no publishable head, so the baseline is empty — but composition succeeds"
        );
    }
    let mut mailbox = SilentMailbox;
    let report = live
        .sync_once(&mut mailbox, None::<&mut MemoryBulkSource>)
        .expect("the pass runs");
    assert_eq!(
        report.pending_heads, 1,
        "the unfetched head is reported, not silently converged"
    );
    drop(parts);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Issue 32, OD-32-C first row: when the composition refresh fails
/// and no trustworthy projection exists, composition is refused with
/// the underlying error — never an `Ok` carrying an empty view that
/// reads as "this drive has no content". The contrast at the end is
/// the point: a genuinely empty drive composes fine, so an `Err` is
/// observably different from an empty `Ok`.
#[test]
fn composition_refusal_is_distinguishable_from_an_empty_view() {
    let dir = headless_dir("composition-refusal");
    let engine = Engine::create(
        dir.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let mut node: WyrdNode<DriveView<MemoryObjectStore, RuntimeMaterialization>> =
        WyrdNode::new(engine, MemoryObjectStore::default()).unwrap();
    // Durable heads the empty view was never told about.
    author_behind_the_view(&mut node, "behind.txt", b"behind");
    // Simulated bitrot after durable commit: the commit watermark no
    // longer parses, so no rebuild can be trusted and the composition
    // may not project from it.
    std::fs::write(dir.join("CURRENT"), b"rot").unwrap();

    let err = match node.into_live(Duration::from_secs(5), &LiveConfig::default()) {
        Err(err) => err,
        Ok(_) => {
            panic!("composition with no trustworthy projection is refused, never served empty")
        }
    };
    assert!(
        matches!(
            err,
            NodeError::Runtime(EngineError::Durable(DurableError::CorruptCurrent))
        ),
        "the refusal carries the underlying error: {err:?}"
    );

    // A genuinely empty drive composes fine and serves empty.
    let empty_dir = headless_dir("composition-refusal-empty");
    let engine = Engine::create(
        empty_dir.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(engine, MemoryObjectStore::default()).unwrap();
    let (_live, parts) = node
        .into_live(Duration::from_secs(5), &LiveConfig::default())
        .expect("an empty drive composes");
    let projection = parts.projection.read().unwrap();
    assert!(
        projection.view().lookup("anything").is_err(),
        "a fresh drive serves empty"
    );
    drop(projection);
    drop(parts);
    std::fs::remove_dir_all(dir).unwrap();
    std::fs::remove_dir_all(empty_dir).unwrap();
}

/// Issue 32, OD-32-C second row: after a successful projection, a
/// failing refresh retains the last successfully installed
/// projection and surfaces the error — it never replaces good state
/// with less trustworthy state. The view store is untouched by the
/// durable damage, so the retained projection keeps serving.
#[test]
fn last_good_projection_is_retained_after_a_refresh_failure() {
    let dir = headless_dir("composition-retained");
    let engine = Engine::create(
        dir.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let mut node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(engine, MemoryObjectStore::default()).unwrap();
    node.put_file("hello.txt", b"hello").unwrap();
    assert_serves_hello(node.view());

    std::fs::write(dir.join("CURRENT"), b"rot").unwrap();
    let err = node
        .refresh_live_heads()
        .expect_err("a refresh past durable damage fails");
    assert!(
        matches!(err, EngineError::Durable(DurableError::CorruptCurrent)),
        "the refresh surfaces the underlying error: {err:?}"
    );
    assert_serves_hello(node.view());
    std::fs::remove_dir_all(dir).unwrap();
}

/// Issue 32, OD-32-C third row: a consumer reaching the drive through
/// the owner can distinguish the four projection states with the
/// facts the core already reports — no core-level degraded mode is
/// invented. Fresh states arrive as `Ok` (empty or serving, with the
/// engine's head count agreeing); no trustworthy projection arrives
/// as `Err`; a retained projection arrives as a surfaced refresh
/// error beside a still-serving view.
#[test]
fn a_consumer_can_observe_projection_staleness() {
    // State 1: fresh, valid, empty. The engine holds no heads, so the
    // empty namespace composition hands over is authoritative.
    let dir_empty = headless_dir("composition-stale-empty");
    let engine = Engine::create(
        dir_empty.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(engine, MemoryObjectStore::default()).unwrap();
    assert!(
        node.engine().live_heads().unwrap().is_empty(),
        "no durable heads, so empty is fresh rather than stale"
    );
    let (_live, parts) = node
        .into_live(Duration::from_secs(5), &LiveConfig::default())
        .expect("an empty drive composes");
    assert!(
        parts.projection.read().unwrap().view().lookup("x").is_err(),
        "fresh empty: composition succeeds and serves nothing"
    );
    drop(parts);

    // State 2: fresh, valid, non-empty. One durable head, served at
    // composition with no explicit refresh.
    let dir_full = headless_dir("composition-stale-full");
    let engine = Engine::create(
        dir_full.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let mut node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(engine, MemoryObjectStore::default()).unwrap();
    node.put_file("hello.txt", b"hello").unwrap();
    assert_eq!(
        node.engine().live_heads().unwrap().len(),
        1,
        "one durable head behind the composition"
    );
    let (_live, parts) = node
        .into_live(Duration::from_secs(5), &LiveConfig::default())
        .expect("a populated drive composes");
    assert_serves_hello(parts.projection.read().unwrap().view());
    drop(parts);

    // State 3: no trustworthy projection. Durable heads exist behind
    // an empty view and the refresh cannot run: composition is an
    // `Err`, never an authoritative-looking empty view.
    let dir_refused = headless_dir("composition-stale-refused");
    let engine = Engine::create(
        dir_refused.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let mut node: WyrdNode<DriveView<MemoryObjectStore, RuntimeMaterialization>> =
        WyrdNode::new(engine, MemoryObjectStore::default()).unwrap();
    author_behind_the_view(&mut node, "behind.txt", b"behind");
    std::fs::write(dir_refused.join("CURRENT"), b"rot").unwrap();
    assert!(
        node.into_live(Duration::from_secs(5), &LiveConfig::default())
            .is_err(),
        "no trustworthy projection: composition is refused"
    );

    // State 4: a retained last-good projection whose freshness could
    // not be established. The refresh surfaces its error and the
    // previously installed heads keep serving.
    let dir_retained = headless_dir("composition-stale-retained");
    let engine = Engine::create(
        dir_retained.clone(),
        "headless-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let mut node: WyrdNode<DriveView<_, RuntimeMaterialization>> =
        WyrdNode::new(engine, MemoryObjectStore::default()).unwrap();
    node.put_file("hello.txt", b"hello").unwrap();
    std::fs::write(dir_retained.join("CURRENT"), b"rot").unwrap();
    assert!(
        node.refresh_live_heads().is_err(),
        "freshness could not be established: the refresh surfaces its error"
    );
    assert_serves_hello(node.view());

    for dir in [&dir_empty, &dir_full, &dir_refused, &dir_retained] {
        std::fs::remove_dir_all(dir).unwrap();
    }
}
