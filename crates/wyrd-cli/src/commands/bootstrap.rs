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
use crate::drive_config::{resolve_credential_path, resolve_relays, LoadedConfig};
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
    pub(crate) supervise_bin: Option<PathBuf>,
    pub(crate) no_service: bool,
}

/// Resolve, then provision: credentials were already loaded once
/// by the dispatcher for its own routing; here only the resolved
/// credential *paths* travel to the service definition so the
/// supervised owner reads the same files.
pub(crate) fn bootstrap(
    args: BootstrapArgs,
    identity: wyrd_sync::keys::DeviceIdentitySecret,
    passphrase: &str,
    config: Option<&LoadedConfig>,
) -> Result<(), CliError> {
    let drive_dir = args.drive_dir.clone();
    let relays = resolve_relays(args.relays.relay, config).relays;
    let identity_file = resolve_credential_path(
        args.credentials.identity_file,
        config.and_then(|c| c.identity_file.clone()),
        "--identity-file",
        "identity_file",
    )?;
    let passphrase_file = resolve_credential_path(
        args.credentials.passphrase_file,
        config.and_then(|c| c.passphrase_file.clone()),
        "--passphrase-file",
        "passphrase_file",
    )?;
    // Services run with a fixed working directory, so every path
    // the definition carries is absolute: canonicalize what must
    // exist (the credential files were just read) and absolutize
    // the binaries without requiring them (a missing supervise
    // wrapper is its own actionable error at provision time).
    let identity_file = canonicalize(&identity_file)?;
    let passphrase_file = canonicalize(&passphrase_file)?;
    let owner_bin = args.owner_bin.map(absolutize);
    let supervise_bin = args.supervise_bin.map(absolutize);
    let init = match &args.invitation_file {
        None => Init::Create,
        Some(path) => {
            let bytes = read_bounded(path, 1024 * 1024)?;
            Init::Join {
                sealed_invitation: bytes,
            }
        }
    };
    let registry_path = args.registry.clone().unwrap_or_else(default_registry_path);
    let report = provision(ProvisionConfig {
        name: args.name,
        drive_dir,
        relays,
        identity,
        passphrase: passphrase.to_string(),
        init,
        registry_path,
        wyrd_bin: owner_bin,
        supervise_bin,
        identity_file,
        passphrase_file,
        manage_service: !args.no_service,
        verify_timeout: Duration::from_secs(30),
    });
    match report {
        ProvisionState::VerifiedAvailable { entry, skipped } => {
            warn_skipped(&skipped);
            println!(
                "drive {} verified available (socket {})",
                entry.name,
                entry.drive_dir.join("owner.sock").display()
            );
            Ok(())
        }
        ProvisionState::RegisteredButUnavailable {
            entry,
            reason,
            skipped,
            ..
        } => {
            warn_skipped(&skipped);
            Err(CliError::Usage(format!(
                "drive {} registered but unavailable: {reason}",
                entry.name
            )))
        }
        ProvisionState::ProvisioningIncomplete { stage, reason, .. } => Err(CliError::Usage(
            format!("provisioning incomplete at {stage}: {reason}"),
        )),
    }
}

/// Surface registry entries the read skipped: provision rewrites
/// only the valid set, so a rotten entry would otherwise vanish
/// without any user-visible signal. Non-fatal — discovery keeps
/// working — but loud. The list travels in the provisioning result
/// itself, because re-reading after the write would find nothing
/// left to report.
fn warn_skipped(skipped: &[String]) {
    for skipped in skipped {
        eprintln!("warning: registry entry skipped: {skipped}");
    }
}

/// Canonicalize a file that must exist (credential files were
/// just read): resolves symlinks and guarantees absoluteness, so
/// the service definition names one unambiguous path.
fn canonicalize(path: &PathBuf) -> Result<PathBuf, CliError> {
    std::fs::canonicalize(path).map_err(|source| CliError::Io {
        path: path.clone(),
        source,
    })
}

/// Absolutize a path that need not exist yet (binaries): joins
/// relative paths onto the working directory without touching the
/// filesystem.
fn absolutize(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir()
        .map(|dir| dir.join(&path))
        .unwrap_or(path)
}
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
