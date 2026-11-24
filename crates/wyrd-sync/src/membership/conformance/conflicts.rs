#![allow(unused_imports)]
use super::super::test_util::{admit, drive, key, sign, Builder};
use super::super::*;
use super::*;
use std::collections::BTreeSet;
use wyrd_format::membership::{set_root, MEMBER_SET_CONTEXT, OWNER_SET_CONTEXT};
use wyrd_format::{Change, DeviceEncryptionKey, DeviceId, MembershipTransition, TransitionId};

// --- conflicts and resolution -------------------------------------------
// Fork fixtures use a *valid* sibling (Admit of a second member): only
// valid children of the same canonical predecessor conflict.

pub(super) fn forked_chain() -> (
    Builder,
    MembershipTransition,
    MembershipTransition,
    MembershipTransition,
    DeviceId,
) {
    let (b, genesis) = Builder::genesis(1);
    let owner = *b.owners.iter().next().unwrap();
    let (_sk2, second) = key(2);
    let a = signed(
        &b,
        2,
        Some(genesis.transition_id()),
        Vec::new(),
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    let fork = signed(
        &b,
        2,
        Some(genesis.transition_id()),
        Vec::new(),
        vec![admit(second)],
        &[owner, second],
        &[owner],
    );
    (b, genesis, a, fork, second)
}

#[test]
fn conflict_same_predecessor_freezes() {
    let (_b, genesis, a, fork, _second) = forked_chain();
    let mut log = MembershipLog::new(drive());
    observe_all(&mut log, &[&genesis, &a.clone(), &fork.clone()]);
    assert_eq!(log.frozen_at(), Some(2));
    assert_eq!(
        log.status(&a.transition_id()),
        Some(TransitionStatus::Contested)
    );
    assert_eq!(
        log.status(&fork.transition_id()),
        Some(TransitionStatus::Contested)
    );
    assert_eq!(
        log.known_state().unwrap().epoch,
        1,
        "canonical below the freeze"
    );
}

#[test]
fn conflicting_transitions_with_different_predecessors_are_voided_by_ancestry() {
    let (b, genesis, a, fork, second) = forked_chain();
    let owner = *b.owners.iter().next().unwrap();
    // Children of the two contenders: valid links, different predecessors.
    let x = signed(
        &b,
        3,
        Some(a.transition_id()),
        Vec::new(),
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    let y = signed(
        &b,
        3,
        Some(fork.transition_id()),
        Vec::new(),
        vec![Change::Rotate],
        &[owner, second],
        &[owner],
    );
    let mut log = MembershipLog::new(drive());
    observe_all(&mut log, &[&genesis, &a.clone(), &fork.clone(), &x, &y]);
    assert_eq!(
        log.status(&x.transition_id()),
        Some(TransitionStatus::Voided)
    );
    assert_eq!(
        log.status(&y.transition_id()),
        Some(TransitionStatus::Voided)
    );
    assert_eq!(log.frozen_at(), Some(2), "the underlying conflict persists");
}

#[test]
fn resolution_selects_the_named_winner() {
    let (b, genesis, a, fork, _second) = forked_chain();
    let owner = *b.owners.iter().next().unwrap();
    // R: prev names the winner, resolves names the voided sibling.
    let r = signed(
        &b,
        3,
        Some(a.transition_id()),
        vec![fork.transition_id()],
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    let mut log = MembershipLog::new(drive());
    observe_all(&mut log, &[&genesis, &a.clone(), &fork.clone(), &r.clone()]);
    assert_eq!(
        log.status(&a.transition_id()),
        Some(TransitionStatus::Canonical)
    );
    assert_eq!(
        log.status(&fork.transition_id()),
        Some(TransitionStatus::Voided)
    );
    assert_eq!(
        log.status(&r.transition_id()),
        Some(TransitionStatus::Canonical)
    );
    assert_eq!(log.frozen_at(), None);
    assert_eq!(log.known_state().unwrap().epoch, 3);

    // The chain continues from R.
    let after = signed(
        &b,
        4,
        Some(r.transition_id()),
        Vec::new(),
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    let mut log2 = MembershipLog::new(drive());
    observe_all(&mut log2, &[&genesis, &a, &fork, &r, &after]);
    assert_eq!(
        log2.status(&after.transition_id()),
        Some(TransitionStatus::Canonical)
    );
    assert_eq!(log2.known_state().unwrap().epoch, 4);
}

/// A conflict at epoch 2, the owner-signed resolution R that heals it,
/// and the valid work the fork's author writes afterwards: x admits a
/// device on the losing branch, x2 continues from x. Returned in one
/// piece so the arrival-order tests can feed the same set in any order.
#[allow(clippy::type_complexity)]
fn resolution_with_losing_work() -> (
    MembershipTransition,
    MembershipTransition,
    MembershipTransition,
    MembershipTransition,
    MembershipTransition,
    MembershipTransition,
) {
    let (b, genesis, a, fork, second) = forked_chain();
    let owner = *b.owners.iter().next().unwrap();
    let (_sk3, third) = key(3);
    let r = signed(
        &b,
        3,
        Some(a.transition_id()),
        vec![fork.transition_id()],
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    let x = signed(
        &b,
        3,
        Some(fork.transition_id()),
        Vec::new(),
        vec![admit(third), Change::Rotate],
        &[owner, second, third],
        &[owner],
    );
    let x2 = signed(
        &b,
        4,
        Some(x.transition_id()),
        Vec::new(),
        vec![Change::Rotate],
        &[owner, second, third],
        &[owner],
    );
    (genesis, a, fork, r, x, x2)
}

/// Resolution authority is final with respect to the conflict it
/// resolves. A device that keeps writing on a branch the resolution
/// already voided holds valid links on that branch, but branch validity
/// grants no authority to reopen a resolved conflict: the drive stays
/// live on the resolution's chain and the later work is retained as
/// historical evidence.
#[test]
fn post_resolution_losing_work_cannot_reopen_the_conflict() {
    let (genesis, a, fork, r, x, x2) = resolution_with_losing_work();
    let mut log = MembershipLog::new(drive());
    observe_all(&mut log, &[&genesis, &a, &fork, &r]);
    assert_eq!(log.frozen_at(), None, "the resolution heals the conflict");
    let (x_id, x2_id) = (x.transition_id(), x2.transition_id());
    let x_members_root = x.members_root;
    log.observe(x);
    log.observe(x2);

    // The conflict does not reopen: the resolution keeps the canonical
    // chain and the drive stays unfrozen.
    assert_eq!(log.frozen_at(), None);
    assert_eq!(
        log.status(&a.transition_id()),
        Some(TransitionStatus::Canonical)
    );
    assert_eq!(
        log.status(&fork.transition_id()),
        Some(TransitionStatus::Voided)
    );
    assert_eq!(
        log.status(&r.transition_id()),
        Some(TransitionStatus::Canonical)
    );
    // The later losing work is historical evidence, never authorizing.
    assert_eq!(log.status(&x_id), Some(TransitionStatus::Voided));
    assert_eq!(log.status(&x2_id), Some(TransitionStatus::Voided));
    // Nothing from the losing branch leaks: the canonical epoch-3 state
    // is the resolution's, so the branch's admission is not in it.
    let known = log.known_state().unwrap();
    assert_eq!(known.epoch, 3);
    assert_eq!(known.transition_id, r.transition_id());
    assert_eq!(known.members_root, r.members_root);
    assert_ne!(known.members_root, x_members_root);
}

/// The adversarial arrival order: the losing branch advances *before* the
/// resolution is observed. Classification is a pure function of the
/// observed set, so the outcome must be the resolution's, not the losing
/// branch's.
#[test]
fn losing_work_observed_first_does_not_block_the_resolution() {
    let (genesis, a, fork, r, x, x2) = resolution_with_losing_work();
    let mut log = MembershipLog::new(drive());
    observe_all(&mut log, &[&genesis, &a, &fork, &x, &x2]);
    assert_eq!(
        log.frozen_at(),
        Some(2),
        "unresolved conflict: the losing work alone resolves nothing"
    );
    let (x_id, x2_id) = (x.transition_id(), x2.transition_id());
    let x_members_root = x.members_root;
    let (r_id, r_members_root) = (r.transition_id(), r.members_root);
    log.observe(r);

    assert_eq!(log.frozen_at(), None);
    let known = log.known_state().unwrap();
    assert_eq!(known.epoch, 3, "the resolution is the canonical successor");
    assert_eq!(known.transition_id, r_id);
    assert_eq!(known.members_root, r_members_root);
    assert_ne!(known.members_root, x_members_root);
    assert_eq!(log.status(&x_id), Some(TransitionStatus::Voided));
    assert_eq!(log.status(&x2_id), Some(TransitionStatus::Voided));
}

/// The mirror case on the winning side: the resolution designates the
/// canonical successor of the winning tip, so another valid child of
/// that tip is evidence, not a rival for the epoch.
#[test]
fn non_designed_valid_child_of_the_winner_is_voided() {
    let (b, genesis, a, fork, _second) = forked_chain();
    let owner = *b.owners.iter().next().unwrap();
    let (_sk3, third) = key(3);
    let r = signed(
        &b,
        3,
        Some(a.transition_id()),
        vec![fork.transition_id()],
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    let y = signed(
        &b,
        3,
        Some(a.transition_id()),
        Vec::new(),
        vec![admit(third), Change::Rotate],
        &[owner, third],
        &[owner],
    );
    let mut log = MembershipLog::new(drive());
    observe_all(&mut log, &[&genesis, &a, &fork, &r, &y]);

    assert_eq!(log.frozen_at(), None);
    let known = log.known_state().unwrap();
    assert_eq!(known.transition_id, r.transition_id());
    assert_eq!(known.members_root, r.members_root);
    assert_ne!(known.members_root, y.members_root);
    assert_eq!(
        log.status(&y.transition_id()),
        Some(TransitionStatus::Voided)
    );
}

/// The r1/r2 contradiction fixture: an epoch-2 fork and two epoch-3
/// resolutions naming opposite winners. Their declared roots match their
/// parents' states (Rotate), so both links are valid and only the
/// resolution rule decides. Returned with the builder so tests can add
/// the recovery transition.
#[allow(clippy::type_complexity)]
fn contradictory_resolutions() -> (
    Builder,
    MembershipTransition,
    MembershipTransition,
    MembershipTransition,
    DeviceId,
    MembershipTransition,
    MembershipTransition,
) {
    let (b, genesis, a, fork, second) = forked_chain();
    let owner = *b.owners.iter().next().unwrap();
    let r1 = signed(
        &b,
        3,
        Some(a.transition_id()),
        vec![fork.transition_id()],
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    let r2 = signed(
        &b,
        3,
        Some(fork.transition_id()),
        vec![a.transition_id()],
        vec![Change::Rotate],
        &[owner, second],
        &[owner],
    );
    (b, genesis, a, fork, second, r1, r2)
}

#[test]
fn contradictory_resolutions_refreeze() {
    let (_b, genesis, a, fork, _second, r1, r2) = contradictory_resolutions();
    let mut log = MembershipLog::new(drive());
    observe_all(
        &mut log,
        &[
            &genesis,
            &a.clone(),
            &fork.clone(),
            &r1.clone(),
            &r2.clone(),
        ],
    );
    assert_eq!(log.frozen_at(), Some(3), "frozen at the resolution epoch");
    assert_eq!(
        log.status(&r1.transition_id()),
        Some(TransitionStatus::Contested)
    );
    assert_eq!(
        log.status(&r2.transition_id()),
        Some(TransitionStatus::Contested)
    );
    assert_eq!(
        log.status(&a.transition_id()),
        Some(TransitionStatus::Contested)
    );
    assert_eq!(
        log.status(&fork.transition_id()),
        Some(TransitionStatus::Contested)
    );
}

/// R3 at F+1 ends the re-freeze and restores one canonical chain. Every
/// earlier contested transition is given its final fate: the winner and
/// its ancestors canonical, the loser and its branch voided.
#[test]
fn second_level_resolution_ends_a_contradictory_refreeze() {
    let (b, genesis, a, fork, _second, r1, r2) = contradictory_resolutions();
    let owner = *b.owners.iter().next().unwrap();
    let r3 = signed(
        &b,
        4,
        Some(r1.transition_id()),
        vec![r2.transition_id()],
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    let mut log = MembershipLog::new(drive());
    observe_all(
        &mut log,
        &[
            &genesis,
            &a.clone(),
            &fork.clone(),
            &r1.clone(),
            &r2.clone(),
            &r3.clone(),
        ],
    );
    assert_eq!(log.frozen_at(), None, "the re-freeze is ended");
    for (id, expected) in [
        (a.transition_id(), TransitionStatus::Canonical),
        (fork.transition_id(), TransitionStatus::Voided),
        (r1.transition_id(), TransitionStatus::Canonical),
        (r2.transition_id(), TransitionStatus::Voided),
        (r3.transition_id(), TransitionStatus::Canonical),
    ] {
        assert_eq!(log.status(&id), Some(expected), "status of {id:?}");
    }
    let tip = log.known_state().expect("a canonical tip exists again");
    assert_eq!(tip.transition_id, r3.transition_id());
    assert_eq!(tip.epoch, 4);

    // The chain continues from R3.
    let after = signed(
        &b,
        5,
        Some(r3.transition_id()),
        Vec::new(),
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    let mut log2 = MembershipLog::new(drive());
    observe_all(&mut log2, &[&genesis, &a, &fork, &r1, &r2, &r3, &after]);
    assert_eq!(
        log2.status(&after.transition_id()),
        Some(TransitionStatus::Canonical)
    );
    assert_eq!(log2.known_state().unwrap().epoch, 5);
    assert_eq!(log2.frozen_at(), None);
}

/// A transition at the recovery epoch does not unfreeze merely by
/// existing. Each case names a different rule that must hold.
#[test]
fn refreeze_holds_unless_the_second_level_resolution_satisfies_every_rule() {
    let (b, genesis, a, fork, second, r1, r2) = contradictory_resolutions();
    let owner = *b.owners.iter().next().unwrap();
    let (other_sk, other_device) = key(2);

    let mut bad_signature = MembershipTransition::new(
        4,
        Some(r1.transition_id()),
        vec![r2.transition_id()],
        vec![Change::Rotate],
        set_root(MEMBER_SET_CONTEXT, &[owner]).unwrap(),
        set_root(OWNER_SET_CONTEXT, &[owner]).unwrap(),
        set_root(READER_SET_CONTEXT, &[]).unwrap(),
        owner,
    )
    .unwrap();
    sign(&mut bad_signature, &other_sk, &b.drive);

    let mut unauthorized = MembershipTransition::new(
        4,
        Some(r1.transition_id()),
        vec![r2.transition_id()],
        vec![Change::Rotate],
        set_root(MEMBER_SET_CONTEXT, &[owner]).unwrap(),
        set_root(OWNER_SET_CONTEXT, &[owner]).unwrap(),
        set_root(READER_SET_CONTEXT, &[]).unwrap(),
        other_device,
    )
    .unwrap();
    sign(&mut unauthorized, &other_sk, &b.drive);

    // `prev` names the loser, not the winner the resolution voids.
    let wrong_prev = signed(
        &b,
        4,
        Some(r2.transition_id()),
        vec![r2.transition_id()],
        vec![Change::Rotate],
        &[owner, second],
        &[owner],
    );
    // No `resolves`: the recovery rule requires naming the other.
    let empty_resolves = signed(
        &b,
        4,
        Some(r1.transition_id()),
        Vec::new(),
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    // Epoch 5 skips the epoch immediately above the freeze.
    let wrong_epoch = signed(
        &b,
        5,
        Some(r1.transition_id()),
        vec![r2.transition_id()],
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    // An unrelated valid transition at the freeze epoch makes `resolves`
    // over-broad.
    let unrelated = signed(
        &b,
        3,
        Some(a.transition_id()),
        Vec::new(),
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    let over_broad = signed(
        &b,
        4,
        Some(r1.transition_id()),
        vec![r2.transition_id(), unrelated.transition_id()],
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );

    for (label, candidate, expected) in [
        (
            "bad signature",
            &bad_signature,
            Some(TransitionStatus::Invalid(InvalidReason::BadSignature)),
        ),
        (
            "unauthorized author",
            &unauthorized,
            Some(TransitionStatus::Invalid(InvalidReason::AuthorNotOwner)),
        ),
        (
            "wrong epoch",
            &wrong_epoch,
            Some(TransitionStatus::Invalid(InvalidReason::PrevWrongEpoch)),
        ),
        ("wrong prev", &wrong_prev, Some(TransitionStatus::Voided)),
        (
            "empty resolves",
            &empty_resolves,
            Some(TransitionStatus::Voided),
        ),
        (
            "over-broad resolves",
            &over_broad,
            Some(TransitionStatus::Voided),
        ),
    ] {
        let mut log = MembershipLog::new(drive());
        observe_all(
            &mut log,
            &[
                &genesis,
                &a.clone(),
                &fork.clone(),
                &r1.clone(),
                &r2.clone(),
                candidate,
                &unrelated,
            ],
        );
        assert_eq!(log.frozen_at(), Some(3), "{label}: freeze must hold");
        assert_eq!(log.status(&candidate.transition_id()), expected, "{label}");
        assert_eq!(
            log.known_state().unwrap().epoch,
            1,
            "{label}: canonical chain stays below the freeze"
        );
    }
}

/// Two contradictory second-level resolutions re-freeze one epoch
/// higher, and a single one of them then ends it.
#[test]
fn contradictory_second_level_resolutions_refreeze_again() {
    let (b, genesis, a, fork, second, r1, r2) = contradictory_resolutions();
    let owner = *b.owners.iter().next().unwrap();
    let s1 = signed(
        &b,
        4,
        Some(r1.transition_id()),
        vec![r2.transition_id()],
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    // r2 won on the fork branch, which admitted `second`: its Rotate
    // must declare that member too, or the link is invalid and cannot
    // be a contradictory candidate.
    let s2 = signed(
        &b,
        4,
        Some(r2.transition_id()),
        vec![r1.transition_id()],
        vec![Change::Rotate],
        &[owner, second],
        &[owner],
    );
    let mut log = MembershipLog::new(drive());
    observe_all(
        &mut log,
        &[
            &genesis,
            &a.clone(),
            &fork.clone(),
            &r1.clone(),
            &r2.clone(),
            &s1.clone(),
            &s2.clone(),
        ],
    );
    assert_eq!(log.frozen_at(), Some(4));
    assert_eq!(
        log.status(&s1.transition_id()),
        Some(TransitionStatus::Contested)
    );
    assert_eq!(
        log.status(&s2.transition_id()),
        Some(TransitionStatus::Contested)
    );

    // A further resolution at F+1 (epoch 5) ends the second re-freeze.
    let t3 = signed(
        &b,
        5,
        Some(s1.transition_id()),
        vec![s2.transition_id()],
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    let mut log2 = MembershipLog::new(drive());
    observe_all(&mut log2, &[&genesis, &a, &fork, &r1, &r2, &s1, &s2, &t3]);
    assert_eq!(log2.frozen_at(), None);
    assert_eq!(log2.known_state().unwrap().epoch, 5);
    assert_eq!(
        log2.status(&t3.transition_id()),
        Some(TransitionStatus::Canonical)
    );
}

#[test]
fn duplicate_resolves_entries_are_invalid() {
    let (mut b, genesis) = Builder::genesis(1);
    let first = b.child(vec![Change::Rotate]);
    let mut r = b
        .child(vec![Change::Rotate])
        .with_resolves(vec![first.transition_id(), first.transition_id()])
        .unwrap();
    sign(&mut r, &b.sk, &b.drive);
    let mut log = MembershipLog::new(drive());
    observe_all(&mut log, &[&genesis, &first, &r]);
    assert_eq!(
        log.status(&r.transition_id()),
        Some(TransitionStatus::Invalid(InvalidReason::DuplicateResolves))
    );
}

#[test]
fn resolution_naming_extra_transitions_is_not_a_resolution() {
    // Conflict A vs fork; a "resolution" that names the sibling AND an
    // unrelated valid transition (or the winner itself) is not the
    // documented protocol: it must not unfreeze the conflict.
    let (b, genesis, a, fork, second) = forked_chain();
    let owner = *b.owners.iter().next().unwrap();
    // An unrelated valid transition at the same epoch on another branch:
    // a child of the fork.
    let unrelated = signed(
        &b,
        3,
        Some(fork.transition_id()),
        Vec::new(),
        vec![Change::Rotate],
        &[owner, second],
        &[owner],
    );
    // Over-broad resolution: names the sibling plus the unrelated one.
    let mut over = signed(
        &b,
        3,
        Some(a.transition_id()),
        vec![fork.transition_id(), unrelated.transition_id()],
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    sign(&mut over, &b.sk, &b.drive);
    let mut log = MembershipLog::new(drive());
    observe_all(
        &mut log,
        &[&genesis, &a.clone(), &fork.clone(), &unrelated, &over],
    );
    // The conflict stays frozen; the over-broad document is invalid as a
    // resolution (ResolvesWithoutConflict: it does not match the
    // conflict's shape) and the winner stays contested.
    assert_eq!(log.frozen_at(), Some(2));
    assert_eq!(
        log.status(&a.transition_id()),
        Some(TransitionStatus::Contested)
    );
    assert_eq!(
        log.status(&over.transition_id()),
        Some(TransitionStatus::Invalid(
            InvalidReason::InvalidResolvesEntry
        ))
    );
}

#[test]
fn resolution_naming_the_winner_is_invalid() {
    let (b, genesis, a, fork, _second) = forked_chain();
    let owner = *b.owners.iter().next().unwrap();
    let mut self_named = signed(
        &b,
        3,
        Some(a.transition_id()),
        vec![fork.transition_id(), a.transition_id()],
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    sign(&mut self_named, &b.sk, &b.drive);
    let mut log = MembershipLog::new(drive());
    observe_all(
        &mut log,
        &[&genesis, &a.clone(), &fork.clone(), &self_named],
    );
    assert_eq!(log.frozen_at(), Some(2), "not a conformant resolution");
    assert_eq!(
        log.status(&a.transition_id()),
        Some(TransitionStatus::Contested)
    );
}

#[test]
fn resolves_without_conflict_is_invalid() {
    let (mut b, genesis) = Builder::genesis(1);
    let first = b.child(vec![Change::Rotate]);
    let mut r = b
        .child(vec![Change::Rotate])
        .with_resolves(vec![first.transition_id()])
        .unwrap();
    sign(&mut r, &b.sk, &b.drive);
    let mut log = MembershipLog::new(drive());
    observe_all(&mut log, &[&genesis, &first, &r]);
    assert_eq!(
        log.status(&r.transition_id()),
        Some(TransitionStatus::Invalid(
            InvalidReason::ResolvesWithoutConflict
        ))
    );
}

#[test]
fn resolves_unknown_entry_is_pending() {
    let (b, genesis, a, fork, _second) = forked_chain();
    let owner = *b.owners.iter().next().unwrap();
    let r = signed(
        &b,
        3,
        Some(a.transition_id()),
        vec![fork.transition_id()],
        vec![Change::Rotate],
        &[owner],
        &[owner],
    );
    // R observed before the sibling it voids: pending, not invalid.
    let mut log = MembershipLog::new(drive());
    observe_all(&mut log, &[&genesis, &a, &r.clone()]);
    assert_eq!(
        log.status(&r.transition_id()),
        Some(TransitionStatus::Pending)
    );
    // Arrival completes the resolution.
    log.observe(fork);
    assert_eq!(
        log.status(&r.transition_id()),
        Some(TransitionStatus::Canonical)
    );
}
