//! The drive-ownership gate: one owner opens the store, and head
//! eligibility is a composer convention the type system does not
//! enforce. Three named tests pin both halves plus the negative
//! invariant that keeps the bypass from spreading.
//!
//! The first test is the gate's own acceptance command: `cargo
//! nextest run -E 'test(drive_ownership)'` selects it and runs it,
//! and a run selecting zero tests fails the gate rather than
//! passing vacuously. Its behaviour duplicates the store-level
//! unit tests deliberately: the gate needs the name, not novelty.
//!
//! The second test demonstrates the reachable bypass in safe code:
//! a correctly signed body for a superseded snapshot authorizes
//! (the token proves a drive-bound signature only), wraps into a
//! head, and serves through a view built from it. That is the
//! hazard, executable: a change that closes the bypass must fail
//! this test and update it deliberately. Head eligibility is
//! decided at composition by the engine classification (only
//! live-lineage eligible heads advance the live view, per
//! `docs/architecture.md` invariant 3), so the sanctioned
//! installer is the composition path and nothing else.
//!
//! The third test is the negative invariant in executable form:
//! outside test code, heads are minted only from engine-classified
//! material in the core composition path, view heads are never
//! constructed by consumers, and head sets are installed only by
//! the node composition. The whole contracts crate counts as test
//! code, like every harness and fixture: the scan covers the
//! shipped surface.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use wyrd_core::view::Head;
use wyrd_format::{
    membership::Admission, Change, Entry, MemoryObjectStore, ObjectKind, ObjectStore, SnapshotId,
    Tree,
};
use wyrd_sync::authorization::{Classification, SnapshotDag};
use wyrd_sync::durable::{AuthorizeSnapshot, AuthorizedSnapshot, DurableError, DurableStore};
use wyrd_sync::membership::MembershipLog;
use wyrd_view::{DriveView, Node, ViewHead};

use crate::support::{
    device, drive, scratch_dir, signed_snapshot, signed_transition, RemoteOnlyMaterialization,
};

