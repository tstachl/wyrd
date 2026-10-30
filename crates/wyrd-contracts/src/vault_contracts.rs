//! The vault-facing contracts: content-id invisibility and manifest
//! rejection.

use wyrd_format::{
    Entry, FetchStatus, MemoryObjectStore, ObjectKind, ObjectStore, Snapshot, TransitionId, Tree,
};
use wyrd_fuse::{DriveView, ViewError};
use wyrd_sync::bulk::{BulkSource, MemoryBulkSource, SealedManifest};
use wyrd_sync::control::{sign_announcement, SnapshotAnnouncement};
use wyrd_sync::public_read::{
    fetch_public_object, fetch_public_snapshot, verify_public_announcement, PublicRelease,
    PUBLIC_RELEASE_LEN,
};
use wyrd_sync::seal::{self, SEAL_VERSION};

use crate::support::{
    device, drive, mount_heads, seal_flat_drive, signed_snapshot, Loaded, RemoteOnlyMaterialization,
};
use wyrd_sync::keys::EpochSecret;

/// The structural vault boundary: vault-visible records are
/// addressed by StorageId and carry AEAD bytes; only the member-only
/// key path (`open_manifest`, `verify`) recovers ContentIds, and each
/// object verifies against its own manifest entry. The byte-window
/// scan below is defense-in-depth — a heuristic, not a proof, since a
/// transformed encoding could leak a ContentId in split form. It
/// includes the manifest id even though the root-manifest record's
/// fetch address is member-visible by design.
#[test]
fn content_ids_never_appear_in_vault_transport_records() {
    let drive = drive();
    let epoch1 = EpochSecret::from_bytes([0x51; 32]);
    let snapshot_id = wyrd_format::SnapshotId::from_bytes([0x5A; 32]);
    let content = crate::support::seal_flat_drive(
        &drive,
        &epoch1,
        1,
        &snapshot_id,
        &[("secret.txt", b"vault payload")],
    );

    let mut ids = vec![content.manifest_id];
    ids.extend(content.content_ids.iter().copied());
    let records = std::iter::once(&content.root.sealed)
        .chain(content.objects.iter().map(|(_, sealed)| sealed));
    for record in records {
        for id in &ids {
            assert!(
                !windows_contains(record, id.as_bytes()),
                "a ContentId leaked into a vault-visible record"
            );
        }
    }

    // The records are real, not noise: the manifest opens under the
    // member-only key, and every object verifies against its entry at
    // the storage address it was published under.
    let manifest_obj = seal::EncryptedObject::decode(&content.root.sealed).unwrap();
    let manifest = seal::open_manifest(
        &epoch1.manifest_key(&drive, 1, &snapshot_id),
        &content.manifest_id,
        &manifest_obj,
    )
    .unwrap();
    assert_eq!(manifest.snapshot(), snapshot_id);
    for entry in manifest.entries() {
        let (_, sealed_bytes) = content
            .objects
            .iter()
            .find(|(storage, _)| *storage == entry.storage_id)
            .expect("every manifest entry has its sealed record");
        let key = epoch1.object_key(
            &drive,
            entry.encryption_epoch,
            &entry.content_id,
            entry.kind,
            SEAL_VERSION,
        );
        let plaintext = seal::verify(entry, &key, sealed_bytes).unwrap();
        assert_eq!(
            wyrd_format::ContentId::derive(entry.kind, &plaintext),
            entry.content_id,
            "the verified record names exactly its content"
        );
    }
}

