//! First-drive provisioning: `wyrd bootstrap` runs the fixed
//! handoff from the desktop-shell bootstrap contract — initialize,
//! register, supervise, verify — and reports exactly which stage a
//! partial run reached. Ready means the socket verified this
//! session; a registry entry alone never reports ready.

use std::path::PathBuf;
use std::time::Duration;

use wyrd_daemon::bootstrap::{
    default_registry_path, provision, Init, ProvisionConfig, ProvisionState,
};

use crate::args::{Credentials, RelayArgs};
use crate::credentials::load_credentials;
use crate::drive_config::resolve_relays;
use crate::CliError;

/// Provision one drive from nothing and hand it to OS supervision.
/// Create needs only credentials; join additionally needs the sealed
/// invitation file the inviter's `member invite` wrote.
pub(crate) struct BootstrapArgs {
    pub(crate) name: String,
    pub(crate) drive_dir: PathBuf,
    pub(crate) relays: RelayArgs,
    pub(crate) credentials: Credentials,
    pub(crate) invitation_file: Option<PathBuf>,
    pub(crate) registry: Option<PathBuf>,
    pub(crate) owner_bin: Option<PathBuf>,
    pub(crate) no_service: bool,
    pub(crate) no_start: bool,
}

pub(crate) fn bootstrap(args: BootstrapArgs, config: Option<PathBuf>) -> Result<(), CliError> {
    let drive_dir = args.drive_dir.clone();
    let (identity, passphrase, _, loaded) =
        load_credentials(&drive_dir, &args.credentials, config.as_deref())?;
    let relays = resolve_relays(args.relays.relay, loaded.as_ref()).relays;

    let init = match &args.invitation_file {
        None => Init::Create,
        Some(path) => {
            let bytes = read_bounded(path, 1024 * 1024)?;
            Init::Join {
                sealed_invitation: bytes,
            }
        }
    };
    let report = provision(ProvisionConfig {
        name: args.name,
        drive_dir,
        relays,
        identity,
        passphrase: passphrase.to_string(),
        init,
        registry_path: args.registry.unwrap_or_else(default_registry_path),
        owner_bin: args.owner_bin,
        manage_service: !args.no_service,
        start_now: !args.no_start,
        verify_timeout: Duration::from_secs(30),
    });
    match report {
        ProvisionState::VerifiedAvailable { entry } => {
            println!(
                "drive {} verified available (socket {})",
                entry.name,
                entry.drive_dir.join("owner.sock").display()
            );
            Ok(())
        }
        ProvisionState::RegisteredButUnavailable { entry, reason } => Err(CliError::Usage(
            format!("drive {} registered but unavailable: {reason}", entry.name),
        )),
        ProvisionState::ProvisioningIncomplete { stage, reason } => Err(CliError::Usage(format!(
            "provisioning incomplete at {stage}: {reason}"
        ))),
    }
}

/// Read a bounded non-credential input file. Invitation bytes are
/// sealed, not secret, so no ownership hardening applies — but an
/// unbounded read lets a corrupt file exhaust memory before decode
/// refuses it. Mirrors the member invite-claim bound; duplicated
/// rather than shared because the two commands must stay able to
/// diverge their limits without coupling.
fn read_bounded(path: &PathBuf, max: usize) -> Result<Vec<u8>, CliError> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|source| CliError::Io {
        path: path.clone(),
        source,
    })?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take((max + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| CliError::Io {
            path: path.clone(),
            source,
        })?;
    if bytes.len() > max {
        return Err(CliError::InvitationTooLarge);
    }
    Ok(bytes)
}
