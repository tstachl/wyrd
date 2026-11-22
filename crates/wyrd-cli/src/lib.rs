#![warn(missing_docs)]
//! `wyrd-cli` library: argument parsing and process orchestration over
//! the daemon's public surface. All drive logic lives in `wyrd-core`
//! and `wyrd-daemon`; this crate only composes them. The `wyrd`
//! binary (`src/main.rs`) is a thin shim that forwards argv to
//! [`command`] and folds the error into an exit status.
#[cfg(test)]
use std::env;
use std::path::PathBuf;

use clap::Parser;

use crate::args::{Cli, Command, MaterializationAction, OwnerAction, RoleAction};
use crate::drive_config::resolve_relays;
use crate::migrate::migrate_drive_credentials;
use wyrd_daemon::process::{install_shutdown_handler, SHUTDOWN};
// Re-exported for the `tests_*` modules, which resolve argument types
// and credential readers through `super::*` and must stay untouched
// by the extraction phases. Test-only: the production library never
// names these through its root.
#[cfg(test)]
pub(crate) use crate::args::Credentials;
use crate::commands::bootstrap::bootstrap;
use crate::commands::device::device;
#[cfg(test)]
pub(crate) use crate::commands::device::{
    device_id_report, device_keys_report, join_confirmation_report, parse_device_id,
    parse_encryption_key,
};
#[cfg(test)]
pub(crate) use crate::commands::drive::delete_drive;
use crate::commands::drive::drive;
use crate::commands::export::export;
#[cfg(test)]
pub(crate) use crate::commands::materialization::{
    evict_render, pin_render, policy_budgets_render, policy_status_render, role_section_render,
    role_status_render, RetentionAccounting,
};
use crate::commands::materialization::{materialization, role_set};
use crate::commands::member::member;
#[cfg(test)]
pub(crate) use crate::commands::member::{
    leave_plan, member_list_report, member_log_report, member_status_report, render_invited,
    render_pending_lines, render_resumed, LeavePlan,
};
#[cfg(feature = "mount")]
use crate::commands::mount::mount;
#[cfg(test)]
pub(crate) use crate::commands::owner::owner_status_report;
use crate::commands::owner::{owner, owner_status};
use crate::commands::snapshot::snapshot;
#[cfg(test)]
pub(crate) use crate::commands::snapshot::{
    frozen_merge_hint, map_merge_error, merge_dry_run_verdict, recovery_dry_run_verdict,
    recovery_plan_report, snapshot_diverged_report, snapshot_heads_report, snapshot_plan_report,
};
use crate::commands::sync::sync;
#[cfg(test)]
pub(crate) use crate::commands::sync::{
    drive_quiet, mailbox_line, run_outcome_error, sync_now_render, sync_status_render, RunOutcome,
    SyncRunReport, MAX_SYNC_NOW_PASSES,
};
use crate::credentials::load_credentials;
#[cfg(test)]
pub(crate) use crate::credentials::{read_credentials, read_identity, read_secret_file};
#[cfg(test)]
pub(crate) use crate::logging::init_mount_diagnostics;
#[cfg(test)]
pub(crate) use crate::teardown::{combine_status, TeardownStatus};
#[cfg(feature = "mount")]
use fuser::{Config, MountOption};
#[cfg(test)]
pub(crate) use std::fs;
#[cfg(test)]
pub(crate) use std::path::Path;
#[cfg(test)]
pub(crate) use std::sync::atomic::Ordering;
#[cfg(test)]
pub(crate) use std::time::Duration;
#[cfg(test)]
pub(crate) use wyrd_core::mailbox::MailboxHealth;
#[cfg(test)]
pub(crate) use wyrd_core::mutation::WriteStats;

#[cfg(test)]
pub(crate) use wyrd_core::policy::{LocalPresence, ResidencyCensus, RetentionPolicy};
#[cfg(test)]
pub(crate) use wyrd_core::status::observe;
#[cfg(test)]
pub(crate) use wyrd_daemon::host::RuntimeMaterialization;
#[cfg(test)]
pub(crate) use wyrd_daemon::DriveView;
use wyrd_daemon::LiveError;
#[cfg(test)]
pub(crate) use wyrd_daemon::SyncReport;
#[cfg(test)]
pub(crate) use wyrd_daemon::{LiveConfig, WyrdNode};
#[cfg(test)]
pub(crate) use wyrd_format::{DeviceId, FsObjectStore};
#[cfg(test)]
pub(crate) use wyrd_sync::control::SealedBootstrap;
#[cfg(test)]
pub(crate) use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::Engine;
#[cfg(test)]
pub(crate) use wyrd_sync::runtime::EngineError;
#[cfg(test)]
pub(crate) use wyrd_sync::transport::mailbox::Mailbox;