/// A malformed manifest cannot become materialized content: the
/// tampered root is rejected on arrival, nothing is marked local, and
/// the view serves nothing — while the compliant manifest, arriving
/// later through the same path, materializes normally and reads.
#[test]
fn malformed_manifests_never_become_materialized_content() {
    let mut loaded = Loaded::new("guarded.txt", b"guarded body");
    let snapshot_id = loaded.snapshot.snapshot_id();
    loaded.publish_body_and_announcement(None);
    let report = loaded.drain();
    assert_eq!(report.accepted, 2, "capability and announcement");

    // One flipped bit inside the sealed manifest: the AEAD tag fails
    // and the record is invalid remote data, not a transport fault.
    let mut tampered = loaded.content.root.sealed.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    loaded.bulk.publish_root(
        snapshot_id,
        SealedManifest {
            content_id: loaded.content.manifest_id,
            sealed: tampered,
        },
    );

    let mut engine = loaded.rig.take_engine();
    let report = engine
        .execute_plan(&mut loaded.bulk, &mut loaded.objects)
        .unwrap();
    assert_eq!(report.manifests, 0, "the tampered root never commits");
    assert!(
        report.invalid >= 1,
        "the tampered root is invalid remote data (retries may repeat it)"
    );
    let runtime = engine.runtime_state().unwrap();
    for id in &loaded.content.content_ids {
        assert_eq!(
            runtime.status(id),
            FetchStatus::RemoteOnly,
            "a rejected manifest materializes nothing"
        );
    }
    // The view agrees: the head exists (the body was verified), but
    // nothing behind it is local, so reads report the gap instead of
    // serving.
    let heads = engine.live_heads().unwrap();
    assert_eq!(heads.len(), 1);
    let view = DriveView::new(
        loaded.objects.clone(),
        RemoteOnlyMaterialization,
        mount_heads(heads),
    );
    assert!(
        matches!(
            view.lookup("guarded.txt"),
            Err(ViewError::NotMaterialized { .. })
        ),
        "nothing behind the rejected manifest is served"
    );

    // The compliant manifest materializes through the same path.
    loaded.publish_all();
    loaded.want_all(&mut engine);
    let report = engine
        .execute_plan(&mut loaded.bulk, &mut loaded.objects)
        .unwrap();
    assert_eq!(report.manifests, 1);
    assert_eq!(report.objects, 2, "the tree and the chunk");
    let heads = engine.live_heads().unwrap();
    let view = DriveView::new(
        loaded.objects,
        RemoteOnlyMaterialization,
        mount_heads(heads),
    );
    let node = view.lookup("guarded.txt").unwrap();
    let file = view.open(&node).unwrap();
    assert_eq!(view.read(&file, 0, 64).unwrap(), b"guarded body");
    loaded.rig.teardown();
}

