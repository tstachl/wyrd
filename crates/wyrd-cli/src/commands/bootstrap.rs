//! First-drive provisioning: `wyrd bootstrap` runs the fixed
//! handoff from the desktop-shell bootstrap contract — initialize,
//! register, supervise, verify — and reports exactly which stage a
//! partial run reached. Ready means the socket verified this
//! session; a registry entry alone never reports ready.

use std::path::PathBuf;
use std::time::Duration;

use wyrd_credential_store::OsStoreBackend;
use wyrd_daemon::bootstrap::{
    default_registry_path, provision, Init, ProvisionConfig, ProvisionState, ServiceCredential,
};
use wyrd_sync::keys::{CredentialError, CredentialLoader, CredentialRef, LoadedCredential};
use zeroize::Zeroizing;

use crate::args::{Credentials, RelayArgs};
use crate::credentials::{effective_backend, loader_to_cli, EffectiveBackend};
use crate::drive_config::{
    resolve_relays, write_os_store_config, LoadedConfig, DEFAULT_CONFIG_NAME,
};
use crate::CliError;

/// Provision one drive from nothing and hand it to OS supervision.
/// Create needs only credentials; join additionally needs the sealed
/// invitation file the inviter's `member invite` wrote.
pub(crate) struct BootstrapArgs {
    pub(crate) name: String,
    pub(crate) drive_dir: PathBuf,
    pub(crate) mode: String,
    pub(crate) mountpoint: Option<PathBuf>,
    pub(crate) relays: RelayArgs,
    pub(crate) credentials: Credentials,
    pub(crate) invitation_file: Option<PathBuf>,
    pub(crate) registry: Option<PathBuf>,
    pub(crate) owner_bin: Option<PathBuf>,
    pub(crate) supervise_bin: Option<PathBuf>,
    pub(crate) no_service: bool,
}

/// Parse the `--mode`/`--mountpoint` pair both provisioning and
/// reconfiguration share: `vault` serves headless and refuses a
/// mountpoint, `mount` serves the FUSE projection and requires one.
/// Anything else refuses as usage before anything is created,
/// stopped, or written. Mountpoint shape (absolute, no escapes) is
/// validated by the daemon at the `validate` stage, not here — the
/// CLI keeps the operator's spelling for the registry.
pub(crate) fn parse_service_mode(
    mode: &str,
    mountpoint: Option<PathBuf>,
) -> Result<(wyrd_daemon::bootstrap::ServiceMode, Option<PathBuf>), CliError> {
    use wyrd_daemon::bootstrap::ServiceMode;
    match mode {
        "vault" => {
            if mountpoint.is_some() {
                return Err(CliError::Usage(
                    "vault mode carries no mountpoint: remove --mountpoint for a vault entry"
                        .to_string(),
                ));
            }
            Ok((ServiceMode::Vault, None))
        }
        "mount" => {
            if mountpoint.is_none() {
                return Err(CliError::Usage(
                    "mount mode needs a mountpoint: pass --mountpoint with an absolute path"
                        .to_string(),
                ));
            }
            Ok((ServiceMode::Mount, mountpoint))
        }
        other => Err(CliError::Usage(format!(
            "unknown service mode {other:?}: this build manages `vault` and `mount`"
        ))),
    }
}

