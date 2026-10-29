//! Live-node process plumbing for this binary's composers (`mount`,
//! `sync now`, `vault`): the loop config and the serving
//! park/shutdown helpers. The shared surface — shutdown latch and
//! installer, teardown bounds, retention cross-check, bulk binder —
//! lives in `wyrd_daemon::process` so the next composer inherits it
//! instead of duplicating it.
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::CliError;
use wyrd_daemon::process::SHUTDOWN;
use wyrd_daemon::process::TRANSPORT_SHUTDOWN_DEADLINE;
#[cfg(any(test, feature = "mount"))]
use wyrd_daemon::LiveConfig;

/// Live-loop config for a mount: the shared local-sync budgets with
/// the idle pacing overridden. One constructor so the flag cannot
/// silently stop reaching the loop — the unit test pins the override.
/// Only the pacing moves: `--interval` shortens how long total silence
/// may delay a pass, never any ceiling.
/// Mount-scoped with the unit test that pins it: without the mount
/// command no non-test caller remains, but the test still runs.
#[cfg(any(test, feature = "mount"))]
pub(crate) fn live_config(interval: u64) -> LiveConfig {
    let mut config = LiveConfig::for_local_sync();
    config.interval = Duration::from_secs(interval);
    config
}

/// Park the `--serve` phase on the shutdown latch: the endpoint
/// serves the converged snapshot while the process waits for
/// SIGINT/SIGTERM. Polls in slices because a signal-handler trip
/// cannot notify a channel — the same slow-path guarantee as the
/// mount's own teardown wait, and fast enough for a path whose
/// teardown already budgets a minute for transport close.
pub(crate) fn park_serving_until_shutdown() {
    loop {
        if SHUTDOWN.load(Ordering::Relaxed) {
            tracing::info!(stage = "serving", "shutdown latch tripped");
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Shut a headless serving endpoint down under the mount's
/// transport bound, or succeed vacuously when `--serve` was absent
/// (OD-23-W: no endpoint, no shutdown, no hang).
pub(crate) fn shutdown_headless_serving(
    serving: Option<wyrd_sync::serving::ServingEndpoint>,
) -> Result<(), CliError> {
    match serving {
        Some(serving) => serving
            .shutdown(TRANSPORT_SHUTDOWN_DEADLINE)
            .map_err(CliError::Serving),
        None => Ok(()),
    }
}
