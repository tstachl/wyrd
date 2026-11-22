//! Drive provisioning commands: create, join, reconfigure, and the
//! three service operations. All run through [`run_provision`]'s
//! per-drive flight and re-observation. Command names, argument types,
//! and result shapes are the external IPC contract.

use std::path::PathBuf;
use tauri::{AppHandle, Manager, Runtime};

use crate::registry::{self, RegistryState, RegistryStatus};
use crate::FlightTracker;

use super::membership::observe_membership;
use super::{FlightRelease, ProvisionReport};

/// Create one drive through the installed CLI (`bootstrap` without
/// an invitation file), then re-read and probe. All arguments are
/// owned strings; secret *paths* travel, never contents. `mode` is
/// `vault` or `mount` (`mount` needs `mountpoint`); any other value
/// refuses before any process spawns.
///
/// Credential addressing is two-shaped: file paths (today's
/// frontend flow — both required) or one OS credential-store
/// reference (`credentialBackend: "os-store"` plus
/// `credentialAccount`; files beside it are the one-time seed
/// inputs for a fresh store-backed bootstrap, absent on a
/// converged re-run). Absent backend means the file backend, as
/// before — existing callers pass paths only and behave
/// byte-identically.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionCreateArgs {
    /// Registry entry name, 1–58 chars of `[a-z0-9-]`.
    pub name: String,
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
    /// Relays for the managed service; empty is refused before anything is created.
    pub relays: Vec<String>,
    /// Credential path, never contents. Required with the file
    /// backend; the seed identity beside a store bootstrap.
    pub identity_file: Option<String>,
    /// Credential path, never contents. Required with the file
    /// backend; the seed passphrase beside a store bootstrap.
    pub passphrase_file: Option<String>,
    /// Credential backend selector: absent or `"file"` reads the
    /// hardened credential files; `"os-store"` resolves the drive
    /// through the OS credential store.
    pub credential_backend: Option<String>,
    /// OS credential-store account naming the drive's item (the
    /// reference, never a secret). Required exactly where the
    /// backend is `"os-store"`, refused everywhere else.
    pub credential_account: Option<String>,
    /// Service mode display string: `vault` or `mount`.
    pub mode: String,
    /// FUSE mountpoint: required in `mount` mode, refused in `vault` mode.
    pub mountpoint: Option<String>,
    /// Register only: skip installing, enabling, and starting the service.
    pub no_service: bool,
}

/// Join one drive: create plus the sealed invitation file selected
/// out-of-band (file dialog) and travelling as a path.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionJoinArgs {
    /// Registry entry name, 1–58 chars of `[a-z0-9-]`.
    pub name: String,
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
    /// Relays for the managed service; empty is refused before anything is created.
    pub relays: Vec<String>,
    /// Credential path, never contents: required with the file
    /// backend, the seed identity beside a store bootstrap.
    pub identity_file: Option<String>,
    /// Credential path, never contents: required with the file
    /// backend, the seed passphrase beside a store bootstrap.
    pub passphrase_file: Option<String>,
    /// Credential backend selector: absent or `"file"` reads the
    /// hardened credential files; `"os-store"` resolves the drive
    /// through the OS credential store.
    pub credential_backend: Option<String>,
    /// OS credential-store account naming the drive's item (the
    /// reference, never a secret). Required exactly where the
    /// backend is `"os-store"`, refused everywhere else.
    pub credential_account: Option<String>,
    /// Sealed invitation file path, selected out-of-band.
    pub invitation_file: String,
    /// Expected owner pin (64 hex): the inviter the invitee meant
    /// to trust. Refuses the join on mismatch.
    pub expected_owner: Option<String>,
    /// Expected drive pin (64 hex): the drive being joined.
    pub expected_drive: Option<String>,
    /// Expected genesis pin (64 hex): the genesis transition the
    /// invitation must carry.
    pub expected_genesis: Option<String>,
    /// Service mode display string: `vault` or `mount`.
    pub mode: String,
    /// FUSE mountpoint: required in `mount` mode, refused in `vault` mode.
    pub mountpoint: Option<String>,
    /// Register only: skip installing, enabling, and starting the service.
    pub no_service: bool,
}

