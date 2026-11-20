#![warn(missing_docs)]
//! The desktop management UI backend: a non-owning client of the
//! drive-owner interface.
//!
//! The UI never owns a drive. The independently installed owner holds
//! the store and serves `owner.sock`; this crate forwards operations
//! through the shared [`wyrd_owner_client::OwnerClient`] and interprets nothing — the
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
pub mod onboarding;
pub mod provision;
pub mod registry;
pub mod service;
pub mod session;
pub mod settings_apply;
pub mod settings_view;

pub use backend::{BackendError, DesktopBackend};
pub use onboarding::{KeyDefaults, OnboardingReport, OnboardingResult};
pub use provision::{LeaveRequest, ProvisionRequest, ProvisionResult, PROVISION_TIMEOUT};
pub use registry::{
    DriveAvailability, DriveView, RegistryEntryWrite, RegistryState, RegistryStatus,
    RegistryWriteError, SessionState,
};
pub use service::EnablementState;
pub use service::ServiceState;
pub use session::{MAX_UNLOCK_RESTARTS, UNLOCK_POLL_INTERVAL, UNLOCK_RESTART_BACKOFF};
pub use settings_apply::{
    SettingsApplyOutcome, SettingsApplyRequest, SettingsApplyResult, SETTINGS_APPLY_TIMEOUT,
};
pub use settings_view::{DriveSettings, DriveSettingsView};

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The managed application state: the drive connection map plus the
/// provisioning flight map.
///
/// One [`DesktopBackend`] per drive, keyed by the absolute drive
/// directory as an exact string. The map tracks connections to
/// owners — never durable stores, never ownership. Each backend
/// holds its connection under exclusive access, and commands clone
/// the drive's `Arc` out of the map under a short lock, then run
/// without holding the map: one drive's poisoned connection or slow
/// owner never blocks another drive's commands. Application-wide
/// failure and individual drive failure stay distinct states.
///
/// Provisioning flights serialize per drive: at most one CLI
/// lifecycle operation runs on a drive at a time. A second request
/// while one is in flight is refused with an explicit busy state,
/// never queued behind a service restart.
///
/// Unlock sessions serialize the same way through the flight map:
/// `drive_unlock` claims the flight for its verify window, and
/// `drive_lock` claims it to take the session — so a lock can
/// never race an unlock's store, and the monitor's respawns (which
/// never take the flight) can never interleave with either.
pub struct AppState {
    inner: Mutex<BTreeMap<PathBuf, Arc<DesktopBackend>>>,
    flights: Mutex<HashSet<PathBuf>>,
    /// Explicit CLI binary for tests. Production stays `None` so
    /// every operation resolves the bundled co-installation first
    /// (`Contents/Resources/wyrd`, `MacOS` sibling as the dev
    /// fallback) — the frontend can never name an arbitrary binary
    /// (no IPC command sets this; only Rust callers, i.e. tests, do).
    cli_override: Mutex<Option<PathBuf>>,
    /// App-held unlock sessions, one per unlocked drive: the
    /// in-memory passphrase plus the supervised owner child. Never
    /// persisted, never logged, never IPC'd back out. Locking
    /// removes the entry (child killed, secret scrubbed); the
    /// monitor removes it past the restart budget.
    sessions: Mutex<HashMap<PathBuf, session::UnlockSession>>,
    /// Monotonic session ids: each stored unlock mints one, and a
    /// monitor exits where its drive's entry carries another — so a
    /// stale watcher can never respawn a locked drive.
    generation: std::sync::atomic::AtomicU64,
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
            flights: Mutex::new(HashSet::new()),
            cli_override: Mutex::new(None),
            sessions: Mutex::new(HashMap::new()),
            generation: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Point the provisioning adapter at an explicit CLI binary.
    /// Test-only seam: production never calls this, so the shipped
    /// shell always resolves the fixed co-installed sibling and the
    /// frontend — which has no IPC command reaching this — can never
    /// name an arbitrary binary.
    pub fn set_cli_override(&self, path: PathBuf) {
        let mut held = self
            .cli_override
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *held = Some(path);
    }

