//! Owner handles and service lifecycle: bind the transport,
//! publish the socket paths, and shut down cleanly. The parent
//! module owns the connection loop and re-exports the public
//! service types.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "macos")]
use std::time::Duration;

use wyrd_core::live::{LiveParts, SharedObservation};
use wyrd_core::mutation::MutationQueue;
use wyrd_core::policy_channel::PolicyChannel;
use wyrd_core::projection::{SharedFreshness, SharedProjection};
use wyrd_core::quarantine::QuarantineQueue;
use wyrd_core::scrub::ScrubQueue;
use wyrd_core::wake::WakeSignal;
use wyrd_core::want::WantRegistry;
use wyrd_format::DriveId;

use crate::diagnostics::DiagnosticsController;
use crate::vault::OwnerView;

use super::socket::{
    bind_group_listener, bind_listener, group_container_from_env, publish_group_symlink,
    DEFAULT_MAX_READ_BYTES,
};
use super::socket::{
    group_label_path, group_socket_path, group_symlink_path, production_group_container,
};
use super::OwnerServer;
use crate::owner::FILEPROVIDER_DRIVE_ID_ENV;
/// The serving handles the owner interface forwards through. Built
/// by the composer from the split parts — never by opening the
/// store, never by constructing a mailbox. Every handle is shared
/// state the loop already publishes; the server only reads.
pub struct OwnerHandles {
    /// The published serving generations.
    pub projection: SharedProjection<OwnerView>,
    /// Demand the read side registers; the loop admits it each pass.
    pub wants: Arc<WantRegistry>,
    /// The single mutation queue the submit side forwards to. The
    /// interface interprets nothing: the queue owns admission,
    /// ordering, and every refusal.
    pub mutations: Arc<MutationQueue>,
    /// The policy channel the four policy operations forward to. The
    /// interface interprets nothing: the loop-thread executor owns
    /// execution, and every refusal carries through as its own code.
    /// Same engine, same thread as the mutation queue — four engine
    /// operations, not mutations.
    pub policy: Arc<PolicyChannel>,
    /// The freshness judgment over the served generation.
    pub freshness: SharedFreshness,
    /// The latest owner observation, if any pass completed.
    pub observation: SharedObservation,
    /// Repair submission for observed-invalid bytes. `None` reads
    /// rejections untouched, like a standalone backend without a
    /// loop to repair through.
    pub quarantine: Option<Arc<QuarantineQueue>>,
    /// Repair submission for observed losses. `None` leaves losses
    /// surfaced, like a standalone backend.
    pub scrub: Option<Arc<ScrubQueue>>,
    /// The loop's pause switch, shared with the loop like the
    /// projection slot (OD-19-D): the owner surface sets it, the loop
    /// reads it once per pass. Process-local only.
    pub paused: Arc<AtomicBool>,
    /// The loop's pacing signal. `set_paused` pokes it so the loop
    /// re-checks promptly instead of waiting out its idle deadline;
    /// `None` (tests that never run a loop) stores bare, which defers
    /// only, never incorrectly.
    pub waker: Option<Arc<WakeSignal>>,
    /// Largest whole-file read served in one request.
    pub max_read_bytes: u64,
    /// The owner's process-local diagnostics state. `diagnostics-set`
    /// and `diagnostics-status` read and change this handle; the
    /// settings path and mirror are owned by the controller, never
    /// by the wire.
    pub diagnostics: Arc<DiagnosticsController>,
}

impl OwnerHandles {
    /// Build from the split parts, with repair channels unwired and
    /// the default read bound. The composer wires repair from the
    /// loop's queues the way the mount composer wires its backend.
    pub fn from_parts(parts: &LiveParts<OwnerView>) -> Self {
        OwnerHandles {
            projection: Arc::clone(&parts.projection),
            wants: Arc::clone(&parts.wants),
            mutations: Arc::clone(&parts.mutations),
            policy: Arc::clone(&parts.policy),
            freshness: Arc::clone(&parts.freshness),
            observation: Arc::clone(&parts.observation),
            quarantine: None,
            scrub: None,
            paused: Arc::clone(&parts.paused),
            waker: None,
            max_read_bytes: DEFAULT_MAX_READ_BYTES,
            diagnostics: Arc::new(DiagnosticsController::inert()),
        }
    }

    /// Wire a real diagnostics controller. Production composers pass
    /// one built from the platform's settings path and app-group
    /// container; fixtures that never persist settings keep the
    /// inert default.
    pub fn with_diagnostics(mut self, diagnostics: Arc<DiagnosticsController>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    /// Wire the loop's pacing signal, like the mount composer wires
    /// its backend beside `shared_with_wants`. The composer always
    /// wires this in production, so `set_paused` pokes; tests that
    /// never run a loop may skip it and store bare.
    pub fn with_waker(mut self, waker: Arc<WakeSignal>) -> Self {
        self.waker = Some(waker);
        self
    }

    /// Set the pause switch with the pacing-signal poke, mirroring
    /// [`LiveNode::set_paused`](wyrd_core::live::LiveNode::set_paused):
    /// the loop re-checks promptly rather than waiting out its idle
    /// deadline. Idempotent; the last writer wins.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
        if let Some(waker) = &self.waker {
            waker.wake();
        }
    }