/// Create one drive through the installed CLI (`bootstrap` without
/// an invitation file), then re-read and probe. All arguments are
/// owned strings; secret *paths* travel, never contents. `mode` is
/// `vault` or `mount` (`mount` needs `mountpoint`); any other value
/// refuses before any process spawns.
/// Map one IPC credential selection to the adapter's credential
/// source. Absent backend (or `"file"`) is the file backend: both
/// paths required, an account beside it refused. `"os-store"`
/// needs the account; file paths beside it are the one-time seed
/// inputs where the operation takes seeds (create/join) and a
/// refusal everywhere else — mirroring the CLI's
/// one-backend-per-drive rule before any process spawns. Anything
/// else refuses with its name. `seeds_allowed` is create/join
/// only; leave, reconfigure, and unlock read the provisioned
/// credential, never bootstrap input.
pub(crate) fn credential_source(
    backend: Option<String>,
    account: Option<String>,
    identity_file: Option<String>,
    passphrase_file: Option<String>,
    seeds_allowed: bool,
) -> Result<crate::provision::CredentialSource, String> {
    use crate::provision::{CredentialSource, SeedFiles};
    match backend.as_deref() {
        None | Some("file") => {
            if let Some(account) = account {
                return Err(format!(
                    "--credential-account {account:?} is set but the credential backend is not \"os-store\": one backend per drive"
                ));
            }
            // Paths pass through untouched: each argv builder
            // consumes what its operation needs and refuses what
            // is missing before any process spawns (unlock, for
            // example, never takes a passphrase file).
            Ok(CredentialSource::File {
                identity_file: identity_file.map(PathBuf::from),
                passphrase_file: passphrase_file.map(PathBuf::from),
            })
        }
        Some("os-store") => {
            let account = account.ok_or_else(|| {
                "credential backend is \"os-store\" but no credential account is set: name the drive's store item".to_string()
            })?;
            if account.is_empty() {
                return Err(
                    "no OS credential-store account: name the drive's store item".to_string(),
                );
            }
            let seed = match (identity_file, passphrase_file) {
                (Some(identity), Some(passphrase)) => {
                    if !seeds_allowed {
                        return Err(
                            "seed files are bootstrap input only: this operation reads the provisioned store item, never files".to_string(),
                        );
                    }
                    Some(SeedFiles {
                        identity_file: PathBuf::from(identity),
                        passphrase_file: PathBuf::from(passphrase),
                    })
                }
                (None, None) => None,
                _ => {
                    return Err(
                        "partial seed files: pass both the identity and passphrase inputs or neither"
                            .to_string(),
                    );
                }
            };
            Ok(CredentialSource::OsStore { account, seed })
        }
        Some(other) => Err(format!(
            "unknown credential backend {other:?}: expected \"file\" or \"os-store\""
        )),
    }
}

/// Create one drive through the installed CLI (`bootstrap` without
/// an invitation file), then re-read and probe. Credential
/// selection rides the IPC args (files or one store reference, with
/// seeds for a fresh store-backed bootstrap); mapping refusals
/// resolve expected-failure without spawning.
#[tauri::command]
pub async fn provision_create<R: Runtime>(
    app: AppHandle<R>,
    args: ProvisionCreateArgs,
) -> ProvisionReport {
    run_provision(
        app,
        "provision_create",
        args.drive_dir.clone(),
        true,
        move |state, dir| {
            let credentials = match credential_source(
                args.credential_backend,
                args.credential_account,
                args.identity_file,
                args.passphrase_file,
                true,
            ) {
                Ok(source) => source,
                Err(reason) => {
                    return crate::provision::ProvisionOutcome {
                        result: crate::provision::ProvisionResult::ExpectedFailure {
                            detail: reason,
                        },
                        stderr: String::new(),
                        code: None,
                    };
                }
            };
            let request = crate::provision::ProvisionRequest {
                name: args.name,
                drive_dir: dir,
                relays: args.relays,
                credentials,
                invitation_file: None,
                expected_owner: None,
                expected_drive: None,
                expected_genesis: None,
                registry_path: None,
                owner_bin: None,
                supervise_bin: None,
                no_service: args.no_service,
                mode: args.mode,
                mountpoint: args.mountpoint.map(PathBuf::from),
            };
            let cli = state.cli_bin();
            match cli {
                Ok(bin) => crate::provision::run_bootstrap(
                    &bin,
                    &request,
                    crate::provision::PROVISION_TIMEOUT,
                ),
                Err(reason) => crate::provision::ProvisionOutcome {
                    result: crate::provision::ProvisionResult::VersionMismatch { reason },
                    stderr: String::new(),
                    code: None,
                },
            }
        },
    )
    .await
}

