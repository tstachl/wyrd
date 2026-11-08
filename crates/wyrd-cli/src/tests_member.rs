use super::tests_harness::{write_secret, TempDir};
use super::*;
use wyrd_format::{ContentId, Entry, FsObjectStore, ObjectKind, ObjectStore, SnapshotId, Tree};
use wyrd_sync::runtime::EngineError;

struct Fixture {
    _temp: TempDir,
    drive: PathBuf,
    identity_file: PathBuf,
    passphrase_file: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new();
        let identity_file = temp.0.join("identity");
        let passphrase_file = temp.0.join("passphrase");
        let drive = temp.0.join("drive");
        write_secret(&identity_file, [0x22; 32]);
        write_secret(&passphrase_file, b"test-pass\n");
        command(vec![
            "init".into(),
            drive.display().to_string(),
            "--identity-file".into(),
            identity_file.display().to_string(),
            "--passphrase-file".into(),
            passphrase_file.display().to_string(),
        ])
        .unwrap();
        Fixture {
            _temp: temp,
            drive,
            identity_file,
            passphrase_file,
        }
    }

    fn args(&self, action: Vec<String>) -> Vec<String> {
        let mut args = vec![
            "member".into(),
            self.drive.display().to_string(),
            "--identity-file".into(),
            self.identity_file.display().to_string(),
            "--passphrase-file".into(),
            self.passphrase_file.display().to_string(),
        ];
        args.extend(action);
        args
    }

    fn open(&self) -> Engine {
        let identity = read_identity(&self.identity_file).unwrap();
        Engine::open_keystore(self.drive.clone(), "test-pass", identity).unwrap()
    }
}

/// The x-only pubkey a 32-byte secret names.
fn device_id_of(secret: &[u8; 32]) -> DeviceId {
    let keys = nostr::key::Keys::new(nostr::key::SecretKey::from_slice(secret).unwrap());
    DeviceId::from_bytes(*keys.public_key().as_bytes())
}

/// The encryption key a 32-byte secret names.
fn encryption_key_of(secret: &[u8; 32]) -> wyrd_format::DeviceEncryptionKey {
    let keys = nostr::key::Keys::new(nostr::key::SecretKey::from_slice(secret).unwrap());
    wyrd_format::DeviceEncryptionKey::from_bytes(*keys.public_key().as_bytes())
}

/// Admit a device through the CLI invite command: the member
/// commands under test administer existing membership, and admission
/// setup exercises the same invite surface a user drives.
fn admit(fixture: &Fixture, secret: &[u8; 32]) -> DeviceId {
    let id = device_id_of(secret);
    let key = encryption_key_of(secret);
    let out = fixture._temp.0.join(format!("invitation-{id}"));
    command(fixture.args(vec![
        "invite".into(),
        id.to_string(),
        key.to_string(),
        out.display().to_string(),
    ]))
    .unwrap();
    assert!(out.exists(), "invite writes the sealed invitation");
    id
}

/// Reads project a fresh drive: epoch 1, the owner alone, canonical
/// genesis, no frozen conflict, and the epoch-1 secret held.
#[test]
fn member_reports_describe_a_fresh_drive() {
    let fixture = Fixture::new();
    command(fixture.args(vec!["list".into()])).unwrap();
    command(fixture.args(vec!["log".into()])).unwrap();
    command(fixture.args(vec!["status".into()])).unwrap();

    let engine = fixture.open();
    let owner = device_id_of(&[0x22; 32]);
    let list = member_list_report(&engine).unwrap();
    assert!(list.contains("epoch 1"), "list names the epoch");
    assert!(
        list.contains(&format!("owner {owner}")),
        "list names the owner"
    );
    let log = member_log_report(&engine).unwrap();
    assert!(log.contains("epoch 1"), "log shows genesis");
    assert!(log.contains("canonical"), "genesis is canonical");
    let status = member_status_report(&engine).unwrap();
    assert!(status.contains("members 1 owners 1"), "one owner-member");
    assert!(status.contains("frozen: no"), "nothing frozen");
    assert!(status.contains("held secrets: 1"), "epoch-1 secret held");
    assert!(
        !status.contains("announcements pending for"),
        "fresh drive names no pending catch-up: {status}"
    );
}