fn windows_contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Issue 33 (OD-11, decided 2026-10-21 option A): the disclosure
/// promise scoped by trust position. Three positions, one body:
///
/// - keyless replica: admissible inputs are the sealed vault records
///   only — StorageId-addressed AEAD bytes, never the plaintext body.
///   None of the body's clear observations (authorship H5, ContentIds
///   H6, membership H8, drive H7) appears in them.
/// - member-hosted composition: the same sealed records open under
///   the member's epoch keys, and the body verifies — the process
///   holds the observations as a member, not as vault disclosure.
/// - public release drive: a genesis-scoped maintainer snapshot under
///   a published epoch-1 secret, with the announcement verifying in
///   the clear and the frozen reader proving the waiver end to end.
///
/// The test uses a real encoded snapshot body throughout, and it
/// compares each position's observations against the matrix — it is
/// not satisfiable by merely showing that a body decodes.
#[test]
fn vault_disclosure_matches_the_agreed_trust_position() {
    // A real drive and its real encoded body.
    let loaded = Loaded::new("disclosure.txt", b"disclosure payload");
    let drive = drive();
    let body = loaded.snapshot.encode();
    let decoded = Snapshot::decode(&body).expect("the body is a real snapshot body");
    assert_eq!(decoded, loaded.snapshot);

    // The body's clear observations: authorship (H5), the tree and
    // snapshot identity (H6), the membership binding (H8), the epoch.
    let snapshot_id = loaded.snapshot.snapshot_id();
    let author = loaded.snapshot.author;
    let membership = loaded.snapshot.membership;
    let tree = loaded.snapshot.tree;
    assert_eq!(decoded.author, author);
    assert_eq!(decoded.membership, membership);
    assert_eq!(decoded.tree, tree);

    // Keyless-replica position: the admissible inputs are the sealed
    // records. The plaintext body is not among them — the boundary is
    // input selection, not decoding — and none of the body's clear
    // observations appears in them, byte-window or otherwise.
    let sealed: Vec<&[u8]> = std::iter::once(loaded.content.root.sealed.as_slice())
        .chain(
            loaded
                .content
                .objects
                .iter()
                .map(|(_, sealed)| sealed.as_slice()),
        )
        .collect();
    for record in &sealed {
        assert_ne!(
            *record,
            body.as_slice(),
            "the plaintext body is not a vault record: keyless admissibility excludes it"
        );
    }
    let manifest_id = loaded.content.manifest_id;
    let probes: Vec<&[u8]> = std::iter::once(manifest_id.as_bytes().as_slice())
        .chain(
            loaded
                .content
                .content_ids
                .iter()
                .map(|id| id.as_bytes().as_slice()),
        )
        .chain([
            snapshot_id.as_bytes().as_slice(),
            tree.as_bytes().as_slice(),
            author.as_bytes().as_slice(),
            membership.as_bytes().as_slice(),
            drive.as_bytes().as_slice(),
        ])
        .collect();
    for record in &sealed {
        for probe in &probes {
            assert!(
                !windows_contains(record, probe),
                "a member observation leaked into a keyless-admissible vault record"
            );
        }
    }

    // Member-hosted position: the same sealed records open under the
    // member's epoch keys. The manifest opens (H4 to members) and an
    // object verifies to its plaintext (H1 to members).
    let epoch2 = loaded.rig.epoch2.clone();
    let manifest_obj = seal::EncryptedObject::decode(&loaded.content.root.sealed).unwrap();
    let manifest = seal::open_manifest(
        &epoch2.manifest_key(&drive, 2, &snapshot_id),
        &manifest_id,
        &manifest_obj,
    )
    .unwrap();
    assert_eq!(manifest.snapshot(), snapshot_id);
    let chunk_entry = manifest
        .entries()
        .iter()
        .find(|entry| entry.kind == ObjectKind::Chunk)
        .expect("the manifest names its chunk")
        .clone();
    let (_, sealed_bytes) = loaded
        .content
        .objects
        .iter()
        .find(|(storage, _)| *storage == chunk_entry.storage_id)
        .expect("every manifest entry has its sealed record");
    let key = epoch2.object_key(
        &drive,
        chunk_entry.encryption_epoch,
        &chunk_entry.content_id,
        chunk_entry.kind,
        SEAL_VERSION,
    );
    let plaintext = seal::verify(&chunk_entry, &key, sealed_bytes).unwrap();
    assert_eq!(plaintext, b"disclosure payload");

    // Public release-drive position: a genesis-scoped maintainer
    // snapshot under a published epoch-1 secret. H7 and H8 arrive in
    // the cleartext release metadata; the announcement verifies in
    // the clear with no mailbox, no capability, and no membership
    // replay (H9 partial by construction); the frozen reader proves
    // the H1-H6 waiver end to end; and the metadata carries no escrow
    // record (H10 never).
    let maintainer = device(0x07);
    let genesis = TransitionId::from_bytes([0x13; 32]);
    let release_secret = EpochSecret::from_bytes([0x17; 32]);
    let release = PublicRelease::new(drive, maintainer.id, genesis, release_secret.clone())
        .expect("fixture release metadata is well-formed");
    let meta = release.encode();
    assert_eq!(meta.len(), PUBLIC_RELEASE_LEN);
    assert_eq!(
        &meta[1..33],
        drive.as_bytes(),
        "the release metadata names its drive in the clear (H7)"
    );
    assert_eq!(
        &meta[65..97],
        genesis.as_bytes(),
        "the release metadata names its genesis in the clear (H8)"
    );
    let mut scratch = MemoryObjectStore::default();
    let chunk = scratch.insert(ObjectKind::Chunk, b"release bytes").unwrap();
    let release_tree = Tree::from_entries(vec![
        Entry::file("release.txt", 13, false, vec![chunk]).unwrap()
    ])
    .unwrap()
    .insert_into(&mut scratch)
    .unwrap();
    let release_snapshot = signed_snapshot(Vec::new(), release_tree, &maintainer, genesis, 1, 1);
    assert_eq!(release_snapshot.author, maintainer.id);
    assert_eq!(release_snapshot.membership, genesis);
    assert_eq!(release_snapshot.epoch, 1);
    let release_id = release_snapshot.snapshot_id();
    let release_content = seal_flat_drive(
        &drive,
        &release_secret,
        1,
        &release_id,
        &[("release.txt", b"release bytes")],
    );
    let release_body = release_snapshot.encode();
    let mut announcement = SnapshotAnnouncement {
        snapshot: release_id,
        author: maintainer.id,
        epoch: 1,
        membership: genesis,
        body_root: wyrd_sync::public_read::transport_root(&release_body),
        root_manifest: release_content.manifest_id,
        root_manifest_transport: wyrd_sync::public_read::transport_root(
            &release_content.root.sealed,
        ),
        node_addr: None,
        signature: [0; 64],
    };
    sign_announcement(&mut announcement, &maintainer.identity, &drive);
    verify_public_announcement(&release, &announcement)
        .expect("the clear announcement verifies with no mailbox behind it");
    let mut source = MemoryBulkSource::default();
    source.publish_transport(release_body.clone());
    source.publish_transport(release_content.root.sealed.clone());
    for (_, sealed) in &release_content.objects {
        source.publish_transport(sealed.clone());
    }
    // O1: transport-root addressing resolves the body.
    let by_root = source
        .fetch_transport(
            &wyrd_sync::public_read::transport_root(&release_body),
            1 << 20,
        )
        .unwrap();
    assert_eq!(by_root, Some(release_body.clone()));
    let verified = fetch_public_snapshot(&release, &announcement, &mut source, 1 << 20)
        .expect("the frozen reader verifies the waived drive");
    assert_eq!(verified.snapshot, release_snapshot);
    let release_entry = verified
        .manifest
        .entries()
        .iter()
        .find(|entry| entry.kind == ObjectKind::Chunk)
        .expect("the release manifest names its chunk")
        .clone();
    let release_plaintext = fetch_public_object(&release, &release_entry, &mut source, 1 << 20)
        .expect("the published secret opens the waived object");
    assert_eq!(release_plaintext, b"release bytes");

    loaded.rig.teardown();
}