/// Join one drive through the installed CLI (`bootstrap` with the
/// sealed invitation file at `invitation_file`), then re-read and
/// probe. The invitation file is selected out-of-band (file dialog)
/// and travels as a path — the adapter reads at most its bounded
/// bytes through the CLI, never its interpretation.
#[tauri::command]
pub async fn provision_join<R: Runtime>(
    app: AppHandle<R>,
    args: ProvisionJoinArgs,
) -> ProvisionReport {
    run_provision(
        app,
        "provision_join",
        args.drive_dir.clone(),
        true,
        move |state, dir| {
            let credentials = match credential_source(
                args.credential_backend,
                args.credential_account,
                args.identity_file,
                args.passphrase_file,
                true,
            ) {
                Ok(source) => source,
                Err(reason) => {
                    return crate::provision::ProvisionOutcome {
                        result: crate::provision::ProvisionResult::ExpectedFailure {
                            detail: reason,
                        },
                        stderr: String::new(),
                        code: None,
                    };
                }
            };
            let request = crate::provision::ProvisionRequest {
                name: args.name,
                drive_dir: dir,
                relays: args.relays,
                credentials,
                invitation_file: Some(PathBuf::from(args.invitation_file)),
                expected_owner: args.expected_owner,
                expected_drive: args.expected_drive,
                expected_genesis: args.expected_genesis,
                registry_path: None,
                owner_bin: None,
                supervise_bin: None,
                no_service: args.no_service,
                mode: args.mode,
                mountpoint: args.mountpoint.map(PathBuf::from),
            };
            let cli = state.cli_bin();
            match cli {
                Ok(bin) => crate::provision::run_bootstrap(
                    &bin,
                    &request,
                    crate::provision::PROVISION_TIMEOUT,
                ),
                Err(reason) => crate::provision::ProvisionOutcome {
                    result: crate::provision::ProvisionResult::VersionMismatch { reason },
                    stderr: String::new(),
                    code: None,
                },
            }
        },
    )
    .await
}

/// Reconfigure one registered drive through the installed CLI
/// (`drive reconfigure`), then re-read and probe. All arguments are
/// owned strings; secret *paths* travel, never contents. `mode` is
/// `vault` or `mount` (`mount` needs `mountpoint`); any other value
/// refuses before any process spawns. `relays`
/// replaces the registered set when non-empty and keeps it when
/// empty, so a credential-path-only reconfigure never clears relays
/// by accident. Serialized against the per-drive flight like every
/// other lifecycle operation, with registry, socket, and membership
/// re-observation after the process completes.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionReconfigureArgs {
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
    /// Replacement relays when non-empty; empty keeps the registered set.
    pub relays: Vec<String>,
    /// Credential path, never contents: required with the file
    /// backend, refused beside a store reference (reconfigure reads
    /// the provisioned credential, never seed inputs).
    pub identity_file: Option<String>,
    /// Credential path, never contents: required with the file
    /// backend, refused beside a store reference.
    pub passphrase_file: Option<String>,
    /// Credential backend selector: absent or `"file"` reads the
    /// hardened credential files; `"os-store"` resolves the drive
    /// through the OS credential store.
    pub credential_backend: Option<String>,
    /// OS credential-store account naming the drive's item (the
    /// reference, never a secret). Required exactly where the
    /// backend is `"os-store"`, refused everywhere else.
    pub credential_account: Option<String>,
    /// Service mode display string: `vault` or `mount`.
    pub mode: String,
    /// FUSE mountpoint: required in `mount` mode, refused in `vault` mode.
    pub mountpoint: Option<String>,
    /// Rewrite the entry without installing any OS service.
    pub no_service: bool,
}