/// Remove administrates membership: the device leaves at a new epoch
/// and vanishes from the list.
#[test]
fn member_remove_administers_membership() {
    let fixture = Fixture::new();
    let second = admit(&fixture, &[0x44; 32]);
    command(fixture.args(vec!["remove".into(), second.to_string()])).unwrap();

    let engine = fixture.open();
    let tip = engine.membership_log().known_state().expect("tip");
    assert_eq!(tip.epoch, 3, "admit plus removal");
    assert!(
        !engine
            .membership_log()
            .members_of(&tip.transition_id)
            .expect("members")
            .contains(&second),
        "removed device leaves"
    );
    assert!(
        !member_list_report(&engine)
            .unwrap()
            .contains(&second.to_string()),
        "list no longer names the removed device"
    );
}

/// Removing a stranger fails closed with the engine's reason.
#[test]
fn member_remove_unknown_is_refused() {
    let fixture = Fixture::new();
    let stranger = device_id_of(&[0x55; 32]);
    let error = command(fixture.args(vec!["remove".into(), stranger.to_string()])).unwrap_err();
    assert!(
        matches!(error, CliError::Engine(EngineError::NotMember)),
        "unexpected: {error:?}"
    );
}

/// A stranger's credentials fail at open: the keystore binds to its
/// owner's device identity, and joined devices carry their own
/// keystore (invite/join is a separate tracked issue). The engine's
/// owner-only authority stays covered by the sync tests and the
/// handover test below.
#[test]
fn member_open_by_non_owner_is_refused() {
    let fixture = Fixture::new();
    let second = admit(&fixture, &[0x44; 32]);
    let stranger_file = fixture._temp.0.join("stranger");
    write_secret(&stranger_file, [0x55; 32]);
    let error = command(vec![
        "member".into(),
        fixture.drive.display().to_string(),
        "--identity-file".into(),
        stranger_file.display().to_string(),
        "--passphrase-file".into(),
        fixture.passphrase_file.display().to_string(),
        "remove".into(),
        second.to_string(),
    ])
    .unwrap_err();
    assert!(
        matches!(error, CliError::Engine(EngineError::OwnerMismatch)),
        "unexpected: {error:?}"
    );
}

/// Last-owner removal is valid but terminal: without `--yes` the CLI
/// refuses to author it; with confirmation the owner set empties.
#[test]
fn member_remove_sole_owner_needs_confirmation() {
    let fixture = Fixture::new();
    let owner = device_id_of(&[0x22; 32]);
    let error = command(fixture.args(vec!["remove".into(), owner.to_string()])).unwrap_err();
    assert!(matches!(error, CliError::Usage(_)), "unexpected: {error:?}");

    command(fixture.args(vec!["remove".into(), owner.to_string(), "--yes".into()])).unwrap();
    let engine = fixture.open();
    let tip = engine.membership_log().known_state().expect("tip");
    assert!(
        engine
            .membership_log()
            .owners_of(&tip.transition_id)
            .expect("owners")
            .is_empty(),
        "owner set empties with the sole owner"
    );
}
/// Rotate bumps the epoch with membership untouched.
#[test]
fn member_rotate_bumps_epoch_keeps_membership() {
    let fixture = Fixture::new();
    let second = admit(&fixture, &[0x44; 32]);
    command(fixture.args(vec!["rotate".into()])).unwrap();

    let engine = fixture.open();
    let tip = engine.membership_log().known_state().expect("tip");
    assert_eq!(tip.epoch, 3, "admit plus rotation");
    let members = engine
        .membership_log()
        .members_of(&tip.transition_id)
        .expect("members");
    assert!(members.contains(&second), "rotation removes nobody");
    assert_eq!(members.len(), 2, "owner plus second");
}

/// Set-owner hands authority over: the old owner loses it on the
/// spot.
#[test]
fn member_set_owner_hands_authority_over() {
    let fixture = Fixture::new();
    let second = admit(&fixture, &[0x44; 32]);
    command(fixture.args(vec!["set-owner".into(), second.to_string()])).unwrap();

    {
        let engine = fixture.open();
        let tip = engine.membership_log().known_state().expect("tip");
        assert_eq!(tip.epoch, 3, "admit plus handover");
        let owners = engine
            .membership_log()
            .owners_of(&tip.transition_id)
            .expect("owners");
        assert_eq!(
            owners.iter().collect::<Vec<_>>(),
            [&second],
            "ownership moves"
        );
    }

    let third = device_id_of(&[0x66; 32]);
    let error = command(fixture.args(vec!["remove".into(), third.to_string()])).unwrap_err();
    assert!(
        matches!(error, CliError::Engine(EngineError::NotOwner)),
        "old owner lost authority: {error:?}"
    );
}