    /// Wire the loop's repair queues, like the mount composer wires
    /// its backend beside `shared_with_wants`.
    pub fn with_repair(mut self, quarantine: Arc<QuarantineQueue>, scrub: Arc<ScrubQueue>) -> Self {
        self.quarantine = Some(quarantine);
        self.scrub = Some(scrub);
        self
    }

    /// Build from the live split the way every owner-mode composer
    /// does: the shared projection, wants, mutation queue, policy
    /// channel, freshness, observation, and pause switch from the
    /// parts, plus the loop's repair queues and pacing signal. The
    /// mount and the headless runner share this constructor so the
    /// socket serves the same handles either presentation serves.
    pub fn for_live(
        parts: &LiveParts<OwnerView>,
        quarantine: Arc<QuarantineQueue>,
        scrub: Arc<ScrubQueue>,
        waker: Arc<WakeSignal>,
    ) -> Self {
        Self::from_parts(parts)
            .with_repair(quarantine, scrub)
            .with_waker(waker)
    }
}

/// Group endpoints bound after the primary listener was already
/// serving. The app normally creates the App Group container before
/// starting an owner; this late path is the safety net for the other
/// ordering (owner at login, app later).
#[derive(Default)]
struct LateGroup {
    socket: Option<PathBuf>,
    symlink: Option<PathBuf>,
    label: Option<PathBuf>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The bound owner interface for one run: the socket path, its stop
/// flag, and the serving thread. Both owner-mode composers (the
/// headless runner and the mount) bind through here, so the socket
/// lifecycle is one copy: bind-before-serve (an owner that cannot
/// serve refuses instead of running deaf), stop-with-the-loop, and
/// remove-on-the-way-out so a later owner never inherits a stale
/// entry. The guard stops on drop, so an early return never leaks a
/// serving socket behind a dead owner.
pub struct OwnerService {
    socket_path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    group_socket_path: Option<PathBuf>,
    group_thread: Option<std::thread::JoinHandle<()>>,
    group_symlink: Option<PathBuf>,
    group_label: Option<PathBuf>,
    retry_thread: Option<std::thread::JoinHandle<()>>,
    late_group: Arc<Mutex<LateGroup>>,
    /// Whether the socket file has been removed. Shutdown removes at
    /// most once: after an explicit shutdown the drop is a no-op, so
    /// a late drop can never unlink a successor owner's socket file
    /// bound at the same path.
    removed: bool,
}

/// Bind the group socket and publish its alias plus a label file.
/// Creates only `Sockets/` inside the resolved container, never the
/// container itself — a hand-created container is a decoy the
/// extension cannot reach. Shared by the startup bind and the late
/// retry path.
fn bind_group_endpoints(
    group_container: &Path,
    drive: &DriveId,
    drive_dir: &Path,
) -> std::io::Result<(PathBuf, PathBuf, PathBuf, std::os::unix::net::UnixListener)> {
    let group_path = group_socket_path(group_container, drive);
    let link_path = group_symlink_path(group_container, drive);
    let label_path = group_label_path(group_container, drive);
    if let Some(sockets) = group_path.parent() {
        std::fs::create_dir_all(sockets)?;
    }
    let listener = bind_group_listener(&group_path)?;
    let fingerprint_file_name = format!("{}.sock", drive.fileprovider_socket_fingerprint());
    if let Err(error) = publish_group_symlink(&link_path, &fingerprint_file_name) {
        let _ = std::fs::remove_file(&group_path);
        return Err(error);
    }
    // The drive directory's basename is the registry name. Only a
    // single path component is ever published as control metadata.
    let name = drive_dir.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "drive dir has no name")
    })?;
    if let Err(error) = std::fs::write(&label_path, name.as_encoded_bytes()) {
        let _ = std::fs::remove_file(&link_path);
        let _ = std::fs::remove_file(&group_path);
        return Err(error);
    }
    Ok((group_path, link_path, label_path, listener))
}