/// Issue 33: the reconciled disclosure promise is stated in exactly
/// one normative place. `docs/architecture.md` invariant 4 owns the
/// sentence; `docs/contracts/trust.md` owns the per-position scope (the matrix,
/// including the public release-drive column); `docs/design/fetch-on-open.md`
/// names the serving exception it implements. The other two reference
/// the canonical sentence — they never restate a competing
/// vault-sees-or-does-not-see-ContentIds claim.
///
/// This one is a string test, and it is legitimate: the defect being
/// prevented is two normative documents making competing claims. (The
/// 2026-10-20 pre-release review quotes the old absolute sentence as
/// evidence; it is historical, not normative, and is excluded from
/// the count for exactly that reason.)
#[test]
fn the_disclosure_contract_is_stated_in_exactly_one_place() {
    let docs = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    let architecture =
        std::fs::read_to_string(docs.join("architecture.md")).expect("architecture.md reads");
    let trust = std::fs::read_to_string(docs.join("trust.md")).expect("trust.md reads");
    let fetch =
        std::fs::read_to_string(docs.join("fetch-on-open.md")).expect("fetch-on-open.md reads");

    // The canonical sentence lives in architecture.md, exactly once,
    // and in no other normative document.
    assert_eq!(
        architecture.matches("never reach vaults").count(),
        1,
        "the canonical disclosure sentence appears once, in architecture.md"
    );
    assert_eq!(
        trust.matches("never reach vaults").count(),
        0,
        "trust.md references the canonical sentence, never restates it"
    );
    assert_eq!(
        fetch.matches("never reach vaults").count(),
        0,
        "fetch-on-open.md references the canonical sentence, never restates it"
    );

    // The canonical sentence names its scope: the keyless position it
    // binds, the member-hosted position it excludes, and the release
    // waiver it permits.
    assert!(
        architecture.contains("keyless-replica"),
        "the canonical sentence scopes the keyless-replica position"
    );
    assert!(
        architecture.contains("member-hosted"),
        "the canonical sentence scopes the member-hosted position"
    );
    assert!(
        architecture.contains("release-read v1"),
        "the canonical sentence names the release waiver"
    );

    // trust.md owns the scope: the release-drive column exists, the
    // keyless H5/H6 verdicts stay never, and vault surfaces still
    // follow the keyless column on every composition.
    assert!(
        trust.contains("| Public release drive |"),
        "the matrix gains the public release-drive column"
    );
    assert!(
        trust.contains("| H5 | snapshot contents and authorship | never (guaranteed) |"),
        "the keyless-replica H5 guarantee stays"
    );
    assert!(
        trust.contains("| H6 | ContentIds | never (guaranteed, no equality oracle) |"),
        "the keyless-replica H6 guarantee stays"
    );
    assert!(
        trust.contains("maintainer authorship is public by construction"),
        "the release column states its H5 waiver"
    );
    assert!(
        trust.contains("follow the keyless-replica"),
        "vault surfaces still follow the keyless column on every composition"
    );

    // fetch-on-open.md cross-references the canonical rule instead of
    // restating a competing exception.
    assert!(
        fetch.contains("architecture.md") && fetch.contains("invariant 4"),
        "the serving exception points at the canonical rule"
    );
    assert!(
        fetch.contains("disclosure matrix"),
        "the serving exception points at the per-position scope"
    );
    assert!(
        fetch.contains("keyless-replica"),
        "the serving exception names the position it never reaches"
    );
}
