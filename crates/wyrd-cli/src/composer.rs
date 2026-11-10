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
use wyrd_daemon::LiveConfig;

/// What opening the store for a live composer returns: the store,
/// the config to compose with, the tally the store charges when a
/// ceiling is configured, and the open timeout the settings file
/// configures (the historical 30s when it names none).
type OpenedStore = (
    wyrd_format::FsObjectStore,
    LiveConfig,
    Option<std::sync::Arc<wyrd_format::RetainedBytes>>,
    Duration,
);

/// Open the object store for a live composer, wiring the retention
/// accountant when a ceiling is configured: the ceiling is a
/// comparison against a number the store keeps, so the store and the
/// config must share the same tally. Returns the store plus the config
/// to compose with (the config's `retained_bytes` is the tally the
/// store charges), plus the open timeout the settings file configures
/// (the historical 30s when it names none).
///
/// Precedence for every setting is flags, then the application
/// settings file, then compiled defaults: the flag quota wins, the
/// file quota fills a missing flag, and the file's other bounds
/// (admission, pass budgets, open timeout) apply in both arms. A
/// corrupt settings file refuses loudly — silently running a
/// misconfigured ceiling is how a bound reads as permanently zero.
pub(crate) fn open_store_with_quota(
    drive_dir: &std::path::Path,
    quota: Option<u64>,
) -> Result<OpenedStore, crate::CliError> {
    let (file_quota, rest) = wyrd_daemon::split_file_overrides(drive_dir).map_err(|reason| {
        crate::CliError::Usage(format!("application settings refused: {reason}"))
    })?;
    let effective_quota = quota.or(file_quota);
    let (store, quota_config) = match effective_quota {
        Some(limit) => {
            let (config, retained) = LiveConfig::with_retained_quota(limit);
            let store = wyrd_format::FsObjectStore::open_with(
                drive_dir.to_path_buf(),
                Some(std::sync::Arc::clone(&retained)),
            )
            .map_err(|error| crate::CliError::Store(error.to_string()))?;
            (store, config)
        }
        None => {
            let store = wyrd_format::FsObjectStore::open(drive_dir.to_path_buf())
                .map_err(|error| crate::CliError::Store(error.to_string()))?;
            (store, LiveConfig::for_local_sync())
        }
    };
    // The quota above already wired its accountant; `rest` carries
    // no quota by construction, so this cannot rebuild the
    // mismatch the composition refuses.
    let applied =
        wyrd_daemon::apply_budget_settings(&quota_config, wyrd_daemon::DEFAULT_OPEN_TIMEOUT, &rest)
            .map_err(|reason| {
                crate::CliError::Usage(format!("application settings refused: {reason}"))
            })?;
    let retained = applied.config.retained_bytes.clone();
    Ok((store, applied.config, retained, applied.open_timeout))
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
