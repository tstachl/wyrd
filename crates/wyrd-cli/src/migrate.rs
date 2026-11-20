//! Failure-safe migration from file credentials to the OS
//! credential store (`docs/contracts/credential-loading.md`,
//! migration slice).
//!
//! The six gated steps, in order — a later step never runs where an
//! earlier one failed, and the old files are never retired before
//! the new path is proven twice (read-back through the new backend,
//! then a real keystore open):
//!
//! 1. Read and validate the existing credential through the
//!    hardened file reader (the source backend's `load`).
//! 2. Store it in the OS credential store under the loader's commit
//!    boundary (the target backend's `store`: validate in memory,
//!    write, read back, compare).
//! 3. Verify the owner retrieves it through the new backend (an
//!    explicit `load` plus comparison — the gate, not a re-read
//!    for display).
//! 4. Update the drive's credential reference (the configuration
//!    rewrite to the store backend and account).
//! 5. Verify the owner opens the existing keystore with the
//!    migrated material (a real [`Engine::open_keystore`], dropped
//!    at once — the proof, not a start).
//! 6. Only then retire the old credential files (the source
//!    backend's `delete`, verified absent afterwards).
//!
//! Backend-generic over the loader boundary: production passes the
//! file backend as source and the OS store backend as target, and
//! tests pass two file backends at different paths — the gated
//! order is backend-independent. Migration changes where the
//! passphrase is kept, never the Drive Root Key, never the keystore
//! wrap: step 5 opens the *existing* keystore, and a failed open
//! stops the migration with both paths intact.
//!
//! Interruption converges: steps are idempotent where the state
//! already matches (an existing store item with identical material
//! continues; a rewritten configuration rewrites identically; an
//! already-migrated drive reports done after verifying), and every
//! other partial state fails closed with the old files untouched.
//! Service definitions are out of scope: the manager's installed
//! unit still names the retired files until the desktop-wiring slice
//! re-renders it, so the command warns where a definition exists.
//! Until then a migrated drive starts through the store-aware owner
//! path, not the old unit.

use std::path::PathBuf;

use wyrd_sync::keys::{CredentialError, CredentialLoader, CredentialRef, LoadedCredential};
use wyrd_sync::runtime::Engine;

use crate::credentials::{loader_to_cli, FileBackend};
use crate::drive_config::{
    load_drive_config, os_store_reference, resolve_credential_path, validate_credential_account,
    write_os_store_config, DEFAULT_CONFIG_NAME,
};
use crate::CliError;

/// What one migration run decided: either the full six steps ran, or
/// the drive was already migrated and the store path re-verified.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MigrateOutcome {
    /// Steps 1-6 ran this invocation: the store item is provisioned
    /// and verified, the configuration names it, the keystore opened
    /// under it, and the old files are retired.
    Migrated {
        /// The store account the credential now lives under.
        account: String,
    },
    /// A previous run already migrated this drive: the configuration
    /// names the store backend, the item loads, and the keystore
    /// opens under it. Nothing was written or deleted.
    AlreadyMigrated {
        /// The store account the credential lives under.
        account: String,
    },
}

/// Inputs to one migration run: resolved paths, never flags — the
/// dispatcher resolves flags-first, file-second before calling, so a
/// re-run after the configuration rewrite (which removes the file
/// paths) passes the same explicit paths back in.
pub(crate) struct MigrateRequest {
    /// Drive directory holding the keystore and the configuration.
    pub drive_dir: PathBuf,
    /// Resolved identity file the file backend reads.
    pub identity_file: PathBuf,
    /// Resolved passphrase file the file backend reads. `None` is
    /// refused: session-posture migration cannot prove step 5
    /// without a passphrase, so managed drives (which always name
    /// one) migrate first.
    pub passphrase_file: Option<PathBuf>,
    /// Store account label the credential migrates under.
    pub account: String,
    /// Configuration file the step-4 rewrite targets: the `--config`
    /// override or the drive directory's `default.toml`, which must
    /// already exist — migration adopts provisioned drives, it never
    /// invents their configuration.
    pub config_path: PathBuf,
}

