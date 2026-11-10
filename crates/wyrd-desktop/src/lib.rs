//! The desktop management UI backend: a non-owning client of the
//! drive-owner interface.
//!
//! The UI never owns a drive. The independently installed owner holds
//! the store and serves `owner.sock`; this crate forwards operations
//! through the shared [`OwnerClient`] and interprets nothing — the
//! same rule as the interface itself
//! (`docs/contracts/owner-interface.md`).
//!
//! One [`DesktopBackend`] per drive connection, held under exclusive
//! access: the shared client is sequential (`&mut self` per exchange)
//! and poisons itself on the first unconfirmed answer, so concurrent
//! callers serialize on the backend's single instance. Recovery is a
//! new connection ([`DesktopBackend::reconnect`]), never a resumed
//! stream.
//!
//! The client is synchronous; the Tauri commands in
//! [`commands`] run each exchange on the blocking pool, so a
//! 120-second response wait never stalls the event loop.
//!
//! The `tauri` runtime link lives here: `build.rs` embeds the
//! application context (`tauri.conf.json` plus `capabilities/`),
//! and [`invoke_handler`] registers the command surface exactly
//! once. The Linux GUI closure (WebKitGTK system dependencies)
//! rides with this link and is proven by the Linux builder, not
//! from macOS.

pub mod backend;
pub mod commands;
pub mod registry;
pub mod settings_view;

pub use backend::{BackendError, DesktopBackend};
pub use registry::{DriveAvailability, DriveView, RegistryState, RegistryStatus};
pub use settings_view::{DriveSettings, DriveSettingsView};

/// The managed application state: the single backend owner, held
/// under exclusive access. Commands reach it through an owned
/// [`tauri::AppHandle`] inside their blocking task — never a
/// borrowed `State` reference across the boundary. The alias pins
/// the exact `State` type: a wrong `State` type fails at runtime
/// (`state not managed`), not compile time.
pub type AppState = std::sync::Mutex<DesktopBackend>;

/// Outstanding IPC work, counted from dispatch to answer. The quit
/// drain reads this — never the backend itself — so waiting never
/// touches the owner connection: draining is a shell concern, and
/// the count covers every command including the local registry and
/// settings queries.
#[derive(Debug, Clone, Default)]
pub struct FlightTracker {
    inner: std::sync::Arc<FlightTrackerInner>,
}

#[derive(Debug, Default)]
struct FlightTrackerInner {
    outstanding: std::sync::atomic::AtomicUsize,
}

impl FlightTracker {
    /// A fresh tracker with nothing outstanding.
    pub fn new() -> Self {
        FlightTracker {
            inner: std::sync::Arc::new(FlightTrackerInner {
                outstanding: std::sync::atomic::AtomicUsize::new(0),
            }),
        }
    }

    /// One exchange dispatched. Callers hold the returned guard for
    /// the exchange's whole life — dispatch queue included — so the
    /// drain waits for work the runtime accepted but has not run yet.
    /// The guard owns its count (no borrow of the app state), so it
    /// moves freely into the blocking task.
    pub(crate) fn enter(&self) -> FlightGuard {
        self.inner
            .outstanding
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        FlightGuard {
            tracker: self.clone(),
        }
    }

    /// How many exchanges are dispatched but unanswered.
    pub fn outstanding(&self) -> usize {
        self.inner
            .outstanding
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The RAII half of [`FlightTracker::enter`]: dropping settles the
/// dispatch, exactly once, on every path including panics.
pub(crate) struct FlightGuard {
    tracker: FlightTracker,
}

impl Drop for FlightGuard {
    fn drop(&mut self) {
        self.tracker
            .inner
            .outstanding
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The registered command handler: the authoritative application
/// IPC surface. Exactly one `generate_handler!` call, covering the
/// [`commands`] surface — never several `invoke_handler` calls.
/// Generic over the runtime so the mock runtime drives the same
/// handler in tests.
///
/// The local registry and settings queries return their state
/// structs directly instead of the owner taxonomy: a dead blocking
/// worker there is a shell bug, not an owner answer, so the failure
/// surfaces as an explicit unreadable state inside the payload. Any
/// other rejection of those commands is a transport failure, never
/// an owner answer.
pub fn invoke_handler<R: tauri::Runtime>() -> impl Fn(tauri::ipc::Invoke<R>) -> bool {
    tauri::generate_handler![
        commands::lookup,
        commands::stat,
        commands::readdir,
        commands::read,
        commands::observe,
        commands::liveness,
        commands::freshness,
        commands::capture_parent,
        commands::submit,
        commands::reconcile_lookup,
        commands::reconnect,
        commands::list_drives,
        commands::drive_settings,
    ]
}
