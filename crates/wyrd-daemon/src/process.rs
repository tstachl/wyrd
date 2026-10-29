//! Live-process plumbing shared by every composer that starts a
//! live loop (`mount`, `sync now`, `vault`, and the desktop host):
//! the shutdown latch and its installer, the teardown bounds, the
//! startup retention cross-check, and the bulk-source binder.
//! Narrowly scoped on purpose — this module must never become a
//! second `main.rs`.
//!
//! The host (`wyrd-app`) composes through exactly this surface so the
//! teardown bounds and the retention check cannot silently diverge
//! between composers. The mount keeps its own `live_config` override
//! (with its unit test) in the CLI: only the pacing moves there, never
//! any ceiling.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use wyrd_core::live::{check_retained_ceiling, LiveConfig, QuotaCheckError};
use wyrd_format::FsObjectStore;

/// Process-wide shutdown latch for a live loop: signal handlers may
/// only set a flag (async-signal-safe), and the loop polls it. There
/// is exactly one per process — every composer in the process shares
/// it — so it lives here rather than in any one binary.
pub static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Arm SIGINT/SIGTERM to trip [`SHUTDOWN`]. Best-effort: if the
/// platform cannot install the handler, termination falls back to the
/// default disposition (same as dying in `fuser::mount` today).
///
/// Portability scope: validated on Linux, where the libc-crate union
/// convention (`sa_sigaction as usize`) addresses the handler union.
/// Other Unix targets keep the gate but are untested — the handler
/// touches only a lock-free flag, so the worst case is the
/// pre-existing default-disposition behavior, never memory unsafety.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn install_shutdown_handler() -> std::io::Result<()> {
    // SAFETY: the handler stores to an AtomicBool (lock-free,
    // async-signal-safe) and touches nothing else. sigaction itself
    // runs at startup on the main thread.
    unsafe extern "C" fn on_signal(_signal: libc::c_int) {
        SHUTDOWN.store(true, Ordering::Relaxed);
    }
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_signal as usize;
        libc::sigemptyset(&mut action.sa_mask);
        action.sa_flags = 0;
        for signal in [libc::SIGINT, libc::SIGTERM] {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn install_shutdown_handler() -> std::io::Result<()> {
    // No Unix credential path exists on this target (see the Windows
    // scope paragraph in `docs/architecture.md`), so no drive can be
    // opened here; without a handler the process dies on signal
    // exactly as before.
    Ok(())
}

/// How long teardown waits for the mailbox tasks to stop before
/// aborting them: bounded so shutdown never hangs on a relay outage
/// that never clears.
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(5);

/// How long teardown waits for the serving endpoint's graceful
/// close: much longer than the task-cancel bound above, because the
/// close drains in-flight transfers over the same degraded links the
/// drive syncs over — a throttled loopback legitimately needs tens of
/// seconds, and mistaking a slow close for a wedged one turns clean
/// shutdowns into mount failures. Still bounded, so a peer that
/// never answers cannot hang teardown forever; a timeout still fails
/// the run. This is a per-endpoint wedge bound, not a share of a
/// total.
pub const TRANSPORT_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(60);

/// The startup retention cross-check, shared by every composer that
/// starts a live node: a quota below what the mounted store already
/// holds would refuse every write, so refuse to start with both
/// numbers instead of discovering it per write. Unset quotas skip
/// the walk entirely.
pub fn check_startup_retention(
    config: &LiveConfig,
    store: &FsObjectStore,
) -> Result<(), QuotaCheckError> {
    if let Some(quota) = config.budgets.retained_bytes_quota {
        check_retained_ceiling(quota, store)?;
    }
    Ok(())
}

/// Bind the fetch side's iroh endpoint (N0 relays for peer
/// reachability) and wrap it in the real bulk source.
pub fn bind_bulk_source() -> std::io::Result<wyrd_sync::bulk::IrohBulkSource> {
    wyrd_sync::bulk::IrohBulkSource::connect_default()
}

#[cfg(test)]
mod tests {

    /// The non-Unix shutdown handler must stay a deliberate no-op:
    /// without it the process dies on signal exactly as before, and
    /// a future edit must not silently install signal handling where
    /// no drive can open. Source-text form because the shape (a
    /// deliberate early return, not an omission) is the contract.
    #[test]
    fn non_unix_handler_stays_a_deliberate_no_op() {
        const PROCESS: &str = include_str!("process.rs");
        assert!(
            PROCESS.contains("#[cfg(not(unix))]\npub fn install_shutdown_handler"),
            "the non-Unix shutdown handler must stay a deliberate no-op"
        );
    }
}