/// Migrate one drive's credential from the source backend to the
/// target backend through the six gated steps. Fails closed at every
/// gate: a failed step returns with earlier state intact (the old
/// files are deleted only in step 6, after two independent proofs of
/// the new path), and an interrupted run converges on re-run.
pub(crate) fn migrate_credential(
    source: &dyn CredentialLoader,
    target: &dyn CredentialLoader,
    request: &MigrateRequest,
) -> Result<MigrateOutcome, CliError> {
    let file = CredentialRef::File {
        identity: request.identity_file.clone(),
        passphrase: request.passphrase_file.clone(),
    };
    let store = CredentialRef::OsStore {
        account: request.account.clone(),
    };
    // Step 1: read and validate through the hardened file reader.
    // A missing identity file is the already-migrated signal, never
    // an auto-create: confirm the store path instead (below). A
    // passphrase-less load is the session posture, refused after the
    // read proves what it is — session drives cannot prove step 5
    // without a passphrase, so managed drives (which always name a
    // passphrase file) migrate first.
    let material = match source.load(&file) {
        Ok(material) => material,
        Err(CredentialError::Missing { .. }) => {
            // A genuine partial pair (one file present, the other
            // gone) is not a migration state: name the leftover
            // file(s) so the operator knows what stands. Only two
            // absent files enter the already-migrated confirmation.
            // (A step-6 interruption names its survivor here: the
            // identity deletes first, so a passphrase-only leftover
            // reports the passphrase, never the missing identity.)
            let mut leftovers: Vec<String> = Vec::new();
            if request.identity_file.exists() {
                leftovers.push(request.identity_file.display().to_string());
            }
            if request
                .passphrase_file
                .as_ref()
                .is_some_and(|path| path.exists())
            {
                leftovers.push(
                    request
                        .passphrase_file
                        .as_ref()
                        .expect("checked present above")
                        .display()
                        .to_string(),
                );
            }
            if !leftovers.is_empty() {
                return Err(CliError::Usage(format!(
                    "credential files partially present (leftover {}): restore the pair or remove it by hand — migration never resumes from half a pair",
                    leftovers.join(", ")
                )));
            }
            return confirm_already_migrated(target, request);
        }
        Err(error) => return Err(loader_to_cli(error)),
    };
    if material.passphrase.is_none() {
        return Err(CliError::Usage(
            "migration needs a passphrase file: session-posture drives (typed passphrase, no file) cannot prove the keystore opens under the migrated credential"
                .to_string(),
        ));
    }
    // Step 2: store under the commit boundary. An existing item with
    // identical material is a converged re-run, not a refusal — but
    // a differing item never overwrites: that is `replace`, an
    // explicit operator act, never a migration side effect.
    // Convergence keys off a load-and-compare, never off matching
    // the backend's wording: a reworded guard must not turn a
    // re-run into a refusal.
    match target.load(&store) {
        Ok(existing) => {
            if !materials_match(&existing, &material) {
                return Err(CliError::Usage(format!(
                    "OS credential-store item for account {} already exists with different material: replace it explicitly, migration never overwrites",
                    request.account
                )));
            }
        }
        Err(CredentialError::Missing { .. }) => {
            match target.store(&store, &material) {
                Ok(()) => {}
                // A concurrent provision between the probe and the
                // write lands here: converge or refuse by comparing
                // the material, never by matching the guard's words.
                // Any other `Invalid` (a facade size or name refusal,
                // not an existing item) leaves nothing behind, so a
                // `Missing` follow-up re-reports the refusal itself —
                // never a missing item that misdirects the operator.
                Err(original @ CredentialError::Invalid { .. }) => match target.load(&store) {
                    Ok(existing) => {
                        if !materials_match(&existing, &material) {
                            return Err(CliError::Usage(format!(
                                    "OS credential-store item for account {} already exists with different material: replace it explicitly, migration never overwrites",
                                    request.account
                                )));
                        }
                    }
                    Err(CredentialError::Missing { .. }) => {
                        return Err(loader_to_cli(original));
                    }
                    Err(error) => return Err(loader_to_cli(error)),
                },
                Err(error) => return Err(loader_to_cli(error)),
            }
        }
        Err(error) => return Err(loader_to_cli(error)),
    }
    // Step 3: the owner retrieves through the new backend — an
    // explicit load plus comparison, the gate, not a re-read.
    let retrieved = target.load(&store).map_err(loader_to_cli)?;
    if !materials_match(&retrieved, &material) {
        return Err(CliError::Usage(format!(
            "OS credential-store item for account {} did not read back: migration stops with the file credential intact",
            request.account
        )));
    }
    // Step 4: update the drive's credential reference.
    let config = load_drive_config(&request.drive_dir, Some(&request.config_path))?;
    let Some(config) = config else {
        return Err(CliError::Usage(format!(
            "no drive configuration at {}: migration adopts provisioned drives, it never invents their configuration",
            request.config_path.display()
        )));
    };
    write_os_store_config(&request.config_path, &config.relays, &request.account)?;
    // The rewritten file parses back to the same reference it
    // records: the reference the owner will resolve, proven before
    // the keystore proof below.
    let reread =
        load_drive_config(&request.drive_dir, Some(&request.config_path))?.ok_or_else(|| {
            CliError::Usage(format!(
                "configuration at {} unreadable after the rewrite",
                request.config_path.display()
            ))
        })?;
    let reread_reference = os_store_reference(&reread)?;
    if reread_reference != store {
        return Err(CliError::Usage(format!(
            "configuration at {} does not name the migrated account after the rewrite",
            request.config_path.display()
        )));
    }
    // Step 5: the owner opens the existing keystore with the migrated
    // material — a real open, dropped at once. The Drive Root Key and
    // the keystore wrap are untouched: this proves the new path
    // opens what the old path opened. The engine holds no secret past
    // the open, so the proof keeps nothing a moment longer than the
    // check.
    let passphrase = retrieved.passphrase.ok_or_else(|| {
        CliError::Usage(
            "migrated credential names no passphrase: the keystore cannot be proven open without one"
                .to_string(),
        )
    })?;
    let identity = retrieved.identity;
    Engine::open_keystore(request.drive_dir.clone(), passphrase.as_str(), identity)?;
    // Step 6: only now retire the old files — verified absent
    // afterwards by the backend's own delete.
    source.delete(&file).map_err(loader_to_cli)?;
    Ok(MigrateOutcome::Migrated {
        account: request.account.clone(),
    })
}

