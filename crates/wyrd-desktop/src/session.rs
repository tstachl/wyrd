//! Per-session unlock sessions: the app-held owner lifecycle for
//! per-session (`--no-service`) drives.
//!
//! The CLI's `drive unlock` runs the persistent owner in the
//! foreground with a passphrase from the stdin pipe — nothing
//! installed, nothing rewritten, so a reboot or an explicit lock
//! locks the drive again. The backend spawns that owner, holds its
//! stdin-piped secret in memory (`Zeroizing`, never persisted,
//! never logged, never IPC'd back out), and supervises the child
//! while the app lives: unexpected death restarts a bounded number
//! of times (R1), then falls back to manual unlock. An app crash
//! never takes a running owner down (the child survives the
//! backend's death); relaunch reattaches through the socket probe,
//! and both-dead re-prompts (R2).
//!
//! Discipline, inherited from the provisioning adapter:
//!
//! - The monitor never retries destructively: a timeout or an early
//!   exit resolves through the paired observation, never through
//!   re-execution of the unlock — respawns only replace a dead
//!   child with the stored secret, up to the restart budget.
//! - `drive_lock` is the only killer, and it only kills children
//!   this app spawned (handles held in the session map): a
//!   reattached owner the app never started is refused, never
//!   killed — the shell stays non-owning.
//! - The monitor thread is not IPC work: it holds no flight guard,
//!   so the quit drain never waits on a persistent owner. Quit
//!   leaves supervised children running (R2 reattach); only an
//!   explicit lock kills and scrubs.
//! - Locking is synchronous under the session-map lock: the entry
//!   (child plus secret) leaves the map before the kill, so a
//!   monitor poll can never respawn a locked drive.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, Runtime};
use zeroize::Zeroizing;

use super::provision::{self, UnlockRequest};
use super::AppState;

/// Bounded unexpected-death restarts while the app lives (R1),
/// then manual unlock. The initial spawn is not a restart: the
/// first unexpected death spends 1, so a crash-looping owner costs
/// at most this many respawns before the drive reads locked again.
pub const MAX_UNLOCK_RESTARTS: u32 = 3;

/// Backoff between supervised restarts: a crash-looping owner must
/// not hot-spin the spawn pipe. Slept by the monitor thread only —
/// the per-poll sweep itself never sleeps, so tests drive it
/// directly without waiting.
pub const UNLOCK_RESTART_BACKOFF: Duration = Duration::from_secs(1);

/// How often one monitor thread polls its drive's session: death
/// detection latency, nothing else. The poll is one map lock plus
/// one non-blocking `try_wait` — never a socket probe, never a
/// spawn — so the cadence costs nothing.
pub const UNLOCK_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// One app-held unlock session: the secret that (re)starts it, the
/// child it supervises, and the argv to respawn with. `Debug` is
/// redacted so no log line carries the secret; the secret field
/// scrubs on drop.
pub(crate) struct UnlockSession {
    /// The piped passphrase, scrubbed on drop. Never persisted,
    /// never logged, never IPC'd back out.
    pub(crate) secret: Zeroizing<Vec<u8>>,
    /// The supervised owner child. `None` only between a reaped
    /// death and its supervised respawn — never observed outside
    /// the map lock.
    pub(crate) child: Option<std::process::Child>,
    /// CLI binary for supervised respawns (version-checked at
    /// unlock; respawns reuse the checked pair).
    pub(crate) cli_bin: PathBuf,
    /// Argv source for supervised respawns.
    pub(crate) request: UnlockRequest,
    /// Supervised respawns spent. Checked before respawning, so a
    /// crash loop ends in removal, never in silence.
    pub(crate) restarts: u32,
    /// Monotonic session id from [`AppState::next_generation`]:
    /// lock removes the entry, so a stale monitor (holding an
    /// older id) exits instead of respawning.
    pub(crate) generation: u64,
}

impl UnlockSession {
    pub(crate) fn new(
        secret: Zeroizing<Vec<u8>>,
        child: std::process::Child,
        cli_bin: PathBuf,
        request: UnlockRequest,
        generation: u64,
    ) -> Self {
        UnlockSession {
            secret,
            child: Some(child),
            cli_bin,
            request,
            restarts: 0,
            generation,
        }
    }
}