/// Provision one drive straight into the OS credential store:
/// the input identity and passphrase come from files or the pipe
/// (read once through the hardened file readers), the drive is
/// recorded as store-backed, and the installed service definition
/// names the store account — no credential files on disk for the
/// manager to read.
///
/// The commit boundary holds end to end: the material validates in
/// memory (the store write reads it back through the production
/// reader), the daemon initializes and verifies the drive on the
/// same material, and only then does the configuration record the
/// backend. A failed read-back never leaves a drive appearing
/// provisioned, and an incomplete daemon run never leaves a
/// half-adopted configuration: re-runs converge where the store
/// holds identical material and refuse where it holds differing
/// material, never overwriting.
///
/// Input rule: `--identity-file` names the one-time input; without
/// it the run reads the already-provisioned store item (a
/// converged re-run or a re-registration, never a fresh drive).
/// `--passphrase-stdin` is honored for the input passphrase — the
/// manager never sees the pipe, only the store item it fills.
pub(crate) fn bootstrap_store(
    args: BootstrapArgs,
    config_override: Option<PathBuf>,
) -> Result<(), CliError> {
    let drive_dir = args.drive_dir.clone();
    let config = crate::drive_config::load_drive_config(&drive_dir, config_override.as_deref())?;
    let account = match effective_backend(&args.credentials, config.as_ref())? {
        EffectiveBackend::OsStore { account } => account,
        EffectiveBackend::File => {
            return Err(CliError::Usage(
                "store-backed bootstrap resolved the file backend: pass --credential-backend os-store with --credential-account".to_string(),
            ));
        }
    };
    let store_ref = CredentialRef::OsStore {
        account: account.clone(),
    };
    // Input material: files or the pipe for a fresh drive, the
    // store item itself for a converged re-run. Either way the
    // same material provisions the store and initializes the
    // drive, so both paths open the same keystore.
    let (identity, passphrase) = if args.credentials.identity_file.is_some() {
        store_input_from_files(&args.credentials)?
    } else {
        if args.credentials.passphrase_stdin || args.credentials.passphrase_file.is_some() {
            return Err(CliError::Usage(
                "store-backed bootstrap without --identity-file reads the provisioned store item: omit the passphrase surfaces".to_string(),
            ));
        }
        let loaded = crate::credentials::load_credential(&store_ref).map_err(loader_to_cli)?;
        let passphrase = loaded.passphrase.ok_or_else(|| {
            CliError::Usage(
                "OS credential-store item names no passphrase: pass --identity-file/--passphrase-file to provision it".to_string(),
            )
        })?;
        (loaded.identity, passphrase)
    };
    // Commit boundary, first half: the store holds identical
    // material before anything records the drive as store-backed.
    // A re-run converges where the item matches and refuses where
    // it differs — provisioning never overwrites.
    let material = LoadedCredential {
        identity: identity.clone(),
        passphrase: Some(passphrase.clone()),
    };
    match OsStoreBackend.store(&store_ref, &material) {
        Ok(()) => {}
        Err(CredentialError::Invalid { .. }) => {
            let existing =
                crate::credentials::load_credential(&store_ref).map_err(loader_to_cli)?;
            if !crate::migrate::materials_match(&existing, &material) {
                return Err(CliError::Usage(format!(
                    "OS credential-store item for account {account} already exists with different material: replace it explicitly, bootstrap never overwrites"
                )));
            }
        }
        Err(error) => return Err(loader_to_cli(error)),
    }
    // Second half: the daemon initializes and verifies the drive
    // on the same material and installs the store-backed service
    // definition. The daemon never reads the configuration here —
    // it takes the explicit reference — so the file records the
    // backend only after the daemon accepted the material.
    let relays = resolve_relays(args.relays.relay.clone(), config.as_ref()).relays;
    let manage_service = !args.no_service;
    let owner_bin = args.owner_bin.clone().map(absolutize);
    let supervise_bin = args.supervise_bin.clone().map(absolutize);
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
    let (mode, mountpoint) = parse_service_mode(&args.mode, args.mountpoint)?;
    let report = provision(ProvisionConfig {
        name: args.name,
        drive_dir: drive_dir.clone(),
        relays: relays.clone(),
        mode,
        mountpoint,
        identity,
        passphrase: passphrase.to_string(),
        init,
        registry_path,
        wyrd_bin: owner_bin,
        supervise_bin,
        credential: ServiceCredential::OsStore {
            account: account.clone(),
        },
        manage_service,
        verify_timeout: Duration::from_secs(30),
    });
    // Record the drive as store-backed once the daemon accepted
    // the material (initialized, verified open, registered): the
    // store item and the on-disk drive agree before the
    // configuration says so. The write targets the resolved
    // configuration path — the `--config` override where one was
    // given, else the drive directory default — so a later run
    // with the same override resolves the backend it just
    // adopted. An incomplete run writes nothing — the input files
    // and the converged store item are the re-run path, never a
    // half-adopted configuration.
    if !matches!(report, ProvisionState::ProvisioningIncomplete { .. }) {
        let config_path = config
            .as_ref()
            .map(|loaded| loaded.path.clone())
            .unwrap_or_else(|| drive_dir.join(DEFAULT_CONFIG_NAME));
        write_os_store_config(&config_path, &relays, &account)?;
    }
    report_bootstrap(report)
}

