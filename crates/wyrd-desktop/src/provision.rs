//! Runtime provisioning through the installed CLI: the constrained
//! adapter the multi-drive commitment requires.
//!
//! Authority contract (issue `nevent1qqsx9357fqqfup9g7kurf7gdcgtjf8qaceay3a90wzxjw6sresg69eqpz9mhxue69uhkwunpwdczuap49eehgcl6hz9`,
//! architecture in `docs/design/desktop-shell.md`):
//!
//! - The desktop launches operations but never becomes the source of
//!   truth. It invokes the installed, version-matched CLI through this
//!   adapter with structured arguments (argv arrays, never
//!   shell-constructed), waits for process completion, then re-reads
//!   the registry through the existing observation path and probes the
//!   owner socket independently. Observed state determines the
//!   display; exit codes indicate operation outcomes, never
//!   authoritative lifecycle state.
//! - Secret contents never cross IPC or argv (paths only).
//! - Operations are non-interactive, with explicit exit semantics
//!   (0 success, 2 usage-or-expected-failure, 78 terminal held-drive).
//! - No overlapping lifecycle operations on the same drive: a second
//!   request while one is in flight is refused with an explicit
//!   per-drive busy state, never queued.
//! - No implicit retries of destructive operations: timeout or process
//!   failure triggers observation, never re-execution.
//! - Version mismatch is a hard failure, never silent fallback.
//!
//! Layering: `std::process` only — no new Cargo edge, so the
//! `wyrd-desktop` dependency-DAG policy is untouched. CLI stdout and
//! stderr travel as opaque display detail and are never parsed for
//! lifecycle facts; the only stable signal is the exit code (0
//! success, 2 usage-or-expected-failure, 78 terminal held-drive),
//! paired with independent re-observation.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long one CLI lifecycle operation may run before the adapter
/// gives up waiting. Expiry triggers observation, never re-execution:
/// the CLI may have committed before the timeout, so the outcome is
/// unknown and only the re-read decides what the display says.
pub const PROVISION_TIMEOUT: Duration = Duration::from_secs(120);

/// Exit codes the CLI contract pins (`lib.rs`, `docs/design/cli.md`):
/// 0 success (including `--help`/`--version` and the non-owner leave
/// refusal, which authors nothing), 78 terminal held-drive (another
/// process holds the store — the supervisor must not retry), 2
/// everything else.
const EXIT_HELD_DRIVE: i32 = 78;

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

/// Create (no invitation) or join (sealed invitation file) one drive.
#[derive(Debug, Clone)]
pub struct ProvisionRequest {
    /// Registry entry name, 1–58 chars of `[a-z0-9-]`.
    pub name: String,
    /// Absolute drive directory, exact string (shared drive-identity rule).
    pub drive_dir: PathBuf,
    /// Relays for the managed service. Empty is refused for managed
    /// services before anything is created.
    pub relays: Vec<String>,
    /// Where the CLI reads the drive credential: files, or one OS
    /// credential-store item (fresh store-backed bootstraps carry
    /// the one-time seed files beside the store reference).
    pub credentials: CredentialSource,
    /// `None` = create, `Some(path)` = join from that invitation file.
    pub invitation_file: Option<PathBuf>,
    /// Explicit registry path (tests); `None` = platform default.
    pub registry_path: Option<PathBuf>,
    /// Explicit owner/supervise binaries (tests / non-standard layout);
    /// `None` = sibling-of-CLI default the CLI applies.
    pub owner_bin: Option<PathBuf>,
    /// Explicit supervise binary (tests / non-standard layout); `None` = CLI default.
    pub supervise_bin: Option<PathBuf>,
    /// `false` (default) installs, enables, and starts the per-drive
    /// service; `true` registers only.
    pub no_service: bool,
    /// Service mode display string: `vault` serves headless, `mount`
    /// serves the FUSE projection at `mountpoint`. Any other value
    /// refuses before any process spawns — the adapter never
    /// provisions a service the registry reader would skip.
    pub mode: String,
    /// FUSE mountpoint. Required in `mount` mode, refused in `vault`
    /// mode.
    pub mountpoint: Option<PathBuf>,
}

/// Leave the drive: end this device's participation where the trust
/// contract admits it. A non-owner authors nothing (the CLI reports
/// the refusal at exit 0); a sole owner freezes the drive permanently
/// and needs `acknowledged = true` (`--yes` at the CLI boundary — a
/// frontend omitting the dialog cannot bypass the requirement).
#[derive(Debug, Clone)]
pub struct LeaveRequest {
    /// Absolute drive directory, exact string (shared drive-identity rule).
    pub drive_dir: PathBuf,
    /// Where the CLI reads the drive credential: files, or one OS
    /// credential-store item. Seeds never apply — leave reads the
    /// provisioned credential, and a seed beside it refuses before
    /// any process spawns.
    pub credentials: CredentialSource,
    /// Sole-owner terminal confirmation. The UI explains the
    /// consequences; this flag carries the acknowledgement to the CLI.
    pub acknowledged: bool,
}

/// Reconfigure a registered drive's supervision in place: stop and
/// clear the old service definition, rewrite the registry entry,
/// reinstall and restart when managed, and verify the socket.
/// `relays` replaces the registered set when non-empty and keeps the
/// fresh set when empty (a credential-path-only reconfigure never
/// clears relays by accident); `mode` is `vault` or `mount` (`mount`
/// needs `mountpoint`), and any other value refuses before any
/// process spawns, like creation.
#[derive(Debug, Clone)]
pub struct ReconfigureRequest {
    /// Absolute drive directory, exact string (shared drive-identity rule).
    pub drive_dir: PathBuf,
    /// Replacement relays when non-empty; empty keeps the registered set.
    pub relays: Vec<String>,
    /// Where the CLI reads the drive credential: files, or one OS
    /// credential-store item. Seeds never apply — reconfigure reads
    /// the provisioned credential, and a seed beside it refuses
    /// before any process spawns.
    pub credentials: CredentialSource,
    /// Explicit registry path (tests); `None` = platform default.
    pub registry_path: Option<PathBuf>,
    /// Explicit owner/supervise binaries (tests / non-standard layout);
    /// `None` = sibling-of-CLI default the CLI applies.
    pub owner_bin: Option<PathBuf>,
    /// Explicit supervise binary (tests / non-standard layout); `None` = CLI default.
    pub supervise_bin: Option<PathBuf>,
    /// `false` (default) disables the old service, rewrites the
    /// entry, and reinstalls, enables, and restarts; `true`
    /// rewrites without installing any OS service.
    pub no_service: bool,
    /// Service mode display string: `vault` serves headless, `mount`
    /// serves the FUSE projection at `mountpoint`. Any other value
    /// refuses before any process spawns.
    pub mode: String,
    /// FUSE mountpoint. Required in `mount` mode, refused in `vault`
    /// mode.
    pub mountpoint: Option<PathBuf>,
}

/// Start one registered drive's installed service without
/// rewriting anything and without touching its login-start
/// preference: the recovery for "installed but stopped". No
/// credentials travel — starting proves nothing about membership
/// and the CLI opens no keystore — only the drive directory plus the
/// registry override (tests and non-standard layouts).
#[derive(Debug, Clone)]
pub struct ServiceStartRequest {
    /// Absolute drive directory, exact string (shared drive-identity rule).
    pub drive_dir: PathBuf,
    /// Explicit registry path (tests); `None` = platform default.
    pub registry_path: Option<PathBuf>,
}

/// Enable one registered drive's login start without starting it:
/// record the preference with the service manager, then verify the
/// record. No credentials travel — enabling opens no keystore —
/// only the drive directory plus the registry override.
#[derive(Debug, Clone)]
pub struct ServiceEnableRequest {
    /// Absolute drive directory, exact string (shared drive-identity rule).
    pub drive_dir: PathBuf,
    /// Explicit registry path (tests); `None` = platform default.
    pub registry_path: Option<PathBuf>,
}

/// Disable one registered drive's login start and stop it now,
/// keeping the definition file: record the preference with the
/// service manager, then verify the record and the stopped state.
/// No credentials travel — only the drive directory plus the
/// registry override.
#[derive(Debug, Clone)]
pub struct ServiceDisableRequest {
    /// Absolute drive directory, exact string (shared drive-identity rule).
    pub drive_dir: PathBuf,
    /// Explicit registry path (tests); `None` = platform default.
    pub registry_path: Option<PathBuf>,
}

/// The adapter's verdict on the *operation*. Never lifecycle state:
/// the caller pairs every variant with a registry re-read, a socket
/// probe, and (for the operated drive) a membership observation, and
/// only that observed state decides the display.
///
/// Deliberately coarse, per the settled contract
/// (`docs/design/desktop-shell.md`): only the exit code decides.
/// CLI stdout/stderr travel as opaque display detail and are never
/// parsed for lifecycle facts — not even the fixed success line.
/// Exit 0 reports no error (including the non-owner leave refusal,
/// which authors nothing); exit 2 reports an expected failure; 78
/// is the terminal held-drive.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "result", rename_all = "lowercase")]
pub enum ProvisionResult {
    /// Exit 0: the CLI reported no error. What actually happened is
    /// decided by the paired observation alone — this never claims a
    /// lifecycle state.
    Completed {
        /// Opaque display detail; the paired observation decides what happened.
        detail: String,
    },
    /// Exit 2 or an adapter-side refusal: usage, expected failure,
    /// or a validation refusal that never spawned a process. Opaque
    /// detail; the paired observation says what actually exists.
    ExpectedFailure {
        /// Opaque display detail; the paired observation says what actually exists.
        detail: String,
    },
    /// Exit 78: another process holds the drive. Terminal — the
    /// supervisor must not retry.
    HeldDrive {
        /// Opaque display detail naming the terminal held-drive.
        detail: String,
    },
    /// CLI binary missing, version-mismatched, or unparsable
    /// `__build_info`: hard failure, never silent fallback.
    VersionMismatch {
        /// Why the CLI binary is unusable; never a silent fallback.
        reason: String,
    },
    /// A lifecycle operation is already in flight on this drive.
    Busy {
        /// Drive refused as busy, as an exact directory string.
        drive_dir: String,
    },
    /// Timeout, spawn failure, or signal death: the outcome is
    /// unknown. Triggers observation, never re-execution.
    Unknown {
        /// Why the outcome is unknown; triggers observation, never re-execution.
        detail: String,
    },
}

