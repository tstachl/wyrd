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
//! the node composition. Test code is the contracts suite,
//! `tests/` dirs, and the pinned test-file list, plus test-gated
//! `#[cfg]` regions inside shipped files (`#[cfg(test)]` and
//! `#[cfg(all(test, ...))]`; `any(test, feature)` ships and stays
//! scanned): the scan covers the shipped surface, and a production
//! module that merely mentions tests in its name is scanned.

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

use crate::contract_source::code_text;
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
    let mut scanned_paths: Vec<String> = Vec::new();
    visit_sources(&root, &mut |path, text| {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if is_test_code(&rel) {
            return;
        }
        scanned += 1;
        scanned_paths.push(rel.clone());
        for (index, line) in live_lines(text) {
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
    for allowed in HEAD_NEW_ALLOWED {
        assert!(
            scanned_paths.iter().any(|p| p == allowed),
            "the scan reached the allow-listed composer {allowed}"
        );
    }
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

/// Test code never carries the gate, by location: the whole
/// contracts suite is a test harness by construction, and
/// integration tests live under `tests/` directories. Unit fixtures
/// live in test-named files (`tests_*`, `test_*`, `tests.rs`) listed
/// in `TEST_FILE_PINS`: a new test-named file is scanned until it is
/// added here deliberately — a shipped module that merely mentions
/// tests in its name (a `support_auth.rs` would previously have
/// evaded the scan) never matches. Test code inside scanned files is
/// exempt by content instead: see `live_lines` for the
/// `#[cfg(test)]` region rule.
/// Pinned test-named files: every `tests_*` / `test_*` / `tests.rs`
/// file outside the contracts suite. A new test file must be added
/// here deliberately; otherwise it is scanned (fail closed). Sorted.
const TEST_FILE_PINS: &[&str] = &[
    "crates/wyrd-cli/src/tests_cli.rs",
    "crates/wyrd-cli/src/tests_device.rs",
    "crates/wyrd-cli/src/tests_drive.rs",
    "crates/wyrd-cli/src/tests_export.rs",
    "crates/wyrd-cli/src/tests_harness.rs",
    "crates/wyrd-cli/src/tests_member.rs",
    "crates/wyrd-cli/src/tests_mount.rs",
    "crates/wyrd-cli/src/tests_policy.rs",
    "crates/wyrd-cli/src/tests_probes.rs",
    "crates/wyrd-cli/src/tests_snapshot.rs",
    "crates/wyrd-cli/src/tests_sync.rs",
    "crates/wyrd-cli/src/update/tests.rs",
    "crates/wyrd-core/src/export/tests_harness.rs",
    "crates/wyrd-core/src/export/tests_staging.rs",
    "crates/wyrd-core/src/export/tests_walk.rs",
    "crates/wyrd-core/src/live/tests_backoff.rs",
    "crates/wyrd-core/src/live/tests_entry_identity.rs",
    "crates/wyrd-core/src/live/tests_entry_precondition.rs",
    "crates/wyrd-core/src/live/tests_mirror_diagnostics.rs",
    "crates/wyrd-core/src/live/tests_parent_mutation.rs",
    "crates/wyrd-core/src/live/tests_pause.rs",
    "crates/wyrd-core/src/live/tests_post_corruption.rs",
    "crates/wyrd-core/src/live/tests_prereq.rs",
    "crates/wyrd-core/src/live/tests_publish_budget.rs",
    "crates/wyrd-core/src/live/tests_quota.rs",
    "crates/wyrd-core/src/live/tests_restart_redelivery.rs",
    "crates/wyrd-core/src/live/tests_serve_barrier.rs",
    "crates/wyrd-core/src/live/tests_terminal.rs",
    "crates/wyrd-core/src/live/tests_trigger.rs",
    "crates/wyrd-core/src/mailbox/tests_backpressure.rs",
    "crates/wyrd-core/src/mailbox/tests_catchup.rs",
    "crates/wyrd-core/src/mailbox/tests_classification.rs",
    "crates/wyrd-core/src/mailbox/tests_crossimpl.rs",
    "crates/wyrd-core/src/mailbox/tests_dedupe.rs",
    "crates/wyrd-core/src/mailbox/tests_delivery.rs",
    "crates/wyrd-core/src/mailbox/tests_drainer.rs",
    "crates/wyrd-core/src/mailbox/tests_harness.rs",
    "crates/wyrd-core/src/mailbox/tests_interop.rs",
    "crates/wyrd-core/src/mailbox/tests_mailbox.rs",
    "crates/wyrd-core/src/mailbox/tests_multirelay.rs",
    "crates/wyrd-core/src/mailbox/tests_outage.rs",
    "crates/wyrd-core/src/mailbox/tests_poison.rs",
    "crates/wyrd-core/src/mailbox/tests_retry_evict.rs",
    "crates/wyrd-core/src/mailbox/tests_shutdown.rs",
    "crates/wyrd-core/src/mailbox/tests_signer_boundary.rs",
    "crates/wyrd-core/src/mutation/tests_batch.rs",
    "crates/wyrd-core/src/mutation/tests_harness.rs",
    "crates/wyrd-core/src/mutation/tests_parents.rs",
    "crates/wyrd-core/src/mutation/tests_queue.rs",
    "crates/wyrd-core/src/read/tests_read.rs",
    "crates/wyrd-core/src/tests_conflict_host.rs",
    "crates/wyrd-core/src/tests_conflict_surface.rs",
    "crates/wyrd-core/src/tests_invitation_surface.rs",
    "crates/wyrd-core/src/tests_key_surface.rs",
    "crates/wyrd-core/src/tests_lifecycle_surface.rs",
    "crates/wyrd-core/src/tests_materialization_surface.rs",
    "crates/wyrd-core/src/tests_member_surface.rs",
    "crates/wyrd-core/src/tests_notifications.rs",
    "crates/wyrd-core/src/tests_relay_posture.rs",
    "crates/wyrd-core/src/tests_settings.rs",
    "crates/wyrd-core/src/tests_status_surface.rs",
    "crates/wyrd-core/src/tests_storage_surface.rs",
    "crates/wyrd-daemon/src/core/tests_backend.rs",
    "crates/wyrd-daemon/src/core/tests_coalescing.rs",
    "crates/wyrd-daemon/src/core/tests_composition.rs",
    "crates/wyrd-daemon/src/core/tests_handles.rs",
    "crates/wyrd-daemon/src/core/tests_harness.rs",
    "crates/wyrd-daemon/src/core/tests_mount.rs",
    "crates/wyrd-daemon/src/core/tests_mount_free.rs",
    "crates/wyrd-daemon/src/core/tests_policy.rs",
    "crates/wyrd-daemon/src/core/tests_publish.rs",
    "crates/wyrd-daemon/src/core/tests_run_loop.rs",
    "crates/wyrd-daemon/src/core/tests_sync.rs",
    "crates/wyrd-daemon/src/core/tests_teardown.rs",
    "crates/wyrd-daemon/src/core/tests_vault.rs",
    "crates/wyrd-daemon/src/fuse/tests_budget.rs",
    "crates/wyrd-daemon/src/fuse/tests_conflicts.rs",
    "crates/wyrd-daemon/src/fuse/tests_errors.rs",
    "crates/wyrd-daemon/src/fuse/tests_handles.rs",
    "crates/wyrd-daemon/src/fuse/tests_harness.rs",
    "crates/wyrd-daemon/src/fuse/tests_inode.rs",
    "crates/wyrd-daemon/src/fuse/tests_readonly.rs",
    "crates/wyrd-daemon/src/fuse/tests_want.rs",
    "crates/wyrd-daemon/src/fuse/tests_write.rs",
    "crates/wyrd-daemon/src/owner/tests_owner.rs",
    "crates/wyrd-format/src/mutation/tests_mutation.rs",
    "crates/wyrd-sync/src/authorization/test_util.rs",
    "crates/wyrd-sync/src/bulk/tests_bulk.rs",
    "crates/wyrd-sync/src/durable/codec/tests_codec.rs",
    "crates/wyrd-sync/src/durable/tests_capability.rs",
    "crates/wyrd-sync/src/durable/tests_corruption.rs",
    "crates/wyrd-sync/src/durable/tests_harness.rs",
    "crates/wyrd-sync/src/durable/tests_lifecycle.rs",
    "crates/wyrd-sync/src/durable/tests_outbox.rs",
    "crates/wyrd-sync/src/durable/tests_permissions.rs",
    "crates/wyrd-sync/src/durable/tests_rebuild.rs",
    "crates/wyrd-sync/src/durable/tests_reconciliation.rs",
    "crates/wyrd-sync/src/durable/tests_rotation.rs",
    "crates/wyrd-sync/src/keys/capability/tests.rs",
    "crates/wyrd-sync/src/membership/test_util.rs",
    "crates/wyrd-sync/src/runtime/author/tests_admission.rs",
    "crates/wyrd-sync/src/runtime/author/tests_admission_failures.rs",
    "crates/wyrd-sync/src/runtime/author/tests_carry.rs",
    "crates/wyrd-sync/src/runtime/author/tests_delivery.rs",
    "crates/wyrd-sync/src/runtime/author/tests_epoch.rs",
    "crates/wyrd-sync/src/runtime/author/tests_fill.rs",
    "crates/wyrd-sync/src/runtime/author/tests_harness.rs",
    "crates/wyrd-sync/src/runtime/author/tests_lifecycle.rs",
    "crates/wyrd-sync/src/runtime/author/tests_merge.rs",
    "crates/wyrd-sync/src/runtime/author/tests_read_paths.rs",
    "crates/wyrd-sync/src/runtime/author/tests_reader.rs",
    "crates/wyrd-sync/src/runtime/author/tests_recover.rs",
    "crates/wyrd-sync/src/runtime/author/tests_reissue.rs",
    "crates/wyrd-sync/src/runtime/author/tests_removal.rs",
    "crates/wyrd-sync/src/runtime/author/tests_resolution.rs",
    "crates/wyrd-sync/src/runtime/author/tests_rotation.rs",
    "crates/wyrd-sync/src/runtime/author/tests_set_owners.rs",
    "crates/wyrd-sync/src/runtime/author/tests_snapshot.rs",
    "crates/wyrd-sync/src/runtime/bootstrap/tests_invitation.rs",
    "crates/wyrd-sync/src/runtime/bootstrap/tests_keystore.rs",
    "crates/wyrd-sync/src/runtime/bootstrap/tests_pairing.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_authoring.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_convergence.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_crash.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_delivery.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_drain.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_durability.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_harness.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_late_joiner.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_lifecycle.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_materialization.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_partition.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_preframing.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_properties.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_reconciliation.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_registration.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_reopen_agreement.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_response.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_restart_equivalence.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_serving.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_stale_obligations.rs",
    "crates/wyrd-sync/src/runtime/engine/tests_terminal.rs",
    "crates/wyrd-sync/src/runtime/fetch/tests_attempts.rs",
    "crates/wyrd-sync/src/runtime/fetch/tests_fetch.rs",
    "crates/wyrd-sync/src/runtime/fetch/tests_limits.rs",
    "crates/wyrd-sync/src/runtime/fetch/tests_limits_e2e.rs",
    "crates/wyrd-sync/src/runtime/fetch/tests_strikes.rs",
    "crates/wyrd-sync/src/runtime/intake/tests_announcement.rs",
    "crates/wyrd-sync/src/runtime/intake/tests_capability.rs",
    "crates/wyrd-sync/src/runtime/intake/tests_deferred.rs",
    "crates/wyrd-sync/src/runtime/intake/tests_harness.rs",
    "crates/wyrd-sync/src/runtime/intake/tests_pipeline.rs",
    "crates/wyrd-sync/src/runtime/intake/tests_reconciliation_request.rs",
    "crates/wyrd-sync/src/runtime/intake/tests_resilience.rs",
    "crates/wyrd-sync/src/runtime/intake/tests_rotation.rs",
    "crates/wyrd-sync/src/runtime/plan/tests_accounting.rs",
    "crates/wyrd-sync/src/runtime/plan/tests_bounded.rs",
    "crates/wyrd-sync/src/runtime/plan/tests_convergence.rs",
    "crates/wyrd-sync/src/runtime/plan/tests_retention.rs",
    "crates/wyrd-sync/src/runtime/test_util.rs",
    "crates/wyrd-sync/src/runtime/tests_reconcile.rs",
    "crates/wyrd-sync/src/runtime/tests_records.rs",
    "crates/wyrd-sync/src/runtime/tests_support.rs",
    "crates/wyrd-sync/src/serving/tests_serving.rs",
    "crates/wyrd-view/src/view/tests_chunks.rs",
    "crates/wyrd-view/src/view/tests_entry_identity.rs",
    "crates/wyrd-view/src/view/tests_grammar.rs",
    "crates/wyrd-view/src/view/tests_harness.rs",
    "crates/wyrd-view/src/view/tests_merge.rs",
    "crates/wyrd-view/src/view/tests_read.rs",
    "crates/wyrd-view/src/view/tests_symlink.rs",
];

/// Test code by location: the contracts suite, `tests/` dirs, or the
/// pinned test-file list.
fn is_test_code(rel: &str) -> bool {
    if rel.starts_with("crates/wyrd-contracts/") {
        return true;
    }
    if rel.split('/').any(|component| component == "tests") {
        return true;
    }
    TEST_FILE_PINS.contains(&rel)
}

/// Whether one lexed source line opens a test-only region: the bare
/// `#[cfg(test)]` form, or `#[cfg(all(test, ...))]` (which still
/// requires `test`). `#[cfg(any(test, ...))]` ships when the other
/// predicate holds (the keystore's `any(test, feature =
/// "insecure-fast-kdf")` constants compile into the feature build),
/// so it is scanned, never exempt — the asymmetry is deliberate and
/// documented here. `not(test)` likewise stays scanned. `cfg_attr`
/// never gates an item, so it never exempts. The input is a
/// `code_text` line (comments and literals blanked), so a `test`
/// inside a string cannot match.
fn is_test_cfg_attr(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix("#[cfg(") else {
        return false;
    };
    // Same-line item (`#[cfg(test)] mod foo;`): the attribute and
    // the item share the line. Exempt the line itself; the caller
    // skips no following line for this form (see `live_lines`).
    let inner = rest.rsplit_once(']').map(|(i, _)| i).unwrap_or(rest);
    // Tokenize bare identifiers; string contents are already blanked
    // by `code_text`, so `feature = "test"` cannot contribute.
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in inner.chars() {
        if is_ident(c) {
            cur.push(c);
        } else if !cur.is_empty() {
            tokens.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    let has = |w: &str| tokens.iter().any(|t| t == w);
    if has("not") || has("cfg_attr") {
        return false;
    }
    if has("any") {
        // `any(test)` alone is test-only; `any(test, feature)` ships.
        return tokens.iter().filter(|t| *t != "any").all(|t| t == "test");
    }
    has("test")
}

/// Shipped lines of one source file: every line outside test-only
/// `#[cfg]` regions, with original line numbers. A test-gated
/// attribute (see `is_test_cfg_attr`) exempts the item that follows
/// it — a brace-balanced block through its matching close brace, or
/// just the attribute line itself when the item carries no braces
/// (a same-line `mod foo;`/`use` item cannot mint a head, and
/// skipping the following line would under-scan production code).
/// Brace counting runs on `code_text` output (comments and literals
/// blanked, newlines preserved), so a brace inside a string never
/// opens or closes a region. An unbalanced region fails closed: lines
/// from the attribute onward are scanned, never silently exempted.
/// Marker matching runs on these same lexed lines (not on
/// comment-stripped text), so `//` inside a string literal cannot
/// hide a mint and a block comment naming a marker cannot trip the
/// gate. Pure so tests pin the rule without touching the filesystem.
fn live_lines(text: &str) -> Vec<(usize, String)> {
    let blanked = code_text(text);
    let stripped: Vec<&str> = blanked.lines().collect();
    let mut out = Vec::new();
    let mut index = 0;
    while index < stripped.len() {
        if !is_test_cfg_attr(stripped[index]) {
            out.push((index, stripped[index].to_owned()));
            index += 1;
            continue;
        }
        // Same-line item: the attribute line holds the whole item
        // (`mod foo;`). Exempt only this line.
        let after_bracket = stripped[index]
            .trim_start()
            .strip_prefix("#[cfg(")
            .and_then(|rest| rest.rsplit_once(']').map(|(_, tail)| tail))
            .unwrap_or("")
            .trim();
        if !after_bracket.is_empty() && !after_bracket.starts_with('{') {
            index += 1;
            continue;
        }
        let mut depth = 0i32;
        let mut opened = false;
        let mut cursor = index;
        while cursor < stripped.len() {
            let line = stripped[cursor];
            depth += line.chars().filter(|&c| c == '{').count() as i32;
            depth -= line.chars().filter(|&c| c == '}').count() as i32;
            opened |= depth > 0;
            cursor += 1;
            if opened && depth == 0 {
                break;
            }
        }
        if opened && depth == 0 {
            index = cursor;
        } else if !opened {
            // Braceless next-line item (`mod foo;` on its own line):
            // the attribute and its single line are the whole region.
            index += 2;
        } else {
            // Unbalanced: fail closed, scan from the attribute onward.
            index += 1;
        }
    }
    out
}

/// A production module that merely mentions tests in its name is
/// scanned: the old substring rule would have exempted
/// `support_auth.rs` outright, hiding a mint from the gate.
#[test]
fn test_marked_production_names_are_scanned() {
    assert!(!is_test_code("crates/wyrd-core/src/support_auth.rs"));
    assert!(!is_test_code("crates/wyrd-core/src/live.rs"));
    assert!(!is_test_code("crates/wyrd-sync/src/durable/facts.rs"));
}

/// Location says test-only: the contracts suite, `tests/`
/// directories, and test-named files. A new test layout that
/// matches none of these is scanned until the list grows
/// deliberately — fail closed, not silent.
#[test]
fn test_code_is_directories_and_file_names() {
    for exempt in [
        "crates/wyrd-contracts/src/deps_contracts.rs",
        "crates/wyrd-cli/tests/bootstrap.rs",
        "crates/wyrd-core/src/live/tests_trigger.rs",
        "crates/wyrd-sync/src/durable/codec/tests_codec.rs",
        "crates/wyrd-sync/src/authorization/test_util.rs",
        "crates/wyrd-sync/src/keys/capability/tests.rs",
    ] {
        assert!(is_test_code(exempt), "{exempt} is test code");
    }
}

/// `#[cfg(test)]` regions inside scanned files are exempt by
/// content: a mint in a production file's test module never trips
/// the gate, while the same line outside the region still does.
#[test]
fn cfg_test_regions_are_exempt_by_content() {
    let text = "use crate::view::Head;\n\
                \n\
                /// Production mint.\n\
                pub fn mint() -> Head {\n\
                \x20   Head::new(authorized())\n\
                }\n\
                \n\
                #[cfg(test)]\n\
                mod tests {\n\
                \x20   #[test]\n\
                \x20   fn wraps() {\n\
                \x20       let _ = Head::new(authorized());\n\
                \x20   }\n\
                }\n";
    let live: Vec<String> = live_lines(text).into_iter().map(|(_, line)| line).collect();
    assert_eq!(
        live.iter()
            .filter(|line| line.contains("Head::new"))
            .count(),
        1,
        "only the production mint survives: {live:?}"
    );
}

/// An unbalanced `#[cfg(test)]` region fails closed: lines from the
/// attribute onward are scanned, never silently exempted.
#[test]
fn unbalanced_cfg_test_region_is_scanned() {
    let text = "#[cfg(test)]\nmod tests {\n    let _ = Head::new(authorized());\n";
    let live: Vec<String> = live_lines(text).into_iter().map(|(_, line)| line).collect();
    assert!(
        live.iter().any(|line| line.contains("Head::new")),
        "the unbalanced region is scanned: {live:?}"
    );
}

/// `#[cfg(all(test, ...))]` still requires `test`, so its region is
/// test-only: a mint there never trips the gate.
#[test]
fn cfg_all_test_regions_are_exempt() {
    let text = "#[cfg(all(test, feature = \"mount\"))]\nmod tests_mount {\n    let _ = Head::new(authorized());\n}\nlet _ = Head::new(authorized());\n";
    let live: Vec<String> = live_lines(text).into_iter().map(|(_, line)| line).collect();
    assert_eq!(
        live.iter()
            .filter(|line| line.contains("Head::new"))
            .count(),
        1,
        "only the production mint survives: {live:?}"
    );
}

/// `#[cfg(any(test, feature = ...))]` ships when the feature holds,
/// so its region is scanned: a mint there trips the gate by design.
#[test]
fn cfg_any_test_feature_regions_are_scanned() {
    let text = "#[cfg(any(test, feature = \"insecure-fast-kdf\"))]\nconst M: u32 = 8;\nlet _ = Head::new(authorized());\n";
    let live: Vec<String> = live_lines(text).into_iter().map(|(_, line)| line).collect();
    assert!(
        live.iter().any(|line| line.contains("Head::new")),
        "the any-gated region is scanned: {live:?}"
    );
    assert!(
        !is_test_cfg_attr("#[cfg(any(test, feature = \"insecure-fast-kdf\"))]"),
        "any(test, feature) is not test-only"
    );
    assert!(
        !is_test_cfg_attr("#[cfg(not(test))]"),
        "not(test) is not test-only"
    );
    assert!(
        is_test_cfg_attr("#[cfg(all(test, feature = \"mount\"))]"),
        "all(test, ..) is test-only"
    );
}

/// A same-line gated item exempts only its own line: the following
/// production line is still scanned.
#[test]
fn same_line_cfg_attr_exempts_only_its_line() {
    let text = "#[cfg(test)] mod foo;\nlet _ = Head::new(authorized());\n";
    let live: Vec<String> = live_lines(text).into_iter().map(|(_, line)| line).collect();
    assert!(
        live.iter().any(|line| line.contains("Head::new")),
        "the production line after a same-line gate is scanned: {live:?}"
    );
}

/// `//` inside a string literal never hides a mint: matching runs on
/// lexed lines, so the URL's slashes are already blanked.
#[test]
fn url_before_mint_is_scanned() {
    let text = "let base = \"http://x\"; let h = Head::new(authorized());\n";
    let live: Vec<String> = live_lines(text).into_iter().map(|(_, line)| line).collect();
    assert!(
        live.iter().any(|line| line.contains("Head::new")),
        "the mint after a URL is scanned: {live:?}"
    );
}

/// A block comment naming a marker never trips the gate: comments
/// are blanked before matching.
#[test]
fn block_comment_marker_does_not_trip() {
    let text = "/* Head::new is forbidden */\nlet x = 1;\n";
    let live: Vec<String> = live_lines(text).into_iter().map(|(_, line)| line).collect();
    assert!(
        !live.iter().any(|line| line.contains("Head::new")),
        "the block comment is not a mint: {live:?}"
    );
}

/// The pinned test-file list matches the tree: a new test-named file
/// outside the contracts suite is scanned until it is added here
/// deliberately (fail closed).
#[test]
fn pinned_test_files_match_tree() {
    let root = workspace_root();
    let mut found: Vec<String> = Vec::new();
    visit_sources(&root, &mut |path, _| {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if rel.starts_with("crates/wyrd-contracts/") {
            return;
        }
        if rel.split('/').any(|c| c == "tests") {
            return;
        }
        let file = rel.rsplit('/').next().unwrap_or(&rel);
        if file == "tests.rs" || file.starts_with("tests_") || file.starts_with("test_") {
            found.push(rel);
        }
    });
    found.sort();
    let pinned: Vec<String> = TEST_FILE_PINS.iter().map(|s| s.to_string()).collect();
    assert!(!pinned.is_empty(), "the pin list is live");
    assert_eq!(
        found, pinned,
        "test-named files drifted: add deliberately here, never silently"
    );
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