/// Rotate carries the namespace: a drive with files serves them at
/// the new epoch afterwards, and the carry extends the old tip.
#[test]
fn member_rotate_carries_files_forward() {
    let fixture = Fixture::new();
    let (tree, first) = write_file(&fixture, "kept.txt", b"kept");
    command(fixture.args(vec!["rotate".into()])).unwrap();

    let engine = fixture.open();
    let heads = engine.live_heads().unwrap();
    assert_eq!(heads.len(), 1, "the carry serves the files after rotate");
    assert_eq!(heads[0].snapshot().epoch, 2);
    assert_eq!(heads[0].snapshot().tree, tree);
    assert_eq!(
        heads[0].snapshot().parents,
        vec![first],
        "the carry extends the old tip"
    );
}

/// Author one file snapshot over the fixture drive, returning its
/// tree and snapshot id.
fn write_file(fixture: &Fixture, name: &str, bytes: &[u8]) -> (ContentId, SnapshotId) {
    let identity = read_identity(&fixture.identity_file).unwrap();
    let mut engine = Engine::open_keystore(fixture.drive.clone(), "test-pass", identity).unwrap();
    let mut store = FsObjectStore::open(fixture.drive.clone()).unwrap();
    let chunk = store.insert(ObjectKind::Chunk, bytes).unwrap();
    let entry = Entry::file(name, bytes.len() as u64, false, vec![chunk]).unwrap();
    let tree = Tree::from_entries(vec![entry])
        .unwrap()
        .insert_into(&mut store)
        .unwrap();
    let first = engine.author_snapshot(&store, tree).unwrap();
    (tree, first.snapshot().snapshot_id())
}

/// Invite carries the namespace: the newcomer's epoch starts from
/// the carried files, not an empty view.
#[test]
fn member_invite_carries_files_forward() {
    let fixture = Fixture::new();
    let (tree, first) = write_file(&fixture, "kept.txt", b"kept");
    admit(&fixture, &[0x44; 32]);

    let engine = fixture.open();
    let heads = engine.live_heads().unwrap();
    assert_eq!(heads.len(), 1, "the carry serves the files after invite");
    assert_eq!(heads[0].snapshot().epoch, 2);
    assert_eq!(heads[0].snapshot().tree, tree);
    assert_eq!(heads[0].snapshot().parents, vec![first]);
}

/// Child-2 progress surface: after an invite the newcomer's queued
/// announcements are visible in `member status` per device, so the
/// operator sees who has not caught up without rerunning invite.
/// The count is every pending announcement, not only admission
/// catch-up; silence means caught up (pinned for the fresh drive
/// above, which asserts no such lines appear).
#[test]
fn member_status_names_devices_with_pending_catch_up() {
    let fixture = Fixture::new();
    write_file(&fixture, "kept.txt", b"kept");
    let newcomer = admit(&fixture, &[0x44; 32]);

    let engine = fixture.open();
    let status = member_status_report(&engine).unwrap();
    let line = format!("announcements pending for {newcomer}:");
    assert!(
        status.contains(&line),
        "newcomer catch-up is visible in status: {status}"
    );
    let owner = device_id_of(&[0x22; 32]);
    assert!(
        !status.contains(&format!("announcements pending for {owner}")),
        "the caught-up owner is not named: {status}"
    );
}