/// One completed operation: the opaque outcome plus the raw CLI
/// stderr (display detail, never parsed) and the exit code observed.
#[derive(Debug, Clone)]
pub struct ProvisionOutcome {
    /// Operation verdict: exit codes only, never lifecycle state.
    pub result: ProvisionResult,
    /// Raw CLI stderr as opaque display detail, never parsed for lifecycle facts.
    pub stderr: String,
    /// Observed exit code, absent where no process completed.
    pub code: Option<i32>,
}

/// Resolve the version-matched CLI binary: an explicit path (tests,
/// non-standard layouts), the `wyrd` binary in the bundle's
/// `Contents/Resources` beside the running app (the fixed
/// co-installation the installer ships), else the `wyrd` sibling of
/// the running desktop binary (devenv/`cargo tauri dev`, where the
/// app binary is `wyrd-desktop` and the CLI is its target-dir
/// sibling). Never a `PATH` search: a silent fallback to an
/// arbitrary binary would defeat the version match.
///
/// The bundle check is existence-gated: on the default
/// case-insensitive APFS volume `Contents/MacOS/wyrd` would resolve
/// to the `Wyrd` app binary itself, so the sibling is only the
/// fallback where no bundled copy exists.
pub fn resolve_cli_bin(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    let exe = std::env::current_exe()
        .map_err(|error| format!("cannot locate the desktop binary: {error}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "the desktop binary has no parent directory".to_string())?;
    if let Some(bundled) = bundled_cli_path(dir) {
        if bundled.is_file() {
            return Ok(bundled);
        }
    }
    Ok(dir.join("wyrd"))
}

/// Bundle-layout candidate for the CLI given the desktop binary's
/// directory: `Contents/MacOS` -> `Contents/Resources/wyrd`. Pure
/// path math, no filesystem probe — the caller existence-gates it.
pub fn bundled_cli_path(exe_dir: &Path) -> Option<PathBuf> {
    exe_dir
        .parent()
        .map(|contents| contents.join("Resources").join("wyrd"))
}

/// Parse `wyrd __build_info` stdout. Stable lines (`update/preflight.rs`):
/// `update build: <semver>`, `update envelope: 0x<NN>`,
/// `update control: 0x<NN>`. Returns the build version; envelope and
/// control versions ride along for display only — the desktop has no
/// sync edge to interpret them, so drift there is caught by the
/// owner-interface `Incompatible` verdict at verification, not here.
pub fn parse_build_info(stdout: &str) -> Result<String, String> {
    for line in stdout.lines() {
        if let Some(version) = line.strip_prefix("update build: ") {
            let version = version.trim().to_string();
            if version.is_empty() {
                return Err("`__build_info` reported an empty build version".to_string());
            }
            return Ok(version);
        }
    }
    Err("`__build_info` reported no `update build:` line".to_string())
}

/// The desktop's own build version: the workspace version both halves
/// ship from. A mismatch is a hard failure — never a silent fallback
/// to the arbitrary binary.
pub fn desktop_build_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Check the CLI is the version-matched pair: run
/// `<cli> __build_info` (lock-free, touches no store) and require the
/// reported build version to equal the desktop's own. Match policy is
/// exact equality (decided in `docs/design/desktop-shell.md`):
/// pre-1.0 versions promise nothing across minor or patch, and both
/// halves ship from one workspace version, so only an exact pair may
/// operate. A missing binary fails here too — the spawn failure maps
/// to `VersionMismatch`, never to a fallback — so every lifecycle
/// runner that gates on this check hard-fails before spawning its
/// argv. Returns the CLI binary on success for the caller's argv run.
pub fn check_cli_version(cli_bin: &Path, timeout: Duration) -> Result<PathBuf, ProvisionResult> {
    let output = run_cli(cli_bin, &["__build_info".into()], None, timeout);
    if output.timed_out {
        return Err(ProvisionResult::Unknown {
            detail: format!("version check against {} timed out", cli_bin.display()),
        });
    }
    if !output.status_success {
        return Err(ProvisionResult::VersionMismatch {
            reason: format!(
                "`__build_info` failed ({}): {}",
                describe_code(output.code),
                truncate(&output.stderr, 300)
            ),
        });
    }
    match parse_build_info(&output.stdout) {
        Ok(build) if build == desktop_build_version() => Ok(cli_bin.to_owned()),
        Ok(build) => Err(ProvisionResult::VersionMismatch {
            reason: format!(
                "CLI build {build} does not match desktop {} — reinstall the matched pair",
                desktop_build_version()
            ),
        }),
        Err(reason) => Err(ProvisionResult::VersionMismatch { reason }),
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

/// Raw completed-process capture: exit code, stdout, stderr, and
/// whether the wait timed out. Stdout/stderr are opaque display
/// detail — callers map on the exit code alone, never on text.
/// Shared with the onboarding adapter, whose commit carries the
/// passphrase on the stdin pipe (the only secret that ever crosses
/// this boundary, and never via argv).
#[derive(Debug, Clone)]
pub(crate) struct CliOutput {
    pub(crate) code: Option<i32>,
    pub(crate) status_success: bool,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) timed_out: bool,
}

/// Max retained bytes per piped stream: the drain consumes the full
/// pipe (so a chatty CLI can never wedge the wait) but keeps only
/// this prefix. 512 KiB total retained is ample for the 500-byte
/// display detail `map_exit_output` keeps, while bounding memory.
const MAX_CLI_CAPTURE_BYTES: usize = 256 * 1024;

/// Drain `pipe` to EOF, retaining at most `MAX_CLI_CAPTURE_BYTES`.
/// Excess is discarded but still consumed, so the child never blocks
/// on a full pipe while the parent waits.
fn drain_capped<R: std::io::Read>(mut pipe: R) -> Vec<u8> {
    let mut retained = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if retained.len() < MAX_CLI_CAPTURE_BYTES {
                    let room = MAX_CLI_CAPTURE_BYTES - retained.len();
                    retained.extend_from_slice(&buf[..n.min(room)]);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    retained
}

/// Spawn a pipe-drain thread. `Builder` (not `spawn`) so thread-creation
/// exhaustion reports instead of panicking across the adapter boundary.
fn spawn_drain<R: std::io::Read + Send + 'static>(
    name: &str,
    pipe: Option<R>,
) -> Option<std::io::Result<std::thread::JoinHandle<Vec<u8>>>> {
    pipe.map(|p| {
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || drain_capped(p))
    })
}

/// Run `<cli> <argv…>` and wait up to `timeout`. `std::process` only:
/// the wait polls `try_wait` while two reader threads drain stdout
/// and stderr concurrently, so a chatty CLI can never wedge the
/// adapter past the bound. Capture keeps at most
/// `MAX_CLI_CAPTURE_BYTES` per stream (the drain still consumes the
/// full pipe). On expiry the child is killed and reaped before
/// returning: the flight releases only after the process is gone, so
/// a timeout can never overlap a later operation on the same drive.
/// Expiry never means failure of the operation itself — the CLI may
/// have committed first, so every timeout resolves `Unknown` and
/// triggers observation.
///
/// `stdin_bytes`, when present, is piped to the child's stdin and
/// the pipe closed; otherwise stdin is null (a CLI that reads stdin
/// fails closed instead of hanging the adapter). Callers must bound
/// piped input: the write runs before the timeout loop, so an
/// unbounded write to a reader that never drains could block past
/// the bound (the sole caller caps stdin at 4096 bytes).
pub(crate) fn run_cli(
    cli_bin: &Path,
    argv: &[OsString],
    stdin_bytes: Option<&[u8]>,
    timeout: Duration,
) -> CliOutput {
    use std::io::Write;
    let mut command = std::process::Command::new(cli_bin);
    command.args(argv);
    if stdin_bytes.is_some() {
        command.stdin(std::process::Stdio::piped());
    } else {
        // Non-interactive, always: the CLI must never block on a prompt
        // the desktop cannot answer. Stdin is null; a CLI that reads
        // stdin fails closed instead of hanging the adapter.
        command.stdin(std::process::Stdio::null());
    }
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return CliOutput {
                code: None,
                status_success: false,
                stdout: String::new(),
                stderr: format!("cannot execute {}: {error}", cli_bin.display()),
                timed_out: false,
            };
        }
    };
    // Drain both pipes concurrently with the wait: a child writing
    // more than the OS pipe buffer blocks until the parent reads, so
    // polling `try_wait` without reading wedges past the timeout and
    // misclassifies a fast deterministic failure as `Unknown`.
    let stdout_thread = spawn_drain("wyrd-run-cli-stdout", child.stdout.take());
    let stderr_thread = spawn_drain("wyrd-run-cli-stderr", child.stderr.take());
    // A drain thread that never starts must fail closed, never panic
    // and never leak a live child holding the drive: kill, reap, and
    // resolve unknown. The missing code maps to `Unknown`, so the
    // paired observation decides — never a retry.
    if stdout_thread.as_ref().is_some_and(|r| r.is_err())
        || stderr_thread.as_ref().is_some_and(|r| r.is_err())
    {
        let _ = child.kill();
        let _ = child.wait();
        return CliOutput {
            code: None,
            status_success: false,
            stdout: String::new(),
            stderr: "cannot spawn CLI drain thread; outcome unknown — re-reading observed state"
                .to_string(),
            timed_out: false,
        };
    }
    let stdout_handle = stdout_thread.and_then(|r| r.ok());
    let stderr_handle = stderr_thread.and_then(|r| r.ok());
    if let Some(bytes) = stdin_bytes {
        if let Some(mut stdin) = child.stdin.take() {
            // Best-effort write then close: a failed write fails the
            // run, never hangs it (the wait below stays bounded and
            // kills past the timeout).
            let _ = stdin.write_all(bytes);
        }
    }
    // Poll for completion; kill and reap past the bound. The wait
    // itself is bounded regardless of how chatty the CLI is.
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // The child exited, so the pipes hit EOF once drained:
                // join the readers to collect the bounded prefix. A
                // join failure loses detail, never the exit code.
                // Residual: a grandchild that inherited a pipe keeps
                // that write end open, so its reader never sees EOF
                // and this join outlives the timeout. Unreachable
                // today (no provisioning or onboarding CLI argv
                // spawns an inheriting child) and shared with the
                // removed `wait_with_output` — documented, not bounded.
                let stdout_bytes = stdout_handle
                    .map(|handle| handle.join().unwrap_or_default())
                    .unwrap_or_default();
                let stderr_bytes = stderr_handle
                    .map(|handle| handle.join().unwrap_or_default())
                    .unwrap_or_default();
                return CliOutput {
                    code: status.code(),
                    status_success: status.success(),
                    stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
                    stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
                    timed_out: false,
                };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    // Best-effort kill, then reap: the flight must
                    // never release while the process is still alive.
                    // The drain threads are detached here: joining
                    // could block on a grandchild holding the pipe
                    // open (the wedge test's `sleep`), which would
                    // break the bound this branch exists to keep.
                    // Lifetime cost: a detached reader lives until the
                    // last pipe writer closes, so a descendant that
                    // outlives the kill keeps it consuming and
                    // discarding until it exits. The caller's wait
                    // stays bounded; the thread lifetime does not in
                    // that latent case.
                    let _ = child.kill();
                    let _ = child.wait();
                    return CliOutput {
                        code: None,
                        status_success: false,
                        stdout: String::new(),
                        stderr: format!(
                            "CLI did not complete within {}s and was stopped; outcome unknown — re-reading observed state",
                            timeout.as_secs()
                        ),
                        timed_out: true,
                    };
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                return CliOutput {
                    code: None,
                    status_success: false,
                    stdout: String::new(),
                    stderr: format!("cannot wait on CLI process: {error}"),
                    timed_out: false,
                };
            }
        }
    }
}

