//! Receive-path retention admission gate (DG-4 A) acceptance tests.
//!
//! Four properties, each pinned separately:
//!
//! 1. Over-quota remote content is refused before storage: nothing
//!    lands in the vault or the object store, no fact commits, no
//!    provider claim is recorded, no serving request is satisfied, no
//!    announcement advances on its account.
//! 2. The refusal survives restart: the durable ledger preserves the
//!    decision and a reopened engine with the same ceiling still
//!    refuses without storing.
//! 3. A refusal is never reported as loss: the sync report counts it
//!    separately from missing/invalid/transport/local, it never
//!    strikes a representation, and it never completes a terminal
//!    verdict (so it never enters loss/repair as lost or corrupt).
//! 4. Vault orphans charge the gate: the open-time vault-dir walk
//!    seeds the vault-seeing counter, so torn-authoring envelopes with
//!    no naming fact still count.
//!
//! DG-4 B (residency refusal) is separately tracked and explicitly out
//! of scope here: these tests pin admission refusal only.

use crate::bulk::MemoryBulkSource;
use crate::keys::EpochSecret;
use crate::membership::test_util::Builder;
use crate::runtime::fetch::FetchOutcome;
use crate::runtime::test_util::{
    admit_engine, fixture, intake_body, intake_published, publish_into, reopen, AnnouncedRoots,
};
use crate::runtime::MaterializationState;
use wyrd_format::{MemoryObjectStore, ObjectStore, RetainedBytes};

/// Build the converged-fetch harness: one admitted engine, one
/// published snapshot with a pinned chunk, intake drained. Returns the
/// fixture, the bulk peer holding the bytes, and the published chunk
/// content id. Mirrors `plan_executes_to_convergence` without asserting
/// convergence, so gate tests share the honest-publisher setup.
fn overdue_harness() -> (
    crate::runtime::test_util::Fixture,
    MemoryBulkSource,
    wyrd_format::ContentId,
) {
    let mut fixture = fixture();
    let device = fixture.recipient;
    let (mut builder, genesis) = Builder::genesis(10);
    let admission = admit_engine(&mut builder, device);
    let epoch_secret = EpochSecret::from_bytes([0x09; 32]);
    let mut bulk = MemoryBulkSource::default();
    let body = intake_body(&builder, &admission);
    let published = publish_into(
        &mut bulk,
        &epoch_secret,
        2,
        &epoch_secret,
        2,
        body.snapshot_id(),
        b"hello wyrd",
    );
    let _body = intake_published(
        &mut fixture,
        &mut bulk,
        &builder,
        &genesis,
        &admission,
        vec![EpochSecret::from_bytes([0x08; 32]), epoch_secret.clone()],
        AnnouncedRoots {
            manifest: published.root_manifest,
            transport: published.root_transport,
        },
    );
    fixture
        .engine
        .set_materialization(published.content, MaterializationState::Pinned)
        .unwrap();
    (fixture, bulk, published.content)
}

/// Install a 1-byte ceiling with a fresh accountant: every
/// representation carries bytes, so every admission leg refuses.
fn arm_one_byte_gate(
    fixture: &mut crate::runtime::test_util::Fixture,
) -> std::sync::Arc<RetainedBytes> {
    let retained = RetainedBytes::new();
    fixture
        .engine
        .set_retention_gate(Some(1), Some(std::sync::Arc::clone(&retained)));
    retained
}

#[test]
fn a_receive_side_refusal_stores_nothing_and_claims_nothing() {
    let (mut fixture, mut bulk, content) = overdue_harness();
    let _retained = arm_one_byte_gate(&mut fixture);
    let mut objects = MemoryObjectStore::default();
    let report = fixture
        .engine
        .execute_plan(&mut bulk, &mut objects)
        .unwrap();
    // Refused, never fulfilled: no manifests, no bodies, no objects.
    assert!(
        report.refused > 0,
        "over-quota fetch must refuse: {report:?}"
    );
    assert_eq!(
        report.manifests, 0,
        "refused manifests never record: {report:?}"
    );
    assert_eq!(
        report.snapshot_bodies, 0,
        "refused bodies never record: {report:?}"
    );
    assert_eq!(report.objects, 0, "refused objects never land: {report:?}");
    assert!(
        report.unfulfilled > 0,
        "refused items stay pending: {report:?}"
    );
    // Nothing stored anywhere: no plaintext, no vault bytes.
    assert!(
        !objects.has(&content).unwrap(),
        "refused bytes never reach the store"
    );
    assert_eq!(
        fixture.engine.vault().seen_bytes(),
        0,
        "refused bytes never reach the vault counter"
    );
    assert_eq!(
        fixture.engine.vault().resident_bytes().unwrap(),
        0,
        "refused bytes never reach the vault directory"
    );
    // No durable claim: no manifest, body, or object fact committed.
    let facts = fixture.engine.store.load().expect("loads");
    assert!(facts.manifests.is_empty(), "no provider claim is recorded");
    assert!(facts.snapshot_bodies.is_empty(), "no body is recorded");
    assert!(facts.local_objects.is_empty(), "no object is marked local");
    // The ledger names the refusal: restart preserves the decision.
    assert!(fixture.engine.refusal_count() > 0, "refusals are recorded");
}