/// The pending-line renderer pins the exact rules without an
/// engine: empty renders empty (after-drain silence), counts render
/// exactly, the reader branch renders like the member branch, and a
/// device outside the roster never renders even with pending
/// entries (removed devices keep stale outbox rows silently).
#[test]
fn pending_lines_render_exactly_and_scope_to_the_roster() {
    use std::collections::{BTreeMap, BTreeSet};

    let member = device_id_of(&[0x44; 32]);
    let reader = device_id_of(&[0x55; 32]);
    let removed = device_id_of(&[0x66; 32]);

    assert_eq!(
        render_pending_lines(&BTreeSet::new(), &BTreeSet::new(), &BTreeMap::new()),
        "",
        "empty outbox renders nothing"
    );

    let members: BTreeSet<DeviceId> = [member].into_iter().collect();
    let readers: BTreeSet<DeviceId> = [reader].into_iter().collect();
    let outstanding: BTreeMap<DeviceId, usize> = [(member, 3), (reader, 1), (removed, 9)]
        .into_iter()
        .collect();
    assert_eq!(
        render_pending_lines(&members, &readers, &outstanding),
        format!("announcements pending for {member}: 3\nannouncements pending for {reader}: 1\n"),
        "counts render exactly on both branches"
    );
    assert!(
        !render_pending_lines(&members, &readers, &outstanding).contains(&removed.to_string()),
        "a device outside the roster never renders"
    );
}

/// Remove carries the namespace for the remaining owner.
#[test]
fn member_remove_carries_files_forward() {
    let fixture = Fixture::new();
    let second = admit(&fixture, &[0x44; 32]);
    let (tree, first) = write_file(&fixture, "kept.txt", b"kept");
    command(fixture.args(vec!["remove".into(), second.to_string()])).unwrap();

    let engine = fixture.open();
    let heads = engine.live_heads().unwrap();
    assert_eq!(heads.len(), 1, "the carry serves the files after remove");
    assert_eq!(heads[0].snapshot().epoch, 3, "admit plus removal");
    assert_eq!(heads[0].snapshot().tree, tree);
    assert_eq!(heads[0].snapshot().parents, vec![first]);
}

/// Handover carries the namespace: the outgoing owner stays a
/// member, so its transition still carries.
#[test]
fn member_set_owner_carries_files_forward() {
    let fixture = Fixture::new();
    let second = admit(&fixture, &[0x44; 32]);
    let (tree, first) = write_file(&fixture, "kept.txt", b"kept");
    command(fixture.args(vec!["set-owner".into(), second.to_string()])).unwrap();

    let engine = fixture.open();
    let heads = engine.live_heads().unwrap();
    assert_eq!(heads.len(), 1, "the carry serves the files after handover");
    assert_eq!(heads[0].snapshot().epoch, 3, "admit plus handover");
    assert_eq!(heads[0].snapshot().tree, tree);
    assert_eq!(heads[0].snapshot().parents, vec![first]);
}

/// Resolve rejects malformed ids at the argument boundary, before any
/// engine state is touched.
#[test]
fn member_resolve_rejects_malformed_ids() {
    let fixture = Fixture::new();
    let error = command(fixture.args(vec![
        "resolve".into(),
        "not-hex".into(),
        "--void".into(),
        "00".into(),
    ]))
    .unwrap_err();
    assert!(matches!(error, CliError::Usage(_)), "unexpected: {error:?}");
}

/// Resolve on an unfrozen drive fails closed with the engine's
/// verdict, authoring nothing.
#[test]
fn member_resolve_without_conflict_fails_closed() {
    use wyrd_sync::runtime::EngineError;
    let fixture = Fixture::new();
    let tip = fixture.open().membership_log().known_state().expect("tip");
    let error = command(fixture.args(vec![
        "resolve".into(),
        tip.transition_id.to_string(),
        "--void".into(),
        "00".repeat(32),
    ]))
    .unwrap_err();
    assert!(
        matches!(error, CliError::Engine(EngineError::NoFrozenConflict)),
        "unexpected: {error:?}"
    );
    let after = fixture.open().membership_log().known_state().expect("tip");
    assert_eq!(after.transition_id, tip.transition_id, "nothing authored");
    assert_eq!(after.epoch, tip.epoch, "no new epoch");
}

