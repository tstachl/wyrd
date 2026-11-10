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

pub use backend::{BackendError, DesktopBackend};

/// The managed application state: the single backend owner, held
/// under exclusive access. Commands reach it through an owned
/// [`tauri::AppHandle`] inside their blocking task — never a
/// borrowed `State` reference across the boundary. The alias pins
/// the exact `State` type: a wrong `State` type fails at runtime
/// (`state not managed`), not compile time.
pub type AppState = std::sync::Mutex<DesktopBackend>;

/// The registered command handler: the authoritative application
/// IPC surface. Exactly one `generate_handler!` call, covering the
/// [`commands`] surface — never several `invoke_handler` calls.
pub fn invoke_handler() -> impl Fn(tauri::ipc::Invoke) -> bool {
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
    ]
}
