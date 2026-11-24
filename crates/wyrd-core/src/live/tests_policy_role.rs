//! Vault-role enforcement at the loop's policy executor: the owner
//! socket submits through the same channel as every other surface, so
//! the role guard must live here rather than in a CLI wrapper.

use super::prereq_tests::{live_over_configured, scratch_file_drive};
use super::*;

use crate::policy::PolicyError;
use crate::policy_channel::PolicyRequest;

/// A settled vault role refuses eviction through the policy channel.
/// The refusal is typed and carries no commit; pin and unpin on the
/// same node still work because they act on stored facts alone.
#[test]
fn policy_channel_evict_refuses_under_the_vault_role() {
    let (engine, dir, store, _chunk, _root, head) = scratch_file_drive("policy-role-vault");
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
        matches!(result, Err(PolicyError::RoleForbidsEviction { .. })),
        "the loop executor owns the vault refusal, got {result:?}"
    );

    // The same channel still admits a pin under the vault role:
    // explicit promises mean what they say, the role is a default.
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

/// An acknowledged narrowing is still `vault` effective: eviction
/// must refuse until the withdrawal actually completes.
#[test]
fn policy_channel_evict_refuses_while_narrowing_is_acknowledged() {
    let (engine, dir, store, _chunk, _root, head) = scratch_file_drive("policy-role-narrowing");
    let role_dir = dir.join("role");
    std::fs::create_dir_all(&role_dir).unwrap();
    crate::role::store(
        &role_dir,
        RoleState {
            role: Role::Mirror,
            transition: Transition::NarrowingAcknowledged,
        },
    )
    .unwrap();
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
        matches!(result, Err(PolicyError::RoleForbidsEviction { .. })),
        "acknowledged narrowing is still vault effective, got {result:?}"
    );

    drop(node);
    std::fs::remove_dir_all(dir).unwrap();
}