/// A refused resolve stages the live head as a carry obligation
/// ahead of authoring — benign and visible: the next drain
/// discharges it without authoring while the head stays eligible.
/// Membership itself is untouched.
#[test]
fn member_resolve_refusal_stages_carry_without_transition() {
    let fixture = Fixture::new();
    let (_tree, head) = write_file(&fixture, "kept.txt", b"kept");
    let tip = fixture.open().membership_log().known_state().expect("tip");
    let error = command(fixture.args(vec![
        "resolve".into(),
        tip.transition_id.to_string(),
        "--void".into(),
        "00".repeat(32),
    ]))
    .unwrap_err();
    assert!(
        matches!(error, CliError::Engine(EngineError::NoFrozenConflict)),
        "unexpected: {error:?}"
    );
    let engine = fixture.open();
    let after = engine.membership_log().known_state().expect("tip");
    assert_eq!(after.transition_id, tip.transition_id, "no transition");
    assert_eq!(after.epoch, tip.epoch, "no new epoch");
    assert_eq!(
        engine.pending_carries().unwrap(),
        vec![head],
        "the live head stages as a carry obligation"
    );
}

/// Render `policy status` for a drive without capturing stdout: the
/// same census and posture block the command prints, so the test
/// below compares exactly what a person would read.
fn policy_status_of(
    drive: &std::path::Path,
    identity_file: &std::path::Path,
    passphrase: &str,
) -> String {
    use wyrd_core::policy::residency_census_with_role;
    let identity = read_identity(identity_file).unwrap();
    let node = super::commands::export::open_policy_node(drive.to_path_buf(), passphrase, identity)
        .unwrap();
    let state = wyrd_core::role::load(drive).unwrap();
    let role = state.effective();
    let (engine, view) = node.parts();
    let census = residency_census_with_role(engine, view, "", role).unwrap();
    let outstanding = wyrd_core::role::withdrawal_outstanding(engine).unwrap();
    policy_status_render(&role_section_render(drive, state, outstanding), "", &census)
}

/// The live head trees a drive serves, sorted: what the drive holds
/// for a person, independent of the epochs the carry stamped on the
/// re-authored snapshots.
fn live_trees(engine: &Engine) -> Vec<ContentId> {
    let mut trees: Vec<ContentId> = engine
        .live_heads()
        .unwrap()
        .iter()
        .map(|head| head.snapshot().tree)
        .collect();
    trees.sort();
    trees
}

/// A joined second device with its own drive dir: pairing, invite,
/// join — the same real surface a person drives, so the removed
/// device below holds genuine local state rather than a fixture.
struct SecondDevice {
    _temp: TempDir,
    drive: PathBuf,
    identity_file: PathBuf,
    device: DeviceId,
}

fn join_second_device(owner: &Fixture) -> SecondDevice {
    let temp = TempDir::new();
    let drive = temp.0.join("newcomer-drive");
    let identity_file = temp.0.join("newcomer-identity");
    let passphrase_file = temp.0.join("newcomer-passphrase");
    let pairing_file = temp.0.join("pairing");
    let invitation_file = temp.0.join("invitation");
    write_secret(&identity_file, [0x33; 32]);
    write_secret(&passphrase_file, b"newcomer-pass\n");
    let device_args = |action: Vec<String>| {
        let mut args = vec![
            "device".into(),
            drive.display().to_string(),
            "--identity-file".into(),
            identity_file.display().to_string(),
            "--passphrase-file".into(),
            passphrase_file.display().to_string(),
        ];
        args.extend(action);
        args
    };
    command(device_args(vec![
        "pairing-request".into(),
        pairing_file.display().to_string(),
    ]))
    .unwrap();
    let pairing = fs::read_to_string(&pairing_file).unwrap();
    let mut lines = pairing.lines();
    let device = lines
        .next()
        .and_then(|line| line.strip_prefix("device "))
        .expect("pairing names the device")
        .to_owned();
    let key = lines
        .next()
        .and_then(|line| line.strip_prefix("encryption-key "))
        .expect("pairing names the encryption key")
        .to_owned();
    command(owner.args(vec![
        "invite".into(),
        device.clone(),
        key,
        invitation_file.display().to_string(),
    ]))
    .unwrap();
    command(device_args(vec![
        "join".into(),
        invitation_file.display().to_string(),
    ]))
    .unwrap();
    SecondDevice {
        _temp: temp,
        drive,
        identity_file,
        device: parse_device_id(&device).unwrap(),
    }
}

