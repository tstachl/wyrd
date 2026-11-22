//! Structured argv builders and the credential-reference vocabulary
//! they share. Secrets never cross this boundary: only paths and store
//! account names become argv elements. The parent module owns the
//! authority contract and the request types.

use std::ffi::OsString;
use std::path::PathBuf;

use super::{
    DeleteRequest, LeaveRequest, ProvisionRequest, ReconfigureRequest, ServiceDisableRequest,
    ServiceEnableRequest, ServiceStartRequest, UnlockRequest,
};

/// Where one lifecycle operation reads the drive credential:
/// credential files on disk, or one OS credential-store item named
/// by its account label. References only, in either backend —
/// secret contents never cross IPC or argv.
///
/// The file backend is the CLI and operator-provisioned default.
/// The store backend is the desktop-managed direction
/// (`docs/contracts/credential-loading.md`): the `os-store`
/// account the configuration (or a migrated drive) names, resolved
/// by the CLI through the loader boundary — the adapter never
/// touches the store itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource {
    /// Hardened credential files, read once by the CLI. Each
    /// operation consumes what it needs and refuses what is
    /// missing before any process spawns — unlock, for example,
    /// names no passphrase file (the session passphrase rides the
    /// stdin pipe), so it carries `None` there honestly instead of
    /// a dummy path.
    File {
        /// Nostr identity secret file (32 raw bytes or 64 hex chars).
        identity_file: Option<PathBuf>,
        /// Keystore passphrase file (UTF-8 text).
        passphrase_file: Option<PathBuf>,
    },
    /// One OS credential-store item (`wyrd-drive-credential`
    /// service, account label naming the drive's item).
    OsStore {
        /// Store account naming the drive's item (the reference,
        /// never a secret).
        account: String,
        /// One-time seed files for a fresh store-backed bootstrap:
        /// the input material the CLI reads, stores under the
        /// account, and reads back through the production reader.
        /// `None` past provisioning — converged re-runs and every
        /// other operation read the item itself, never files.
        seed: Option<SeedFiles>,
    },
}

/// One-time seed inputs for a fresh store-backed bootstrap: the
/// onboarding-written files the CLI's commit boundary reads before
/// the drive is recorded as store-backed. Bootstrap input only —
/// every other operation refuses a seed before any process spawns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedFiles {
    /// Nostr identity secret file seeding the store item.
    pub identity_file: PathBuf,
    /// Keystore passphrase file seeding the store item.
    pub passphrase_file: PathBuf,
}

/// Render one credential source into the CLI credential flags.
/// File paths ride `--identity-file`/`--passphrase-file`; a store
/// reference rides `--credential-backend os-store` plus
/// `--credential-account`, with the seed files beside them where
/// the operation takes seed input. `seeds_allowed` is bootstrap
/// only: every other operation refuses a seed before any process
/// spawns, mirroring the CLI's one-backend-per-drive refusal.
fn push_credential_flags(
    argv: &mut Vec<OsString>,
    source: &CredentialSource,
    seeds_allowed: bool,
) -> Result<(), String> {
    match source {
        CredentialSource::File {
            identity_file,
            passphrase_file,
        } => {
            let identity = identity_file.as_ref().ok_or_else(|| {
                "no identity file given: pass the credential path the drive reads".to_string()
            })?;
            let passphrase = passphrase_file.as_ref().ok_or_else(|| {
                "no passphrase file given: pass the credential path the drive reads".to_string()
            })?;
            argv.push("--identity-file".into());
            argv.push(identity.as_os_str().to_owned());
            argv.push("--passphrase-file".into());
            argv.push(passphrase.as_os_str().to_owned());
            Ok(())
        }
        CredentialSource::OsStore { account, seed } => {
            if account.is_empty() {
                return Err(
                    "no OS credential-store account: name the drive's store item with --credential-account"
                        .to_string(),
                );
            }
            if let Some(seed) = seed {
                if !seeds_allowed {
                    return Err(
                        "seed files are bootstrap input only: this operation reads the provisioned store item, never files"
                            .to_string(),
                    );
                }
                argv.push("--identity-file".into());
                argv.push(seed.identity_file.as_os_str().to_owned());
                argv.push("--passphrase-file".into());
                argv.push(seed.passphrase_file.as_os_str().to_owned());
            }
            argv.push("--credential-backend".into());
            argv.push("os-store".into());
            argv.push("--credential-account".into());
            argv.push(account.into());
            Ok(())
        }
    }
}

