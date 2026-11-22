//! Session unlock and lock commands. Unlock spawns the persistent
//! owner with the passphrase on stdin, verifies the socket, and
//! hands the child to the session monitor. Lock kills only a child
//! this app holds. Both run through the per-drive flight.

use std::path::Path;
use tauri::{AppHandle, Manager, Runtime};
use wyrd_owner_client::OwnerClient;

use crate::registry::{self, RegistryState, RegistryStatus};
use crate::FlightTracker;

use super::membership::observe_membership;
use super::provision::credential_source;
use super::{FlightRelease, ProvisionReport};

/// Unlock arguments: the drive identity, the device identity file
/// path (or the store reference), and the passphrase text. The text
/// travels backend -> CLI on the stdin pipe (never argv); the
/// frontend holds it only for the unlock entry, never persisted
/// there. `Debug` is redacted
/// so no log line carries the secret.
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveUnlockArgs {
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
    /// Credential path, never contents: the device identity file
    /// the start-up gate verified. Absent beside a store
    /// reference, which names the identity instead.
    pub identity_file: Option<String>,
    /// Credential backend selector: absent or `"file"` reads the
    /// identity file; `"os-store"` loads the identity from the
    /// drive's store item.
    pub credential_backend: Option<String>,
    /// OS credential-store account naming the drive's item (the
    /// reference, never a secret). Required exactly where the
    /// backend is `"os-store"`, refused everywhere else.
    pub credential_account: Option<String>,
    /// Passphrase text piped to the CLI on stdin, never argv.
    pub passphrase: String,
}

impl std::fmt::Debug for DriveUnlockArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriveUnlockArgs")
            .field("drive_dir", &self.drive_dir)
            .field("identity_file", &self.identity_file)
            .field("credential_backend", &self.credential_backend)
            .field("credential_account", &self.credential_account)
            .field("passphrase", &"REDACTED")
            .finish()
    }
}

/// Lock arguments: the drive identity only. Locking kills only
/// the app-held unlock child and scrubs the session secret —
/// never a reattached owner the app did not start.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveLockArgs {
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
}

/// Unlock one drive for the session: spawn the persistent owner
/// through the version-matched CLI with the passphrase on the
/// stdin pipe, verify the socket answers, then hold the child
/// under the session monitor and re-read. Infallible like the
/// other lifecycle commands: every path — including timeouts and
/// spawn failures — returns a report the paired observation
/// decides, never a throw.
#[tauri::command]
pub async fn drive_unlock<R: Runtime>(app: AppHandle<R>, args: DriveUnlockArgs) -> ProvisionReport {
    run_unlock(app, "drive_unlock", args.drive_dir.clone(), args).await
}

/// Lock one drive for the session: kill the app-held unlock child
/// and scrub the session secret, then re-read. Refuses where this
/// app holds no unlock session — a reattached owner the app did
/// not start is never killed.
#[tauri::command]
pub async fn drive_lock<R: Runtime>(app: AppHandle<R>, args: DriveLockArgs) -> ProvisionReport {
    run_lock(app, "drive_lock", args.drive_dir.clone()).await
}