#[test]
fn a_refusal_is_durable_across_restart() {
    let (mut fixture, mut bulk, _content) = overdue_harness();
    let _retained = arm_one_byte_gate(&mut fixture);
    let mut objects = MemoryObjectStore::default();
    let first = fixture
        .engine
        .execute_plan(&mut bulk, &mut objects)
        .unwrap();
    assert!(first.refused > 0, "first pass refuses: {first:?}");
    let units = fixture.engine.refused_units();
    assert!(!units.is_empty(), "ledger holds refused units");
    // Simulated restart: fresh engine over the same directory, same
    // ceiling reinstalled by the composer. The ledger reloads from
    // disk; the gate still refuses without storing.
    let mut reopened = reopen(&mut fixture);
    reopened.set_retention_gate(Some(1), Some(RetainedBytes::new()));
    assert_eq!(
        reopened.refusal_count(),
        units.len(),
        "the ledger survives restart: refused units are reloaded, not forgotten"
    );
    assert_eq!(reopened.refused_units(), units);
    let mut objects = MemoryObjectStore::default();
    let second = reopened.execute_plan(&mut bulk, &mut objects).unwrap();
    assert!(
        second.refused > 0,
        "reopened engine still refuses: {second:?}"
    );
    assert_eq!(second.manifests, 0);
    assert_eq!(second.objects, 0);
    let facts = reopened.store.load().expect("loads");
    assert!(
        facts.manifests.is_empty(),
        "restart turns no refusal into a claim"
    );
    assert!(facts.local_objects.is_empty());
}

#[test]
fn a_refusal_is_not_reported_as_content_loss() {
    let (mut fixture, mut bulk, content) = overdue_harness();
    let _retained = arm_one_byte_gate(&mut fixture);
    let mut objects = MemoryObjectStore::default();
    let report = fixture
        .engine
        .execute_plan(&mut bulk, &mut objects)
        .unwrap();
    assert!(report.refused > 0, "refusals are counted: {report:?}");
    // Distinct from every other class: refused is not missing,
    // invalid, transport trouble, or a local failure — and never a
    // fatal store condition (the pass completes).
    assert_eq!(report.invalid, 0, "refused is not corruption: {report:?}");
    assert_eq!(
        report.local_failures, 0,
        "refused is not a local failure: {report:?}"
    );
    assert_eq!(
        report.transport_errors, 0,
        "refused is not transport trouble: {report:?}"
    );
    // Never struck, never terminal: refused representations stay
    // eligible and block terminality instead of completing a loss or
    // corruption verdict that would enter repair.
    assert!(
        fixture.engine.terminal_snapshot().is_empty(),
        "refusal never completes a terminal verdict"
    );
    let runtime = fixture.engine.runtime_state().unwrap();
    assert!(
        !runtime.is_local(&content),
        "refused content is never marked local"
    );
    // The settled projection (dead code today, FUSE tomorrow) must
    // never resolve a refusal to corrupt: corrupt enters scrub and
    // quarantine, and refusal must never enter loss/repair as lost.
    assert!(!matches!(
        FetchOutcome::<()>::Refused.settled(1),
        wyrd_format::FetchStatus::Corrupt
    ));
}

#[test]
fn vault_orphans_are_charged_against_the_gate() {
    use crate::runtime::test_util::TestDir;
    use crate::serving::Vault;

    // An orphan with no naming fact: bytes land in the vault (torn
    // authoring, retried import) but no manifest or object fact ever
    // commits them.
    let dir = TestDir::new("retention-orphans");
    let vault = Vault::open(&dir.path).unwrap();
    assert_eq!(vault.seen_bytes(), 0);
    let orphan = vec![0xA5; 1024];
    vault.import(&orphan).unwrap();
    assert_eq!(vault.seen_bytes(), 1024);
    // Reopen re-seeds from disk: the orphan still charges the gate
    // even though no fact names it. This is the crash-matrix half —
    // bytes written before a crash count after it.
    let reopened = Vault::open(&dir.path).unwrap();
    assert_eq!(
        reopened.seen_bytes(),
        1024,
        "the open-time walk charges orphans: torn authoring cannot hide retained bytes"
    );
    assert_eq!(reopened.resident_bytes().unwrap(), 1024);
    // Scratch debris is not retention: crashed-import `.tmp-*` files
    // are excluded from the seed like the observational report.
    std::fs::write(dir.path.join("vault").join(".tmp-crashed"), vec![0x5A; 512]).unwrap();
    let reseeded = Vault::open(&dir.path).unwrap();
    assert_eq!(
        reseeded.seen_bytes(),
        1024,
        "import scratch is not retained content"
    );
    // A gate over the seeded counter refuses: orphans consume the same
    // headroom fresh imports would.
    let retained = RetainedBytes::new();
    let gate = crate::runtime::retention::RetentionGate::new(
        Some(1025),
        Some(retained),
        reopened.seen_handle(),
    );
    assert!(gate.check(0, 0).is_ok(), "under the ceiling admits");
    assert!(
        gate.check(1, 0).is_err(),
        "orphans spend the ceiling: 1024 + 1 >= 1025"
    );
}

