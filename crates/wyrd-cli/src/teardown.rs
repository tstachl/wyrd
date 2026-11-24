//! Process exit-status fold: the mount, sync, and owner composers
//! run loop, session, and serving stages through the same
//! dominance order. Teardown never short-circuits — every stage
//! runs, and the first failure wins.
use crate::CliError;

/// Teardown outcomes as named fields, so a call site cannot silently
/// swap two stages the way positional arguments allow. The dominance
/// policy itself is the `.and()` chain below, in field order: keep
/// the chain and the declaration in the same order. Bulk has no field:
/// its shutdown is graceful-or-abort and infallible by construction,
/// so there is no bulk outcome to fold. The mount's unmount has a
/// field; other composers have no mountpoint to release and pass
/// `Ok(())`.
pub(crate) struct TeardownStatus {
    pub loop_result: Result<(), CliError>,
    pub session_result: Result<(), CliError>,
    pub unmount_result: Result<(), CliError>,
    pub serving_result: Result<(), CliError>,
}

/// Fold the loop, session, unmount, and serving outcomes into the
/// process exit status. Teardown never short-circuits — every stage
/// runs, and the first failure wins: a loop failure dominates (it
/// names the operational cause), then a dead session, then a failed
/// unmount, then the serving shutdown. Success requires a clean stop,
/// a cleanly reaped server, a released mountpoint, and a clean
/// serving shutdown alike. A failed unmount must not be discarded:
/// otherwise an empty mountpoint is satisfied by process-exit
/// detachment rather than by a clean unmount.
pub(crate) fn combine_status(status: TeardownStatus) -> Result<(), CliError> {
    status
        .loop_result
        .and(status.session_result)
        .and(status.unmount_result)
        .and(status.serving_result)
}