/// The rendered removal confirmation carries the canonical
/// OD-14-A sentence — the retained-knowledge boundary and the
/// no-re-encryption history sentence — and none of the euphemisms
/// that would overstate the boundary.
#[test]
fn removal_confirmation_states_the_retained_knowledge_boundary() {
    use wyrd_core::member_surface::{
        render_removal_confirmation, HISTORY_NEVER_REENCRYPTED, REMOVAL_BOUNDARY,
        SOLE_OWNER_TERMINAL,
    };
    let device = device_id_of(&[0x44; 32]);
    for terminal in [false, true] {
        let out = render_removal_confirmation(&device, 3, 1, terminal);
        assert!(
            out.contains(REMOVAL_BOUNDARY),
            "canonical boundary verbatim: {out}"
        );
        assert!(
            out.contains(HISTORY_NEVER_REENCRYPTED),
            "canonical history sentence: {out}"
        );
        for forbidden in [
            "wiped",
            "no longer has access",
            "revoked all access",
            "can no longer read",
        ] {
            assert!(
                !out.contains(forbidden),
                "no euphemism {forbidden:?}: {out}"
            );
        }
    }
    let ordinary = render_removal_confirmation(&device, 3, 1, false);
    assert!(
        !ordinary.contains(SOLE_OWNER_TERMINAL),
        "ordinary removal states no terminal consequence: {ordinary}"
    );
    let terminal = render_removal_confirmation(&device, 2, 0, true);
    assert!(
        terminal.contains(SOLE_OWNER_TERMINAL),
        "terminal removal states the terminal consequence: {terminal}"
    );
}

/// Removal changes nothing about materialized data (C7): the
/// remaining owner's quadrant totals, the served trees, and the
/// removed device's own local cache and materialization facts are
/// all identical across the removal. The carry re-authors the served
/// head at the new epoch over the same tree (transition continuity),
/// so the test compares trees — what the drive holds — rather than
/// snapshot ids, which the carry legitimately restamps.
#[test]
fn removal_leaves_materialized_data_byte_identical() {
    let fixture = Fixture::new();
    let (_tree, _first) = write_file(&fixture, "kept.txt", b"kept");
    let newcomer = join_second_device(&fixture);
    let owner_before = policy_status_of(&fixture.drive, &fixture.identity_file, "test-pass");
    assert!(
        owner_before.contains("kept.txt"),
        "census names the file before removal"
    );
    let newcomer_before =
        policy_status_of(&newcomer.drive, &newcomer.identity_file, "newcomer-pass");
    let trees_before = live_trees(&fixture.open());

    command(fixture.args(vec!["remove".into(), newcomer.device.to_string()])).unwrap();

    assert_eq!(
        policy_status_of(&fixture.drive, &fixture.identity_file, "test-pass"),
        owner_before,
        "owner quadrant totals and rows identical across removal"
    );
    assert_eq!(
        live_trees(&fixture.open()),
        trees_before,
        "served trees identical across removal"
    );
    assert_eq!(
        policy_status_of(&newcomer.drive, &newcomer.identity_file, "newcomer-pass"),
        newcomer_before,
        "removed device's own local facts identical across removal"
    );
}

/// Last-owner removal needs the acknowledgement before authoring:
/// without `--yes` the surface renders the terminal consequence and
/// authors nothing, and a non-owner is refused with nothing
/// committed (owner-only authority at `remove_device`).
#[test]
fn last_owner_removal_requires_acknowledgement_before_authoring() {
    use wyrd_core::member_surface::SOLE_OWNER_TERMINAL;
    let fixture = Fixture::new();
    let owner = device_id_of(&[0x22; 32]);
    let tip_before = fixture.open().membership_log().known_state().expect("tip");
    let error = command(fixture.args(vec!["remove".into(), owner.to_string()])).unwrap_err();
    assert!(
        matches!(error, CliError::Usage(_)),
        "refusal, not an inline warning: {error:?}"
    );
    assert!(
        error.to_string().contains(SOLE_OWNER_TERMINAL),
        "refusal renders the terminal consequence: {error}"
    );
    let (tip_id, owners_hold_owner) = {
        let log = fixture.open();
        let tip_after = log.membership_log().known_state().expect("tip");
        (
            tip_after.transition_id,
            log.membership_log()
                .owners_of(&tip_after.transition_id)
                .expect("owners")
                .contains(&owner),
        )
    };
    assert_eq!(
        tip_id, tip_before.transition_id,
        "nothing authored without acknowledgement"
    );
    assert!(owners_hold_owner, "owner set untouched");

    // A stranger's credentials fail at open: the keystore binds to
    // its owner's device identity, so a non-owner reaches no
    // transition — and the tip above proves nothing was committed.
    let stranger_file = fixture._temp.0.join("stranger");
    write_secret(&stranger_file, [0x55; 32]);
    let error = command(vec![
        "member".into(),
        fixture.drive.display().to_string(),
        "--identity-file".into(),
        stranger_file.display().to_string(),
        "--passphrase-file".into(),
        fixture.passphrase_file.display().to_string(),
        "remove".into(),
        owner.to_string(),
    ])
    .unwrap_err();
    assert!(
        matches!(error, CliError::Engine(EngineError::OwnerMismatch)),
        "non-owner refused: {error:?}"
    );
}