/// The CLI's error vocabulary: usage errors, IO failures against a
/// named path, and the daemon/engine failures the dispatcher maps
/// onto exit statuses. Public because [`command`] returns it to the
/// binary entry point, which folds it into an exit status.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Operator usage error: bad flags, paths, or preconditions.
    #[error("{0}")]
    Usage(String),
    /// Filesystem IO failure against a named path.
    #[error("{path}: {source}")]
    Io {
        /// The path the operation targeted.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// A credential file failed its hardening checks.
    #[error("credential file {path}: {reason}")]
    Credential {
        /// The credential file at fault.
        path: PathBuf,
        /// Why it was refused.
        reason: &'static str,
    },
    /// An identity file is neither 32 raw bytes nor 64 hex characters.
    #[error("identity file must contain exactly 32 raw bytes or 64 hex characters")]
    IdentityFormat,
    /// A sparse settings edit lost a commit race: the file changed
    /// between the transaction's read and commit. Distinct from
    /// every other failure — the edit is valid, the file moved, and
    /// the operator re-reads and retries. Exits 3 (see
    /// [`EXIT_SETTINGS_CONFLICT`]), never 2: the desktop renders
    /// the conflict state from the code alone, without parsing text.
    #[error("settings changed under the transaction: {reason}")]
    SettingsConflict {
        /// What moved between read and commit.
        reason: String,
    },
    /// A sparse settings edit committed but the verification
    /// re-read failed: the bytes landed, only the final read-back
    /// did not. Never reported as a refusal — the refusal taxonomy
    /// must not claim otherwise (OD §6 never conflates outcomes).
    /// Exits 4 (see [`EXIT_SETTINGS_UNVERIFIED`]): the desktop shows
    /// the fresh re-read beside the unverified outcome.
    #[error("settings committed but the verification read failed: {reason}")]
    SettingsUnverified {
        /// Why the verification re-read failed.
        reason: String,
    },
    /// An invitation file does not decode as a sealed bootstrap.
    #[error("invitation file does not decode as a sealed bootstrap")]
    InvitationFormat,
    /// An invitation file exceeds the 1 MiB size limit.
    #[error("invitation file exceeds the 1 MiB size limit")]
    InvitationTooLarge,
    /// The identity secret is cryptographically invalid.
    #[error("identity secret is invalid: {0}")]
    Identity(#[from] wyrd_sync::keys::CryptoError),
    /// The engine failed.
    #[error("engine failed: {0}")]
    Engine(#[from] wyrd_sync::runtime::EngineError),
    /// A second owner against a drive another process holds: the
    /// store lock is kernel-held, so this is definite contention,
    /// never a stale record — and it names the drive, because the
    /// wrapped open error does not. One owner or mount per drive.
    #[error("another wyrd process holds the drive at {drive}: only one owner or mount per drive")]
    OwnerLocked {
        /// The drive directory the other process holds.
        drive: PathBuf,
    },
    /// Daemon construction failed.
    #[error("daemon construction failed: {0}")]
    WyrdNode(#[from] wyrd_daemon::NodeError),
    /// The object store failed.
    #[error("object store failed: {0}")]
    Store(String),
    /// A retention quota is misconfigured.
    #[error("retention quota misconfigured: {0}")]
    Quota(#[from] wyrd_core::live::QuotaCheckError),
    /// The mailbox failed.
    #[error("mailbox failed: {0}")]
    Mailbox(#[from] wyrd_sync::transport::mailbox::MailboxError),
    /// Live sync failed.
    #[error("live sync failed: {0}")]
    Live(#[from] LiveError),
    /// The serving endpoint failed.
    #[error("serving endpoint failed: {0}")]
    Serving(std::io::Error),
    /// The bulk source failed.
    #[error("bulk source failed: {0}")]
    Bulk(std::io::Error),
    /// An export failed.
    #[error("export failed: {0}")]
    Export(#[from] wyrd_core::export::ExportError),
    /// A materialization policy operation failed.
    #[error("policy failed: {0}")]
    Policy(#[from] wyrd_core::policy::PolicyError),
    /// A bounded headless run stopped at the pass limit with work
    /// still owed: rerun to continue converging. Reported, never
    /// silent, so automation cannot mistake a capped run for a
    /// converged device. Exits non-zero like any other failure.
    #[error("Sync did not finish: the maximum number of passes was reached with {pending} pieces of work still pending. Run `wyrd sync now` again to continue.")]
    Incomplete {
        /// Obligations still owed when the cap hit.
        pending: usize,
    },
    /// A run that went quiet with received reconciliation statements
    /// still unanswered: a peer asked this device to prove its state
    /// and the run could not answer — an epoch it never learned, or
    /// a statement that landed after the last answer pass. The gap is
    /// control-plane, so the run fails even though the outbox is
    /// quiet: automation keying on exit status must see it. Exits
    /// non-zero like any other failure.
    #[error(
        "sync incomplete: {unanswered} reconciliation statements awaiting answer, {stalled} stalled"
    )]
    ReconciliationOutstanding {
        /// Received statements still awaiting an answer.
        unanswered: usize,
        /// Statements stalled past their deadline.
        stalled: usize,
    },
    /// A run that stopped on a quiet local state while the mailbox
    /// could not prove intake worked: no relay connected, a relay
    /// closed our subscription mid-run, or a blind stretch healed
    /// mid-run (a non-zero recovery-attempt total proves a
    /// supervisor episode ran — post-attachment only, so a slow cold
    /// start never counts). Quiet observed through a blind intake is
    /// unverified, never converged — automation must not mistake it
    /// for success. Exits non-zero like any other failure; the stdout
    /// lines still carry the pass and pending counts for forensics.
    /// The counts use wyrd-core's unit (loop iterations, not
    /// episodes): one slow episode converges over several attempts.
    #[error(
        "sync unverified: mailbox degraded ({connected} of {total} relays connected, {closed} subscriptions closed by relay, {attempts} recovery attempts during the run)"
    )]
    Unverified {
        /// Relays connected when the run went quiet.
        connected: usize,
        /// Relays configured for the run.
        total: usize,
        /// Subscriptions the relay closed mid-run.
        closed: u64,
        /// Recovery attempts observed during the run.
        attempts: u64,
    },
    /// No mailbox snapshot was ever recorded on the report. Unreachable
    /// in production (`sync_now` always stores the observed snapshot),
    /// and it exists so that any future path that renders or maps a
    /// report without observing the mailbox fails closed instead of
    /// inheriting a fabricated posture.
    #[error("sync unverified: mailbox posture was never recorded")]
    Unobserved,
    /// `__update_verify` refused the drive: the U4 report on stdout
    /// names the invariant; this carries the verdict to the exit
    /// path. Operational failures (held lock, wrong passphrase)
    /// never reach it — they print no verdict at all, so a verdict
    /// without completed checks cannot exist. Exits non-zero like
    /// any other failure.
    #[error("update verification failed: {0}")]
    Verification(String),
    /// macOS FUSE preflight failed (macOS mount builds and tests only).
    #[error("macOS FUSE preflight failed: {0}")]
    #[cfg(any(test, all(target_os = "macos", feature = "mount")))]
    Preflight(String),
    /// FUSE mount failed.
    #[error("FUSE mount failed: {0}")]
    Mount(#[from] std::io::Error),
}

/// The drive-already-owned exit status (OD-34-C): `EX_CONFIG` from
/// sysexits, a configuration error the supervisor must not retry.
/// The supervisor wrapper maps exactly this code to its terminal
/// status; every other failure keeps the historical exit 2, so a
/// wrapper that treats 78 as "held" never misfires on a runtime
/// error. Recorded in `docs/design/cli.md`'s exit-code list.
pub const EXIT_DRIVE_LOCKED: i32 = 78;
/// A sparse settings edit lost its commit race: the file changed
/// between read and commit. The edit is valid and the file moved,
/// so the operator re-reads and retries — a supervisor must not
/// treat this as terminal (unlike 78), and automation must not
/// collapse it into generic failure (unlike 2). Only `settings
/// apply` exits this way. Recorded in `docs/design/cli.md`'s
/// exit-code list.
pub const EXIT_SETTINGS_CONFLICT: i32 = 3;
/// A sparse settings edit committed but the verification re-read
/// failed: the bytes landed, only the read-back did not. The
/// operator trusts the fresh re-read, not a retry — re-running a
/// landed edit is harmless (idempotent sparse update) but the
/// outcome is already decided. Only `settings apply` exits this
/// way. Recorded in `docs/design/cli.md`'s exit-code list.
pub const EXIT_SETTINGS_UNVERIFIED: i32 = 4;

impl CliError {
    /// Fold the error into a process exit status: the
    /// drive-already-owned code for lock contention (direct or
    /// wrapped in an engine error — the mount contends through the
    /// latter), the settings codes for a lost commit race (3) and a
    /// committed-but-unverified write (4), 2 for everything else.
    pub fn exit_code(&self) -> i32 {
        match self {
            CliError::OwnerLocked { .. } => EXIT_DRIVE_LOCKED,
            CliError::SettingsConflict { .. } => EXIT_SETTINGS_CONFLICT,
            CliError::SettingsUnverified { .. } => EXIT_SETTINGS_UNVERIFIED,
            CliError::Engine(wyrd_sync::runtime::EngineError::Durable(
                wyrd_sync::durable::DurableError::StoreLocked,
            )) => EXIT_DRIVE_LOCKED,
            _ => 2,
        }
    }
}

/// Owner composition failures map to the CLI surface that already
/// names each cause: the daemon's `OwnerError` is the shared
/// vocabulary, and every variant lands on the same `CliError` the
/// hand-rolled composition mapped to before the consolidation.
impl From<wyrd_daemon::OwnerError> for CliError {
    fn from(error: wyrd_daemon::OwnerError) -> Self {
        use wyrd_daemon::OwnerError as E;
        match error {
            E::Store(message) => CliError::Store(message),
            E::Engine(error) => CliError::Engine(error),
            E::Node(error) => CliError::WyrdNode(error),
            E::Retention(error) => CliError::Quota(error),
            E::Settings(reason) => {
                CliError::Usage(format!("application settings refused: {reason}"))
            }
            E::Io(error) => CliError::Serving(error),
        }
    }
}

/// Run one `wyrd` invocation from user arguments (argv\[0\] already
/// stripped): parse, load credentials, and dispatch to the command.
/// The binary entry point calls this and folds the error into an
/// exit status; tests call it the same way.
pub fn command(args: Vec<String>) -> Result<(), CliError> {
    // `args` carries user arguments only (main strips argv[0]); clap's
    // parse_from expects the binary name first. Help/version requests
    // arrive as errors too: print them as asked and exit successfully.
    let cli = match Cli::try_parse_from(std::iter::once("wyrd".to_owned()).chain(args)) {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            let _ = error.print();
            return Ok(());
        }
        Err(error) => return Err(CliError::Usage(error.to_string())),
    };

    // Commands that must not open the keystore resolve here. `None`
    // means the command needs the shared credential prelude below.
    if let Some(result) = dispatch_secretless(&cli) {
        return result;
    }

    // Every credential-bearing command loads through this one match,
    // so a new command cannot skip the shared rules.
    let (identity, passphrase, credential_provenance, config) = match &cli.command {
        #[cfg(feature = "mount")]
        Command::Mount {
            drive_dir,
            credentials,
            ..
        } => load_credentials(drive_dir, credentials, cli.config.as_deref())?,
        Command::Export {
            drive_dir,
            credentials,
            ..
        }
        | Command::Member {
            drive_dir,
            credentials,
            ..
        }
        | Command::Snapshot {
            drive_dir,
            credentials,
            ..
        }
        | Command::Device {
            drive_dir,
            credentials,
            ..
        }
        | Command::Drive {
            drive_dir,
            credentials,
            ..
        }
        | Command::Sync {
            drive_dir,
            credentials,
            ..
        }
        | Command::Owner {
            drive_dir,
            credentials,
            ..
        }
        | Command::UpdateVerify {
            drive_dir,
            credentials,
            ..
        } => load_credentials(drive_dir, credentials, cli.config.as_deref())?,
        Command::Materialization {
            drive_dir, action, ..
        } => match action {
            MaterializationAction::Role {
                action: RoleAction::Status { credentials },
            }
            | MaterializationAction::Status { credentials, .. }
            | MaterializationAction::Budgets { credentials }
            | MaterializationAction::Pin { credentials, .. }
            | MaterializationAction::Unpin { credentials, .. }
            | MaterializationAction::Evict { credentials, .. } => {
                load_credentials(drive_dir, credentials, cli.config.as_deref())?
            }
            MaterializationAction::Role {
                action: RoleAction::Set { .. },
            } => unreachable!("materialization role set resolves before credential loading"),
        },
        // Listed explicitly so adding a command variant is a compile
        // error here, not a runtime panic in the exhaustiveness arm.
        Command::Update { .. }
        | Command::BuildInfo
        | Command::Identity { .. }
        | Command::Settings { .. }
        | Command::CredentialReference { .. } => {
            unreachable!("secretless command reached the credential prelude")
        }
    };

    match cli.command {
        Command::UpdateVerify { drive_dir, .. } => {
            update::run_update_verify(&drive_dir, identity, &passphrase)
        }
        // Dead by construction: these resolved in `dispatch_secretless`
        // before any credential was loaded. Keep them ahead of the
        // general drive arm.
        Command::Drive {
            action: crate::args::DriveAction::Unlock { .. },
            ..
        } => unreachable!("drive unlock runs before credential loading"),
        Command::Drive {
            action: crate::args::DriveAction::MigrateCredentials { .. },
            ..
        } => unreachable!("drive migrate-credentials runs before credential loading"),
        Command::Drive {
            drive_dir,
            action,
            credentials,
            ..
        } => match action {
            crate::args::DriveAction::Init => {
                Engine::create(drive_dir, &passphrase, identity)?;
                Ok(())
            }
            crate::args::DriveAction::Bootstrap {
                name,
                mode,
                mountpoint,
                relays,
                invitation_file,
                expected_owner,
                expected_drive,
                expected_genesis,
                registry,
                owner_bin,
                supervise_bin,
                no_service,
            } => bootstrap(
                crate::commands::bootstrap::BootstrapArgs {
                    name,
                    drive_dir,
                    mode,
                    mountpoint,
                    relays,
                    credentials,
                    invitation_file,
                    expected_owner,
                    expected_drive,
                    expected_genesis,
                    registry,
                    owner_bin,
                    supervise_bin,
                    no_service,
                },
                identity,
                &passphrase,
                config.as_ref(),
            ),
            action => drive(
                drive_dir,
                action,
                &passphrase,
                identity,
                &credentials,
                config.as_ref(),
            ),
        },
        #[cfg(feature = "mount")]
        Command::Mount {
            drive_dir,
            mountpoint,
            relays,
            verbose,
            interval,
            retained_quota,
            ..
        } => {
            // Ceiling as well as floor: past a day the deadline stops
            // being pacing and the value is a typo, so refuse it here
            // instead of overflowing `Instant + Duration` in the loop.
            if !(1..=86_400).contains(&interval) {
                return Err(CliError::Usage(format!(
                    "--interval must be 1-86400 seconds (a sanity ceiling: past a day the deadline stops being pacing): got {interval}"
                )));
            }
            mount(
                drive_dir,
                mountpoint,
                resolve_relays(relays.relay, config.as_ref()).relays,
                verbose,
                interval,
                retained_quota,
                &passphrase,
                identity,
            )
        }
        Command::Export {
            drive_dir, out_dir, ..
        } => export(drive_dir, out_dir, &passphrase, identity),
        Command::Member {
            drive_dir, action, ..
        } => member(drive_dir, action, &passphrase, identity),
        Command::Snapshot {
            drive_dir, action, ..
        } => snapshot(drive_dir, action, &passphrase, identity),
        Command::Device {
            drive_dir, action, ..
        } => device(drive_dir, action, &passphrase, identity),

        Command::Sync {
            drive_dir,
            relays,
            action,
            ..
        } => {
            let resolution = resolve_relays(relays.relay, config.as_ref());
            sync(
                drive_dir,
                resolution,
                config.as_ref(),
                action,
                &passphrase,
                identity,
            )
        }
        Command::Owner {
            drive_dir,
            relays,
            log_file,
            action,
            ..
        } => match action {
            None => {
                let resolution = resolve_relays(relays.relay, config.as_ref());
                owner(
                    drive_dir,
                    resolution,
                    config.as_ref(),
                    log_file,
                    None,
                    &passphrase,
                    identity,
                )
            }
            Some(OwnerAction::Start { retained_quota }) => {
                let resolution = resolve_relays(relays.relay, config.as_ref());
                owner(
                    drive_dir,
                    resolution,
                    config.as_ref(),
                    log_file,
                    retained_quota,
                    &passphrase,
                    identity,
                )
            }
            // `--log-file` lives on the parent command so clap cannot
            // express this (same shape as `sync now`'s `--offline`
            // guard above): refuse here rather than silently ignore.
            Some(OwnerAction::Status) if log_file.is_some() => Err(CliError::Usage(
                "--log-file is start-only: status writes no diagnostics".into(),
            )),
            Some(OwnerAction::Status) => owner_status(drive_dir, &passphrase, identity),
        },
        Command::Materialization {
            drive_dir, action, ..
        } => {
            // Materialization names no relays of its own: the effective set is
            // whatever the file declares, and the budgets report says
            // where it came from.
            let resolution = resolve_relays(Vec::new(), config.as_ref());
            materialization(
                drive_dir,
                action,
                &resolution,
                config.as_ref(),
                &credential_provenance,
                &passphrase,
                identity,
            )
        }
        // Dead by construction: these resolved in `dispatch_secretless`
        // before any credential was loaded. Listing them keeps the
        // match exhaustive, so a new command variant cannot slip in.
        Command::Update { .. }
        | Command::BuildInfo
        | Command::Identity { .. }
        | Command::Settings { .. }
        | Command::CredentialReference { .. } => {
            unreachable!("secretless command reached the credential dispatch")
        }
    }
}

/// Resolve the commands that must run before the shared credential
/// prelude. `None` means the command needs credentials; `Some` is its
/// outcome. A file-backed `drive bootstrap` falls through after the
/// backend check, while a store-backed one resolves here.
fn dispatch_secretless(cli: &Cli) -> Option<Result<(), CliError>> {
    match &cli.command {
        // Update runs secretless (credential paths forward to the
        // verification child): `update::update_from_cli` returns
        // before credential loading.
        Command::Update {
            drive_dir,
            archive,
            relays,
            restart,
            credentials,
            expect_blake3,
        } => Some(update::update_from_cli(
            drive_dir,
            archive,
            relays,
            *restart,
            credentials,
            cli.config.as_deref(),
            expect_blake3.clone(),
        )),
        Command::BuildInfo => {
            println!("{}", update::render_build_info(&update::own_build_info()));
            Some(Ok(()))
        }
        Command::CredentialReference { drive_dir } => {
            // Read-only observation of the deployment config: no key
            // load, no store lock, and no capability is granted. The
            // unknown case carries its reason so the shell can keep
            // an explicit selector rather than guessing a backend.
            println!(
                "{}",
                crate::drive_config::credential_reference_json(drive_dir, cli.config.as_deref())
            );
            Some(Ok(()))
        }
        Command::Identity { action } => Some(match action {
            crate::args::IdentityAction::Generate {
                identity_file,
                passphrase_file,
            } => {
                crate::commands::identity::generate(identity_file.clone(), passphrase_file.clone())
            }
            crate::args::IdentityAction::Verify {
                identity_file,
                passphrase_file,
            } => crate::commands::identity::verify(identity_file.clone(), passphrase_file.clone()),
            crate::args::IdentityAction::NewPassphrase => {
                crate::commands::identity::new_passphrase()
            }
            crate::args::IdentityAction::Commit {
                identity_file,
                passphrase_file,
            } => crate::commands::identity::commit(identity_file.clone(), passphrase_file.clone()),
        }),
        Command::Drive {
            drive_dir,
            action:
                crate::args::DriveAction::Unlock {
                    relays,
                    retained_quota,
                },
            credentials,
            ..
        } => {
            // Unlock never loads the passphrase from a file: the
            // terminal prompt or the stdin pipe, then the owner in
            // the foreground exactly like `owner start`. Nothing is
            // installed, enabled, or rewritten — the manager learns
            // nothing, so a reboot or an explicit stop locks the
            // drive again.
            use std::io::IsTerminal as _;
            let stdin_is_tty = std::io::stdin().is_terminal();
            let config =
                match crate::drive_config::load_drive_config(drive_dir, cli.config.as_deref()) {
                    Ok(config) => config,
                    Err(error) => return Some(Err(error)),
                };
            let (identity, passphrase, _) = match crate::credentials::read_unlock_credentials(
                credentials,
                config.as_ref(),
                stdin_is_tty,
            ) {
                Ok(values) => values,
                Err(error) => return Some(Err(error)),
            };
            let resolution = resolve_relays(relays.relay.clone(), config.as_ref());
            Some(owner(
                drive_dir.clone(),
                resolution,
                config.as_ref(),
                None,
                *retained_quota,
                &passphrase,
                identity,
            ))
        }
        Command::Drive {
            drive_dir,
            action: crate::args::DriveAction::ServiceStart { registry },
            credentials,
            ..
        } => {
            // Starting needs no keystore and no credentials: resolve
            // before credential loading, so a stopped service starts
            // even where key files are momentarily unavailable. The
            // per-session stdin handoff has no direct-spawn target
            // yet (that slice lands the app-supervised start), so a
            // stdin flag here would parse cleanly and then silently
            // start the file-backed definition — refuse it loudly
            // instead of running a start the flag cannot affect.
            if credentials.passphrase_stdin {
                return Some(Err(CliError::Usage(
                    "--passphrase-stdin has no effect on `drive service-start` yet: the direct-spawn handoff is a follow-up slice; start the file-backed service without it".to_string(),
                )));
            }
            Some(crate::commands::drive::service_start_drive(
                drive_dir.clone(),
                registry.clone(),
            ))
        }
        Command::Drive {
            drive_dir,
            action: crate::args::DriveAction::ServiceEnable { registry },
            credentials,
            ..
        } => {
            // Enabling records a login-start preference and opens
            // nothing: resolve before credential loading, like
            // service-start, and refuse the stdin flag for the same
            // reason — it cannot affect an enablement record.
            if credentials.passphrase_stdin {
                return Some(Err(CliError::Usage(
                    "--passphrase-stdin has no effect on `drive service-enable`: enabling records a login-start preference and opens no keystore".to_string(),
                )));
            }
            Some(crate::commands::drive::service_enable_drive(
                drive_dir.clone(),
                registry.clone(),
            ))
        }
        Command::Drive {
            drive_dir,
            action: crate::args::DriveAction::ServiceDisable { registry },
            credentials,
            ..
        } => {
            // Disabling stops the unit now and records no login
            // start; it opens nothing, so the same stdin refusal
            // applies.
            if credentials.passphrase_stdin {
                return Some(Err(CliError::Usage(
                    "--passphrase-stdin has no effect on `drive service-disable`: disabling stops the service and records a preference, and opens no keystore".to_string(),
                )));
            }
            Some(crate::commands::drive::service_disable_drive(
                drive_dir.clone(),
                registry.clone(),
            ))
        }
        Command::Drive {
            drive_dir,
            action: crate::args::DriveAction::MigrateCredentials { account },
            credentials,
            ..
        } => {
            // Migration resolves its own credentials: the files it
            // migrates from, never the pipe — a piped passphrase
            // would leave no file pair to retire and no proof the
            // keystore opens under persisted material.
            if credentials.passphrase_stdin {
                return Some(Err(CliError::Usage(
                    "migration needs credential files, not --passphrase-stdin: pass --identity-file/--passphrase-file or set them in the drive configuration".to_string(),
                )));
            }
            Some(migrate_drive_credentials(
                drive_dir.clone(),
                account.clone(),
                credentials,
                cli.config.clone(),
            ))
        }
        Command::Drive {
            drive_dir,
            action:
                crate::args::DriveAction::Bootstrap {
                    name,
                    mode,
                    mountpoint,
                    relays,
                    invitation_file,
                    expected_owner,
                    expected_drive,
                    expected_genesis,
                    registry,
                    owner_bin,
                    supervise_bin,
                    no_service,
                },
            credentials,
            ..
        } => {
            // Store-backed bootstrap provisions its input before
            // anything loads from the store: the input material
            // comes from files or the pipe, the drive it
            // provisions is store-backed, and the generic loader
            // (which would read the not-yet-provisioned store item)
            // is the wrong path. File-backed bootstrap falls through
            // to the generic loader unchanged.
            let peek =
                match crate::drive_config::load_drive_config(drive_dir, cli.config.as_deref()) {
                    Ok(peek) => peek,
                    Err(error) => return Some(Err(error)),
                };
            let backend = match crate::credentials::effective_backend(credentials, peek.as_ref()) {
                Ok(backend) => backend,
                Err(error) => return Some(Err(error)),
            };
            if matches!(
                backend,
                crate::credentials::EffectiveBackend::OsStore { .. }
            ) {
                return Some(crate::commands::bootstrap::bootstrap_store(
                    crate::commands::bootstrap::BootstrapArgs {
                        name: name.clone(),
                        drive_dir: drive_dir.clone(),
                        mode: mode.clone(),
                        mountpoint: mountpoint.clone(),
                        relays: relays.clone(),
                        credentials: credentials.clone(),
                        invitation_file: invitation_file.clone(),
                        expected_owner: expected_owner.clone(),
                        expected_drive: expected_drive.clone(),
                        expected_genesis: expected_genesis.clone(),
                        registry: registry.clone(),
                        owner_bin: owner_bin.clone(),
                        supervise_bin: supervise_bin.clone(),
                        no_service: *no_service,
                    },
                    cli.config.clone(),
                ));
            }
            None
        }
        Command::Materialization {
            drive_dir,
            action:
                MaterializationAction::Role {
                    action: RoleAction::Set { role, yes },
                },
            ..
        } => {
            // Setting touches one local file and no key: return
            // before credential loading, so a role is settable on
            // a machine whose keystore is unavailable (OD-24-R).
            // Every other action loads in the common prelude — the
            // credential flags ride each action, so `policy role
            // set` alone takes neither.
            Some(role_set(drive_dir.clone(), role.clone(), *yes))
        }
        Command::Settings { drive_dir, action } => {
            // Settings touch one per-user file and no key: return
            // before credential loading, so a save works where the
            // keystore is unavailable — including a stopped drive,
            // whose owner cannot serve and whose keys may be
            // momentarily unreachable. Every other command loads
            // in the common prelude like every other family.
            Some(crate::commands::settings::settings(
                drive_dir.clone(),
                action.clone(),
            ))
        }
        _ => None,
    }
}

/// macOS mount-failure checklist, printed next to the raw error.
/// macFUSE's libfuse2 mount can return -1 without setting errno, so
/// the errno in the error line may be stale (observed: EOPNOTSUPP
/// left over from an iroh socket op, ENOTTY from elsewhere).
/// The checklist names the fix; the README carries the full procedure.
/// macOS- and mount-specific: without the mount there is nothing to
/// hint at.
#[cfg(all(target_os = "macos", feature = "mount"))]
pub(crate) const MACOS_MOUNT_HINT: &str = "macOS hint: the errno above may be stale; check \
    the kext (ls /dev/macfuse0; if missing: load macFUSE, approve it in \
    Privacy & Security, reboot), the mount daemon (pgrep -af \
    io.macfuse.app.launchservice.daemon; if missing: sudo launchctl kickstart \
    -k system/io.macfuse.app.launchservice.daemon), and the README macOS section.";

/// Fail fast when the macOS FUSE runtime cannot mount: macFUSE
/// missing entirely, or installed with its kext unloaded. Both states
/// are plain path probes so the logic stays unit-testable; only the
/// wiring (real /Library and /dev roots) is macOS-gated at the call
/// site. The mountpoint itself must already be a directory — fuser
/// would reject anything else with a bare ENOENT.
///
/// Best-effort only: a passing preflight does not guarantee the mount
/// will succeed (stale device nodes, alternate install layouts), and
/// the real mount error remains authoritative.
/// The mount serves read-write: the session backend already carries
/// the live daemon's mutation channel, so the kernel must not gate
/// writes behind a read-only flag. The FSName keeps the volume
/// identifiable in mount tables. Behind the `mount` feature with the
/// rest of the mount path: without system libfuse there is nothing
/// to configure a session with.
#[cfg(feature = "mount")]
pub(crate) fn session_config() -> Config {
    let mut config = Config::default();
    config.mount_options = vec![MountOption::FSName("wyrd".into())];
    config
}

mod args;
mod commands;
mod composer;
mod credentials;
mod drive_config;
mod logging;
mod migrate;
mod preflight;
mod teardown;
mod update;

#[cfg(test)]
mod tests_cli;
#[cfg(test)]
mod tests_device;
#[cfg(test)]
mod tests_drive;
#[cfg(test)]
mod tests_export;
#[cfg(test)]
mod tests_harness;
#[cfg(test)]
mod tests_member;
#[cfg(test)]
mod tests_mount;
#[cfg(test)]
mod tests_policy;
#[cfg(test)]
mod tests_probes;
#[cfg(test)]
mod tests_snapshot;
#[cfg(test)]
mod tests_sync;
