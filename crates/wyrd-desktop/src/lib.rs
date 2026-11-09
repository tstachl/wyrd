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
//! The client is synchronous; the Tauri commands that will call these
//! methods run them off the async runtime (blocking pool), so a
//! 120-second response wait never stalls the event loop. The exact
//! pool wiring lands with the Tauri command layer (first view); it is
//! implementation detail, but the concurrency contract above is not.
//!
//! The `tauri` crate itself is not linked yet, deliberately: the Linux
//! GUI closure (WebKitGTK system dependencies) cannot be verified
//! from this machine, and linking the toolkit before the Nix stanzas
//! in this same scaffold are proven would break the Linux gates
//! blind. The crate shape (app identity, `tauri.conf.json`, IPC
//! surface, frontend directory) and the dependency boundary (the
//! `layer_contracts` desktop policy and closure test) land here; the
//! `#[tauri::command]` wrappers land with the first view against the
//! already-pinned surface.

pub mod backend;

pub use backend::{BackendError, DesktopBackend};
