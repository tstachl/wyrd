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

use std::path::PathBuf;
use std::time::Duration;

mod argv;
mod lifecycle;
mod process;
mod unlock;

pub use argv::{
    bootstrap_argv, leave_argv, reconfigure_argv, service_disable_argv, service_enable_argv,
    service_start_argv, unlock_argv, CredentialSource, SeedFiles,
};
pub use lifecycle::{
    run_bootstrap, run_leave, run_reconfigure, run_service_disable, run_service_enable,
    run_service_start,
};
pub use process::{
    bundled_cli_path, check_cli_version, desktop_build_version, parse_build_info, resolve_cli_bin,
};
#[cfg(test)]
pub(crate) use process::{map_exit_output, MAX_CLI_CAPTURE_BYTES};
pub(crate) use process::{run_cli, truncate, CliOutput};
pub use unlock::{
    collect_unlock_exit_detail, spawn_unlock, validate_unlock_passphrase, SpawnedUnlock,
    UnlockRequest, MAX_UNLOCK_PASSPHRASE_BYTES, UNLOCK_VERIFY_TIMEOUT,
};

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

#[cfg(test)]
#[path = "../tests_provision.rs"]
mod tests_provision;
