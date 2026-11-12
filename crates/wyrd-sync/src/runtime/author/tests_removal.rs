use super::tests_harness::{device_of, frozen_engine, owner_engine, stranger_engine};
use crate::keys::{DeviceEncryptionSecret, DeviceIdentitySecret};
use crate::membership::test_util::key;
use crate::runtime::engine::EngineError;
use crate::runtime::test_util::encryption_key;

#[test]
fn remove_device_removes_member_and_queues_catch_up_for_the_rest() {
    let (_dir, mut engine, _genesis) = owner_engine("remove-device");
    let (_owner_sk, owner_id) = key(10);
    let second = DeviceIdentitySecret::generate().unwrap();
    let second_encryption = DeviceEncryptionSecret::generate().unwrap();
    let second_id = device_of(&second);
    let third = DeviceIdentitySecret::generate().unwrap();
    let third_encryption = DeviceEncryptionSecret::generate().unwrap();
    let third_id = device_of(&third);
    engine
        .admit_device(second_id, encryption_key(&second_encryption))
        .unwrap();
    engine
        .admit_device(third_id, encryption_key(&third_encryption))
        .unwrap();

    let transition = engine.remove_device(second_id).unwrap();
    assert_eq!(transition.epoch, 4, "removal opens a new epoch");
    let tip = engine.log.known_state().expect("canonical tip");
    assert_eq!(tip.transition_id, transition.transition_id());
    let members = engine
        .log
        .members_of(&tip.transition_id)
        .expect("post state");
    assert!(!members.contains(&second_id), "removed device leaves");
    assert!(members.contains(&owner_id), "owner stays");
    assert!(members.contains(&third_id), "third device stays");

    // Catch-up goes to the remaining members only: the removed device
    // is owed no new-epoch material. Stale pre-removal queue entries
    // for it (its admission-epoch wraps, never delivered here) stay:
    // they carry historical epochs only, which revocation permits.
    let rebuilt = engine.store.rebuild(engine.device()).unwrap();
    assert!(
        rebuilt
            .runtime
            .pending_capabilities()
            .contains(&(4, third_id)),
        "remaining member gets the new-epoch wrap"
    );
    assert!(
        !rebuilt
            .runtime
            .pending_capabilities()
            .iter()
            .any(|(epoch, recipient)| *recipient == second_id && *epoch >= 4),
        "removed device gets no new-epoch wrap"
    );
    assert!(
        !rebuilt
            .runtime
            .pending_transitions()
            .iter()
            .any(|(id, recipient)| *id == tip.transition_id && *recipient == second_id),
        "removed device gets no removal transition"
    );
}

#[test]
fn remove_device_requires_owner_authority() {
    let (_dir, mut engine, _genesis) = owner_engine("remove-device-owner");
    let second = DeviceIdentitySecret::generate().unwrap();
    let second_encryption = DeviceEncryptionSecret::generate().unwrap();
    let second_id = device_of(&second);
    engine
        .admit_device(second_id, encryption_key(&second_encryption))
        .unwrap();
    let mut stranger = stranger_engine("remove-device-stranger");
    assert!(matches!(
        stranger.remove_device(second_id),
        Err(EngineError::NotOwner)
    ));
}

#[test]
fn remove_unknown_device_is_refused() {
    let (_dir, mut engine, _genesis) = owner_engine("remove-device-unknown");
    let stranger = DeviceIdentitySecret::generate().unwrap();
    assert!(matches!(
        engine.remove_device(device_of(&stranger)),
        Err(EngineError::NotMember)
    ));
}

/// Deletion refuses on a frozen membership: the self-removal half of
/// `drive delete` is `remove_device(self)`, and a frozen drive
/// commits nothing new until the conflict resolves — so the surface
/// (which removes the directory only after a successful commit)
/// leaves the directory untouched by construction. This pins the
/// refusal the surface relies on.
#[test]
fn drive_deletion_refuses_on_a_frozen_membership() {
    let (_dir, mut engine, owner, _genesis, _winner, _rival) =
        frozen_engine("drive-deletion-frozen");
    assert_eq!(engine.device(), owner, "the frozen engine is the owner");
    let tip_before = engine.log.known_state().expect("tip").transition_id;
    assert!(
        matches!(
            engine.remove_device(owner),
            Err(EngineError::FrozenConflictRemains)
        ),
        "self-removal refuses while frozen"
    );
    assert_eq!(
        engine.log.known_state().expect("tip").transition_id,
        tip_before,
        "nothing authored while frozen"
    );
    assert_eq!(engine.log.frozen_at(), Some(2), "still frozen");
}