/// Reconfigure one registered drive through the installed CLI
/// (`drive reconfigure`), then re-read and probe.
#[tauri::command]
pub async fn provision_reconfigure<R: Runtime>(
    app: AppHandle<R>,
    args: ProvisionReconfigureArgs,
) -> ProvisionReport {
    run_provision(
        app,
        "provision_reconfigure",
        args.drive_dir.clone(),
        true,
        move |state, dir| {
            let credentials = match credential_source(
                args.credential_backend,
                args.credential_account,
                args.identity_file,
                args.passphrase_file,
                false,
            ) {
                Ok(source) => source,
                Err(reason) => {
                    return crate::provision::ProvisionOutcome {
                        result: crate::provision::ProvisionResult::ExpectedFailure {
                            detail: reason,
                        },
                        stderr: String::new(),
                        code: None,
                    };
                }
            };
            let request = crate::provision::ReconfigureRequest {
                drive_dir: dir,
                relays: args.relays,
                credentials,
                registry_path: None,
                owner_bin: None,
                supervise_bin: None,
                no_service: args.no_service,
                mode: args.mode,
                mountpoint: args.mountpoint.map(PathBuf::from),
            };
            let cli = state.cli_bin();
            match cli {
                Ok(bin) => crate::provision::run_reconfigure(
                    &bin,
                    &request,
                    crate::provision::PROVISION_TIMEOUT,
                ),
                Err(reason) => crate::provision::ProvisionOutcome {
                    result: crate::provision::ProvisionResult::VersionMismatch { reason },
                    stderr: String::new(),
                    code: None,
                },
            }
        },
    )
    .await
}

/// Start one registered drive's installed service through the
/// installed CLI (`drive service-start`), then re-read and probe.
/// No credentials travel — starting opens no keystore. The recovery
/// for "installed but stopped": explicit, never automatic, and a
/// missing definition refuses with the reinstall pointer (the
/// reconfigure recovery below).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionServiceStartArgs {
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
}

/// Start one registered drive's installed service through the
/// installed CLI (`drive service-start`), then re-read and probe.
#[tauri::command]
pub async fn provision_service_start<R: Runtime>(
    app: AppHandle<R>,
    args: ProvisionServiceStartArgs,
) -> ProvisionReport {
    run_provision(
        app,
        "provision_service_start",
        args.drive_dir.clone(),
        true,
        move |state, dir| {
            let request = crate::provision::ServiceStartRequest {
                drive_dir: dir,
                registry_path: None,
            };
            let cli = state.cli_bin();
            match cli {
                Ok(bin) => crate::provision::run_service_start(
                    &bin,
                    &request,
                    crate::provision::PROVISION_TIMEOUT,
                ),
                Err(reason) => crate::provision::ProvisionOutcome {
                    result: crate::provision::ProvisionResult::VersionMismatch { reason },
                    stderr: String::new(),
                    code: None,
                },
            }
        },
    )
    .await
}

/// Enable one registered drive's login start through the installed
/// CLI (`drive service-enable`), then re-read and probe. No start
/// happens: the preference is recorded and verified, and the fresh
/// enablement probe decides the display.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionServiceEnableArgs {
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
}