/// Run one unlock exchange serialized per drive: validate the
/// identity, claim the flight (refuse `Busy` when held), spawn the
/// persistent owner, verify the socket answers within the unlock
/// bound, store the session and start its monitor, then re-read
/// the registry, probe every drive, and re-observe the operated
/// drive's membership — verification-via-reread on every path.
/// Everything runs on the blocking pool like `run_provision`. A
/// dead blocking worker resolves `Unknown`, never success: the
/// outcome is unknown by definition, and only the observed state
/// decides the display.
async fn run_unlock<R>(
    app: AppHandle<R>,
    op: &'static str,
    drive_dir: String,
    args: DriveUnlockArgs,
) -> ProvisionReport
where
    R: Runtime,
{
    let _flight = app.state::<FlightTracker>().enter();
    tauri::async_runtime::spawn_blocking(move || {
        // Validate the drive identity before touching the flight map:
        // a non-canonical spelling must never key a second flight
        // or a second session for one drive.
        let dir = match crate::validate_drive_path(&drive_dir, "drive_dir") {
            Ok(dir) => dir,
            Err(reason) => {
                return ProvisionReport {
                    result: crate::provision::ProvisionResult::ExpectedFailure { detail: reason },
                    observed: registry::list_drives(),
                    membership: None,
                };
            }
        };
        let state = app.state::<crate::AppState>();
        if !state.claim_flight(&dir) {
            return ProvisionReport {
                result: crate::provision::ProvisionResult::Busy { drive_dir },
                observed: registry::list_drives(),
                membership: observe_membership(&dir),
            };
        }
        let _release = FlightRelease::claim(&state, dir.clone());
        let outcome = unlock_exchange(&state, &app, &dir, &args);
        // The flight releases only after the observation below: the
        // observation is part of the operation, so a second request
        // on the same drive waits for the full verified picture
        // instead of interleaving with it. (The spawned owner is
        // held by the session monitor past this point, never
        // reaped here — holding here serializes observation, never
        // process overlap.)
        let report = ProvisionReport {
            result: outcome.result,
            observed: registry::list_drives(),
            membership: observe_membership(&dir),
        };
        drop(_release);
        report
    })
    .await
    .unwrap_or_else(|join| ProvisionReport {
        result: crate::provision::ProvisionResult::Unknown {
            detail: format!("ipc worker failed for {op}: {join}"),
        },
        observed: RegistryState {
            status: RegistryStatus::Unreadable {
                reason: format!("ipc worker failed for {op}: {join}"),
            },
            drives: Vec::new(),
            skipped: Vec::new(),
        },
        membership: None,
    })
}