/// The handover confirmation reports the carried head count to the
/// person and attributes the handover to the outgoing owner — and
/// only a current member can be named (OD-14-C, option A: v0
/// ownership is a singleton, so the picker offers members only).
#[test]
fn set_owner_reports_the_carried_head_count() {
    use wyrd_core::member_surface::render_handover_confirmation;
    let fixture = Fixture::new();
    let owner = device_id_of(&[0x22; 32]);
    let second = admit(&fixture, &[0x44; 32]);
    let stranger = device_id_of(&[0x55; 32]);

    // A non-member target fails with the engine's verdict and
    // authors nothing: the picker offers members only.
    let tip_before = fixture.open().membership_log().known_state().expect("tip");
    let error = command(fixture.args(vec!["set-owner".into(), stranger.to_string()])).unwrap_err();
    assert!(
        matches!(error, CliError::Engine(EngineError::NotMember)),
        "handover to a non-member refused: {error:?}"
    );
    assert_eq!(
        fixture
            .open()
            .membership_log()
            .known_state()
            .expect("tip")
            .transition_id,
        tip_before.transition_id,
        "refused handover authors nothing"
    );

    let (_tree, _first) = write_file(&fixture, "kept.txt", b"kept");
    command(fixture.args(vec!["set-owner".into(), second.to_string()])).unwrap();
    // The person-visible line: the carried count and the outgoing
    // author, over the handover the CLI just authored (admit, file,
    // handover: epoch 3, one head carried).
    let out = render_handover_confirmation(&second, &owner, 3, 1);
    assert!(out.contains("(1 carried)"), "carried count: {out}");
    assert!(
        out.contains(&format!("handed over by {owner}")),
        "outgoing author: {out}"
    );
    let engine = fixture.open();
    let tip = engine.membership_log().known_state().expect("tip");
    assert_eq!(tip.epoch, 3, "admit plus handover");
    assert_eq!(
        engine
            .membership_log()
            .owners_of(&tip.transition_id)
            .expect("owners")
            .iter()
            .collect::<Vec<_>>(),
        [&second],
        "ownership moves"
    );
}

/// The rotation confirmation names the fresh epoch secret and the
/// unchanged membership — and offers no device-key control, because
/// the object model cannot express one.
#[test]
fn rotate_copy_does_not_claim_the_device_key_changed() {
    use wyrd_core::member_surface::render_rotation_confirmation;
    let out = render_rotation_confirmation(3, 1);
    assert!(
        out.contains("fresh epoch secret"),
        "what rotation mints: {out}"
    );
    assert!(
        out.contains("membership unchanged"),
        "what rotation touches: {out}"
    );
    assert!(
        !out.contains("device key"),
        "rotation claims no device-key change: {out}"
    );

    // No such control exists at the argument boundary either:
    // replacing a device is a removal plus an admission under a
    // fresh identity, never a rotation of a key.
    let fixture = Fixture::new();
    for action in ["rotate-key", "rotate-device-key"] {
        let error = command(fixture.args(vec![action.into()])).unwrap_err();
        assert!(
            matches!(error, CliError::Usage(_)),
            "no {action:?} control: {error:?}"
        );
    }
    let tip = fixture.open().membership_log().known_state().expect("tip");
    assert_eq!(tip.epoch, 1, "refused controls author nothing");
}