fn describe_code(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("exit {code}"),
        None => "no exit code".to_string(),
    }
}

/// Truncate display text to `max` bytes without splitting a
/// multi-byte character: the cut lands on the last character
/// boundary at or under `max`, so non-ASCII drive directories
/// echoed in CLI output can never panic the adapter.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= max {
        return trimmed.to_string();
    }
    let mut end = max;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", trimmed[..end].trim_end())
}

/// Map a completed process run onto the operation verdict. Only the
/// exit code decides: 0 reports no error (stdout/stderr ride along
/// as opaque display detail), 2 is an expected failure, 78 is the
/// terminal held-drive. No prefix is matched, no lifecycle fact is
/// derived — the paired observation alone decides what the display
/// says. Shared by `bootstrap` and `member leave`: both CLIs obey
/// the same exit contract, and both need the same honesty (a
/// non-owner leave exits 0 while authoring nothing, so exit 0 must
/// never mean "left").
fn map_exit_output(output: &CliOutput) -> ProvisionOutcome {
    if output.timed_out {
        return ProvisionOutcome {
            result: ProvisionResult::Unknown {
                detail: output.stderr.clone(),
            },
            stderr: output.stderr.clone(),
            code: None,
        };
    }
    // A missing code without a timeout is a spawn or collection
    // failure: the outcome is unknown, same as a timeout.
    let Some(code) = output.code else {
        return ProvisionOutcome {
            result: ProvisionResult::Unknown {
                detail: output.stderr.clone(),
            },
            stderr: output.stderr.clone(),
            code: None,
        };
    };
    // Opaque detail only: the full CLI text (trimmed and bounded)
    // travels for display, never for mapping.
    let detail = truncate(&format!("{}\n{}", output.stdout, output.stderr), 500);
    let stderr = output.stderr.clone();
    if output.status_success {
        return ProvisionOutcome {
            result: ProvisionResult::Completed { detail },
            stderr,
            code: Some(code),
        };
    }
    if code == EXIT_HELD_DRIVE {
        return ProvisionOutcome {
            result: ProvisionResult::HeldDrive { detail },
            stderr,
            code: Some(code),
        };
    }
    ProvisionOutcome {
        result: ProvisionResult::ExpectedFailure { detail },
        stderr,
        code: Some(code),
    }
}

/// Run one `bootstrap` provisioning through the version-matched CLI:
/// version check first (hard failure on mismatch), then the structured
/// argv run, then the exit-code map. Every path — including timeouts
/// (killed and reaped) and spawn failures — returns an outcome the
/// caller pairs with a registry re-read, socket probes, and a
/// membership re-observation; no path reports lifecycle state.
pub fn run_bootstrap(
    cli_bin: &Path,
    request: &ProvisionRequest,
    timeout: Duration,
) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = match bootstrap_argv(request) {
        Ok(argv) => argv,
        Err(reason) => {
            return ProvisionOutcome {
                result: ProvisionResult::ExpectedFailure { detail: reason },
                stderr: String::new(),
                code: None,
            };
        }
    };
    let output = run_cli(cli_bin, &argv, None, timeout);
    map_exit_output(&output)
}

/// Run one `member leave` through the version-matched CLI, with the
/// same version-first, observation-after contract as
/// [`run_bootstrap`].
pub fn run_leave(cli_bin: &Path, request: &LeaveRequest, timeout: Duration) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = match leave_argv(request) {
        Ok(argv) => argv,
        Err(reason) => {
            return ProvisionOutcome {
                result: ProvisionResult::ExpectedFailure { detail: reason },
                stderr: String::new(),
                code: None,
            };
        }
    };
    let output = run_cli(cli_bin, &argv, None, timeout);
    map_exit_output(&output)
}

/// Run one `drive reconfigure` through the version-matched CLI,
/// with the same version-first, observation-after contract as
/// [`run_bootstrap`]. The caller pairs every path with a registry
/// re-read, socket probes, and a membership re-observation; no path
/// reports lifecycle state.
pub fn run_reconfigure(
    cli_bin: &Path,
    request: &ReconfigureRequest,
    timeout: Duration,
) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = match reconfigure_argv(request) {
        Ok(argv) => argv,
        Err(reason) => {
            return ProvisionOutcome {
                result: ProvisionResult::ExpectedFailure { detail: reason },
                stderr: String::new(),
                code: None,
            };
        }
    };
    let output = run_cli(cli_bin, &argv, None, timeout);
    map_exit_output(&output)
}

/// Run one `drive service-start` through the version-matched CLI,
/// with the same version-first, observation-after contract as
/// [`run_bootstrap`]. The caller pairs every path with a registry
/// re-read, socket probes, and a membership re-observation; no path
/// reports lifecycle state. No credentials travel: the CLI opens no
/// keystore for a start.
pub fn run_service_start(
    cli_bin: &Path,
    request: &ServiceStartRequest,
    timeout: Duration,
) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = service_start_argv(request);
    let output = run_cli(cli_bin, &argv, None, timeout);
    map_exit_output(&output)
}

/// Run one `drive service-enable` through the version-matched CLI,
/// with the same version-first, observation-after contract as
/// [`run_bootstrap`]. The caller pairs every path with a registry
/// re-read and a fresh enablement probe; no path reports
/// enablement state. No credentials travel: the CLI opens no
/// keystore for an enable.
pub fn run_service_enable(
    cli_bin: &Path,
    request: &ServiceEnableRequest,
    timeout: Duration,
) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = service_enable_argv(request);
    let output = run_cli(cli_bin, &argv, None, timeout);
    map_exit_output(&output)
}

/// Run one `drive service-disable` through the version-matched CLI,
/// with the same version-first, observation-after contract as
/// [`run_bootstrap`]. The caller pairs every path with a registry
/// re-read and a fresh enablement probe; no path reports
/// enablement state. No credentials travel: the CLI opens no
/// keystore for a disable.
pub fn run_service_disable(
    cli_bin: &Path,
    request: &ServiceDisableRequest,
    timeout: Duration,
) -> ProvisionOutcome {
    if let Err(mismatch) = check_cli_version(cli_bin, timeout) {
        return ProvisionOutcome {
            result: mismatch,
            stderr: String::new(),
            code: None,
        };
    }
    let argv = service_disable_argv(request);
    let output = run_cli(cli_bin, &argv, None, timeout);
    map_exit_output(&output)
}

/// Unlock one registered drive for the session: start the
/// persistent owner in the foreground with a passphrase from the
/// stdin pipe (`--passphrase-stdin`), never from a file. The
/// identity secret still comes from its file; only the passphrase
/// rides the pipe. Nothing is installed, enabled, or rewritten —
/// the manager learns nothing, so a reboot or an explicit lock
/// locks the drive again.
#[derive(Debug, Clone)]
pub struct UnlockRequest {
    /// Absolute drive directory, exact string (shared drive-identity rule).
    pub drive_dir: PathBuf,
    /// Where the CLI reads the drive identity: the device identity
    /// file the start-up gate verified, or one OS credential-store
    /// item. The session passphrase always rides the stdin pipe
    /// (`--passphrase-stdin`), never a file and never the store —
    /// unlock borrows the store's identity and proves the keystore
    /// with what the operator types. Seeds never apply.
    pub credentials: CredentialSource,
    /// Control-plane relays for the owner session: the registry's
    /// registered set for the drive. Non-empty emits `--relay`
    /// flags, which *replace* the drive config file (CLI precedence:
    /// flags win when given); empty omits the flags and the CLI
    /// falls back to the config file. Unusable values never reach
    /// this field — `registry::registry_relays_for` skips them, so
    /// an all-unusable set falls back exactly like no set.
    pub relays: Vec<String>,
    /// Per-device retained-bytes ceiling in bytes. `None` enforces
    /// no ceiling, like `owner start`.
    pub retained_quota: Option<u64>,
}

/// Bound on one piped unlock passphrase: the CLI's stdin bound
/// (`read_passphrase_stdin`), mirrored here so an overlong pipe
/// refuses before any process spawns instead of failing closed
/// inside the child.
pub const MAX_UNLOCK_PASSPHRASE_BYTES: usize = 4096;