/// Run one lock exchange serialized per drive: validate the
/// identity, claim the flight (refuse `Busy` when an unlock
/// verify is in flight), take the app-held session, kill its
/// owner, scrub its secret, then re-read and re-observe. A dead
/// blocking worker resolves `Unknown`, never success.
async fn run_lock<R>(app: AppHandle<R>, op: &'static str, drive_dir: String) -> ProvisionReport
where
    R: Runtime,
{
    let _flight = app.state::<FlightTracker>().enter();
    tauri::async_runtime::spawn_blocking(move || {
        let dir = match crate::validate_drive_path(&drive_dir, "drive_dir") {
            Ok(dir) => dir,
            Err(reason) => {
                return ProvisionReport {
                    result: crate::provision::ProvisionResult::ExpectedFailure { detail: reason },
                    observed: registry::list_drives(),
                    membership: None,
                };
            }
        };
        let state = app.state::<crate::AppState>();
        if !state.claim_flight(&dir) {
            return ProvisionReport {
                result: crate::provision::ProvisionResult::Busy { drive_dir },
                observed: registry::list_drives(),
                membership: observe_membership(&dir),
            };
        }
        let _release = FlightRelease::claim(&state, dir.clone());
        // Take the session out of the map before touching the
        // child: the kill runs lock-free, so a monitor poll can
        // never respawn a drive being locked — it sees no entry
        // and exits.
        let taken = state.sessions().lock().unwrap_or_else(|poison| poison.into_inner()).remove(&dir);
        let Some(entry) = taken else {
            return ProvisionReport {
                result: crate::provision::ProvisionResult::ExpectedFailure {
                    detail: "no unlock session held by this app for this drive: nothing to lock (a reattached owner the app did not start is never killed — stop it out-of-band)".to_string(),
                },
                observed: registry::list_drives(),
                membership: observe_membership(&dir),
            };
        };
        // Destructure to drop the secret (scrubbed) and kill the
        // child: best-effort, then reaped. An already-exited owner
        // still resolves completed — the session is gone either
        // way, and the observation says what serves now. Evict only
        // once the child is reaped: a probe landing between evict
        // and kill could otherwise cache a connection to the
        // still-alive old owner and poison on it later.
        let crate::session::UnlockSession { mut child, .. } = entry;
        if let Some(mut child) = child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        state.evict_drive(&dir);
        let report = ProvisionReport {
            result: crate::provision::ProvisionResult::Completed {
                detail: "drive locked for this session: the owner is stopped and the session secret scrubbed"
                    .to_string(),
            },
            observed: registry::list_drives(),
            membership: observe_membership(&dir),
        };
        drop(_release);
        report
    })
    .await
    .unwrap_or_else(|join| ProvisionReport {
        result: crate::provision::ProvisionResult::Unknown {
            detail: format!("ipc worker failed for {op}: {join}"),
        },
        observed: RegistryState {
            status: RegistryStatus::Unreadable {
                reason: format!("ipc worker failed for {op}: {join}"),
            },
            drives: Vec::new(),
            skipped: Vec::new(),
        },
        membership: None,
    })
}

/// Spawn the unlock owner and verify it serves: an early exit maps
/// by code (78 is the terminal held-drive, anything else an
/// expected failure with the bounded CLI detail), a socket answer
/// within the bound completes, and expiry is unknown — the owner
/// may still come up under the session monitor, so the session is
/// stored on both the completed and the unknown paths, never on
/// the early-exit paths. Storing means moving the child and the
/// secret into the session map and starting its monitor; the
/// entry's generation lets a stale watcher exit instead of
/// respawning a locked drive.
fn unlock_exchange<R>(
    state: &crate::AppState,
    app: &AppHandle<R>,
    dir: &Path,
    args: &DriveUnlockArgs,
) -> crate::provision::ProvisionOutcome
where
    R: Runtime,
{
    use crate::provision::{ProvisionResult, UNLOCK_VERIFY_TIMEOUT};
    use zeroize::Zeroizing;
    // Already unlocked in this session: refuse before any spawn —
    // retyping means lock-then-unlock, never a second owner on one
    // store (the CLI would refuse it with the held-drive lock
    // anyway, but failing fast names the repair).
    if state
        .sessions()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .contains_key(dir)
    {
        return crate::provision::ProvisionOutcome {
            result: ProvisionResult::ExpectedFailure {
                detail: "already unlocked in this session: lock first to re-unlock with a different passphrase"
                    .to_string(),
            },
            stderr: String::new(),
            code: None,
        };
    }
    // Managed drives start through the manager, never beside it:
    // spawning an app-supervised owner over an installed definition
    // would create a second supervisor path. The store lock would
    // refuse a live manager anyway, but failing fast names the
    // repair — and a definition-less drive (the per-session
    // posture, or a removed definition) proceeds.
    if registry::managed_definition_exists(dir) {
        return crate::provision::ProvisionOutcome {
            result: ProvisionResult::ExpectedFailure {
                detail: "this drive has a managed service definition: start it with `drive service-start` instead of spawning a second supervisor (or reconfigure with `--no-service` for the per-session posture)".to_string(),
            },
            stderr: String::new(),
            code: None,
        };
    }
    let secret = Zeroizing::new(args.passphrase.clone().into_bytes());
    let cli = match state.cli_bin() {
        Ok(bin) => bin,
        Err(reason) => {
            return crate::provision::ProvisionOutcome {
                result: ProvisionResult::VersionMismatch { reason },
                stderr: String::new(),
                code: None,
            };
        }
    };
    // Unlock never takes a passphrase file: the session passphrase
    // rides the stdin pipe, with the file backend naming only the
    // identity file and the store backend naming only the item.
    let credentials = match credential_source(
        args.credential_backend.clone(),
        args.credential_account.clone(),
        args.identity_file.clone(),
        None,
        false,
    ) {
        Ok(source) => source,
        Err(reason) => {
            return crate::provision::ProvisionOutcome {
                result: ProvisionResult::ExpectedFailure { detail: reason },
                stderr: String::new(),
                code: None,
            };
        }
    };
    let request = crate::provision::UnlockRequest {
        drive_dir: dir.to_path_buf(),
        credentials,
        // The registered relay set: managed starts bake it into the
        // service definition, but unlock spawns the owner directly —
        // without the registry relays a drive with no `default.toml`
        // refuses relay-less. Empty where the drive is unregistered
        // or the registry is unreadable, so the CLI falls back to
        // the drive config file exactly as before.
        relays: registry::registry_relays_for(dir),
        retained_quota: None,
    };
    let mut spawned = match crate::provision::spawn_unlock(&cli, &request, &secret) {
        Ok(spawned) => spawned,
        Err(result) => {
            return crate::provision::ProvisionOutcome {
                result,
                stderr: String::new(),
                code: None,
            };
        }
    };
    // The spawn replaced whatever served this socket before: forget
    // the cached connection so the first probe past this point mints
    // a fresh one instead of breaking on the pre-restart stream
    // (which would poison the client and force a manual reconnect).
    state.evict_drive(dir);
    let deadline = std::time::Instant::now() + UNLOCK_VERIFY_TIMEOUT;
    loop {
        match spawned.child.try_wait() {
            Ok(Some(_)) => {
                let (code, detail) = crate::provision::collect_unlock_exit_detail(spawned);
                let result = match code {
                    Some(78) => ProvisionResult::HeldDrive { detail },
                    _ => ProvisionResult::ExpectedFailure { detail },
                };
                return crate::provision::ProvisionOutcome {
                    result,
                    stderr: String::new(),
                    code,
                };
            }
            Ok(None) => {}
            Err(error) => {
                let _ = spawned.child.kill();
                let _ = spawned.child.wait();
                return crate::provision::ProvisionOutcome {
                    result: ProvisionResult::Unknown {
                        detail: format!(
                            "cannot poll the unlock owner: {error}; outcome unknown — re-reading observed state"
                        ),
                    },
                    stderr: String::new(),
                    code: None,
                };
            }
        }
        if unlock_socket_answers(dir) {
            let detail =
                "owner answering this session (socket verified) — the drive is open".to_string();
            store_unlock_session(state, app, dir, secret, spawned, &cli, request);
            return crate::provision::ProvisionOutcome {
                result: ProvisionResult::Completed { detail },
                stderr: String::new(),
                code: None,
            };
        }
        if std::time::Instant::now() >= deadline {
            store_unlock_session(state, app, dir, secret, spawned, &cli, request);
            return crate::provision::ProvisionOutcome {
                result: ProvisionResult::Unknown {
                    detail: "the owner did not answer within 30s; it may still come up under the session monitor — re-reading observed state".to_string(),
                },
                stderr: String::new(),
                code: None,
            };
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Move the verified child and its secret into the session map and
/// start its monitor: the app now owns this drive's session. The
/// drain threads detach with the live owner (consume and discard
/// until the last pipe writer closes).
fn store_unlock_session<R>(
    state: &crate::AppState,
    app: &AppHandle<R>,
    dir: &Path,
    secret: zeroize::Zeroizing<Vec<u8>>,
    spawned: crate::provision::SpawnedUnlock,
    cli: &std::path::Path,
    request: crate::provision::UnlockRequest,
) where
    R: Runtime,
{
    let crate::provision::SpawnedUnlock {
        child,
        stdout_thread,
        stderr_thread,
    } = spawned;
    drop(stdout_thread);
    drop(stderr_thread);
    let generation = state.next_generation();
    state
        .sessions()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(
            dir.to_path_buf(),
            crate::session::UnlockSession::new(
                secret,
                child,
                cli.to_path_buf(),
                request,
                generation,
            ),
        );
    crate::session::watch_session(app.clone(), dir.to_path_buf(), generation);
}

/// Whether the drive's owner answers now: connect plus one
/// terminal `liveness` answer under a short bound. A terminal
/// refusal still proves the owner answers (transport exists), so
/// it reads ready — availability is transport-level, never a
/// judgment on the drive. Anything else is not-an-answer-yet.
fn unlock_socket_answers(drive_dir: &std::path::Path) -> bool {
    const UNLOCK_SOCKET_BOUND: std::time::Duration = std::time::Duration::from_secs(2);
    let client = match OwnerClient::connect_drive(drive_dir) {
        Ok(client) => client,
        Err(_) => return false,
    };
    let mut client = match client.with_timeout(UNLOCK_SOCKET_BOUND) {
        Ok(bounded) => bounded,
        Err(_) => return false,
    };
    matches!(
        client.liveness(),
        Ok(_) | Err(wyrd_owner_client::ClientError::Refused(_))
    )
}