impl std::fmt::Debug for UnlockSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnlockSession")
            .field("secret", &"REDACTED")
            .field("child", &self.child.as_ref().map(|child| child.id()))
            .field("cli_bin", &self.cli_bin)
            .field("request", &self.request)
            .field("restarts", &self.restarts)
            .field("generation", &self.generation)
            .finish()
    }
}

/// What one monitor poll concluded. The thread loops on `Alive`
/// and `Respawned` (backing off after a respawn) and exits on
/// `Gone` (locked or superseded) and `GaveUp` (budget spent, entry
/// removed, drive reads locked again).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SweepOutcome {
    /// The child serves (or the respawn window has not resolved
    /// yet): keep watching.
    Alive,
    /// The child died and a supervised respawn replaced it: keep
    /// watching, backed off.
    Respawned,
    /// No entry for this generation: locked or superseded — stop
    /// watching.
    Gone,
    /// The budget is spent: the entry is removed (secret scrubbed)
    /// and the drive reads locked again — stop watching.
    GaveUp,
}

/// One monitor poll over one drive's session: reap a dead child
/// and respawn it from the stored secret within budget, remove the
/// entry past budget, exit where the entry is gone or superseded.
/// Never sleeps, never blocks past one non-blocking `try_wait`
/// and (on death within budget) one bounded spawn: the thread owns
/// the cadence, tests own the determinism.
///
/// Respawn failures (version skew since unlock, spawn refusal)
/// remove the entry like a spent budget: a session the app can no
/// longer supervise reads locked, never half-held.
pub(crate) fn sweep_session(
    sessions: &Mutex<HashMap<PathBuf, UnlockSession>>,
    drive_dir: &Path,
    generation: u64,
) -> SweepOutcome {
    // Death, budget, and respawn-source resolve under one lock;
    // the spawn itself runs lock-free, then reinstalls under a
    // second lock guarded by the same generation — so a lock that
    // lands mid-respawn still wins (the fresh child is killed, not
    // installed).
    enum AfterLock {
        GaveUp,
        Respawn {
            secret: Zeroizing<Vec<u8>>,
            cli_bin: PathBuf,
            request: UnlockRequest,
        },
    }
    let next = {
        let mut held = sessions.lock().unwrap_or_else(|poison| poison.into_inner());
        let Some(entry) = held.get_mut(drive_dir) else {
            return SweepOutcome::Gone;
        };
        if entry.generation != generation {
            return SweepOutcome::Gone;
        }
        let exited = match entry.child.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(status) => status.is_some(),
                // A `try_wait` failure answers nothing about the
                // child: keep watching rather than respawning over
                // a live owner or dropping a dead one unseen.
                Err(_) => false,
            },
            None => false,
        };
        if !exited {
            return SweepOutcome::Alive;
        }
        // Reaped (or already reaped): the handle can never serve
        // again, so drop it before deciding — a respawn installs a
        // fresh child, a give-up removes the secret with it.
        entry.child = None;
        if entry.restarts >= MAX_UNLOCK_RESTARTS {
            held.remove(drive_dir);
            AfterLock::GaveUp
        } else {
            entry.restarts += 1;
            AfterLock::Respawn {
                secret: entry.secret.clone(),
                cli_bin: entry.cli_bin.clone(),
                request: entry.request.clone(),
            }
        }
    };
    match next {
        AfterLock::GaveUp => SweepOutcome::GaveUp,
        AfterLock::Respawn {
            secret,
            cli_bin,
            request,
        } => match provision::spawn_unlock(&cli_bin, &request, &secret) {
            Ok(spawned) => {
                let mut held = sessions.lock().unwrap_or_else(|poison| poison.into_inner());
                match held.get_mut(drive_dir) {
                    Some(entry) if entry.generation == generation => {
                        entry.child = Some(spawned.child);
                        // The drains detach with the live owner:
                        // they consume and discard until the last
                        // pipe writer closes — the same discipline
                        // as the initial spawn.
                        SweepOutcome::Respawned
                    }
                    // Locked or superseded mid-respawn: the fresh
                    // child is killed, never installed — a locked
                    // drive stays locked.
                    _ => {
                        drop(held);
                        let mut child = spawned.child;
                        let _ = child.kill();
                        let _ = child.wait();
                        SweepOutcome::Gone
                    }
                }
            }
            Err(_) => {
                let mut held = sessions.lock().unwrap_or_else(|poison| poison.into_inner());
                if let Some(entry) = held.get(drive_dir) {
                    if entry.generation == generation {
                        held.remove(drive_dir);
                    }
                }
                SweepOutcome::GaveUp
            }
        },
    }
}