/// Enable one registered drive's login start through the installed
/// CLI (`drive service-enable`), then re-read and probe.
#[tauri::command]
pub async fn provision_service_enable<R: Runtime>(
    app: AppHandle<R>,
    args: ProvisionServiceEnableArgs,
) -> ProvisionReport {
    run_provision(
        app,
        "provision_service_enable",
        args.drive_dir.clone(),
        true,
        move |state, dir| {
            let request = crate::provision::ServiceEnableRequest {
                drive_dir: dir,
                registry_path: None,
            };
            let cli = state.cli_bin();
            match cli {
                Ok(bin) => crate::provision::run_service_enable(
                    &bin,
                    &request,
                    crate::provision::PROVISION_TIMEOUT,
                ),
                Err(reason) => crate::provision::ProvisionOutcome {
                    result: crate::provision::ProvisionResult::VersionMismatch { reason },
                    stderr: String::new(),
                    code: None,
                },
            }
        },
    )
    .await
}

/// Disable one registered drive's login start and stop it now
/// through the installed CLI (`drive service-disable`), then
/// re-read and probe. The definition file stays: the fresh
/// enablement probe and the service verdict decide the display.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionServiceDisableArgs {
    /// Absolute drive directory as an exact string.
    pub drive_dir: String,
}

/// Disable one registered drive's login start and stop it now
/// through the installed CLI (`drive service-disable`), then
/// re-read and probe.
#[tauri::command]
pub async fn provision_service_disable<R: Runtime>(
    app: AppHandle<R>,
    args: ProvisionServiceDisableArgs,
) -> ProvisionReport {
    run_provision(
        app,
        "provision_service_disable",
        args.drive_dir.clone(),
        true,
        move |state, dir| {
            let request = crate::provision::ServiceDisableRequest {
                drive_dir: dir,
                registry_path: None,
            };
            let cli = state.cli_bin();
            match cli {
                Ok(bin) => crate::provision::run_service_disable(
                    &bin,
                    &request,
                    crate::provision::PROVISION_TIMEOUT,
                ),
                Err(reason) => crate::provision::ProvisionOutcome {
                    result: crate::provision::ProvisionResult::VersionMismatch { reason },
                    stderr: String::new(),
                    code: None,
                },
            }
        },
    )
    .await
}

/// Run one lifecycle operation serialized per drive: validate the
/// identity, claim the flight (refuse `Busy` when held), run the
/// blocking CLI exchange, release the flight, then re-read the
/// registry, probe every drive, and re-observe the operated drive's
/// membership — verification-via-reread on every path, including
/// timeouts and spawn failures. Everything runs on the blocking
/// pool (the registry probe fans out over sockets with a per-drive
/// bound and must never park an async runtime worker). A dead
/// blocking worker resolves `Unknown`, never success: the outcome
/// is unknown by definition, and only the observed state decides
/// the display. The flight releases through an RAII guard, so a
/// panicking exchange can never wedge the drive as busy — and the
/// CLI process itself is killed and reaped past the timeout before
/// the release, so no two operations can overlap.
pub(crate) async fn run_provision<R, F>(
    app: AppHandle<R>,
    op: &'static str,
    drive_dir: String,
    evict_after: bool,
    call: F,
) -> ProvisionReport
where
    F: FnOnce(&crate::AppState, PathBuf) -> crate::provision::ProvisionOutcome + Send + 'static,
    R: Runtime,
{
    let _flight = app.state::<FlightTracker>().enter();
    tauri::async_runtime::spawn_blocking(move || {
        // Validate the drive identity before touching the flight map:
        // a non-canonical spelling must never key a second flight
        // for one drive.
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
        let outcome = call(&state, dir.clone());
        // Owner-restarting operations evict the cached connection:
        // the next exchange mints a fresh one instead of breaking
        // on the pre-restart stream (see `evict_drive`). Leave
        // authors membership without replacing the server, so it
        // keeps the connection it had.
        if evict_after {
            state.evict_drive(&dir);
        }
        // The flight releases only after the observation below: the
        // observation is part of the operation, so a second request
        // on the same drive waits for the full verified picture
        // instead of interleaving with it. (The process itself is
        // already gone — the adapter kills and reaps past the
        // timeout — so holding here serializes observation, never
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
        // A dead worker is a shell bug, never an owner answer: report
        // the registry as unreadable with the reason instead of
        // probing sockets from the fallback path.
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