    /// The CLI binary one provisioning run uses: the test override
    /// where set, else the bundled co-installation first with the
    /// `MacOS` sibling as the dev fallback (never `PATH`).
    pub(crate) fn cli_bin(&self) -> Result<PathBuf, String> {
        let held = self
            .cli_override
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(path) = held.clone() {
            return Ok(path);
        }
        drop(held);
        provision::resolve_cli_bin(None)
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
        // The value checked is the value keyed: refuse paths that
        // are not valid UTF-8 outright, and store the `PathBuf` the
        // rule returns — never the raw path beside it.
        let validated = match backend.drive_dir().to_str() {
            Some(dir) => validate_drive_path(dir, "drive_dir")?,
            None => {
                return Err(format!(
                    "drive_dir {} is not valid UTF-8",
                    backend.drive_dir().display()
                ));
            }
        };
        let mut held = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        held.insert(validated, Arc::new(backend));
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

    /// Forget the cached connection for one drive without replacing
    /// it: the next exchange mints a fresh connection on demand.
    /// Called wherever an operation may replace the serving owner —
    /// unlock spawning a new owner, lock killing the app-held one,
    /// owner-restarting provision operations — so the first probe
    /// past the restart never runs on the pre-restart stream (which
    /// would break, poison the client, and force a manual reconnect
    /// for every later probe). Map removal only, never IO: a failed
    /// or refused operation evicts just as safely, and in-flight
    /// exchanges hold their own `Arc` and complete undisturbed.
    pub fn evict_drive(&self, drive_dir: &Path) {
        let mut held = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        held.remove(drive_dir);
    }

    /// Try to claim the lifecycle flight for one drive: at most one
    /// CLI operation runs on a drive at a time. Returns `true` when
    /// the caller owns the flight and must call
    /// [`AppState::release_flight`] after the process completes;
    /// `false` when another operation is already in flight — the
    /// caller refuses with an explicit busy state, never queues.
    pub fn claim_flight(&self, drive_dir: &std::path::Path) -> bool {
        let mut held = self
            .flights
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        held.insert(drive_dir.to_owned())
    }

    /// Release a claimed lifecycle flight, exactly once per
    /// successful [`AppState::claim_flight`].
    pub fn release_flight(&self, drive_dir: &std::path::Path) {
        let mut held = self
            .flights
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        held.remove(drive_dir);
    }

    /// The app-held unlock sessions: the in-memory passphrase plus
    /// the supervised owner child per unlocked drive. The session
    /// module owns the lifecycle; this only exposes the map.
    pub(crate) fn sessions(&self) -> &Mutex<HashMap<PathBuf, session::UnlockSession>> {
        &self.sessions
    }

    /// Mint the next session generation: each stored unlock carries
    /// a fresh id, and a monitor exits where its drive's entry
    /// carries another.
    pub(crate) fn next_generation(&self) -> u64 {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// The drive-identity rule, shared by every drive-identity
/// surface in the crate: the absolute drive directory as an exact
/// string — the shared spelling rule from `wyrd_settings`, so the
/// command boundary, the registry, and the settings keys can never
/// drift. Empty, relative, `..`-carrying, and control-byte
/// spellings all refuse with their reason, so no caller resolves
/// against the process directory and no non-canonical spelling keys
/// a second map entry for one drive.
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
    wyrd_settings::validate_drive_spelling(value, noun)?;
    Ok(PathBuf::from(value))
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
        commands::drive_settings_write,
        commands::provision_create,
        commands::provision_join,
        commands::provision_leave,
        commands::provision_reconfigure,
        commands::provision_service_start,
        commands::provision_service_enable,
        commands::provision_service_disable,
        commands::drive_unlock,
        commands::drive_lock,
        commands::onboarding_defaults,
        commands::onboarding_new_passphrase,
        commands::onboarding_commit,
        commands::onboarding_verify,
        commands::onboarding_adopt,
        commands::reveal_in_folder,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The drive-identity rule refuses exactly the settings-key
    /// violations: empty, relative, `..`, and control bytes — while
    /// an absolute clean path validates. It delegates to the shared
    /// `wyrd_settings` spelling rule, so this test pins the unity
    /// structurally: the two surfaces cannot drift. No caller
    /// resolves against the process directory.
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

    /// Evicting a drive with no cached connection is a silent no-op:
    /// unlock, lock, and provisioned restarts evict unconditionally
    /// (map removal only, never IO), so the absent-entry case must
    /// never fail. The positive case — a stale entry stops being
    /// served — pins through the `drive_unlock` integration test,
    /// which needs a real socket the unit context cannot stage.
    #[test]
    fn evict_drive_without_a_cached_connection_is_a_noop() {
        let state = AppState::new();
        state.evict_drive(Path::new("/drives/alpha"));
    }
}