/// Watch one drive's unlock session until it is locked,
/// superseded, or past budget: poll, back off after respawns, and
/// hold no flight guard — this is supervision, not IPC work, so
/// the quit drain never waits on it. Spawned once per stored
/// session, named per drive for thread dumps.
pub(crate) fn watch_session<R: Runtime>(app: AppHandle<R>, drive_dir: PathBuf, generation: u64) {
    let _ = std::thread::Builder::new()
        .name(format!("wyrd-unlock-{}", drive_dir.display()))
        .spawn(move || loop {
            std::thread::sleep(UNLOCK_POLL_INTERVAL);
            let state = app.state::<AppState>();
            match sweep_session(state.sessions(), &drive_dir, generation) {
                SweepOutcome::Alive => {}
                SweepOutcome::Respawned => {
                    // The respawn replaced the serving owner: forget
                    // the cached connection with it, or the next
                    // probe breaks on the dead stream and poisons the
                    // client (see `AppState::evict_drive`).
                    state.evict_drive(&drive_dir);
                    std::thread::sleep(UNLOCK_RESTART_BACKOFF);
                }
                SweepOutcome::GaveUp => {
                    // The budget is spent and the owner is gone: the
                    // same stale trap as a lock, so evict with it.
                    state.evict_drive(&drive_dir);
                    return;
                }
                SweepOutcome::Gone => return,
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake `wyrd` CLI for session tests: answers the version
    /// check with the workspace version, then runs the body for
    /// every unlock argv. `exit` bodies die at once (death and
    /// budget tests); `sleep` bodies serve until killed (lock
    /// tests).
    fn fake_cli(tag: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-session-fakecli-{}-{}",
            std::process::id(),
            tag
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wyrd");
        let version = env!("CARGO_PKG_VERSION");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\n{body}\n"
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn unlock_request(dir: &Path) -> UnlockRequest {
        UnlockRequest {
            drive_dir: dir.join("drive"),
            identity_file: dir.join("identity"),
            relays: Vec::new(),
            retained_quota: None,
        }
    }

    fn store_for(
        sessions: &Mutex<HashMap<PathBuf, UnlockSession>>,
        cli: &Path,
        dir: &Path,
        generation: u64,
    ) -> PathBuf {
        let request = unlock_request(dir);
        let spawned =
            provision::spawn_unlock(cli, &request, b"hunter2-hunter2").expect("spawn holds");
        let drive_dir = request.drive_dir.clone();
        sessions.lock().unwrap().insert(
            drive_dir.clone(),
            UnlockSession::new(
                Zeroizing::new(b"hunter2-hunter2".to_vec()),
                spawned.child,
                cli.to_path_buf(),
                request,
                generation,
            ),
        );
        drive_dir
    }

    /// A session whose owner dies at once burns exactly the restart
    /// budget, then the entry is gone (secret scrubbed) and the
    /// sweeper stops: crash loops end in locked, never in silence
    /// and never in an unbounded spawn loop.
    #[test]
    fn crash_loop_burns_the_budget_then_gives_up() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-session-{}-budget",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cli = fake_cli("budget", "exit 2");
        let sessions: Mutex<HashMap<PathBuf, UnlockSession>> = Mutex::new(HashMap::new());
        let drive_dir = store_for(&sessions, &cli, &dir, 7);
        // The initial child already exited: each sweep reaps and
        // respawns until the budget is spent.
        for spent in 1..=MAX_UNLOCK_RESTARTS {
            // The dead child needs a scheduling beat before the
            // non-blocking reap sees it.
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                if sweep_session(&sessions, &drive_dir, 7) == SweepOutcome::Respawned {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "sweep {spent}: a dead child respawns"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(
                sessions.lock().unwrap().get(&drive_dir).unwrap().restarts,
                spent,
                "respawn {spent} spends one restart"
            );
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if sweep_session(&sessions, &drive_dir, 7) == SweepOutcome::GaveUp {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "a spent budget gives up"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            sessions.lock().unwrap().get(&drive_dir).is_none(),
            "a spent budget removes the session (secret scrubbed)"
        );
        assert_eq!(
            sweep_session(&sessions, &drive_dir, 7),
            SweepOutcome::Gone,
            "past removal the sweeper stops"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A removed entry stops its sweeper: lock wins over supervision,
    /// and a locked drive never respawns.
    #[test]
    fn removed_entry_reads_gone() {
        let sessions: Mutex<HashMap<PathBuf, UnlockSession>> = Mutex::new(HashMap::new());
        let dir = PathBuf::from("/drives/alpha");
        assert_eq!(
            sweep_session(&sessions, &dir, 1),
            SweepOutcome::Gone,
            "no entry stops the sweeper"
        );
        // A superseded generation stops it too: the entry belongs
        // to a newer unlock, which owns its own watcher.
        let cli = PathBuf::from("/no/such/wyrd");
        sessions.lock().unwrap().insert(
            dir.clone(),
            UnlockSession {
                secret: Zeroizing::new(b"hunter2-hunter2".to_vec()),
                child: None,
                cli_bin: cli,
                request: UnlockRequest {
                    drive_dir: dir.clone(),
                    identity_file: PathBuf::from("/keys/identity"),
                    relays: Vec::new(),
                    retained_quota: None,
                },
                restarts: 0,
                generation: 9,
            },
        );
        assert_eq!(
            sweep_session(&sessions, &dir, 8),
            SweepOutcome::Gone,
            "a superseded generation stops the stale watcher"
        );
        let _ = std::fs::remove_dir_all("/tmp/wyrd-desktop-session-unused");
    }

    /// R2, app-crash half: dropping the held session without
    /// `drive_lock` (the crash: the map is gone, no kill, no wait)
    /// leaves the owner running — only an explicit lock kills an
    /// app-held child. The relaunched app reattaches through the
    /// socket probe, which reads `Unlocked` without consulting the
    /// (empty) session map (`session_gate_pins_the_posture_rule` in
    /// `registry.rs` pins the pure rule; this pins the survival
    /// half). `sh` supplies the signal probe so the test needs no
    /// new dependency; `exec` keeps one pid so the cleanup kill
    /// leaves no orphaned sleeper behind.
    #[cfg(unix)]
    #[test]
    fn crash_drop_leaves_the_owner_running_for_reattach() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-session-{}-crashdrop",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // `exec` replaces the shell: one pid serves and dies.
        let cli = fake_cli("crashdrop", "exec sleep 30");
        let request = unlock_request(&dir);
        let spawned =
            provision::spawn_unlock(&cli, &request, b"hunter2-hunter2").expect("spawn holds");
        let pid = spawned.child.id();
        // The crash: the session map (and its child handles) drops
        // with no kill and no wait — exactly what process death does
        // to plain `std` children (no kill-on-drop, no `Drop` impl
        // on the session changes that).
        drop(spawned);
        let signal = |sig: &str| {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("kill -{sig} {pid}"))
                .status()
                .expect("sh runs the signal probe")
                .success()
        };
        assert!(
            signal("0"),
            "a dropped session leaves the owner alive for reattach"
        );
        // Cleanup kills like `drive_lock` kills.
        assert!(signal("KILL"), "cleanup kills the test owner");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `Debug` never leaks the secret: the redacted marker shows,
    /// the bytes do not.
    #[test]
    fn debug_redacts_the_secret() {
        let session = UnlockSession {
            secret: Zeroizing::new(b"hunter2-hunter2".to_vec()),
            child: None,
            cli_bin: PathBuf::from("/no/such/wyrd"),
            request: UnlockRequest {
                drive_dir: PathBuf::from("/drives/alpha"),
                identity_file: PathBuf::from("/keys/identity"),
                relays: Vec::new(),
                retained_quota: None,
            },
            restarts: 0,
            generation: 1,
        };
        let rendered = format!("{session:?}");
        assert!(rendered.contains("REDACTED"), "rendered: {rendered}");
        assert!(!rendered.contains("hunter2"), "rendered: {rendered}");
    }
}
