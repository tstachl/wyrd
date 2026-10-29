//! Host-side composition tests over the node: the daemon wires the
//! node to its presentation backends here, and behavior tests live
//! alongside, one file per theme. The node itself
//! ([`wyrd_core::node`]) and the live loop ([`wyrd_core::live`]) own
//! the engine/view/loop composition; this module only re-exports
//! their surface for the composer and its tests.

#[cfg(test)]
mod tests_backend;
#[cfg(all(test, feature = "mount"))]
mod tests_coalescing;
#[cfg(test)]
mod tests_composition;
#[cfg(all(test, feature = "mount"))]
mod tests_handles;
#[cfg(test)]
mod tests_harness;
#[cfg(all(test, feature = "mount"))]
mod tests_mount;
#[cfg(test)]
mod tests_mount_free;
#[cfg(test)]
mod tests_policy;
#[cfg(all(test, feature = "mount"))]
mod tests_publish;
#[cfg(test)]
mod tests_run_loop;
#[cfg(test)]
mod tests_sync;
#[cfg(test)]
mod tests_teardown;
#[cfg(test)]
mod tests_vault;

pub use wyrd_core::budgets::ResourceBudgets;
pub use wyrd_core::live::{
    admit_wants, FailureClass, LiveConfig, LiveError, LiveNode, LiveParts, LiveSummary, PassHook,
    PendingObligations, ServingBarrier, SyncReport, MAILBOX_MAX_CONSECUTIVE_ERRORS,
    STORE_MAX_CONSECUTIVE_ERRORS,
};
pub use wyrd_core::node::{NodeError, WriteError, WyrdNode};
pub use wyrd_core::view::RuntimeMaterialization;