#[test]
fn remove_sole_owner_is_valid_and_terminal() {
    // Removing the only owner is valid but ends owner-authorized
    // evolution: the owner set empties, the author (now a non-member)
    // installs no self capability, and no further transition can be
    // authorized — the next admit fails NotOwner, not silently.
    let (_dir, mut engine, _genesis) = owner_engine("remove-device-terminal");
    let (_owner_sk, owner_id) = key(10);
    let transition = engine.remove_device(owner_id).unwrap();
    assert_eq!(transition.epoch, 2);
    let tip = engine.log.known_state().expect("canonical tip");
    assert_eq!(tip.transition_id, transition.transition_id());
    assert!(
        engine
            .log
            .owners_of(&tip.transition_id)
            .expect("state")
            .is_empty(),
        "owner set empties with the sole owner"
    );
    let newcomer = DeviceIdentitySecret::generate().unwrap();
    let newcomer_encryption = DeviceEncryptionSecret::generate().unwrap();
    let newcomer_key = encryption_key(&newcomer_encryption);
    let newcomer_id = device_of(&newcomer);
    assert!(matches!(
        engine.admit_device(newcomer_id, newcomer_key),
        Err(EngineError::NotOwner)
    ));
}

#[test]
fn admit_retired_device_is_refused_before_signing() {
    // The chain rule stays authoritative, but authoring refuses early
    // with a renderable error instead of signing a doomed transition:
    // no phantom tip, no batch, nothing durable.
    let (_dir, mut engine, _genesis) = owner_engine("remove-device-retired");
    let second = DeviceIdentitySecret::generate().unwrap();
    let second_encryption = DeviceEncryptionSecret::generate().unwrap();
    let second_id = device_of(&second);
    let tip_before = engine.log.known_state().expect("tip").transition_id;
    engine
        .admit_device(second_id, encryption_key(&second_encryption))
        .unwrap();
    engine.remove_device(second_id).unwrap();
    assert!(matches!(
        engine.admit_device(second_id, encryption_key(&second_encryption)),
        Err(EngineError::RetiredDevice)
    ));
    let tip = engine.log.known_state().expect("tip");
    assert_eq!(tip.epoch, 3, "refused admit commits nothing");
    assert_ne!(tip.transition_id, tip_before);
}

#[test]
fn sole_owner_leave_empties_the_owner_set() {
    // The leave the surface offers the sole owner: Remove(self)
    // through the existing owner gate. The transition commits, the
    // owner set empties, the epoch bumps, nothing is installed for
    // the author — and no later transition can be authored, so the
    // drive freezes permanently.
    let (_dir, mut engine, _genesis) = owner_engine("sole-owner-leave");
    let this = engine.device();
    let transition = engine.remove_device(this).unwrap();
    assert_eq!(transition.epoch, 2, "leave opens a new epoch");
    let tip = engine.log.known_state().expect("canonical tip");
    assert_eq!(tip.transition_id, transition.transition_id());
    assert!(
        engine
            .log
            .owners_of(&tip.transition_id)
            .expect("state")
            .is_empty(),
        "leave empties the owner set with the author"
    );
    assert!(
        !engine.held_epochs().unwrap().contains(&transition.epoch),
        "the removed author installs no self capability for the new epoch"
    );
    let rebuilt = engine.store.rebuild(engine.device()).unwrap();
    assert!(
        !rebuilt
            .runtime
            .pending_capabilities()
            .iter()
            .any(|(epoch, _)| *epoch == transition.epoch),
        "leave queues no new-epoch material for the author"
    );
    assert!(matches!(engine.rotate_epoch(), Err(EngineError::NotOwner)));
    let newcomer = DeviceIdentitySecret::generate().unwrap();
    let newcomer_encryption = DeviceEncryptionSecret::generate().unwrap();
    assert!(matches!(
        engine.admit_device(device_of(&newcomer), encryption_key(&newcomer_encryption)),
        Err(EngineError::NotOwner)
    ));
}