impl OwnerService {
    /// Bind the drive-directory socket and serve the shared handles
    /// until [`OwnerService::shutdown`]. The caller holds the store
    /// lock (single owner by kernel enforcement), so no competing
    /// binder races the stale-socket reclaim.
    pub fn bind(handles: OwnerHandles, drive_dir: &Path) -> std::io::Result<Self> {
        let server = OwnerServer::new(handles);
        let socket_path = crate::owner::socket_path_for(drive_dir);
        let listener = bind_listener(&socket_path)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || server.serve_on(&listener, &stop))
        };
        Ok(OwnerService {
            socket_path,
            stop,
            thread: Some(thread),
            group_socket_path: None,
            group_thread: None,
            group_symlink: None,
            group_label: None,
            retry_thread: None,
            late_group: Arc::new(Mutex::new(LateGroup::default())),
            removed: false,
        })
    }

    /// Bind the drive-directory socket **and** the App Group socket
    /// for the drive, serving both from one
    /// [`OwnerServer`] (one owner-instance identity, one uid check).
    /// Private: composers enter through [`OwnerService::bind_for_composer`],
    /// which checks a dev-configured drive against the served one
    /// before anything binds. All-or-nothing: a group-side failure
    /// unbinds the primary and refuses, so the owner never runs
    /// half-bound — deaf to the extension but alive to the CLI.
    /// The caller holds the store lock throughout, as with
    /// [`OwnerService::bind`]. Shared by the production auto-bind
    /// (container from the platform convention, drive from the
    /// served drive) and the dev-only env bootstrap.
    fn bind_with_group_container(
        handles: OwnerHandles,
        drive_dir: &Path,
        drive: &DriveId,
        group_container: &Path,
    ) -> std::io::Result<Self> {
        let server = Arc::new(OwnerServer::new(handles));
        let socket_path = crate::owner::socket_path_for(drive_dir);
        let listener = bind_listener(&socket_path)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (group_path, link_path, label_path, group_listener) =
            match bind_group_endpoints(group_container, drive, drive_dir) {
                Ok(bound) => bound,
                Err(error) => {
                    let _ = std::fs::remove_file(&socket_path);
                    return Err(error);
                }
            };
        let thread = {
            let (server, stop, listener) = (Arc::clone(&server), Arc::clone(&stop), listener);
            std::thread::spawn(move || server.serve_on(&listener, &stop))
        };
        let group_thread = {
            let (server, stop) = (Arc::clone(&server), Arc::clone(&stop));
            std::thread::spawn(move || server.serve_on(&group_listener, &stop))
        };
        tracing::info!(
            primary = %socket_path.display(),
            group = %group_path.display(),
            link = %link_path.display(),
            "owner interface serving drive-directory and fileprovider sockets"
        );
        Ok(OwnerService {
            socket_path,
            stop,
            thread: Some(thread),
            group_socket_path: Some(group_path),
            group_thread: Some(group_thread),
            group_symlink: Some(link_path),
            group_label: Some(label_path),
            retry_thread: None,
            late_group: Arc::new(Mutex::new(LateGroup::default())),
            removed: false,
        })
    }

    /// Primary-only bind with a late group retry. Used when the App
    /// Group container is not there yet: the app normally creates it
    /// before starting the owner, but the owner also starts at login
    /// where the ordering is not guaranteed. The retry shares the
    /// already-serving `OwnerServer` — it never rebinds the primary
    /// socket and never manufactures an alias itself.
    #[cfg(target_os = "macos")]
    fn bind_with_group_retry(
        handles: OwnerHandles,
        drive_dir: &Path,
        drive: DriveId,
    ) -> std::io::Result<Self> {
        let server = Arc::new(OwnerServer::new(handles));
        let socket_path = crate::owner::socket_path_for(drive_dir);
        let listener = bind_listener(&socket_path)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (server, stop) = (Arc::clone(&server), Arc::clone(&stop));
            std::thread::spawn(move || server.serve_on(&listener, &stop))
        };
        let late_group = Arc::new(Mutex::new(LateGroup::default()));
        let retry_stop = Arc::clone(&stop);
        let retry_late = Arc::clone(&late_group);
        let retry_server = Arc::clone(&server);
        let retry_drive_dir = drive_dir.to_path_buf();
        let retry_thread = std::thread::spawn(move || {
            // Bounded: a present-but-unbindable group path must not
            // leave the owner looking healthy while permanently deaf.
            // A minute is ample for the app to create the container at
            // launch; after that the run stays primary-only with a
            // terminal, visible verdict.
            for _ in 0..30 {
                if retry_stop.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(container) = production_group_container() {
                    match bind_group_endpoints(&container, &drive, &retry_drive_dir) {
                        Ok((group_path, link_path, label_path, group_listener)) => {
                            let group_thread = {
                                let (server, stop) =
                                    (Arc::clone(&retry_server), Arc::clone(&retry_stop));
                                std::thread::spawn(move || server.serve_on(&group_listener, &stop))
                            };
                            let mut late = retry_late.lock().unwrap_or_else(|p| p.into_inner());
                            late.socket = Some(group_path);
                            late.symlink = Some(link_path);
                            late.label = Some(label_path);
                            late.thread = Some(group_thread);
                            return;
                        }
                        Err(error) => {
                            tracing::warn!(%error, "late App Group bind failed; will retry");
                        }
                    }
                }
                // Sleep in short slices so shutdown never waits out a
                // long tick before joining this thread.
                for _ in 0..20 {
                    if retry_stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            tracing::error!(
                "App Group container did not become bindable; owner remains primary-only"
            );
        });
        Ok(OwnerService {
            socket_path,
            stop,
            thread: Some(thread),
            group_socket_path: None,
            group_thread: None,
            group_symlink: None,
            group_label: None,
            retry_thread: Some(retry_thread),
            late_group,
            removed: false,
        })
    }

    /// The bound socket path, for diagnostics and tests.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// The bound App Group socket path, when
    /// `OwnerService::bind_with_group_container` configured one.
    pub fn group_socket_path(&self) -> Option<&Path> {
        self.group_socket_path.as_deref()
    }

    /// Bind for an owner-mode composer (`wyrd owner`, `wyrd mount`):
    /// the drive-directory socket, plus the App Group socket through
    /// the registry-owned production association. With the dev-only
    /// env bootstrap set, the configured drive is checked against the
    /// drive actually served *before* anything binds: a stale or
    /// mistyped `WYRD_FILEPROVIDER_DRIVE_ID` refuses the run instead
    /// of publishing another drive's tree under this drive's alias.
    /// With both env vars unset (production), the container derives
    /// from the platform convention and the drive from the served
    /// drive itself — no env-var fallback, so a clean machine with
    /// no Wyrd env vars still publishes the alias the File Provider
    /// domain resolves through. Absent container (Linux, CI, or a
    /// machine whose group container was never materialised) means
    /// the drive-directory socket only. Malformed or mismatched
    /// dev configuration refuses; it never falls back silently.
    pub fn bind_for_composer(
        handles: OwnerHandles,
        drive_dir: &Path,
        served: DriveId,
    ) -> std::io::Result<Self> {
        match group_container_from_env() {
            None => match production_group_container() {
                Some(container) => {
                    Self::bind_with_group_container(handles, drive_dir, &served, &container)
                }
                // The app is expected to have created the container
                // first; this path keeps a login-time owner from
                // staying permanently deaf if it did not. Only macOS
                // has an App Group container to wait for.
                #[cfg(target_os = "macos")]
                None => Self::bind_with_group_retry(handles, drive_dir, served),
                #[cfg(not(target_os = "macos"))]
                None => Self::bind(handles, drive_dir),
            },
            Some(Ok(config)) => {
                if config.drive != served {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!(
                            "{} names drive {} but this owner serves {}: refusing the group bind",
                            FILEPROVIDER_DRIVE_ID_ENV, config.drive, served
                        ),
                    ));
                }
                Self::bind_with_group_container(
                    handles,
                    drive_dir,
                    &served,
                    &config.group_container,
                )
            }
            Some(Err(detail)) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                detail,
            )),
        }
    }

    /// Stop serving, join the accept thread, and remove the socket
    /// file. Idempotent: a second call (including the drop) neither
    /// rejoins nor re-removes. A thread that ended unexpectedly logs,
    /// never panics — the run's own outcome already carries the
    /// verdict. Call while the store lock is still held (after the
    /// transport tail, before the live node drops): the removal must
    /// precede the lock release, or a successor owner binding the
    /// same path races this unlink.
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::warn!("owner interface thread ended unexpectedly");
            }
        }
        if let Some(thread) = self.group_thread.take() {
            if thread.join().is_err() {
                tracing::warn!("owner interface group thread ended unexpectedly");
            }
        }
        if let Some(thread) = self.retry_thread.take() {
            if thread.join().is_err() {
                tracing::warn!("owner interface group retry thread ended unexpectedly");
            }
        }
        {
            let mut late = self.late_group.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(thread) = late.thread.take() {
                if thread.join().is_err() {
                    tracing::warn!("late owner interface group thread ended unexpectedly");
                }
            }
            if let Some(link) = late.symlink.take() {
                let _ = std::fs::remove_file(&link);
            }
            if let Some(path) = late.socket.take() {
                let _ = std::fs::remove_file(&path);
            }
            if let Some(label) = late.label.take() {
                let _ = std::fs::remove_file(&label);
            }
        }
        if !self.removed {
            self.removed = true;
            if let Some(label) = self.group_label.take() {
                let _ = std::fs::remove_file(&label);
            }
            if let Some(link) = self.group_symlink.take() {
                let _ = std::fs::remove_file(&link);
            }
            if let Some(path) = self.group_socket_path.take() {
                let _ = std::fs::remove_file(&path);
            }
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

impl Drop for OwnerService {
    fn drop(&mut self) {
        self.shutdown();
    }
}