/// Read one store-bootstrap input from files or the pipe: the
/// identity through the hardened file reader, the passphrase from
/// the named file or stdin — never the store, which may not hold
/// the item yet. Mirrors the session-posture input shape, so the
/// provisioned material opens the same keystore every other
/// surface opens.
fn store_input_from_files(
    creds: &Credentials,
) -> Result<(wyrd_sync::keys::DeviceIdentitySecret, Zeroizing<String>), CliError> {
    if creds.passphrase_stdin && creds.passphrase_file.is_some() {
        return Err(CliError::Usage(
            "--passphrase-stdin conflicts with --passphrase-file: pass one passphrase surface"
                .to_string(),
        ));
    }
    let identity_file = creds.identity_file.clone().ok_or_else(|| {
        CliError::Usage(
            "store-backed bootstrap needs --identity-file for the input identity (read once, then the store serves it)".to_string(),
        )
    })?;
    let loaded = crate::credentials::load_credential(&CredentialRef::File {
        identity: identity_file,
        passphrase: creds.passphrase_file.clone(),
    })
    .map_err(loader_to_cli)?;
    if creds.passphrase_stdin {
        let passphrase = crate::credentials::read_passphrase_stdin(std::io::stdin().lock())?;
        return Ok((loaded.identity, passphrase));
    }
    let passphrase = loaded.passphrase.ok_or_else(|| {
        CliError::Usage(
            "store-backed bootstrap needs a passphrase: pass --passphrase-file or pipe it with --passphrase-stdin".to_string(),
        )
    })?;
    Ok((loaded.identity, passphrase))
}

/// Surface the provisioning report the way every bootstrap does:
/// ready only where the socket verified this session, every other
/// stage naming what stopped.
fn report_bootstrap(report: ProvisionState) -> Result<(), CliError> {
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
/// Resolve, then provision (file-backed drives and converged
/// store-backed re-provisions whose material already loaded):
/// credentials were already loaded once by the dispatcher for its
/// own routing; here only the resolved credential *reference*
/// travels to the service definition so the supervised owner
/// reads the same backend.
pub(crate) fn bootstrap(
    args: BootstrapArgs,
    identity: wyrd_sync::keys::DeviceIdentitySecret,
    passphrase: &str,
    config: Option<&LoadedConfig>,
) -> Result<(), CliError> {
    let drive_dir = args.drive_dir.clone();
    let relays = resolve_relays(args.relays.relay, config).relays;
    let manage_service = !args.no_service;
    // Credential reference for the definition: file paths where
    // the backend is the file backend (the identity file always,
    // the passphrase file only when a definition will actually
    // render one — session posture with `--no-service` skips the
    // file entirely), the store account where the backend is the
    // OS credential store.
    let credential =
        crate::credentials::resolve_service_credential(&args.credentials, config, manage_service)?;
    // Services run with a fixed working directory, so every path
    // the definition carries is absolute: canonicalize what must
    // exist (the credential files were just read) and absolutize
    // the binaries without requiring them (a missing supervise
    // wrapper is its own actionable error at provision time).
    // Store references name an account, not a path: nothing to
    // canonicalize, and nothing secret travels.
    let credential = match credential {
        wyrd_daemon::bootstrap::ServiceCredential::File {
            identity_file,
            passphrase_file,
        } => {
            let identity_file = canonicalize(&identity_file)?;
            let passphrase_file = passphrase_file
                .map(|path| canonicalize(&path))
                .transpose()?;
            wyrd_daemon::bootstrap::ServiceCredential::File {
                identity_file,
                passphrase_file,
            }
        }
        store => store,
    };
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
    let (mode, mountpoint) = parse_service_mode(&args.mode, args.mountpoint)?;
    let report = provision(ProvisionConfig {
        name: args.name,
        drive_dir,
        relays,
        mode,
        mountpoint,
        identity,
        passphrase: passphrase.to_string(),
        init,
        registry_path,
        wyrd_bin: owner_bin,
        supervise_bin,
        credential,
        manage_service,
        verify_timeout: Duration::from_secs(30),
    });
    report_bootstrap(report)
}

/// Surface registry entries the read skipped: the writer carries
/// the valid set plus the skipped raws forward, so a rotten entry
/// would otherwise vanish without any user-visible signal. Non-fatal
/// — discovery keeps working — but loud. The list travels in the
/// provisioning result itself, because re-reading after the write
/// would find nothing left to report.
pub(crate) fn warn_skipped(skipped: &[String]) {
    for skipped in skipped {
        eprintln!("warning: registry entry skipped: {skipped}");
    }
}

/// Canonicalize a file that must exist (credential files were
/// just read): resolves symlinks and guarantees absoluteness, so
/// the service definition names one unambiguous path.
pub(crate) fn canonicalize(path: &PathBuf) -> Result<PathBuf, CliError> {
    std::fs::canonicalize(path).map_err(|source| CliError::Io {
        path: path.clone(),
        source,
    })
}

/// Absolutize a path that need not exist yet (binaries): joins
/// relative paths onto the working directory without touching the
/// filesystem.
pub(crate) fn absolutize(path: PathBuf) -> PathBuf {
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