/// Two loaded materials name the same credential: same device (by
/// public id, never by bytes) and byte-identical passphrase posture
/// (`None` matches only `None` — a session item is never equal to a
/// named one).
fn materials_match(first: &LoadedCredential, second: &LoadedCredential) -> bool {
    first.identity.device_id() == second.identity.device_id()
        && first.passphrase.as_deref() == second.passphrase.as_deref()
}

/// The files are already gone: either a previous run completed
/// (configuration names the store backend) or the operator removed
/// them by hand. Confirm the store path end to end — item loads,
/// reference resolves, keystore opens — and report done without
/// writing or deleting anything. Anything less is a refusal naming
/// what is missing, never an auto-create.
fn confirm_already_migrated(
    target: &dyn CredentialLoader,
    request: &MigrateRequest,
) -> Result<MigrateOutcome, CliError> {
    let config = load_drive_config(&request.drive_dir, Some(&request.config_path))?;
    let Some(config) = config else {
        return Err(CliError::Usage(format!(
            "no credential file {} and no drive configuration at {}: nothing to migrate from",
            request.identity_file.display(),
            request.config_path.display()
        )));
    };
    let reference = os_store_reference(&config)?;
    let expected = CredentialRef::OsStore {
        account: request.account.clone(),
    };
    if reference != expected {
        return Err(CliError::Usage(format!(
            "no credential file {} and the configuration names a different store item: pass the configured account or restore the files",
            request.identity_file.display()
        )));
    }
    let retrieved = target.load(&reference).map_err(loader_to_cli)?;
    let passphrase = retrieved.passphrase.ok_or_else(|| {
        CliError::Usage(
            "migrated credential names no passphrase: the keystore cannot be proven open without one"
                .to_string(),
        )
    })?;
    Engine::open_keystore(
        request.drive_dir.clone(),
        passphrase.as_str(),
        retrieved.identity,
    )?;
    Ok(MigrateOutcome::AlreadyMigrated {
        account: request.account.clone(),
    })
}