/// A retry of already-stored bytes admits as a no-op instead of
/// refusing: both stores charge only fresh bytes, so re-charging
/// `incoming` would double-count — including the torn-import retry
/// whose held-root path heals directory durability. The fresh-bytes
/// control below proves the same gate refuses what is not held.
#[test]
fn an_already_held_object_admits_despite_the_ceiling() {
    use crate::keys::capability::{Capability, DriveKeyring};
    use crate::keys::{DeviceEncryptionSecret, EpochSecret};
    use crate::membership::test_util::{drive as member_drive, Builder};
    use crate::membership::MembershipLog;
    use crate::runtime::fetch::object;
    use crate::runtime::retention::{RefusalLedger, RetentionGate};
    use crate::runtime::test_util::{encryption_key, TestDir};
    use crate::runtime::PendingObjectFetch;
    use crate::seal::{seal, SEAL_VERSION};
    use wyrd_format::membership::Admission;
    use wyrd_format::{Change, DeviceId, ObjectKind};

    let epoch = 2;
    let device = DeviceId::from_bytes([0x2A; 32]);
    let encryption = encryption_key(&DeviceEncryptionSecret::from_bytes([0xE1; 32]).unwrap());
    let (mut builder, genesis) = Builder::genesis(10);
    let admit = builder.child(vec![Change::Admit(Admission {
        device,
        encryption_key: encryption,
    })]);
    let mut log = MembershipLog::new(member_drive());
    log.observe(genesis);
    log.observe(admit.clone());
    let secret = EpochSecret::from_bytes([0x77; 32]);
    let first = EpochSecret::from_bytes([0x76; 32]);
    let cap = Capability::new(
        member_drive(),
        device,
        encryption,
        admit.transition_id(),
        epoch,
        vec![first, secret],
    )
    .unwrap();
    let mut keyring = DriveKeyring::new(member_drive(), device);
    keyring.install(&cap, &log).unwrap();

    let plaintext = b"already stored";
    let content = wyrd_format::ContentId::derive(ObjectKind::Chunk, plaintext);
    let key = keyring.secret(epoch).expect("epoch secret").object_key(
        &member_drive(),
        epoch,
        &content,
        ObjectKind::Chunk,
        SEAL_VERSION,
    );
    let sealed = seal(&key, ObjectKind::Chunk, &content, plaintext).unwrap();
    let candidate = PendingObjectFetch {
        content_id: content,
        storage_id: sealed.storage_id(),
        transport: crate::seal::transport_root(&sealed),
        kind: ObjectKind::Chunk,
        version: SEAL_VERSION,
        encryption_epoch: epoch,
        size: plaintext.len() as u64,
    };
    let mut bulk = MemoryBulkSource::default();
    bulk.publish_sealed(sealed.storage_id(), sealed.encode());
    bulk.publish_transport(sealed.encode());

    // A 1-byte ceiling refuses every fresh representation.
    let dir = TestDir::new("retention-held-admits");
    let vault = crate::serving::Vault::open(&dir.path).unwrap();
    let retained = RetainedBytes::new();
    let gate = RetentionGate::new(
        Some(1),
        Some(std::sync::Arc::clone(&retained)),
        vault.seen_handle(),
    );
    let refusals = RefusalLedger::load(&dir.path).unwrap();

    // Control: fresh bytes refuse — nothing stored, nothing charged.
    let mut fresh_objects = MemoryObjectStore::default();
    let attempt = object(
        &member_drive(),
        &mut bulk,
        &keyring,
        &mut fresh_objects,
        &vault,
        &content,
        std::slice::from_ref(&candidate),
        &gate,
        &refusals,
    );
    assert!(
        matches!(attempt.aggregate, FetchOutcome::Refused),
        "fresh bytes over the ceiling refuse: {:?}",
        attempt.aggregate
    );

    // The same bytes, already stored in both dimensions, admit as a
    // no-op: the import and the insert below store nothing new.
    vault.import(&sealed.encode()).unwrap();
    let mut held_objects = MemoryObjectStore::default();
    held_objects
        .insert_verified(ObjectKind::Chunk, &content, plaintext)
        .unwrap();
    let attempt = object(
        &member_drive(),
        &mut bulk,
        &keyring,
        &mut held_objects,
        &vault,
        &content,
        std::slice::from_ref(&candidate),
        &gate,
        &refusals,
    );
    assert!(
        matches!(attempt.aggregate, FetchOutcome::Fulfilled(())),
        "already-held bytes admit despite the ceiling: {:?}",
        attempt.aggregate
    );
}