/// Pure argv builder for `wyrd drive <dir> bootstrap …`: every argument is a
/// separate argv element, never shell-constructed. Mirrors
/// `crates/wyrd-cli/src/args.rs` (`DriveAction::Bootstrap`).
pub fn bootstrap_argv(request: &ProvisionRequest) -> Result<Vec<OsString>, String> {
    match request.mode.as_str() {
        "vault" => {
            if request.mountpoint.is_some() {
                return Err(
                    "vault mode carries no mountpoint: remove the mountpoint for a vault entry"
                        .to_string(),
                );
            }
        }
        "mount" => {
            if request.mountpoint.is_none() {
                return Err(
                    "mount mode needs a mountpoint: pass a mountpoint with an absolute path"
                        .to_string(),
                );
            }
        }
        other => {
            return Err(format!(
                "unknown service mode {other:?}: this build manages `vault` and `mount`"
            ));
        }
    }
    let mut argv: Vec<OsString> = vec!["drive".into(), request.drive_dir.as_os_str().to_owned()];
    // Fresh store-backed bootstraps seed the item from the
    // one-time input files; every other source combination renders
    // the same flags the CLI's dispatcher routes on.
    push_credential_flags(&mut argv, &request.credentials, true)?;
    argv.push("bootstrap".into());
    argv.push("--name".into());
    argv.push(request.name.clone().into());
    argv.push("--mode".into());
    argv.push(request.mode.clone().into());
    if let Some(mountpoint) = &request.mountpoint {
        argv.push("--mountpoint".into());
        argv.push(mountpoint.as_os_str().to_owned());
    }
    for relay in &request.relays {
        argv.push("--relay".into());
        argv.push(relay.into());
    }
    if let Some(invitation) = &request.invitation_file {
        argv.push("--invitation-file".into());
        argv.push(invitation.as_os_str().to_owned());
    }
    if let Some(owner) = &request.expected_owner {
        argv.push("--expected-owner".into());
        argv.push(owner.into());
    }
    if let Some(drive) = &request.expected_drive {
        argv.push("--expected-drive".into());
        argv.push(drive.into());
    }
    if let Some(genesis) = &request.expected_genesis {
        argv.push("--expected-genesis".into());
        argv.push(genesis.into());
    }
    if let Some(registry) = &request.registry_path {
        argv.push("--registry".into());
        argv.push(registry.as_os_str().to_owned());
    }
    if let Some(owner) = &request.owner_bin {
        argv.push("--owner-bin".into());
        argv.push(owner.as_os_str().to_owned());
    }
    if let Some(supervise) = &request.supervise_bin {
        argv.push("--supervise-bin".into());
        argv.push(supervise.as_os_str().to_owned());
    }
    if request.no_service {
        argv.push("--no-service".into());
    }
    Ok(argv)
}

/// Pure argv builder for `wyrd member <drive> leave [--yes] …`.
/// Credentials ride as file paths or one store reference, never
/// contents. `Credentials` is
/// flattened on the parent (`Command::Member`), not global, so the
/// credential flags must precede the `leave` verb — clap rejects them
/// after it. A seed beside a store reference refuses: leave reads
/// the provisioned credential, never bootstrap input.
pub fn leave_argv(request: &LeaveRequest) -> Result<Vec<OsString>, String> {
    let mut argv: Vec<OsString> = vec!["member".into(), request.drive_dir.as_os_str().to_owned()];
    push_credential_flags(&mut argv, &request.credentials, false)?;
    argv.push("leave".into());
    if request.acknowledged {
        argv.push("--yes".into());
    }
    Ok(argv)
}

/// Pure argv builder for `wyrd drive <dir> delete --yes`.
/// Credentials are flattened on `Command::Drive`, so the flags precede
/// the `delete` verb. Seeds never apply: deletion reads the
/// provisioned credential. The CLI authors `Remove(self)` before it
/// removes the directory, and refuses unacknowledged, non-owner and
/// frozen drives without touching anything.
pub fn delete_argv(request: &DeleteRequest) -> Result<Vec<OsString>, String> {
    let mut argv: Vec<OsString> = vec!["drive".into(), request.drive_dir.as_os_str().to_owned()];
    push_credential_flags(&mut argv, &request.credentials, false)?;
    argv.push("delete".into());
    if request.acknowledged {
        argv.push("--yes".into());
    }
    Ok(argv)
}

/// Pure argv builder for `wyrd drive <dir> reconfigure …`: every
/// argument is a separate argv element, never shell-constructed.
/// Mirrors `crates/wyrd-cli/src/args.rs` (`DriveAction::Reconfigure`).
/// Empty `relays` emit no `--relay` flags (the CLI keeps the
/// registered set); non-empty relays replace it.
pub fn reconfigure_argv(request: &ReconfigureRequest) -> Result<Vec<OsString>, String> {
    match request.mode.as_str() {
        "vault" => {
            if request.mountpoint.is_some() {
                return Err(
                    "vault mode carries no mountpoint: remove the mountpoint for a vault entry"
                        .to_string(),
                );
            }
        }
        "mount" => {
            if request.mountpoint.is_none() {
                return Err(
                    "mount mode needs a mountpoint: pass a mountpoint with an absolute path"
                        .to_string(),
                );
            }
        }
        other => {
            return Err(format!(
                "unknown service mode {other:?}: this build manages `vault` and `mount`"
            ));
        }
    }
    let mut argv: Vec<OsString> = vec!["drive".into(), request.drive_dir.as_os_str().to_owned()];
    // Seeds refuse here: reconfigure reads the provisioned
    // credential (files or the store item), never bootstrap input.
    push_credential_flags(&mut argv, &request.credentials, false)?;
    argv.push("reconfigure".into());
    argv.push("--mode".into());
    argv.push(request.mode.clone().into());
    if let Some(mountpoint) = &request.mountpoint {
        argv.push("--mountpoint".into());
        argv.push(mountpoint.as_os_str().to_owned());
    }
    for relay in &request.relays {
        argv.push("--relay".into());
        argv.push(relay.into());
    }
    if let Some(registry) = &request.registry_path {
        argv.push("--registry".into());
        argv.push(registry.as_os_str().to_owned());
    }
    if let Some(owner) = &request.owner_bin {
        argv.push("--owner-bin".into());
        argv.push(owner.as_os_str().to_owned());
    }
    if let Some(supervise) = &request.supervise_bin {
        argv.push("--supervise-bin".into());
        argv.push(supervise.as_os_str().to_owned());
    }
    if request.no_service {
        argv.push("--no-service".into());
    }
    Ok(argv)
}

