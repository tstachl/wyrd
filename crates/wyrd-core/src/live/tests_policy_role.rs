//! Vault-role enforcement at the loop's policy executor: the owner
//! socket submits through the same channel as every other surface, so
//! the role guard must live here rather than in a CLI wrapper.

use super::prereq_tests::{live_over_configured, scratch_file_drive_with_identity};
use super::*;

use crate::policy::PolicyError;
use crate::policy_channel::PolicyRequest;

/// The three states whose effective posture is `vault`. Every surface
/// must refuse eviction in all three, not only settled vault: an
/// acknowledged or pending narrowing is still a promise not to prune.
const VAULT_STATES: [(&str, RoleState); 3] = [
    (
        "settled-vault",
        RoleState {
            role: Role::Vault,
            transition: Transition::Settled,
        },
    ),
    (
        "narrowing-acknowledged",
        RoleState {
            role: Role::Mirror,
            transition: Transition::NarrowingAcknowledged,
        },
    ),
    (
        "withdrawal-pending",
        RoleState {
            role: Role::Mirror,
            transition: Transition::WithdrawalPending,
        },
    ),
];

/// One surface case: submit evict through the loop channel under a
/// vault-effective state, assert the typed refusal, then reopen the
/// engine from custody and prove the durable state is byte-identical
/// to the pre-drain baseline. A refused eviction authors no policy
/// fact on any of the three states.
fn evict_refusal_keeps_durable_state(state: RoleState, tag: &str) {
    let (engine, dir, store, _chunk, _root, head, identity) = scratch_file_drive_with_identity(tag);
    let before = engine.runtime_state().unwrap();
    let role_dir = dir.join("role");
    std::fs::create_dir_all(&role_dir).unwrap();
    crate::role::store(&role_dir, state).unwrap();
    let mut node = live_over_configured(engine, store, &[head], &LiveConfig::default());
    node.set_role_dir(role_dir);

    let channel = Arc::clone(node.policy_channel());
    let waiter = Arc::clone(&channel);
    let worker = std::thread::spawn(move || {
        waiter.submit(PolicyRequest::Evict {
            path: "f".to_string(),
        })
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !channel.has_queued() {
        assert!(
            std::time::Instant::now() < deadline,
            "{tag}: the eviction request was never queued"
        );
        std::thread::yield_now();
    }
    node.drain_policy();
    let result = worker.join().expect("drain wakes the submitter");
    assert!(
        matches!(result, Err(PolicyError::RoleForbidsEviction { .. })),
        "{tag}: the loop executor owns the vault refusal, got {result:?}"
    );
    drop(node);

    // The store lock is released with the node; reopening replays the
    // durable facts. No policy fact was appended, so this equals the
    // baseline captured before composition.
    let reopened = Engine::open_keystore(dir.clone(), "core-test-pass", identity).unwrap();
    assert_eq!(
        reopened.runtime_state().unwrap(),
        before,
        "{tag}: a refused eviction commits no durable fact"
    );
    drop(reopened);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn policy_channel_evict_refuses_in_every_vault_state_without_commit() {
    for (tag, state) in VAULT_STATES {
        evict_refusal_keeps_durable_state(state, tag);
    }
}

/// An unreadable role file has no effective posture: a status read
/// fails closed with a typed refusal (not an assumed `mirror`) and a
/// second request in the same drain gets the same typed failure. The
/// loader is not cached on failure, so the drain stays fail-closed.
#[test]
fn policy_channel_role_file_failure_is_a_typed_refusal() {
    let (engine, dir, store, _chunk, _root, head, _identity) =
        scratch_file_drive_with_identity("policy-role-unreadable");
    let role_dir = dir.join("role");
    std::fs::create_dir_all(&role_dir).unwrap();
    std::fs::write(role_dir.join("role"), b"not a role\n").unwrap();
    let mut node = live_over_configured(engine, store, &[head], &LiveConfig::default());
    node.set_role_dir(role_dir);

    let channel = Arc::clone(node.policy_channel());
    let waiter = Arc::clone(&channel);
    let worker = std::thread::spawn(move || {
        waiter.submit(PolicyRequest::Evict {
            path: "f".to_string(),
        })
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !channel.has_queued() {
        assert!(
            std::time::Instant::now() < deadline,
            "the eviction request was never queued"
        );
        std::thread::yield_now();
    }
    node.drain_policy();
    let result = worker.join().expect("drain wakes the submitter");
    assert!(
        matches!(result, Err(PolicyError::RoleUnreadable { .. })),
        "a torn role file must fail role-dependent policy closed, got {result:?}"
    );

    drop(node);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A settled vault role still admits an explicit pin: the role is a
/// default for unpinned content, not an override of an explicit
/// promise. Pin, unpin, and evict are four engine operations, not
/// mutations, and the role guard applies to eviction only.
#[test]
fn policy_channel_pin_still_works_under_vault() {
    let (engine, dir, store, _chunk, _root, head, _identity) =
        scratch_file_drive_with_identity("policy-role-pin");
    let role_dir = dir.join("role");
    std::fs::create_dir_all(&role_dir).unwrap();
    crate::role::store(
        &role_dir,
        RoleState {
            role: Role::Vault,
            transition: Transition::Settled,
        },
    )
    .unwrap();
    let mut node = live_over_configured(engine, store, &[head], &LiveConfig::default());
    node.set_role_dir(role_dir);

    let channel = Arc::clone(node.policy_channel());
    let waiter = Arc::clone(&channel);
    let worker = std::thread::spawn(move || {
        waiter.submit(PolicyRequest::Pin {
            path: "f".to_string(),
        })
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !channel.has_queued() {
        assert!(
            std::time::Instant::now() < deadline,
            "the pin request was never queued"
        );
        std::thread::yield_now();
    }
    node.drain_policy();
    assert!(
        worker.join().expect("drain wakes the submitter").is_ok(),
        "a vault role does not refuse an explicit pin"
    );

    drop(node);
    std::fs::remove_dir_all(dir).unwrap();
}