/// The gate's named acceptance test: one owner holds the store, a
/// second open of the same directory refuses with `StoreLocked`,
/// and dropping the holder releases the directory with nothing
/// committed.
#[test]
fn drive_ownership() {
    const PASSPHRASE: &str = "ownership-gate-pass";
    let dir = scratch_dir("ownership-gate");
    let holder = DurableStore::open(dir.clone(), drive(), PASSPHRASE).unwrap();
    // A second store on the same directory is refused while the
    // first holds the lock: two single-writer stores must never
    // share one state directory.
    let second = DurableStore::open(dir.clone(), drive(), PASSPHRASE);
    assert!(
        matches!(second, Err(DurableError::StoreLocked)),
        "expected StoreLocked, got {:?}",
        second.map(|_| ()).err()
    );
    drop(holder);
    // Dropping the holder released the advisory lock: the directory
    // reopens with nothing committed.
    let reopened = DurableStore::open(dir.clone(), drive(), PASSPHRASE).unwrap();
    assert_eq!(reopened.current(), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A stale fork classifies superseded, yet its correctly signed
/// body still authorizes, wraps, and serves: the bypass is real,
/// reachable in safe code, and named here rather than left silent.
/// No `unsafe` block appears anywhere in this test.
#[test]
fn head_eligibility_is_a_composer_convention() {
    let owner = device(0xA0);
    let newcomer = device(0xB0);

    // The membership log: genesis admitting the owner, then an
    // admission advancing the log to epoch 2.
    let mut log = MembershipLog::new(drive());
    let genesis = sign_transition_genesis(&owner);
    let genesis_id = log.observe(genesis);
    let admission = signed_transition(
        2,
        Some(genesis_id),
        Vec::new(),
        vec![Change::Admit(Admission {
            device: newcomer.id,
            encryption_key: newcomer.encryption_key,
        })],
        &[owner.id, newcomer.id],
        &[owner.id],
        &owner,
    );
    let admission_id = log.observe(admission);

    // Four trees with distinct bytes in one store: the view serves
    // the superseded body from these same bytes below.
    let mut objects = MemoryObjectStore::default();
    let tree_base = tree_with(&mut objects, "base.txt", b"base");
    let tree_first = tree_with(&mut objects, "first.txt", b"first");
    let tree_stale = tree_with(&mut objects, "stale.txt", b"stale");
    let tree_third = tree_with(&mut objects, "third.txt", b"third");

    // The stale fork: two same-epoch heads, the log advances, new
    // work parents the first head only. The second head is retained
    // history, never live.
    let base = signed_snapshot(Vec::new(), tree_base, &owner, genesis_id, 1, 1);
    let mut dag = SnapshotDag::new(drive());
    let id_base = dag.observe(base);
    let first = signed_snapshot(vec![id_base], tree_first, &owner, genesis_id, 1, 2);
    let id_first = dag.observe(first);
    let stale = signed_snapshot(vec![id_base], tree_stale, &owner, genesis_id, 1, 3);
    let id_stale = dag.observe(stale.clone());
    let third = signed_snapshot(vec![id_first], tree_third, &owner, admission_id, 2, 4);
    let id_third = dag.observe(third);

    let verdicts: HashMap<SnapshotId, Classification> = dag.classify(&log);
    assert_eq!(
        verdicts.get(&id_third),
        Some(&Classification::Eligible),
        "the continued branch stays live"
    );
    assert_eq!(
        verdicts.get(&id_first),
        Some(&Classification::CanonicalHistory),
        "the continued parent is history, not a head"
    );
    assert_eq!(
        verdicts.get(&id_stale),
        Some(&Classification::Superseded),
        "the stale fork is retained history, never live"
    );
    assert_eq!(dag.eligible_heads(&log), vec![id_third]);

    // The token cannot see any of that: the stale body carries a
    // valid drive-bound signature, so authorization succeeds and a
    // head wraps it in safe code.
    let authorized: AuthorizedSnapshot =
        AuthorizedSnapshot::authorize(stale, &drive()).expect("a correctly signed body authorizes");
    let head = Head::new(authorized);
    let view = DriveView::new(
        objects,
        RemoteOnlyMaterialization,
        vec![ViewHead::new(head)],
    );
    assert!(
        matches!(view.lookup("stale.txt"), Ok(Node::File { .. })),
        "the superseded body serves through a directly built view"
    );
}

/// Build the genesis transition admitting the owner as sole member
/// and owner: the trust root every snapshot below is bound to.
fn sign_transition_genesis(owner: &crate::support::Device) -> wyrd_format::MembershipTransition {
    signed_transition(
        1,
        None,
        Vec::new(),
        vec![
            Change::Admit(Admission {
                device: owner.id,
                encryption_key: owner.encryption_key,
            }),
            Change::SetOwners(vec![owner.id]),
        ],
        &[owner.id],
        &[owner.id],
        owner,
    )
}

/// Insert one file's bytes and return its tree: distinct labels hash
/// to distinct identities in any store.
fn tree_with(store: &mut MemoryObjectStore, name: &str, label: &[u8]) -> wyrd_format::ContentId {
    let chunk = store
        .insert(ObjectKind::Chunk, label)
        .expect("the chunk inserts");
    Tree::from_entries(vec![Entry::file(
        name,
        label.len() as u64,
        false,
        vec![chunk],
    )
    .expect("the entry builds")])
    .expect("the tree builds")
    .insert_into(store)
    .expect("the tree inserts")
}

/// Outside test code, heads are minted only from engine-classified
/// material in the core composition path, view heads are never
/// constructed by consumers, and head sets are installed only by
/// the node composition. A new production call site for any of
/// the three fails here and belongs in the allow list deliberately
/// or not at all.
#[test]
fn only_refresh_live_heads_installs_heads() {
    const HEAD_NEW_ALLOWED: &[&str] = &[
        "crates/wyrd-core/src/node.rs",
        "crates/wyrd-core/src/live/pass.rs",
        "crates/wyrd-core/src/live/apply.rs",
    ];
    const SET_HEADS_CALL_ALLOWED: &[&str] = &["crates/wyrd-core/src/node.rs"];
    let root = workspace_root();
    let mut head_violations = Vec::new();
    let mut view_head_violations = Vec::new();
    let mut set_heads_violations = Vec::new();
    let mut scanned = 0;
    visit_sources(&root, &mut |path, text| {
        let rel_path = path.strip_prefix(&root).unwrap_or(path);
        if is_test_code(rel_path) || is_self(path) {
            return;
        }
        scanned += 1;
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        for (index, line) in code_lines(text).enumerate() {
            if line.contains("Head::new") && !HEAD_NEW_ALLOWED.contains(&rel.as_str()) {
                head_violations.push(format!("{rel}:{} names Head::new", index + 1));
            }
            if line.contains("ViewHead::new") {
                view_head_violations.push(format!("{rel}:{} names ViewHead::new", index + 1));
            }
            if line.contains("set_heads(")
                && !line.contains("fn set_heads")
                && !SET_HEADS_CALL_ALLOWED.contains(&rel.as_str())
            {
                set_heads_violations.push(format!("{rel}:{} installs a head set", index + 1));
            }
        }
    });
    assert!(scanned > 0, "the scan covered shipped sources");
    assert!(
        head_violations.is_empty(),
        "head minting outside the core composition path:\n- {}",
        head_violations.join("\n- ")
    );
    assert!(
        view_head_violations.is_empty(),
        "consumer view-head construction outside test code:\n- {}",
        view_head_violations.join("\n- ")
    );
    assert!(
        set_heads_violations.is_empty(),
        "head-set installation outside the node composition:\n- {}",
        set_heads_violations.join("\n- ")
    );
}

/// The workspace root above this crate: two levels up from
/// `crates/wyrd-contracts`.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

/// Test code never carries the gate: the whole contracts suite is a
/// test harness by construction, and unit fixtures live beside
/// their modules under test-marked names.
fn is_test_code(path: &Path) -> bool {
    path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy().to_lowercase();
        name.contains("test")
            || name.contains("harness")
            || name.contains("support")
            || name.contains("contracts")
            || name.contains("fixture")
    })
}

/// This file exempts itself by name: the scan's own marker strings
/// would otherwise flag the gate that enforces them. The precedent
/// is the shipped-surface exemption for the interface's own tests.
fn is_self(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == "ownership_contracts.rs")
}

/// Recurse into every Rust source under the root, skipping build
/// output and version control.
fn visit_sources(root: &Path, visit: &mut impl FnMut(&Path, &str)) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            // Never follow symlinks: the checkout hosts store links
            // (`.devenv`, `result`) whose targets would explode the
            // walk or loop it.
            if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                continue;
            }
            if path.is_dir() {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if name == "target" || name.starts_with('.') {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    visit(&path, &text);
                }
            }
        }
    }
}

/// Source lines with comments removed: doc and line comments never
/// carry a gate, and a marker named in prose must not trip it.
fn code_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(|line| {
        let code = match line.find("//") {
            Some(index) => &line[..index],
            None => line,
        };
        code.trim()
    })
}