/// Dispatcher entry for `drive migrate-credentials`: resolve the
/// file paths flags-first, file-second, validate the account, run
/// the six gated steps, and report. The configuration must already
/// exist (migration adopts provisioned drives); where it already
/// names the store backend, resolution falls back to nominal paths
/// so a re-run after the step-4 rewrite converges through the
/// already-migrated confirmation instead of refusing on missing
/// file keys.
pub(crate) fn migrate_drive_credentials(
    drive_dir: PathBuf,
    account: String,
    credentials: &crate::args::Credentials,
    config_override: Option<PathBuf>,
) -> Result<(), CliError> {
    use wyrd_credential_store::OsStoreBackend;

    use crate::drive_config::LoadedConfig;
    let config_path = match &config_override {
        Some(path) => path.clone(),
        None => drive_dir.join(DEFAULT_CONFIG_NAME),
    };
    validate_credential_account(&config_path, &account)?;
    let config: LoadedConfig = load_drive_config(&drive_dir, config_override.as_deref())?
        .ok_or_else(|| {
            CliError::Usage(format!(
            "no drive configuration at {}: migration adopts provisioned drives — create the configuration (relays plus the credential file paths) or pass --config",
            config_path.display()
        ))})?;
    let store_configured = config.credential_backend.as_deref() == Some("os-store")
        && config.credential_account.as_deref() == Some(account.as_str());
    let identity_file = match resolve_credential_path(
        credentials.identity_file.clone(),
        config.identity_file.clone(),
        "--identity-file",
        "identity_file",
    ) {
        Ok(path) => path,
        // Post-rewrite re-runs removed the file keys: fall back to a
        // nominal path so the missing-file branch confirms the store
        // path instead of refusing on resolution.
        Err(error) if store_configured => {
            let _ = error;
            drive_dir.join("identity")
        }
        Err(error) => return Err(error),
    };
    let passphrase_file = match (
        credentials.passphrase_file.clone(),
        config.passphrase_file.clone(),
    ) {
        (Some(path), _) | (None, Some(path)) => Some(path),
        // Same post-rewrite fallback as the identity above: `None`
        // reaches the already-migrated confirmation (which never
        // needs the files) rather than the session-posture refusal.
        (None, None) if store_configured => None,
        (None, None) => {
            return Err(CliError::Usage(
                "no --passphrase-file given and no passphrase_file in the drive configuration: migration needs the passphrase file it retires".to_string(),
            ));
        }
    };
    let outcome = migrate_credential(
        &FileBackend,
        &OsStoreBackend,
        &MigrateRequest {
            drive_dir: drive_dir.clone(),
            identity_file,
            passphrase_file,
            account: account.clone(),
            config_path: config_path.clone(),
        },
    )?;
    match outcome {
        MigrateOutcome::Migrated { account } => {
            println!(
                "migrated {} to OS credential store account {account} (credential files retired)",
                drive_dir.display()
            );
            eprintln!(
                "warning: the OS service definition (if installed) still names the retired files: reinstall it once the desktop-wiring slice lands; until then start the owner with the migrated configuration, not the old unit"
            );
        }
        MigrateOutcome::AlreadyMigrated { account } => {
            println!(
                "already migrated: {} uses OS credential store account {account} (verified)",
                drive_dir.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::io::Write as _;

    use wyrd_sync::keys::{
        CredentialError, CredentialLoader, CredentialRef, CredentialSubject, DeviceIdentitySecret,
        LoadedCredential,
    };
    use zeroize::Zeroizing;

    use crate::tests_harness::TempDir;

    /// In-memory backend behind the loader boundary: keyed by the
    /// reference's description (file path or store account), holding
    /// the canonical text forms. Mirrors the production guards the
    /// migration relies on — provision refuses where an item exists
    /// (`Invalid` naming "already exists"), deletion verifies
    /// absence — so the gated order pins without a platform store.
    struct MockBackend {
        items: RefCell<HashMap<String, (String, Option<String>)>>,
    }

    impl MockBackend {
        fn new() -> Self {
            MockBackend {
                items: RefCell::new(HashMap::new()),
            }
        }

        fn key(reference: &CredentialRef) -> (String, CredentialSubject) {
            match reference {
                CredentialRef::File { identity, .. } => (
                    format!("file:{}", identity.display()),
                    CredentialSubject::File {
                        path: identity.clone(),
                    },
                ),
                CredentialRef::OsStore { account } => (
                    format!("store:{account}"),
                    CredentialSubject::Account {
                        account: account.clone(),
                    },
                ),
            }
        }
    }

    impl CredentialLoader for MockBackend {
        fn load(&self, reference: &CredentialRef) -> Result<LoadedCredential, CredentialError> {
            let (key, subject) = Self::key(reference);
            let items = self.items.borrow();
            let Some((identity_hex, passphrase)) = items.get(&key) else {
                let detail = match reference {
                    CredentialRef::File { identity, .. } => {
                        format!("{}: No such file or directory", identity.display())
                    }
                    CredentialRef::OsStore { account } => {
                        format!("no OS credential-store item for account {account}")
                    }
                };
                return Err(CredentialError::Missing { subject, detail });
            };
            let raw = hex::decode(identity_hex).map_err(|_| CredentialError::Invalid {
                subject: subject.clone(),
                detail: "mock item names an identity that is not hex".to_string(),
            })?;
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(&raw);
            let identity =
                DeviceIdentitySecret::from_bytes(bytes).map_err(|_| CredentialError::Invalid {
                    subject: subject.clone(),
                    detail: "mock item names an identity that is not a valid scalar".to_string(),
                })?;
            Ok(LoadedCredential {
                identity,
                passphrase: passphrase.clone().map(Zeroizing::new),
            })
        }

        fn store(
            &self,
            reference: &CredentialRef,
            material: &LoadedCredential,
        ) -> Result<(), CredentialError> {
            let (key, subject) = Self::key(reference);
            if self.items.borrow().contains_key(&key) {
                return Err(CredentialError::Invalid {
                    subject,
                    detail: "mock item already exists: replace, never re-provision".to_string(),
                });
            }
            self.replace(reference, material)
        }

        fn replace(
            &self,
            reference: &CredentialRef,
            material: &LoadedCredential,
        ) -> Result<(), CredentialError> {
            let (key, subject) = Self::key(reference);
            let identity_hex = material.identity.to_hex().to_string();
            let passphrase = material.passphrase.as_ref().map(|text| text.to_string());
            self.items
                .borrow_mut()
                .insert(key.clone(), (identity_hex, passphrase));
            // Commit boundary like production: read back and compare.
            let loaded = self.load(reference)?;
            if loaded.identity.device_id() != material.identity.device_id()
                || loaded.passphrase.as_deref() != material.passphrase.as_deref()
            {
                return Err(CredentialError::Invalid {
                    subject,
                    detail: "mock item did not read back".to_string(),
                });
            }
            Ok(())
        }

        fn delete(&self, reference: &CredentialRef) -> Result<(), CredentialError> {
            let (key, subject) = Self::key(reference);
            if self.items.borrow_mut().remove(&key).is_none() {
                let detail = match reference {
                    CredentialRef::File { identity, .. } => {
                        format!("no credential file {}", identity.display())
                    }
                    CredentialRef::OsStore { account } => {
                        format!("no OS credential-store item for account {account}")
                    }
                };
                return Err(CredentialError::Missing { subject, detail });
            }
            Ok(())
        }
    }

    /// One queued `load` answer: canned material to serve, or
    /// `None` for a `Missing` refusal.
    type QueuedLoad = Option<(String, Option<String>)>;

    /// Scripted target backend for the step-2 catch arms: replays
    /// a canned answer per `load` call and refuses every `store`
    /// with one `Invalid` detail, so tests reach the race the
    /// production backends only hit under concurrency. Queued
    /// answers (`Some` material, `None` for `Missing`) drain first;
    /// once exhausted, loads serve the last material the queue (or
    /// a successful `store`) recorded. Reference-agnostic: the
    /// migration uses one store reference throughout.
    struct ScriptedTarget {
        account: String,
        loads: RefCell<std::collections::VecDeque<QueuedLoad>>,
        store_refusal: Option<String>,
        stored: RefCell<Option<(String, Option<String>)>>,
    }

    impl ScriptedTarget {
        fn refusing(account: &str, detail: &str) -> Self {
            ScriptedTarget {
                account: account.to_string(),
                loads: RefCell::new(std::collections::VecDeque::new()),
                store_refusal: Some(detail.to_string()),
                stored: RefCell::new(None),
            }
        }

        fn replay(account: &str, loads: Vec<QueuedLoad>, store_refusal: &str) -> Self {
            ScriptedTarget {
                account: account.to_string(),
                loads: RefCell::new(loads.into()),
                store_refusal: Some(store_refusal.to_string()),
                stored: RefCell::new(None),
            }
        }

        fn subject(&self) -> CredentialSubject {
            CredentialSubject::Account {
                account: self.account.clone(),
            }
        }

        fn canned(material: &LoadedCredential) -> (String, Option<String>) {
            (
                material.identity.to_hex().to_string(),
                material.passphrase.as_ref().map(|text| text.to_string()),
            )
        }

        fn materialize(
            &self,
            canned: &(String, Option<String>),
        ) -> Result<LoadedCredential, CredentialError> {
            let raw = hex::decode(&canned.0).map_err(|_| CredentialError::Invalid {
                subject: self.subject(),
                detail: "scripted item names an identity that is not hex".to_string(),
            })?;
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(&raw);
            let identity =
                DeviceIdentitySecret::from_bytes(bytes).map_err(|_| CredentialError::Invalid {
                    subject: self.subject(),
                    detail: "scripted item names an identity that is not a valid scalar"
                        .to_string(),
                })?;
            Ok(LoadedCredential {
                identity,
                passphrase: canned.1.clone().map(Zeroizing::new),
            })
        }
    }

    impl CredentialLoader for ScriptedTarget {
        fn load(&self, _reference: &CredentialRef) -> Result<LoadedCredential, CredentialError> {
            // Queued answers drain first; afterwards the recorded
            // material serves (or `Missing` where nothing was).
            if let Some(answer) = self.loads.borrow_mut().pop_front() {
                return match answer {
                    Some(canned) => {
                        *self.stored.borrow_mut() = Some(canned.clone());
                        self.materialize(&canned)
                    }
                    None => Err(CredentialError::Missing {
                        subject: self.subject(),
                        detail: format!("no OS credential-store item for account {}", self.account),
                    }),
                };
            }
            match self.stored.borrow().clone() {
                Some(canned) => self.materialize(&canned),
                None => Err(CredentialError::Missing {
                    subject: self.subject(),
                    detail: format!("no OS credential-store item for account {}", self.account),
                }),
            }
        }

        fn store(
            &self,
            _reference: &CredentialRef,
            material: &LoadedCredential,
        ) -> Result<(), CredentialError> {
            if let Some(detail) = &self.store_refusal {
                return Err(CredentialError::Invalid {
                    subject: self.subject(),
                    detail: detail.clone(),
                });
            }
            *self.stored.borrow_mut() = Some(Self::canned(material));
            Ok(())
        }

        fn replace(
            &self,
            reference: &CredentialRef,
            material: &LoadedCredential,
        ) -> Result<(), CredentialError> {
            self.store(reference, material)
        }

        fn delete(&self, _reference: &CredentialRef) -> Result<(), CredentialError> {
            *self.stored.borrow_mut() = None;
            self.loads.borrow_mut().clear();
            Ok(())
        }
    }

    /// Fixture: a real drive (keystore proofs open it), file
    /// credentials on disk, and a file-backed configuration — the
    /// pre-migration state of a desktop-managed drive.
    struct MigrateFixture {
        _temp: TempDir,
        drive: PathBuf,
        identity_file: PathBuf,
        passphrase_file: PathBuf,
        config_path: PathBuf,
        account: String,
        identity: DeviceIdentitySecret,
    }

    impl MigrateFixture {
        fn new() -> Self {
            let temp = TempDir::new();
            let drive = temp.0.join("drive");
            let identity_file = temp.0.join("identity");
            let passphrase_file = temp.0.join("passphrase");
            let identity = DeviceIdentitySecret::from_bytes([0x22; 32]).unwrap();
            // Raw identity plus a newline-terminated passphrase: the
            // exact shapes the production reader accepts.
            std::fs::write(&identity_file, [0x22; 32]).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&identity_file, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
            }
            std::fs::write(&passphrase_file, b"test-pass\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&passphrase_file, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
            }
            Engine::create(drive.clone(), "test-pass", identity.clone())
                .expect("fixture drive creates");
            let config_path = drive.join("default.toml");
            let mut config = std::fs::File::create(&config_path).unwrap();
            writeln!(
                config,
                "[drive]\nrelays = [\"wss://one.example\"]\nidentity_file = \"{}\"\npassphrase_file = \"{}\"",
                identity_file.display(),
                passphrase_file.display()
            )
            .unwrap();
            MigrateFixture {
                _temp: temp,
                drive,
                identity_file,
                passphrase_file,
                config_path,
                account: "wyrd-drive-test".to_string(),
                identity,
            }
        }

        fn request(&self) -> MigrateRequest {
            MigrateRequest {
                drive_dir: self.drive.clone(),
                identity_file: self.identity_file.clone(),
                passphrase_file: Some(self.passphrase_file.clone()),
                account: self.account.clone(),
                config_path: self.config_path.clone(),
            }
        }
    }

    /// The six steps run end to end: the store item holds the
    /// migrated material, the configuration names it, the keystore
    /// opened under it, and the old files are retired — with the
    /// Drive Root Key untouched (the drive still opens under the
    /// migrated credential afterwards).
    #[test]
    fn six_gated_steps_migrate_then_retire() {
        let fixture = MigrateFixture::new();
        let target = MockBackend::new();
        let outcome = migrate_credential(&FileBackend, &target, &fixture.request())
            .expect("migration runs the six steps");
        assert_eq!(
            outcome,
            MigrateOutcome::Migrated {
                account: fixture.account.clone()
            }
        );
        // The store item holds the migrated material, verbatim.
        let stored = target
            .load(&CredentialRef::OsStore {
                account: fixture.account.clone(),
            })
            .expect("store item loads");
        assert_eq!(stored.identity.device_id(), fixture.identity.device_id());
        assert_eq!(
            stored.passphrase.expect("named item loads").as_str(),
            "test-pass"
        );
        // The configuration names the store backend and nothing else.
        let reread = load_drive_config(&fixture.drive, Some(&fixture.config_path))
            .expect("config reads")
            .expect("config present");
        assert_eq!(reread.credential_backend.as_deref(), Some("os-store"));
        assert!(reread.identity_file.is_none());
        // The old files are retired only after every proof above.
        assert!(!fixture.identity_file.exists());
        assert!(!fixture.passphrase_file.exists());
        // The keystore still opens under the migrated credential:
        // the Root Key and the wrap never changed, only where the
        // passphrase is kept.
        Engine::open_keystore(fixture.drive.clone(), "test-pass", stored.identity)
            .expect("the existing keystore opens under the migrated credential");
    }

    /// A re-run after completion converges: no files to read, so the
    /// store path re-verifies (item loads, keystore opens) and
    /// reports done without writing or deleting anything.
    #[test]
    fn rerun_after_completion_reverifies_and_reports_done() {
        let fixture = MigrateFixture::new();
        let target = MockBackend::new();
        migrate_credential(&FileBackend, &target, &fixture.request()).expect("first run migrates");
        let outcome = migrate_credential(&FileBackend, &target, &fixture.request())
            .expect("re-run converges");
        assert_eq!(
            outcome,
            MigrateOutcome::AlreadyMigrated {
                account: fixture.account.clone()
            }
        );
    }

    /// An interruption between the configuration rewrite and the
    /// retire (files restored by hand, store item present)
    /// converges on re-run: the identical store item continues, the
    /// rewrite is idempotent, and the files retire this time.
    #[test]
    fn rerun_after_interruption_converges_where_material_matches() {
        let fixture = MigrateFixture::new();
        let target = MockBackend::new();
        migrate_credential(&FileBackend, &target, &fixture.request()).expect("first run migrates");
        // Simulate the interruption: the files are back (restored
        // from backup), the store item and configuration stand.
        crate::tests_harness::write_secret(&fixture.identity_file, [0x22; 32]);
        crate::tests_harness::write_secret(&fixture.passphrase_file, b"test-pass\n");
        let outcome = migrate_credential(&FileBackend, &target, &fixture.request())
            .expect("re-run converges");
        assert_eq!(
            outcome,
            MigrateOutcome::Migrated {
                account: fixture.account.clone()
            }
        );
        assert!(!fixture.identity_file.exists());
        assert!(!fixture.passphrase_file.exists());
    }

    /// A differing store item never overwrites: migration refuses
    /// with the files and the configuration untouched — that is
    /// `replace`, an explicit operator act.
    #[test]
    fn a_differing_store_item_refuses_without_touching_files() {
        let fixture = MigrateFixture::new();
        let target = MockBackend::new();
        let other = LoadedCredential {
            identity: DeviceIdentitySecret::from_bytes([0x33; 32]).unwrap(),
            passphrase: Some(Zeroizing::new("other-pass".to_string())),
        };
        target
            .store(
                &CredentialRef::OsStore {
                    account: fixture.account.clone(),
                },
                &other,
            )
            .expect("a foreign item pre-exists");
        let error = migrate_credential(&FileBackend, &target, &fixture.request())
            .expect_err("a differing item refuses");
        assert!(
            error.to_string().contains("never overwrites"),
            "names the rule: {error}"
        );
        assert!(fixture.identity_file.exists());
        assert!(fixture.passphrase_file.exists());
        let reread = load_drive_config(&fixture.drive, Some(&fixture.config_path))
            .expect("config reads")
            .expect("config present");
        assert!(
            reread.identity_file.is_some(),
            "the configuration is untouched"
        );
    }

    /// A step-6 interruption names its survivor: the identity
    /// deletes first, so a passphrase-only leftover reports the
    /// passphrase — never the missing identity — and the store path
    /// stands untouched.
    #[test]
    fn a_partial_retire_names_the_leftover_not_the_missing_file() {
        let fixture = MigrateFixture::new();
        let target = MockBackend::new();
        migrate_credential(&FileBackend, &target, &fixture.request()).expect("first run migrates");
        // Simulate the interruption: only the passphrase survived
        // (identity retired, passphrase delete failed).
        crate::tests_harness::write_secret(&fixture.passphrase_file, b"test-pass\n");
        assert!(!fixture.identity_file.exists());
        let error = migrate_credential(&FileBackend, &target, &fixture.request())
            .expect_err("a half-retired pair must refuse");
        let message = error.to_string();
        assert!(
            message.contains("partially present"),
            "names the state: {message}"
        );
        assert!(
            message.contains(&fixture.passphrase_file.display().to_string()),
            "names the leftover passphrase: {message}"
        );
        // The store path stands: the migrated item still loads.
        let stored = target
            .load(&CredentialRef::OsStore {
                account: fixture.account.clone(),
            })
            .expect("the store item stands");
        assert_eq!(stored.identity.device_id(), fixture.identity.device_id());
    }

    /// Step 2 re-reports a facade refusal instead of mislabelling
    /// it a missing item: a target that refuses the write
    /// (`Invalid`, like a size or platform-name rejection) and then
    /// answers `Missing` surfaces the refusal detail — never "no
    /// item". Both paths stay intact and the configuration is
    /// untouched.
    #[test]
    fn a_facade_refusal_is_rereported_not_a_missing_item() {
        let fixture = MigrateFixture::new();
        let refusal =
            "OS credential store refused account wyrd-drive-test: passphrase exceeds the 8 limit";
        let target = ScriptedTarget::refusing(&fixture.account, refusal);
        let error = migrate_credential(&FileBackend, &target, &fixture.request())
            .expect_err("a refused write must fail");
        let message = error.to_string();
        assert!(
            message.contains("refused account"),
            "the refusal survives: {message}"
        );
        assert!(
            !message.contains("no OS credential-store item"),
            "never mislabelled Missing: {message}"
        );
        assert!(
            fixture.identity_file.exists() && fixture.passphrase_file.exists(),
            "both files stand"
        );
        let reread = load_drive_config(&fixture.drive, Some(&fixture.config_path))
            .expect("config reads")
            .expect("config present");
        assert!(
            reread.identity_file.is_some(),
            "the configuration is untouched"
        );
    }

    /// The race arm converges or refuses by comparison: an
    /// `Invalid` write with identical material on re-probe runs the
    /// remaining steps; differing material refuses without touching
    /// files or configuration.
    #[test]
    fn a_raced_provision_converges_or_refuses_by_comparison() {
        // Identical material on re-probe: the migration continues
        // end to end and retires the files.
        let fixture = MigrateFixture::new();
        let identical = LoadedCredential {
            identity: fixture.identity.clone(),
            passphrase: Some(Zeroizing::new("test-pass".to_string())),
        };
        let target = ScriptedTarget::replay(
            &fixture.account,
            vec![None, Some(ScriptedTarget::canned(&identical))],
            "mock item already exists (race)",
        );
        let outcome = migrate_credential(&FileBackend, &target, &fixture.request())
            .expect("an identical race converges");
        assert_eq!(
            outcome,
            MigrateOutcome::Migrated {
                account: fixture.account.clone()
            }
        );
        assert!(!fixture.identity_file.exists());
        assert!(!fixture.passphrase_file.exists());
        // Differing material on re-probe: refuse, files and
        // configuration untouched.
        let fixture = MigrateFixture::new();
        let other = LoadedCredential {
            identity: DeviceIdentitySecret::from_bytes([0x33; 32]).unwrap(),
            passphrase: Some(Zeroizing::new("other-pass".to_string())),
        };
        let target = ScriptedTarget::replay(
            &fixture.account,
            vec![None, Some(ScriptedTarget::canned(&other))],
            "mock item already exists (race)",
        );
        let error = migrate_credential(&FileBackend, &target, &fixture.request())
            .expect_err("a differing race refuses");
        assert!(
            error.to_string().contains("never overwrites"),
            "names the rule: {error}"
        );
        assert!(fixture.identity_file.exists());
        assert!(fixture.passphrase_file.exists());
        let reread = load_drive_config(&fixture.drive, Some(&fixture.config_path))
            .expect("config reads")
            .expect("config present");
        assert!(
            reread.identity_file.is_some(),
            "the configuration is untouched"
        );
    }

    /// A keystore that will not open under the migrated credential
    /// stops the migration with both paths intact: the store item
    /// stands (steps 1-4 proved), the files stand, and nothing is
    /// retired on an unproven path.
    #[test]
    fn a_failed_keystore_proof_blocks_the_retire() {
        let fixture = MigrateFixture::new();
        // Re-key the passphrase file so the migrated material cannot
        // open the fixture's keystore (created under "test-pass").
        crate::tests_harness::write_secret(&fixture.passphrase_file, b"wrong-pass\n");
        let target = MockBackend::new();
        let error = migrate_credential(&FileBackend, &target, &fixture.request())
            .expect_err("an unopenable keystore refuses");
        assert!(fixture.identity_file.exists());
        assert!(fixture.passphrase_file.exists());
        // Steps 1-4 proved before the failure: the store item holds
        // the (wrong) material and the configuration names it — the
        // operator fixes the files and re-runs, nothing was lost.
        let stored = target
            .load(&CredentialRef::OsStore {
                account: fixture.account.clone(),
            })
            .expect("the store item stands");
        assert_eq!(
            stored.passphrase.expect("named item loads").as_str(),
            "wrong-pass"
        );
        let _ = error;
    }
}
