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
//! access and keyed by drive directory in [`AppState`]: the shared
//! client is sequential (`&mut self` per exchange) and poisons
//! itself on the first unconfirmed answer, so concurrent callers on
//! one drive serialize on that drive's instance. Recovery is a new
//! connection ([`AppState::reconnect_drive`]), never a resumed
//! stream — per drive, never touching other drives' connections.
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

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The managed application state: the drive connection map.
///
/// One [`DesktopBackend`] per drive, keyed by the absolute drive
/// directory as an exact string. The map tracks connections to
/// owners — never durable stores, never ownership. Each backend
/// holds its connection under exclusive access, and commands clone
/// the drive's `Arc` out of the map under a short lock, then run
/// without holding the map: one drive's poisoned connection or slow
/// owner never blocks another drive's commands. Application-wide
/// failure and individual drive failure stay distinct states.
pub struct AppState {
    inner: Mutex<BTreeMap<PathBuf, Arc<DesktopBackend>>>,
}

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

impl AppState {
    /// An empty connection map: no drive connects at startup.
    /// Commands connect lazily on first use per drive; the
    /// installer-provided `--drive-dir` pre-connects through
    /// [`AppState::insert`] where the owner answers, and startup
    /// never fails where it does not.
    pub fn new() -> Self {
        AppState {
            inner: Mutex::new(BTreeMap::new()),
        }
    }

    /// Insert a pre-connected backend (startup's `--drive-dir`).
    /// Replaces any existing connection for the same drive.
    ///
    /// The map is the chokepoint for the identity rule: the
    /// backend's drive directory is re-validated here with the
    /// shared rule, and a violation refuses the insert instead of
    /// keying a second entry for one drive. Callers that already
    /// validated (startup's argv) never trip this; it exists so no
    /// future caller can smuggle an unvalidated path into the map.
    pub fn insert(&self, backend: DesktopBackend) -> Result<(), String> {
        let drive = backend.drive_dir().to_owned();
        validate_drive_path(&drive.to_string_lossy(), "drive_dir")?;
        let mut held = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        held.insert(drive, Arc::new(backend));
        Ok(())
    }

    /// Resolve the drive directory the frontend named: the absolute
    /// drive directory as an exact string — the same rule as the
    /// settings keys (absolute, no `..`, no control bytes) and the
    /// registry entries. Anything else refuses loudly instead of
    /// resolving against the process directory: the frontend's
    /// selection is presentation state, never an authorization
    /// boundary, and the backend must not guess which drive a
    /// relative or non-canonical spelling meant. The strictness is
    /// also what keeps "one connection per drive" true: a spelling
    /// like `/drives/a/../b` must never key a second map entry for
    /// the same directory.
    ///
    /// A refusal here is a local argument refusal through the
    /// `Incompatible` kind — the fixed error channel of the owner
    /// commands — never an owner answer: no socket was touched. The
    /// frontend's taxonomy renderer labels that kind "Incompatible
    /// owner", which overstates a local refusal; that wording gap is
    /// why the local `drive_settings` query answers its explicit
    /// unreadable state instead. Unreachable through the shipped UI,
    /// which only sends registry-validated paths; the rule exists so
    /// a second integration surface can never smuggle a second
    /// meaning for one drive through this boundary.
    pub(crate) fn validate_drive_dir(drive_dir: &str) -> Result<PathBuf, BackendError> {
        match validate_drive_path(drive_dir, "drive_dir") {
            Ok(path) => Ok(path),
            Err(reason) => Err(BackendError::Incompatible { detail: reason }),
        }
    }

    /// The connection for one drive, connecting lazily on first use.
    /// The map lock covers only the lookup and the insert — never
    /// the socket connect or any owner exchange — so one drive's
    /// slow or poisoned owner never blocks another drive. A
    /// concurrent first use may connect twice; the second insert
    /// wins and the extra socket drops, which is harmless.
    pub fn connection_for(&self, drive_dir: &str) -> Result<Arc<DesktopBackend>, BackendError> {
        let path = Self::validate_drive_dir(drive_dir)?;
        {
            let held = self
                .inner
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if let Some(backend) = held.get(&path) {
                return Ok(Arc::clone(backend));
            }
        }
        let fresh = DesktopBackend::connect(&path)?;
        let mut held = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match held.get(&path) {
            Some(existing) => Ok(Arc::clone(existing)),
            None => {
                let backend = Arc::new(fresh);
                held.insert(path, Arc::clone(&backend));
                Ok(backend)
            }
        }
    }

    /// Drop the connection for one drive and mint a new instance
    /// against the same drive — that drive only. Every other
    /// drive's connection is untouched. Where no connection exists
    /// yet, this establishes one: retry after `Unavailable` must
    /// work without a prior success.
    pub fn reconnect_drive(&self, drive_dir: &str) -> Result<(), BackendError> {
        let path = Self::validate_drive_dir(drive_dir)?;
        let fresh = DesktopBackend::connect(&path)?;
        let mut held = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        held.insert(path, Arc::new(fresh));
        Ok(())
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// The drive-identity rule, shared by every drive-identity
/// surface in the crate: the absolute drive directory as an exact
/// string — the same rule as the settings keys
/// (`settings_view::settings_key_for` plus its key validation) and
/// the registry entries. Empty, relative, `..`-carrying, and
/// control-byte spellings all refuse with their reason, so no
/// caller resolves against the process directory and no
/// non-canonical spelling keys a second map entry for one drive.
///
/// `noun` names the artifact at fault in the reason text: the
/// command boundary and the registry say `drive_dir`, the settings
/// reader says `settings drive key` — the same rule, attributed to
/// the surface that rejected it.
///
/// `pub` because startup's argv validation lives in the binary
/// target outside this crate: the pre-connect exit-2 contract must
/// call the rule before any socket is touched. The structural
/// guarantee stays inside the crate at [`AppState::insert`].
pub fn validate_drive_path(value: &str, noun: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        return Err(format!("{noun} must not be empty"));
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(format!("{noun} {value:?} must be absolute"));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("{noun} {value:?} must not contain .."));
    }
    if value.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
        return Err(format!(
            "{noun} {value:?} must not contain control characters"
        ));
    }
    Ok(path)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The drive-identity rule refuses exactly the settings-key
    /// violations: empty, relative, `..`, and control bytes — while
    /// an absolute clean path validates. No caller resolves
    /// against the process directory.
    #[test]
    fn drive_identity_follows_the_settings_key_rule() {
        assert!(validate_drive_path("/drives/alpha", "drive_dir").is_ok());
        for bad in [
            "",
            "drives/alpha",
            "/drives/a/../b",
            "/drives/al\x00pha",
            "/drives/al\npha",
        ] {
            assert!(
                validate_drive_path(bad, "drive_dir").is_err(),
                "drive identity refuses {bad:?}"
            );
        }
        // The noun travels into the reason: the settings surface
        // names its artifact, not the wire argument.
        assert!(validate_drive_path("drives/alpha", "settings drive key")
            .unwrap_err()
            .contains("settings drive key"));
        // The command boundary reports the refusal through its
        // fixed error channel, never as an owner answer.
        match AppState::validate_drive_dir("drives/alpha") {
            Err(BackendError::Incompatible { detail }) => {
                assert!(detail.contains("absolute"), "detail: {detail}");
            }
            other => panic!("a relative drive refuses loudly: {other:?}"),
        }
    }
}
