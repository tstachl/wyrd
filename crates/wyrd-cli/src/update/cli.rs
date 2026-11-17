//! The `Command::Update` arm of the `command()` dispatcher: secretless
//! setup plus the supervision divergence. Update runs the whole
//! transaction below without secrets (credential paths forward to
//! the verification child, who opens custody), so it returns before
//! credential loading. Quiescence, staging, swap, verify,
//! commit-or-rollback, hand back: [`update_run`].

use super::{supervise, update_run, UpdateOutcome, UpdateRequest};
use crate::args::{Credentials, RelayArgs};
use crate::drive_config::{
    load_drive_config, resolve_credential_path, resolve_relays, update_request_relays,
};
use crate::CliError;
use std::path::{Path, PathBuf};

/// Run the update transaction from CLI values: resolve the drive
/// config, credential paths, and effective relay set, run
/// [`update_run`], and either finish or diverge
/// into [`supervise()`]. Returns before credential
/// loading by construction — the caller keeps the early return, so
/// no secrets are opened on this path.
pub(crate) fn update_from_cli(
    drive_dir: &Path,
    archive: &Path,
    relays: &RelayArgs,
    restart: bool,
    credentials: &Credentials,
    config_override: Option<&Path>,
    expect_blake3: Option<String>,
) -> Result<(), CliError> {
    let exe = std::env::current_exe().map_err(|source| CliError::Io {
        path: PathBuf::from("<current executable>"),
        source,
    })?;
    // The handoff child never reads the configuration: the
    // parent resolves the effective set and forwards it
    // flattened as `--relay`/`--identity-file`/`--passphrase-file`
    // argv, so the child takes no `--config` and inherits no
    // file. The two update guards therefore judge different
    // surfaces: "restart needs a relay" judges the effective
    // set (a configured relay satisfies it), while
    // "relay-is-restart-only" judges the flag list (a file the
    // operator adopted for every other subcommand must not
    // fail an update that never consumes relays).
    let update_config = load_drive_config(drive_dir, config_override)?;
    let update_config = update_config.as_ref();
    let identity_file = resolve_credential_path(
        credentials.identity_file.clone(),
        update_config.and_then(|c| c.identity_file.clone()),
        "--identity-file",
        "identity_file",
    )?;
    let passphrase_file = resolve_credential_path(
        credentials.passphrase_file.clone(),
        update_config.and_then(|c| c.passphrase_file.clone()),
        "--passphrase-file",
        "passphrase_file",
    )?;
    let resolution = resolve_relays(relays.relay.clone(), update_config);
    // B1 shape: without `--restart` relays travel nowhere, so
    // only explicitly passed flags can be a mistake worth
    // refusing; with `--restart` the handoff consumes the
    // effective set.
    let request_relays = update_request_relays(restart, &relays.relay, &resolution.relays);
    let outcome = update_run(&UpdateRequest {
        drive_dir,
        archive,
        identity_file: &identity_file,
        passphrase_file: &passphrase_file,
        relays: request_relays,
        restart,
        exe: &exe,
        expected_blake3: expect_blake3.as_deref(),
    })?;
    // Supervision diverges by design (the wait loop only
    // exits the process), so a returned outcome is either
    // Done or a supervision request — never an Ok from the
    // supervisor itself.
    match outcome {
        UpdateOutcome::Done => Ok(()),
        UpdateOutcome::Supervise {
            exe,
            drive,
            relays,
            identity_file,
            passphrase_file,
        } => {
            #[cfg(unix)]
            {
                supervise(&exe, &drive, &relays, &identity_file, &passphrase_file)
            }
            #[cfg(not(unix))]
            {
                let _ = (exe, drive, relays, identity_file, passphrase_file);
                return Err(CliError::Usage(
                    "--restart needs Unix signal forwarding".into(),
                ));
            }
        }
    }
}