/// How long `drive_unlock` waits for the spawned owner to answer
/// before reporting unknown: long enough for the keystore KDF plus
/// the socket bind, bounded so a wedged owner cannot hold the
/// flight past the operation. Expiry resolves `Unknown`, never
/// failure — the owner may still come up under the session
/// monitor, and the paired observation decides the display.
pub const UNLOCK_VERIFY_TIMEOUT: Duration = Duration::from_secs(30);

/// Check one unlock passphrase's pipe shape before any process
/// spawns: non-empty, bounded, UTF-8, no line breaks. The CLI
/// re-validates on read (bounded take, UTF-8, single trailing
/// newline stripped, empty refused) — this mirror only fails fast
/// with the adapter's wording. Line breaks always refuse: the
/// commit path never writes one, so a break is certainly a paste
/// error, never a real passphrase.
pub fn validate_unlock_passphrase(passphrase: &[u8]) -> Result<(), String> {
    if passphrase.is_empty() {
        return Err(
            "no passphrase given: type the drive passphrase to unlock (the owner never starts without it)"
                .to_string(),
        );
    }
    if passphrase.len() > MAX_UNLOCK_PASSPHRASE_BYTES {
        return Err("passphrase exceeds the 4096-byte size limit".to_string());
    }
    let text =
        std::str::from_utf8(passphrase).map_err(|_| "passphrase must be UTF-8 text".to_string())?;
    if text.contains('\n') || text.contains('\r') {
        return Err(
            "passphrase must not contain line breaks: retype the single-line passphrase"
                .to_string(),
        );
    }
    Ok(())
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

/// One spawned unlock owner plus its pipe-drain threads. The child
/// runs the persistent owner — it is held, never waited on here:
/// the session monitor owns the wait, and `drive_lock` owns the
/// kill. Drop detaches the drain threads (they consume and discard
/// until the last pipe writer closes); the child itself is owned
/// by the session entry and is never orphaned by a drop.
///
/// `Debug` is safe: the struct carries pipe handles, never the
/// secret (the caller keeps that in its own `Zeroizing`).
#[derive(Debug)]
pub struct SpawnedUnlock {
    /// The persistent owner child: stdin already carries the
    /// passphrase and is closed; stdout/stderr are piped to the
    /// drain threads below.
    pub child: std::process::Child,
    /// Stdout drain thread: discards everything (retaining the
    /// bounded prefix only so an early exit still reports detail).
    /// Detached where the owner keeps serving; joined where it
    /// exits before the verify bound.
    pub stdout_thread: Option<std::thread::JoinHandle<Vec<u8>>>,
    /// Stderr drain thread, same discipline as stdout.
    pub stderr_thread: Option<std::thread::JoinHandle<Vec<u8>>>,
}

/// Spawn one `drive unlock` through the version-matched CLI:
/// version check first (hard failure on mismatch, like every
/// other runner), then the structured argv run with the passphrase
/// piped on stdin and the pipe closed. Returns the held child —
/// never waits: `drive unlock` runs the owner until SIGINT/SIGTERM,
/// so the wait-to-completion `run_cli` would wedge past its
/// timeout and then kill a healthy owner. The caller verifies
/// readiness (socket probe) and hands the child to the session
/// monitor; an early exit joins the drains for the exit-code
/// detail, a live owner detaches them.
///
/// The secret crosses only as pipe bytes, never argv, never a
/// file, never a log: validation bounds it before the spawn, the
/// write is one bounded `write_all` to a fresh pipe (never
/// blocking on a reader that never drains — the CLI reads stdin
/// first), and the caller's `Zeroizing` wrapper scrubs the source
/// on drop.
pub fn spawn_unlock(
    cli_bin: &Path,
    request: &UnlockRequest,
    passphrase: &[u8],
) -> Result<SpawnedUnlock, ProvisionResult> {
    use std::io::Write as _;
    if let Err(reason) = validate_unlock_passphrase(passphrase) {
        return Err(ProvisionResult::ExpectedFailure { detail: reason });
    }
    check_cli_version(cli_bin, PROVISION_TIMEOUT)?;
    let argv = match unlock_argv(request) {
        Ok(argv) => argv,
        Err(reason) => {
            return Err(ProvisionResult::ExpectedFailure { detail: reason });
        }
    };
    let mut command = std::process::Command::new(cli_bin);
    command.args(&argv);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Err(ProvisionResult::Unknown {
                detail: format!("cannot execute {}: {error}", cli_bin.display()),
            });
        }
    };
    let stdout_thread = spawn_drain("wyrd-unlock-stdout", child.stdout.take());
    let stderr_thread = spawn_drain("wyrd-unlock-stderr", child.stderr.take());
    // A drain thread that never starts must fail closed, never leak
    // a live owner holding the drive: kill, reap, and resolve
    // unknown — the paired observation decides, never a retry.
    if stdout_thread.as_ref().is_some_and(|r| r.is_err())
        || stderr_thread.as_ref().is_some_and(|r| r.is_err())
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ProvisionResult::Unknown {
            detail: "cannot spawn unlock drain thread; outcome unknown — re-reading observed state"
                .to_string(),
        });
    }
    if let Some(mut stdin) = child.stdin.take() {
        // One bounded write to a fresh pipe, then close: the CLI
        // reads stdin before serving, so this never blocks — and
        // closing tells the CLI the line is complete.
        if let Err(error) = stdin.write_all(passphrase) {
            if error.kind() == std::io::ErrorKind::BrokenPipe {
                // The child closed its read end before the write
                // landed: it exited early (a rejected passphrase, a
                // held drive) without reading stdin. That is an
                // early exit, not an unknown outcome — hand the
                // child back so the caller's try_wait loop maps its
                // code (78 -> held drive) and joins the drains for
                // the bounded detail. Killing here discards exactly
                // the code the caller needs.
            } else {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProvisionResult::Unknown {
                    detail: format!(
                        "cannot pipe the passphrase to the unlock owner: {error}; outcome unknown — re-reading observed state"
                    ),
                });
            }
        }
    }
    Ok(SpawnedUnlock {
        child,
        stdout_thread: stdout_thread.and_then(|r| r.ok()),
        stderr_thread: stderr_thread.and_then(|r| r.ok()),
    })
}