/// Pure argv builder for `wyrd drive <dir> service-start …`: every
/// argument is a separate argv element, never shell-constructed.
/// Mirrors `crates/wyrd-cli/src/args.rs`
/// (`DriveAction::ServiceStart`). No credentials: starting opens no
/// keystore and proves nothing about membership.
pub fn service_start_argv(request: &ServiceStartRequest) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        "drive".into(),
        request.drive_dir.as_os_str().to_owned(),
        "service-start".into(),
    ];
    if let Some(registry) = &request.registry_path {
        argv.push("--registry".into());
        argv.push(registry.as_os_str().to_owned());
    }
    argv
}

/// Pure argv builder for `wyrd drive <dir> service-enable …`: every
/// argument is a separate argv element, never shell-constructed.
/// Mirrors `crates/wyrd-cli/src/args.rs`
/// (`DriveAction::ServiceEnable`). No credentials: enabling records
/// a login-start preference and opens no keystore.
pub fn service_enable_argv(request: &ServiceEnableRequest) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        "drive".into(),
        request.drive_dir.as_os_str().to_owned(),
        "service-enable".into(),
    ];
    if let Some(registry) = &request.registry_path {
        argv.push("--registry".into());
        argv.push(registry.as_os_str().to_owned());
    }
    argv
}

/// Pure argv builder for `wyrd drive <dir> service-disable …`:
/// every argument is a separate argv element, never
/// shell-constructed. Mirrors `crates/wyrd-cli/src/args.rs`
/// (`DriveAction::ServiceDisable`). No credentials: disabling stops
/// the unit now and records no login start, and opens no keystore.
pub fn service_disable_argv(request: &ServiceDisableRequest) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        "drive".into(),
        request.drive_dir.as_os_str().to_owned(),
        "service-disable".into(),
    ];
    if let Some(registry) = &request.registry_path {
        argv.push("--registry".into());
        argv.push(registry.as_os_str().to_owned());
    }
    argv
}

/// Pure argv builder for `wyrd drive <dir> unlock …`: every
/// argument is a separate argv element, never shell-constructed.
/// Mirrors `crates/wyrd-cli/src/args.rs` (`DriveAction::Unlock`).
/// Credential flags ride the `drive` parent (flattened, not
/// global), so they precede the `unlock` verb — clap rejects them
/// after it, like the `leave` argv above. The session passphrase
/// always rides the stdin pipe (`--passphrase-stdin`): no
/// `--passphrase-file` ever, in either backend. A file-backed
/// unlock names `--identity-file`; a store-backed unlock names the
/// store reference and loads the identity from the item. Seeds
/// refuse: unlock reads the provisioned identity, never bootstrap
/// input.
pub fn unlock_argv(request: &UnlockRequest) -> Result<Vec<OsString>, String> {
    let mut argv: Vec<OsString> = vec!["drive".into(), request.drive_dir.as_os_str().to_owned()];
    match &request.credentials {
        CredentialSource::File { identity_file, .. } => {
            let identity = identity_file.as_ref().ok_or_else(|| {
                "no identity file given: pass the credential path the drive reads".to_string()
            })?;
            argv.push("--identity-file".into());
            argv.push(identity.as_os_str().to_owned());
        }
        CredentialSource::OsStore { account, seed } => {
            if account.is_empty() {
                return Err(
                    "no OS credential-store account: name the drive's store item with --credential-account"
                        .to_string(),
                );
            }
            if seed.is_some() {
                return Err(
                    "seed files are bootstrap input only: unlock reads the provisioned store identity"
                        .to_string(),
                );
            }
            argv.push("--credential-backend".into());
            argv.push("os-store".into());
            argv.push("--credential-account".into());
            argv.push(account.into());
        }
    }
    argv.push("--passphrase-stdin".into());
    argv.push("unlock".into());
    for relay in &request.relays {
        argv.push("--relay".into());
        argv.push(relay.into());
    }
    if let Some(quota) = request.retained_quota {
        argv.push("--retained-quota".into());
        argv.push(quota.to_string().into());
    }
    Ok(argv)
}