/// Collect the bounded display detail from an unlock owner that
/// exited before serving: join both drains (EOF follows the exit)
/// and truncate like every other runner. Call only where the
/// child already exited — joining a live owner's drains would
/// wedge past the verify bound.
pub fn collect_unlock_exit_detail(spawned: SpawnedUnlock) -> (Option<i32>, String) {
    let mut child = spawned.child;
    let code = child.wait().ok().and_then(|status| status.code());
    let stdout_bytes = spawned
        .stdout_thread
        .map(|handle| handle.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr_bytes = spawned
        .stderr_thread
        .map(|handle| handle.join().unwrap_or_default())
        .unwrap_or_default();
    (
        code,
        truncate(
            &format!(
                "{}\n{}",
                String::from_utf8_lossy(&stdout_bytes),
                String::from_utf8_lossy(&stderr_bytes)
            ),
            500,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_request() -> ProvisionRequest {
        ProvisionRequest {
            name: "alpha".to_string(),
            drive_dir: PathBuf::from("/drives/alpha"),
            relays: vec!["wss://relay.example".to_string()],
            credentials: CredentialSource::File {
                identity_file: Some(PathBuf::from("/keys/identity")),
                passphrase_file: Some(PathBuf::from("/keys/passphrase")),
            },
            invitation_file: None,
            registry_path: None,
            owner_bin: None,
            supervise_bin: None,
            no_service: false,
            mode: "vault".to_string(),
            mountpoint: None,
        }
    }

    /// The bundle-layout candidate is pure path math:
    /// `Contents/MacOS` -> `Contents/Resources/wyrd`, and the sibling
    /// stays the fallback. The existence gate lives in
    /// `resolve_cli_bin`, not here.
    #[test]
    fn bundled_cli_path_maps_macos_to_resources() {
        let macos = Path::new("/Applications/Wyrd.app/Contents/MacOS");
        assert_eq!(
            bundled_cli_path(macos),
            Some(PathBuf::from(
                "/Applications/Wyrd.app/Contents/Resources/wyrd"
            ))
        );
        assert_eq!(
            resolve_cli_bin(Some(PathBuf::from("/custom/wyrd"))).unwrap(),
            PathBuf::from("/custom/wyrd"),
            "an explicit path passes through untouched"
        );
    }

    /// The create argv is one structured element per argument: no
    /// shell, no joined command line, credential paths as bare
    /// values — contents never travel.
    #[test]
    fn create_argv_is_structured_per_element() {
        let argv = bootstrap_argv(&create_request()).unwrap();
        let rendered: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "drive",
                "/drives/alpha",
                "--identity-file",
                "/keys/identity",
                "--passphrase-file",
                "/keys/passphrase",
                "bootstrap",
                "--name",
                "alpha",
                "--mode",
                "vault",
                "--relay",
                "wss://relay.example",
            ]
        );
        assert!(
            !rendered
                .iter()
                .any(|arg| arg.contains(' ') && arg.contains("--")),
            "no joined shell string: {rendered:?}"
        );
    }

    /// Join adds exactly the invitation-file pair; `--no-service`
    /// and explicit paths thread through untouched.
    #[test]
    fn join_argv_adds_the_invitation_pair() {
        let mut request = create_request();
        request.invitation_file = Some(PathBuf::from("/tmp/invite.sealed"));
        request.no_service = true;
        request.registry_path = Some(PathBuf::from("/tmp/drives.json"));
        let argv = bootstrap_argv(&request).unwrap();
        let rendered: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(rendered.contains(&"--invitation-file".to_string()));
        assert!(rendered.contains(&"/tmp/invite.sealed".to_string()));
        assert!(rendered.contains(&"--no-service".to_string()));
        assert!(rendered.contains(&"--registry".to_string()));
    }

    /// Mount argv carries `--mode mount` plus the mountpoint pair:
    /// the FUSE projection the managed mount mode serves.
    #[test]
    fn mount_argv_carries_the_mode_and_mountpoint_pairs() {
        let mut request = create_request();
        request.mode = "mount".to_string();
        request.mountpoint = Some(PathBuf::from("/mnt/alpha"));
        let argv = bootstrap_argv(&request).unwrap();
        let rendered: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(rendered.contains(&"--mode".to_string()));
        assert!(rendered.contains(&"mount".to_string()));
        assert!(rendered.contains(&"--mountpoint".to_string()));
        assert!(rendered.contains(&"/mnt/alpha".to_string()));
    }

    /// Mount without a mountpoint refuses before any process spawns,
    /// like vault carrying one: the pair travels together or not at
    /// all.
    #[test]
    fn mount_without_a_mountpoint_refuses_before_spawning() {
        let mut request = create_request();
        request.mode = "mount".to_string();
        assert!(request.mountpoint.is_none());
        let reason = bootstrap_argv(&request).unwrap_err();
        assert!(reason.contains("needs a mountpoint"), "{reason}");
        let mut stray = create_request();
        stray.mountpoint = Some(PathBuf::from("/mnt/stray"));
        let reason = bootstrap_argv(&stray).unwrap_err();
        assert!(reason.contains("carries no mountpoint"), "{reason}");
    }

    /// Unknown modes refuse before any process spawns: the adapter
    /// never provisions a service the registry reader would skip.
    #[test]
    fn unknown_mode_refuses_before_spawning() {
        let mut request = create_request();
        request.mode = "fuse".to_string();
        match bootstrap_argv(&request) {
            Err(reason) => assert!(
                reason.contains("unknown service mode"),
                "reason names the unknown mode: {reason}"
            ),
            Ok(_) => panic!("an unknown mode must refuse"),
        }
    }

    /// A fresh store-backed bootstrap seeds the item from the
    /// one-time input files and names the store reference: seed
    /// flags first (the CLI reads them as input), then the backend
    /// pair the dispatcher routes on. No seed material in the
    /// reference — only file paths and the account label.
    #[test]
    fn store_backed_bootstrap_seeds_then_references() {
        let mut request = create_request();
        request.credentials = CredentialSource::OsStore {
            account: "wyrd-drive-alpha".to_string(),
            seed: Some(SeedFiles {
                identity_file: PathBuf::from("/keys/identity"),
                passphrase_file: PathBuf::from("/keys/passphrase"),
            }),
        };
        let rendered: Vec<String> = bootstrap_argv(&request)
            .unwrap()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "drive",
                "/drives/alpha",
                "--identity-file",
                "/keys/identity",
                "--passphrase-file",
                "/keys/passphrase",
                "--credential-backend",
                "os-store",
                "--credential-account",
                "wyrd-drive-alpha",
                "bootstrap",
                "--name",
                "alpha",
                "--mode",
                "vault",
                "--relay",
                "wss://relay.example",
            ]
        );
    }

    /// A converged store-backed re-run names only the reference:
    /// the item already holds the material, so no seed files ride
    /// along — the CLI reads the provisioned item itself.
    #[test]
    fn store_backed_bootstrap_without_seed_references_only() {
        let mut request = create_request();
        request.credentials = CredentialSource::OsStore {
            account: "wyrd-drive-alpha".to_string(),
            seed: None,
        };
        let rendered: Vec<String> = bootstrap_argv(&request)
            .unwrap()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "drive",
                "/drives/alpha",
                "--credential-backend",
                "os-store",
                "--credential-account",
                "wyrd-drive-alpha",
                "bootstrap",
                "--name",
                "alpha",
                "--mode",
                "vault",
                "--relay",
                "wss://relay.example",
            ]
        );
        assert!(
            !rendered.iter().any(|arg| arg == "--identity-file"),
            "a converged re-run names no files: {rendered:?}"
        );
    }

    /// Leave and reconfigure on a store-backed drive name the item,
    /// never files: the CLI resolves the provisioned material
    /// through the loader boundary, and file flags beside the
    /// store backend would refuse as one-backend-per-drive.
    #[test]
    fn store_leave_and_reconfigure_reference_the_item() {
        let store = CredentialSource::OsStore {
            account: "wyrd-drive-alpha".to_string(),
            seed: None,
        };
        let leave = LeaveRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            credentials: store.clone(),
            acknowledged: true,
        };
        let rendered: Vec<String> = leave_argv(&leave)
            .unwrap()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "member",
                "/drives/alpha",
                "--credential-backend",
                "os-store",
                "--credential-account",
                "wyrd-drive-alpha",
                "leave",
                "--yes",
            ]
        );
        let mut reconfigure = reconfigure_request();
        reconfigure.credentials = store;
        let rendered: Vec<String> = reconfigure_argv(&reconfigure)
            .unwrap()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(rendered.contains(&"--credential-account".to_string()));
        assert!(rendered.contains(&"wyrd-drive-alpha".to_string()));
        assert!(
            !rendered.iter().any(|arg| arg == "--passphrase-file"),
            "a store reconfigure names no files: {rendered:?}"
        );
    }

    /// Seeds are bootstrap input only: leave, reconfigure, and
    /// unlock read the provisioned credential, so a seed anywhere
    /// else refuses before any process spawns — mirroring the
    /// CLI's one-backend-per-drive refusal at the adapter.
    #[test]
    fn seeds_refuse_outside_bootstrap() {
        let seeded = CredentialSource::OsStore {
            account: "wyrd-drive-alpha".to_string(),
            seed: Some(SeedFiles {
                identity_file: PathBuf::from("/keys/identity"),
                passphrase_file: PathBuf::from("/keys/passphrase"),
            }),
        };
        let leave = LeaveRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            credentials: seeded.clone(),
            acknowledged: false,
        };
        let reason = leave_argv(&leave).unwrap_err();
        assert!(reason.contains("bootstrap input only"), "{reason}");
        let mut reconfigure = reconfigure_request();
        reconfigure.credentials = seeded.clone();
        let reason = reconfigure_argv(&reconfigure).unwrap_err();
        assert!(reason.contains("bootstrap input only"), "{reason}");
        let mut unlock = unlock_request();
        unlock.credentials = seeded;
        let reason = unlock_argv(&unlock).unwrap_err();
        assert!(reason.contains("bootstrap input only"), "{reason}");
    }

    /// A store-backed unlock loads the identity from the item and
    /// still pipes the session passphrase: the store pair plus
    /// `--passphrase-stdin`, never `--identity-file` (the CLI
    /// refuses file flags beside a store unlock) and never
    /// `--passphrase-file`.
    #[test]
    fn store_unlock_reads_the_item_identity() {
        let mut request = unlock_request();
        request.credentials = CredentialSource::OsStore {
            account: "wyrd-drive-alpha".to_string(),
            seed: None,
        };
        let rendered: Vec<String> = unlock_argv(&request)
            .unwrap()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "drive",
                "/drives/alpha",
                "--credential-backend",
                "os-store",
                "--credential-account",
                "wyrd-drive-alpha",
                "--passphrase-stdin",
                "unlock",
                "--relay",
                "wss://relay.example",
            ]
        );
    }

    /// An empty store account names no item: every operation
    /// refuses before any process spawns instead of rendering
    /// flags the CLI would reject without the reference.
    #[test]
    fn empty_store_account_refuses_before_spawning() {
        let empty = CredentialSource::OsStore {
            account: String::new(),
            seed: None,
        };
        let mut bootstrap = create_request();
        bootstrap.credentials = empty.clone();
        let reason = bootstrap_argv(&bootstrap).unwrap_err();
        assert!(
            reason.contains("no OS credential-store account"),
            "{reason}"
        );
        let leave = LeaveRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            credentials: empty.clone(),
            acknowledged: false,
        };
        let reason = leave_argv(&leave).unwrap_err();
        assert!(
            reason.contains("no OS credential-store account"),
            "{reason}"
        );
        let unlock = UnlockRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            credentials: empty,
            relays: Vec::new(),
            retained_quota: None,
        };
        let reason = unlock_argv(&unlock).unwrap_err();
        assert!(
            reason.contains("no OS credential-store account"),
            "{reason}"
        );
    }

    /// Leave argv carries `--yes` only on explicit acknowledgement:
    /// a frontend omitting the dialog cannot bypass the CLI boundary.
    /// Credentials precede the `leave` verb: they are flattened on the
    /// parent, not global, so anything after the verb is rejected.
    #[test]
    fn leave_argv_gates_yes_on_acknowledgement() {
        let base = LeaveRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            credentials: CredentialSource::File {
                identity_file: Some(PathBuf::from("/keys/identity")),
                passphrase_file: Some(PathBuf::from("/keys/passphrase")),
            },
            acknowledged: false,
        };
        let plain: Vec<String> = leave_argv(&base)
            .unwrap()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            !plain.contains(&"--yes".to_string()),
            "no acknowledgement, no --yes: {plain:?}"
        );
        assert_eq!(
            plain,
            vec![
                "member",
                "/drives/alpha",
                "--identity-file",
                "/keys/identity",
                "--passphrase-file",
                "/keys/passphrase",
                "leave",
            ],
            "credentials precede the verb even without --yes: {plain:?}"
        );
        let acked = LeaveRequest {
            acknowledged: true,
            ..base
        };
        let confirmed: Vec<String> = leave_argv(&acked)
            .unwrap()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            confirmed,
            vec![
                "member",
                "/drives/alpha",
                "--identity-file",
                "/keys/identity",
                "--passphrase-file",
                "/keys/passphrase",
                "leave",
                "--yes",
            ]
        );
    }

    /// `__build_info` parsing reads the fixed first line and refuses
    /// output without it — the version gate never guesses.
    #[test]
    fn build_info_parses_the_fixed_line() {
        let stdout = "update build: 0.3.0-alpha\nupdate envelope: 0x01\nupdate control: 0x02\n";
        assert_eq!(parse_build_info(stdout).unwrap(), "0.3.0-alpha");
        assert!(
            parse_build_info("nothing here\n").is_err(),
            "output without the line refuses"
        );
        assert!(
            parse_build_info("update build: \n").is_err(),
            "an empty version refuses"
        );
    }

    /// Exit mapping uses the exit code only — never the CLI text.
    /// Exit 0 completes (the paired observation decides what that
    /// means — a non-owner leave also exits 0 while authoring
    /// nothing), exit 2 is an expected failure with opaque detail,
    /// 78 is the terminal held-drive, and timeouts resolve unknown.
    #[test]
    fn exit_mapping_uses_the_code_only() {
        // Exit 0 with the success line: completed, and the opaque
        // detail rides along for display — never for mapping.
        let ready = CliOutput {
            code: Some(0),
            status_success: true,
            stdout: "drive alpha verified available (socket /drives/alpha/owner.sock)\n"
                .to_string(),
            stderr: String::new(),
            timed_out: false,
        };
        match map_exit_output(&ready).result {
            ProvisionResult::Completed { detail } => {
                assert!(detail.contains("verified available"), "detail: {detail}")
            }
            other => panic!("exit 0 completes: {other:?}"),
        }
        // A non-owner leave refusal also exits 0: completed as well —
        // exit 0 must never mean "left". The membership observation
        // decides.
        let refusal = CliOutput {
            code: Some(0),
            status_success: true,
            stdout: "only an owner can remove a device: ask the owner to run: member remove <id>\n"
                .to_string(),
            stderr: String::new(),
            timed_out: false,
        };
        assert!(matches!(
            map_exit_output(&refusal).result,
            ProvisionResult::Completed { .. }
        ));
        // Exit 2 with any text: expected failure, detail opaque.
        let failed = CliOutput {
            code: Some(2),
            status_success: false,
            stdout: String::new(),
            stderr: "error: drive alpha registered but unavailable: verify-socket: owner.sock never appeared\n".to_string(),
            timed_out: false,
        };
        match map_exit_output(&failed).result {
            ProvisionResult::ExpectedFailure { detail } => {
                assert!(
                    detail.contains("registered but unavailable"),
                    "detail: {detail}"
                )
            }
            other => panic!("exit 2 fails expectedly: {other:?}"),
        }
        // An unacknowledged sole-owner leave (exit 2) is the same
        // shape: no confirm-stage taxonomy, just opaque detail plus
        // observation.
        let unacked = CliOutput {
            code: Some(2),
            status_success: false,
            stdout: String::new(),
            stderr: "error: Removing the sole owner is valid but terminal: pass --yes to confirm\n"
                .to_string(),
            timed_out: false,
        };
        assert!(matches!(
            map_exit_output(&unacked).result,
            ProvisionResult::ExpectedFailure { .. }
        ));
        let held = CliOutput {
            code: Some(78),
            status_success: false,
            stdout: String::new(),
            stderr: "error: another process holds the drive\n".to_string(),
            timed_out: false,
        };
        assert!(matches!(
            map_exit_output(&held).result,
            ProvisionResult::HeldDrive { .. }
        ));
        let timed_out = CliOutput {
            code: None,
            status_success: false,
            stdout: String::new(),
            stderr: "CLI did not complete within 120s".to_string(),
            timed_out: true,
        };
        assert!(matches!(
            map_exit_output(&timed_out).result,
            ProvisionResult::Unknown { .. }
        ));
    }

    /// Truncation never splits a multi-byte character: the cut lands
    /// on a character boundary even when the budget falls mid-glyph,
    /// so non-ASCII drive directories in CLI output cannot panic.
    #[test]
    fn truncate_never_splits_a_character() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("  padded  ", 10), "padded");
        let text = "drive /drives/\u{00e9}t\u{00e9} ready";
        assert!(truncate(text, 100).contains("\u{00e9}t\u{00e9}"));
        // A budget landing inside the two-byte \u{00e9} shortens to the
        // boundary instead of panicking.
        let cut = truncate(text, 15);
        assert!(cut.is_ascii() || cut.len() <= 18, "cut: {cut}");
        // Full multi-byte stress: every budget is a valid string.
        let wide = "\u{65e5}\u{672c}\u{8a9e}\u{30c6}\u{30b9}\u{30c8} drive";
        for max in 0..=wide.len() + 4 {
            let out = truncate(wide, max);
            assert!(out.len() <= max + 3, "budget {max}: {out}");
        }
    }

    /// A wedged CLI is killed and reaped past the timeout: the
    /// adapter resolves unknown, the spawned process is gone, and no
    /// wait is left hanging on it. (The kill targets the spawned
    /// shell itself; a grandchild it spawned is the shell's problem,
    /// not the adapter's — which is why this test asserts the
    /// bounded return and the release, not process-table absence.)
    #[test]
    fn wedged_cli_is_killed_past_the_timeout() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-fakecli-{}-wedge", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("wyrd");
        // Answers the version check instantly, then sleeps past any
        // test timeout on every other argv.
        let version = env!("CARGO_PKG_VERSION");
        std::fs::write(
            &cli,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nsleep 30\nexit 0\n"
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let start = std::time::Instant::now();
        let outcome = run_bootstrap(&cli, &create_request(), Duration::from_secs(2));
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the wait is bounded by the timeout"
        );
        assert!(
            matches!(outcome.result, ProvisionResult::Unknown { .. }),
            "a wedged CLI resolves unknown: {:?}",
            outcome.result
        );
        // The wait was bounded by the timeout, and the reaped child
        // leaves the flight free: the IPC flight test pins the
        // release, and a leaked child would wedge the next claim.
        // (The shell script's own `sleep` grandchild is not the
        // adapter's to reap.)
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// A chatty CLI that writes 1 MiB to stdout and exits 2 must not
    /// wedge the adapter: the pipes are drained concurrently with the
    /// wait, so the exit code (expected failure) arrives well under
    /// the timeout instead of resolving `Unknown` after a kill. Both
    /// pipes are drained (1 MiB each) so a stderr flood cannot wedge
    /// a stdout drain either; capture keeps only a bounded prefix.
    #[test]
    fn chatty_cli_drains_pipes_instead_of_wedging() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-chatty",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("wyrd");
        std::fs::write(
            &cli,
            "#!/bin/sh\nhead -c 1048576 /dev/zero | tr '\\0' 'A'\nhead -c 1048576 /dev/zero | tr '\\0' 'B' >&2\nexit 2\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let start = std::time::Instant::now();
        let output = run_cli(&cli, &["probe".into()], None, Duration::from_secs(10));
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "a 1 MiB exit-2 must return well under the timeout, took {elapsed:?}"
        );
        assert!(
            !output.timed_out,
            "a fast exit-2 is not a timeout: {output:?}"
        );
        assert_eq!(output.code, Some(2), "exit code survives: {output:?}");
        assert!(
            output.stdout.len() <= super::MAX_CLI_CAPTURE_BYTES,
            "stdout capture stays bounded: {} bytes",
            output.stdout.len()
        );
        assert!(
            output.stderr.len() <= super::MAX_CLI_CAPTURE_BYTES,
            "stderr capture stays bounded: {} bytes",
            output.stderr.len()
        );
        assert!(
            !output.stdout.is_empty() && !output.stderr.is_empty(),
            "bounded prefix still carries output for display"
        );
        assert!(
            matches!(
                map_exit_output(&output).result,
                ProvisionResult::ExpectedFailure { .. }
            ),
            "exit 2 maps to expected failure: {:?}",
            map_exit_output(&output).result
        );
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// Write a fake `wyrd` CLI: a shell script answering
    /// `__build_info` with the workspace version and every other
    /// invocation with the given stdout and exit code. The adapter
    /// spawns it exactly like the installed binary — no special
    /// path, no mock.
    fn fake_cli(tag: &str, body_stdout: &str, body_code: i32) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-{}",
            std::process::id(),
            tag
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wyrd");
        let version = env!("CARGO_PKG_VERSION");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\necho \"update envelope: 0x01\"\necho \"update control: 0x02\"\nexit 0\nfi\nprintf '%s' \"{body_stdout}\"\nexit {body_code}\n"
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

    /// End to end through a fake CLI binary: version check passes
    /// against the workspace version, the structured argv run exits
    /// 0, and the outcome completes with the opaque CLI text — the
    /// full `run_bootstrap` path the IPC command drives, with no
    /// mock. What the text says is display detail; the code decides.
    #[test]
    fn run_bootstrap_completes_through_a_fake_cli() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-fakecli-{}-ready", std::process::id()));
        let cli = fake_cli(
            "ready",
            "drive alpha verified available (socket /drives/alpha/owner.sock)",
            0,
        );
        let outcome = run_bootstrap(&cli, &create_request(), Duration::from_secs(30));
        match &outcome.result {
            ProvisionResult::Completed { detail } => {
                assert!(detail.contains("verified available"), "detail: {detail}")
            }
            other => panic!("fake exit 0 completes: {other:?}"),
        }
        assert_eq!(outcome.code, Some(0));
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// A version-skewed CLI is a hard failure before any lifecycle
    /// argv runs: the body would report ready, but the version gate
    /// refuses first — never a silent fallback.
    #[test]
    fn run_bootstrap_refuses_a_version_skewed_cli() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-fakecli-{}-skew", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("wyrd");
        std::fs::write(
            &cli,
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: 9.9.9-mismatch\"\nexit 0\nfi\necho \"drive alpha verified available (socket /x/owner.sock)\"\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let outcome = run_bootstrap(&cli, &create_request(), Duration::from_secs(30));
        assert!(
            matches!(outcome.result, ProvisionResult::VersionMismatch { .. }),
            "skew refuses before the lifecycle argv runs: {:?}",
            outcome.result
        );
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// A missing CLI binary is the same hard failure as a skewed one:
    /// the spawn failure maps to `VersionMismatch` with the binary
    /// named, never to a fallback or a silent skip.
    #[test]
    fn version_check_against_a_missing_binary_is_a_hard_failure() {
        let missing =
            std::env::temp_dir().join(format!("wyrd-desktop-no-such-cli-{}", std::process::id()));
        assert!(
            !missing.exists(),
            "the missing-binary probe must not exist: {}",
            missing.display()
        );
        match check_cli_version(&missing, Duration::from_secs(30)) {
            Err(ProvisionResult::VersionMismatch { reason }) => {
                assert!(
                    reason.contains(&missing.display().to_string()),
                    "the reason names the missing binary: {reason}"
                );
            }
            other => panic!("a missing CLI hard-fails: {other:?}"),
        }
    }

    /// A failing fake CLI maps through the exit-code table: exit 2
    /// is an expected failure with opaque detail, and the re-read —
    /// not the detail — decides lifecycle state.
    #[test]
    fn run_leave_fails_expectedly_through_a_fake_cli() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-leave-refused",
            std::process::id()
        ));
        let cli = fake_cli("leave-refused", "", 2);
        let outcome = run_leave(
            &cli,
            &LeaveRequest {
                drive_dir: PathBuf::from("/drives/alpha"),
                credentials: CredentialSource::File {
                    identity_file: Some(PathBuf::from("/keys/identity")),
                    passphrase_file: Some(PathBuf::from("/keys/passphrase")),
                },
                acknowledged: false,
            },
            Duration::from_secs(30),
        );
        assert!(
            matches!(outcome.result, ProvisionResult::ExpectedFailure { .. }),
            "exit 2 fails expectedly: {:?}",
            outcome.result
        );
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// Join runs the same collapsed path as create: the invitation
    /// argv is structured (pinned above) and an exit-0 join
    /// completes with opaque detail through the fake CLI.
    #[test]
    fn run_join_completes_through_a_fake_cli() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-join-ready",
            std::process::id()
        ));
        let cli = fake_cli(
            "join-ready",
            "drive beta verified available (socket /drives/beta/owner.sock)",
            0,
        );
        let mut request = create_request();
        request.name = "beta".to_string();
        request.drive_dir = PathBuf::from("/drives/beta");
        request.invitation_file = Some(PathBuf::from("/tmp/invite.sealed"));
        let outcome = run_bootstrap(&cli, &request, Duration::from_secs(30));
        assert!(
            matches!(outcome.result, ProvisionResult::Completed { .. }),
            "fake exit-0 join completes: {:?}",
            outcome.result
        );
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    fn reconfigure_request() -> ReconfigureRequest {
        ReconfigureRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            relays: vec!["wss://relay.example".to_string()],
            credentials: CredentialSource::File {
                identity_file: Some(PathBuf::from("/keys/identity")),
                passphrase_file: Some(PathBuf::from("/keys/passphrase")),
            },
            registry_path: None,
            owner_bin: None,
            supervise_bin: None,
            no_service: false,
            mode: "vault".to_string(),
            mountpoint: None,
        }
    }

    /// The reconfigure argv is one structured element per argument:
    /// `drive <dir> --identity-file … --passphrase-file … reconfigure
    /// --mode vault`, relay pairs after the verb — never a joined
    /// shell string. Credentials precede the verb because they are
    /// flattened on the parent, not global.
    #[test]
    fn reconfigure_argv_is_structured_per_element() {
        let argv = reconfigure_argv(&reconfigure_request()).unwrap();
        let rendered: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "drive",
                "/drives/alpha",
                "--identity-file",
                "/keys/identity",
                "--passphrase-file",
                "/keys/passphrase",
                "reconfigure",
                "--mode",
                "vault",
                "--relay",
                "wss://relay.example",
            ]
        );
        assert!(
            !rendered
                .iter()
                .any(|arg| arg.contains(' ') && arg.contains("--")),
            "no joined shell string: {rendered:?}"
        );
        // Position pin, not just contents: the credential pair must
        // sit before the subcommand verb (the nix/drive owner-argv
        // order-pin precedent) — after the verb clap rejects it.
        let verb = rendered
            .iter()
            .position(|arg| arg == "reconfigure")
            .unwrap();
        let identity = rendered
            .iter()
            .position(|arg| arg == "--identity-file")
            .unwrap();
        let passphrase = rendered
            .iter()
            .position(|arg| arg == "--passphrase-file")
            .unwrap();
        assert!(
            identity < verb && passphrase < verb,
            "credentials precede the verb: {rendered:?}"
        );
    }

    /// Empty relays emit no `--relay` flags: the CLI keeps the
    /// registered set, so a credential-path-only reconfigure never
    /// clears relays by accident.
    #[test]
    fn reconfigure_argv_omits_relay_flags_when_empty() {
        let mut request = reconfigure_request();
        request.relays.clear();
        request.no_service = true;
        let argv = reconfigure_argv(&request).unwrap();
        let rendered: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            !rendered.contains(&"--relay".to_string()),
            "no relay flags: {rendered:?}"
        );
        assert!(rendered.contains(&"--no-service".to_string()));
    }

    /// Mount reconfigure argv carries `--mode mount` plus the
    /// mountpoint pair; unknown modes refuse before any process
    /// spawns — the adapter never reconfigures into a mode the
    /// registry reader skips.
    #[test]
    fn mount_reconfigure_carries_the_mode_and_mountpoint_pairs() {
        let mut request = reconfigure_request();
        request.mode = "mount".to_string();
        request.mountpoint = Some(PathBuf::from("/mnt/alpha"));
        let argv = reconfigure_argv(&request).unwrap();
        let rendered: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(rendered.contains(&"--mode".to_string()));
        assert!(rendered.contains(&"mount".to_string()));
        assert!(rendered.contains(&"--mountpoint".to_string()));
        assert!(rendered.contains(&"/mnt/alpha".to_string()));
    }

    #[test]
    fn unknown_reconfigure_mode_refuses_before_spawning() {
        let mut request = reconfigure_request();
        request.mode = "fuse".to_string();
        match reconfigure_argv(&request) {
            Err(reason) => assert!(
                reason.contains("unknown service mode"),
                "reason names the unknown mode: {reason}"
            ),
            Ok(_) => panic!("an unknown reconfigure must refuse"),
        }
    }

    /// End to end through a fake CLI binary: version check passes
    /// and an exit-0 reconfigure completes with opaque detail — the
    /// full `run_reconfigure` path the IPC command drives.
    #[test]
    fn run_reconfigure_completes_through_a_fake_cli() {
        let cli = fake_cli(
            "reconf-ready",
            "drive alpha reconfigured and verified available (socket /drives/alpha/owner.sock)",
            0,
        );
        let outcome = run_reconfigure(&cli, &reconfigure_request(), Duration::from_secs(30));
        match &outcome.result {
            ProvisionResult::Completed { detail } => {
                assert!(detail.contains("reconfigured"), "detail: {detail}")
            }
            other => panic!("fake exit-0 reconfigure completes: {other:?}"),
        }
        assert_eq!(outcome.code, Some(0));
    }

    /// A version-skewed CLI is a hard failure before the reconfigure
    /// argv runs — never a silent fallback.
    #[test]
    fn run_reconfigure_refuses_a_version_skewed_cli() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-reconf-skew",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("wyrd");
        std::fs::write(
            &cli,
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: 9.9.9-mismatch\"\nexit 0\nfi\necho \"drive alpha reconfigured\"\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let outcome = run_reconfigure(&cli, &reconfigure_request(), Duration::from_secs(30));
        assert!(
            matches!(outcome.result, ProvisionResult::VersionMismatch { .. }),
            "skew refuses before the reconfigure argv runs: {:?}",
            outcome.result
        );
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// A wedged CLI is killed and reaped past the timeout on the
    /// reconfigure path too: the outcome resolves unknown and
    /// triggers observation, never re-execution.
    #[test]
    fn run_reconfigure_times_out_to_unknown() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-reconf-wedge",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("wyrd");
        // Answers the version check instantly, then sleeps past any
        // test timeout on every other argv.
        let version = env!("CARGO_PKG_VERSION");
        std::fs::write(
            &cli,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nsleep 30\nexit 0\n"
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let start = std::time::Instant::now();
        let outcome = run_reconfigure(&cli, &reconfigure_request(), Duration::from_secs(2));
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the wait is bounded by the timeout"
        );
        assert!(
            matches!(outcome.result, ProvisionResult::Unknown { .. }),
            "a wedged reconfigure resolves unknown: {:?}",
            outcome.result
        );
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// The service-start argv is one structured element per argument:
    /// `drive <dir> service-start`, no credentials — starting opens
    /// no keystore — plus the registry override where set.
    #[test]
    fn service_start_argv_is_structured_without_credentials() {
        let request = ServiceStartRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            registry_path: Some(PathBuf::from("/tmp/drives.json")),
        };
        let rendered: Vec<String> = service_start_argv(&request)
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "drive",
                "/drives/alpha",
                "service-start",
                "--registry",
                "/tmp/drives.json",
            ]
        );
        assert!(
            !rendered.iter().any(|arg| arg.contains("identity")),
            "no credential paths travel on a start: {rendered:?}"
        );
    }

    /// The service-enable argv is one structured element per
    /// argument: `drive <dir> service-enable` — recording a
    /// login-start preference opens no keystore, so no credential
    /// paths travel — plus the registry override where set.
    #[test]
    fn service_enable_argv_is_structured_without_credentials() {
        let request = ServiceEnableRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            registry_path: Some(PathBuf::from("/tmp/drives.json")),
        };
        let rendered: Vec<String> = service_enable_argv(&request)
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "drive",
                "/drives/alpha",
                "service-enable",
                "--registry",
                "/tmp/drives.json",
            ]
        );
        assert!(
            !rendered.iter().any(|arg| arg.contains("identity")),
            "no credential paths travel on an enable: {rendered:?}"
        );
    }

    /// The service-disable argv is one structured element per
    /// argument: `drive <dir> service-disable` — stopping now and
    /// recording no login start opens no keystore — plus the
    /// registry override where set.
    #[test]
    fn service_disable_argv_is_structured_without_credentials() {
        let request = ServiceDisableRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            registry_path: None,
        };
        let rendered: Vec<String> = service_disable_argv(&request)
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(rendered, vec!["drive", "/drives/alpha", "service-disable"]);
        assert!(
            !rendered.iter().any(|arg| arg.contains("identity")),
            "no credential paths travel on a disable: {rendered:?}"
        );
    }

    /// End to end through a fake CLI binary: version check passes
    /// and an exit-0 start completes with opaque detail — the full
    /// `run_service_start` path the IPC command drives.
    #[test]
    fn run_service_start_completes_through_a_fake_cli() {
        let cli = fake_cli(
            "start-ready",
            "drive alpha service started and verified available (socket /drives/alpha/owner.sock)",
            0,
        );
        let request = ServiceStartRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            registry_path: None,
        };
        let outcome = run_service_start(&cli, &request, Duration::from_secs(30));
        match &outcome.result {
            ProvisionResult::Completed { detail } => {
                assert!(detail.contains("verified available"), "detail: {detail}")
            }
            other => panic!("fake exit-0 start completes: {other:?}"),
        }
        assert_eq!(outcome.code, Some(0));
    }

    fn unlock_request() -> UnlockRequest {
        UnlockRequest {
            drive_dir: PathBuf::from("/drives/alpha"),
            credentials: CredentialSource::File {
                identity_file: Some(PathBuf::from("/keys/identity")),
                passphrase_file: Some(PathBuf::from("/keys/passphrase")),
            },
            relays: vec!["wss://relay.example".to_string()],
            retained_quota: None,
        }
    }

    /// The unlock argv is one structured element per argument:
    /// `drive <dir> --identity-file … --passphrase-stdin unlock`,
    /// relay pairs and the quota pair after the verb — never a
    /// joined shell string, never `--passphrase-file` (unlock
    /// refuses a file-backed start by construction), never secret
    /// contents (the passphrase rides the stdin pipe).
    #[test]
    fn unlock_argv_is_structured_with_the_pipe_flag() {
        let argv = unlock_argv(&unlock_request()).unwrap();
        let rendered: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "drive",
                "/drives/alpha",
                "--identity-file",
                "/keys/identity",
                "--passphrase-stdin",
                "unlock",
                "--relay",
                "wss://relay.example",
            ]
        );
        // Position pin, not just contents: the parent credential
        // flags must sit before the `unlock` verb (the `leave`
        // precedent) — after the verb clap rejects them.
        let verb = rendered.iter().position(|arg| arg == "unlock").unwrap();
        let identity = rendered
            .iter()
            .position(|arg| arg == "--identity-file")
            .unwrap();
        let pipe = rendered
            .iter()
            .position(|arg| arg == "--passphrase-stdin")
            .unwrap();
        assert!(
            identity < verb && pipe < verb,
            "credential flags precede the verb: {rendered:?}"
        );
        assert!(
            !rendered.iter().any(|arg| arg == "--passphrase-file"),
            "unlock never names a passphrase file: {rendered:?}"
        );
        // Empty relays and no quota emit no flags: the CLI resolves
        // the drive configuration default, like every other
        // networked command.
        let bare = UnlockRequest {
            relays: Vec::new(),
            retained_quota: None,
            ..unlock_request()
        };
        let rendered: Vec<String> = unlock_argv(&bare)
            .unwrap()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            !rendered.contains(&"--relay".to_string()),
            "no relay flags: {rendered:?}"
        );
        let mut quoted = unlock_request();
        quoted.retained_quota = Some(4096);
        let rendered: Vec<String> = unlock_argv(&quoted)
            .unwrap()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(rendered.contains(&"--retained-quota".to_string()));
        assert!(rendered.contains(&"4096".to_string()));
    }

    /// Passphrase pipe-shape pins: empty refuses, overlong refuses,
    /// non-UTF-8 refuses, line breaks refuse (the commit path never
    /// writes one, so a break is a paste error, never a real
    /// passphrase) — all before any process spawns.
    #[test]
    fn unlock_passphrase_validation_fails_closed_before_spawning() {
        assert!(validate_unlock_passphrase(b"hunter2-hunter2").is_ok());
        assert!(validate_unlock_passphrase(b"").is_err());
        assert!(validate_unlock_passphrase(&vec![b'x'; 4097]).is_err());
        assert!(validate_unlock_passphrase(&vec![b'x'; 4096]).is_ok());
        assert!(validate_unlock_passphrase(&[0xff, 0xfe]).is_err());
        assert!(validate_unlock_passphrase(b"two\nlines").is_err());
        assert!(validate_unlock_passphrase(b"trailing-newline\n").is_err());
    }

    /// `spawn_unlock` pipes the passphrase on stdin and holds the
    /// child: the fake CLI (version gate, then drain stdin to a
    /// file, then sleep) proves the pipe carries the exact bytes —
    /// never argv — and the child stays alive past the spawn, so
    /// the session monitor owns the wait.
    #[test]
    fn spawn_unlock_pipes_the_passphrase_and_holds_the_child() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-unlock-pipe",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("wyrd");
        let captured = dir.join("stdin-captured");
        let version = env!("CARGO_PKG_VERSION");
        std::fs::write(
            &cli,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\ncat > {}\nsleep 30\nexit 0\n",
                captured.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut spawned =
            spawn_unlock(&cli, &unlock_request(), b"hunter2-hunter2").expect("spawn holds");
        // The fake drains stdin before sleeping, so the bytes land
        // promptly; the child must still be alive (persistent owner).
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !captured.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            std::fs::read(&captured).unwrap(),
            b"hunter2-hunter2",
            "the pipe carries the exact passphrase bytes"
        );
        assert!(
            spawned.child.try_wait().unwrap().is_none(),
            "the unlock child is held, not reaped"
        );
        let _ = spawned.child.kill();
        let _ = spawned.child.wait();
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// Credential isolation, spawn half: the passphrase crosses to
    /// the unlock owner only as stdin pipe bytes — never argv,
    /// never the environment. The recording fake logs its argv and
    /// environment and captures stdin; the test asserts the secret
    /// appears in exactly one of the three. (Argv shape pins
    /// statically in `unlock_argv_is_structured_with_the_pipe_flag`;
    /// `Debug` redaction pins in
    /// `unlock_args_debug_redacts_the_passphrase` and
    /// `debug_redacts_the_secret` in `commands.rs`/`session.rs`.)
    #[test]
    fn spawn_unlock_confines_the_secret_to_stdin() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-unlock-isolation",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("wyrd");
        let argv_log = dir.join("argv.log");
        let env_log = dir.join("env.log");
        let stdin_captured = dir.join("stdin.bin");
        let version = env!("CARGO_PKG_VERSION");
        std::fs::write(
            &cli,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nprintf '%s\\n' \"$@\" > {}\nenv > {}\ncat > {}\nexit 2\n",
                argv_log.display(),
                env_log.display(),
                stdin_captured.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Distinctive enough that no path, flag, or inherited
        // environment variable can contain it by accident.
        let secret = b"isolation-probe-per-session-9x";
        let mut spawned = spawn_unlock(&cli, &unlock_request(), secret).expect("spawn holds");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !stdin_captured.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let stdin = std::fs::read(&stdin_captured).unwrap();
        let argv = std::fs::read_to_string(&argv_log).unwrap();
        let env = std::fs::read_to_string(&env_log).unwrap();
        assert_eq!(
            stdin, secret,
            "stdin carries the exact secret bytes, nothing else"
        );
        assert!(
            !argv.contains("isolation-probe"),
            "the secret never rides argv: {argv:?}"
        );
        assert!(
            !env.contains("isolation-probe"),
            "the secret never rides the environment"
        );
        let _ = spawned.child.kill();
        let _ = spawned.child.wait();
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// A version-skewed CLI refuses before spawning the unlock argv:
    /// the pipe never opens, the secret never leaves the caller.
    #[test]
    fn spawn_unlock_refuses_a_version_skewed_cli() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-unlock-skew",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("wyrd");
        std::fs::write(
            &cli,
            "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: 9.9.9-mismatch\"\nexit 0\nfi\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        match spawn_unlock(&cli, &unlock_request(), b"hunter2-hunter2") {
            Err(ProvisionResult::VersionMismatch { .. }) => {}
            other => panic!("skew refuses before spawning: {other:?}"),
        }
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }

    /// An invalid passphrase refuses before any process spawns: no
    /// version check runs, no child exists to hold a secret.
    #[test]
    fn spawn_unlock_validates_before_spawning() {
        let missing =
            std::env::temp_dir().join(format!("wyrd-desktop-no-such-cli-{}", std::process::id()));
        match spawn_unlock(&missing, &unlock_request(), b"") {
            Err(ProvisionResult::ExpectedFailure { detail }) => {
                assert!(detail.contains("no passphrase"), "detail: {detail}");
            }
            other => panic!("an empty passphrase refuses first: {other:?}"),
        }
    }

    /// An unlock owner that exits before serving reports its bounded
    /// detail: the drains join past EOF and the exit code survives
    /// for the caller's mapping.
    #[test]
    fn collect_unlock_exit_detail_joins_the_drains() {
        let cli = fake_cli("unlock-exit", "wrong passphrase\n", 2);
        let spawned =
            spawn_unlock(&cli, &unlock_request(), b"wrong-passphrase").expect("spawn holds");
        let (code, detail) = collect_unlock_exit_detail(spawned);
        assert_eq!(code, Some(2));
        assert!(detail.contains("wrong passphrase"), "detail: {detail}");
    }

    /// An unlock owner that closes its stdin before the passphrase
    /// write lands still maps its exit code: the write sees EPIPE, but
    /// the child is handed back for the caller to reap, so exit 78
    /// reports a held drive instead of an unknown outcome. The fake
    /// closes fd 0 explicitly and lingers, making the broken-pipe write
    /// deterministic rather than scheduler-dependent.
    #[test]
    fn spawn_unlock_maps_an_early_exit_that_closed_stdin() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-desktop-fakecli-{}-unlock-early-exit",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("wyrd");
        let version = env!("CARGO_PKG_VERSION");
        std::fs::write(
            &cli,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"__build_info\" ]; then\necho \"update build: {version}\"\nexit 0\nfi\nexec 0<&-\necho \"another process holds the drive\" >&2\nsleep 1\nexit 78\n"
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let spawned =
            spawn_unlock(&cli, &unlock_request(), b"hunter2-hunter2").expect("spawn holds");
        let (code, detail) = collect_unlock_exit_detail(spawned);
        assert_eq!(
            code,
            Some(78),
            "the early exit code is reaped, not killed: {detail}"
        );
        assert!(
            detail.contains("another process holds the drive"),
            "detail: {detail}"
        );
        std::fs::remove_dir_all(&dir).unwrap_or(());
    }
}
