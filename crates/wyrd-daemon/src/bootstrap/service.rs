//! OS service-definition rendering and supervision: launchd on
//! macOS, systemd on Linux. The parent module owns the authority
//! contract; registry validation and error taxonomy come from
//! [`super::registry`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::registry::{
    restrict_owner_only, sync_file, validate_entry, BootstrapError, RegistryEntry, ServiceMode,
};

/// XML-escape one plist string value. Paths, relays, and binary
/// locations are operator input; the template must not break or
/// reinterpret them.
fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Shell-quote one systemd `ExecStart` argument. systemd splits the
/// line itself, so arguments with spaces, quotes, or backslashes are
/// double-quoted with backslash escapes. `%` is doubled first:
/// systemd expands bare specifiers (`%i`, `%n`, …) even inside
/// quotes, and a relay or path must never become a specifier.
fn systemd_quote(arg: &str) -> String {
    let escaped = arg.replace('%', "%%");
    if escaped
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_./:=,".contains(&byte))
    {
        return escaped;
    }
    let mut quoted = String::with_capacity(escaped.len() + 2);
    quoted.push('"');
    for ch in escaped.chars() {
        if ch == '"' || ch == '\\' {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

/// The supervised owner command: the `wyrd-supervise` wrapper from
/// `nix/drive` (OD-34-C) around `wyrd owner start` or `wyrd mount` with
/// file credentials. Managers cannot distinguish the binary's
/// collapsed exit codes on their own — every CLI failure is 2 except
/// the terminal held-drive 78 — so the wrapper maps the wait status
/// onto the exit the supervisor's policy wants (restart-worthy per
/// manager, 78 never). Routing through the wrapper is what makes the
/// restart policy below truthful; a bare owner binary cannot express
/// What credential the supervised owner starts with: exactly one
/// backend per drive, carried as a reference (paths or the store
/// account label) — never secret bytes. The service definition
/// renders this verbatim, so the manager starts the backend the
/// drive is configured for; ambiguity between backends is a
/// construction-time refusal, never a precedence rule. Owner and
/// mount share the shape: both startup paths select identically.
#[derive(Debug, Clone)]
pub enum ServiceCredential {
    /// Hardened credential files, resolved the way
    /// `load_credentials` resolves them. Paths, never secrets,
    /// travel here.
    File {
        /// Identity credential file named by the service definition.
        identity_file: PathBuf,
        /// Passphrase credential file named by the service
        /// definition. `None` means session-supplied (prompt or
        /// stdin pipe): no managed definition may render it, so a
        /// managed run with `None` refuses at install time instead
        /// of writing a definition the manager could never start.
        /// `no_service` registrations carry `None` freely — that is
        /// the per-session posture.
        passphrase_file: Option<PathBuf>,
    },
    /// One item in the OS credential store, named by its account
    /// label (the reference, never a secret). The supervised owner
    /// resolves it through the loader boundary at start — no files
    /// on disk, no pipe, no prompt.
    OsStore {
        /// The store account label naming the drive's item.
        account: String,
    },
}

/// The supervised owner (or mount) command the service manager
/// runs: wrapper, restart policy, posture path, then the owner or
/// mount command. Owner and mount share the credential and relay
/// flags; mount adds its mountpoint and serves the mount posture
/// sidecar — the restart contract is identical, since a held mount
/// is terminal the same way a held vault is.
#[derive(Debug, Clone)]
pub struct SupervisedOwner {
    /// `wyrd-supervise` (`nix/drive/supervise.py`): same installer
    /// layout as the `wyrd` binary, overridable per command.
    pub supervise_bin: PathBuf,
    /// The `wyrd` binary whose `vault` or `mount` subcommand serves
    /// the drive and binds the interface socket.
    pub wyrd_bin: PathBuf,
    /// Which credential the supervised command starts with: file
    /// paths or the store reference. Paths here name the same files
    /// (or the same store item) the operator already keeps.
    pub credential: ServiceCredential,
}

impl SupervisedOwner {
    /// Append the credential selection to a supervised argv: file
    /// backends name both files, store backends name the backend and
    /// the account. Exactly one backend renders — a store drive never
    /// carries file flags, a file drive never carries store flags.
    fn push_credential_argv(&self, argv: &mut Vec<String>) {
        match &self.credential {
            ServiceCredential::File {
                identity_file,
                passphrase_file,
            } => {
                argv.push("--identity-file".to_string());
                argv.push(identity_file.to_string_lossy().into_owned());
                argv.push("--passphrase-file".to_string());
                argv.push(
                    passphrase_file
                        .as_ref()
                        .expect("managed file credential carries a passphrase file: validated at install")
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            ServiceCredential::OsStore { account } => {
                argv.push("--credential-backend".to_string());
                argv.push("os-store".to_string());
                argv.push("--credential-account".to_string());
                argv.push(account.clone());
            }
        }
    }

    /// The full supervised argv: wrapper, per-manager restart
    /// status, posture path, `--` separator, then the owner or mount
    /// command. Owner and mount share the credential and relay flags;
    /// mount adds its mountpoint and serves the mount posture sidecar.
    pub fn argv(&self, entry: &RegistryEntry, restart_status: &str) -> Vec<String> {
        let posture = match entry.mode {
            ServiceMode::Mount => "mount.posture",
            _ => "vault.posture",
        };
        let mut argv = vec![
            self.supervise_bin.to_string_lossy().into_owned(),
            "--restart-status".to_string(),
            restart_status.to_string(),
            "--posture-path".to_string(),
            entry.drive_dir.join(posture).to_string_lossy().into_owned(),
            "--".to_string(),
            self.wyrd_bin.to_string_lossy().into_owned(),
        ];
        match entry.mode {
            ServiceMode::Mount => {
                argv.push("mount".to_string());
                argv.push(entry.drive_dir.to_string_lossy().into_owned());
                argv.push(
                    entry
                        .mountpoint
                        .as_ref()
                        .expect("mount entry carries a mountpoint: validate before argv")
                        .to_string_lossy()
                        .into_owned(),
                );
                self.push_credential_argv(&mut argv);
                for relay in &entry.relays {
                    argv.push("--relay".to_string());
                    argv.push(relay.clone());
                }
            }
            _ => {
                // Parent flags precede the subcommand: clap parses
                // `owner <dir> [flags] start`, never flags after `start`.
                argv.push("owner".to_string());
                argv.push(entry.drive_dir.to_string_lossy().into_owned());
                self.push_credential_argv(&mut argv);
                for relay in &entry.relays {
                    argv.push("--relay".to_string());
                    argv.push(relay.clone());
                }
                argv.push("start".to_string());
            }
        }
        argv
    }
}

/// Render the per-drive launchd agent property list. `KeepAlive`
/// with `SuccessfulExit` restarts the wrapper's exit 0 (unexpected
/// death mapped restart-worthy) and never its 78 (terminal held
/// drive) — the `nix/drive` semantics, which only hold because the
/// wrapper normalizes the child's collapsed codes first.
pub fn render_launchd_plist(label: &str, owner: &SupervisedOwner, entry: &RegistryEntry) -> String {
    let mut argv = String::new();
    for arg in owner.argv(entry, "0") {
        argv.push_str(&format!("    <string>{}</string>\n", xml_escape(&arg)));
    }
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
            "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
            "<plist version=\"1.0\">\n",
            "<dict>\n",
            "    <key>Label</key>\n",
            "    <string>{label}</string>\n",
            "    <key>ProgramArguments</key>\n",
            "    <array>\n{argv}",
            "    </array>\n",
            "    <key>WorkingDirectory</key>\n",
            "    <string>/</string>\n",
            "    <key>RunAtLoad</key>\n",
            "    <true/>\n",
            "    <key>KeepAlive</key>\n",
            "    <dict>\n",
            "        <key>SuccessfulExit</key>\n",
            "        <true/>\n",
            "    </dict>\n",
            "    <key>ThrottleInterval</key>\n",
            "    <integer>30</integer>\n",
            "    <key>StandardOutPath</key>\n",
            "    <string>{log}</string>\n",
            "    <key>StandardErrorPath</key>\n",
            "    <string>{log}</string>\n",
            "</dict>\n",
            "</plist>\n"
        ),
        label = xml_escape(label),
        argv = argv,
        log = xml_escape(&entry.drive_dir.join("service.log").to_string_lossy()),
    )
}

/// Render the per-drive systemd user unit. `Restart=on-failure`
/// with `RestartPreventExitStatus=78` mirrors the `nix/drive`
/// contract against the wrapper's normalized statuses: unexpected
/// death restarts rate-limited, the terminal held drive stands down.
pub fn render_systemd_unit(
    description: &str,
    owner: &SupervisedOwner,
    entry: &RegistryEntry,
) -> String {
    let exec = owner
        .argv(entry, "1")
        .iter()
        .map(|arg| systemd_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        concat!(
            "[Unit]\n",
            "Description={description}\n",
            "After=network-online.target\n",
            "Wants=network-online.target\n\n",
            "[Service]\n",
            "Type=simple\n",
            "WorkingDirectory=/\n",
            "ExecStart={exec}\n",
            "Restart=on-failure\n",
            "RestartPreventExitStatus=78\n",
            "RestartSec=5\n",
            "StartLimitIntervalSec=300\n",
            "StartLimitBurst=3\n\n",
            "[Install]\n",
            "WantedBy=default.target\n"
        ),
        description = description,
        exec = exec,
    )
}

/// Where the per-drive service definition lives on this
/// platform: the launchd agent property list on macOS, the systemd
/// user unit elsewhere. Shared by installation (write it) and
/// reconfiguration (stop and clear it): both must name the same
/// file, or a reconfigure would disable one definition while the
/// manager loads another.
#[cfg(unix)]
pub(super) fn service_definition_path(entry: &RegistryEntry) -> PathBuf {
    service_definition_path_for_service(&entry.service)
}

/// Platform definition path for one service token. `pub` (hidden)
/// for the desktop mirror test: `wyrd-desktop` duplicates this
/// derivation for observation-only probes and cannot depend on this
/// crate, so the integration test pins both sides to the same path
/// instead of letting them drift silently.
#[doc(hidden)]
pub fn service_definition_path_for_service(service: &str) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("Library/LaunchAgents");
        let label = format!("md.enesis.wyrd.{service}");
        dir.join(format!("{label}.plist"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join(".config")
            })
            .join("systemd/user");
        dir.join(format!("wyrd-{service}.service"))
    }
}

/// Install the per-drive service definition for this platform and
/// return its path. Writes atomically with owner-only permissions:
/// a half-written unit must never be what the manager loads.
/// Installation writes the definition; enabling and starting are
/// separate steps so registration can report exactly which one
/// failed.
///
/// `definition_override` names the file directly (tests); `None`
/// resolves the platform path. The reconfigure override covers
/// clearing and installing together, so the two can never name
/// different files.
#[cfg(unix)]
pub fn install_service(
    entry: &RegistryEntry,
    owner: &SupervisedOwner,
    definition_override: Option<&PathBuf>,
) -> Result<PathBuf, BootstrapError> {
    // Managed modes only: the template renders `vault` and `mount`
    // argv, so an unknown future mode or a mismatched mountpoint
    // refuses here instead of rendering a definition the manager
    // would run deaf. Provisioning and reconfiguration validate
    // before installing, so this is the backstop for direct callers.
    validate_entry(&entry.name, entry)?;
    let path = match definition_override {
        Some(path) => path.clone(),
        None => service_definition_path(entry),
    };
    let dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    std::fs::create_dir_all(&dir).map_err(|source| BootstrapError::CreateDir {
        path: dir.clone(),
        source,
    })?;
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{}", entry.service);
        write_service_file(&path, &render_launchd_plist(&label, owner, entry))?;
        Ok(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        write_service_file(
            &path,
            &render_systemd_unit(&format!("Wyrd drive {}", entry.name), owner, entry),
        )?;
        Ok(path)
    }
}

/// Atomic owner-only service write: temporary sibling, sync,
/// replace — the manager never loads a half-written definition.
#[cfg(unix)]
pub(super) fn write_service_file(path: &Path, contents: &str) -> Result<(), BootstrapError> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, contents).map_err(|source| BootstrapError::WriteFile {
        path: tmp.clone(),
        source,
    })?;
    restrict_owner_only(&tmp)?;
    sync_file(&tmp)?;
    std::fs::rename(&tmp, path).map_err(|source| BootstrapError::ReplaceFile {
        path: path.to_path_buf(),
        source,
    })?;
    restrict_owner_only(path)
}

#[cfg(not(unix))]
pub fn install_service(
    _entry: &RegistryEntry,
    _owner: &SupervisedOwner,
    _definition_override: Option<&PathBuf>,
) -> Result<PathBuf, BootstrapError> {
    Err(BootstrapError::ServicesUnsupported)
}

/// Enable the installed service for startup-at-login. Separate
/// from installation and from starting, so each failure reports its
/// own stage. On macOS `load -w` loads now and starts the agent, so
/// enablement is the start; re-enabling unloads first, which
/// restarts a running service rather than duplicating it. Disabling
/// later is the reconfigure stop path (`launchctl unload -w` plus
/// definition removal / `systemctl --user disable --now` plus
/// removal) — the documented stop, distinct from a crash the manager
/// retries.
#[cfg(unix)]
pub fn enable_service(
    entry: &RegistryEntry,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] definition: &Path,
) -> Result<(), BootstrapError> {
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{}", entry.service);
        // Unload-then-load: a disabled leftover re-enables rather
        // than duplicating, and a running service restarts on the
        // new definition. Errors from the unload are ignored — the
        // service may never have loaded — but the load must succeed.
        let _ = run("launchctl", &["unload", &definition.to_string_lossy()]);
        run("launchctl", &["load", "-w", &definition.to_string_lossy()]).map_err(|source| {
            BootstrapError::EnableService {
                name: label,
                source: Box::new(source),
            }
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Session bound first: a lingering user manager would let
        // the enabled unit outlive logout. Refusal, never a silent
        // configuration change.
        check_session_bound()?;
        let unit = format!("wyrd-{}.service", entry.service);
        run("systemctl", &["--user", "daemon-reload"]).map_err(|source| {
            BootstrapError::ReloadUnits {
                source: Box::new(source),
            }
        })?;
        run("systemctl", &["--user", "enable", &unit]).map_err(|source| {
            BootstrapError::EnableService {
                name: unit,
                source: Box::new(source),
            }
        })
    }
}

/// Start the enabled service now. On macOS enablement already
/// started the agent, so this only verifies it is loaded — a second
/// start path would double-start. On systemd enablement persists
/// without starting, so this starts explicitly.
#[cfg(unix)]
pub fn start_service(entry: &RegistryEntry) -> Result<(), BootstrapError> {
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{}", entry.service);
        run("launchctl", &["list", &label]).map_err(|source| BootstrapError::AgentNotLoaded {
            label,
            source: Box::new(source),
        })?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = format!("wyrd-{}.service", entry.service);
        // Restart, not start: enablement leaves a running unit
        // alone, and a re-run that changed relays or credential
        // paths must bring the new definition live instead of
        // verifying the still-running old owner.
        run("systemctl", &["--user", "restart", &unit]).map_err(|source| {
            BootstrapError::RestartService {
                unit,
                source: Box::new(source),
            }
        })
    }
}

/// Persistent login-start preference for one drive: whether the
/// service manager starts the unit at login. Independent of
/// whether the unit runs now. `Unknown` is the no-answer verdict
/// (missing tool, unparseable answer, no macOS record) — never
/// `Disabled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceEnabled {
    /// The manager starts the unit at login.
    Enabled,
    /// The manager does not start the unit at login.
    Disabled,
    /// The manager did not answer, or keeps no record: retry the
    /// probe. Not evidence about login behavior.
    Unknown,
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn current_uid() -> u32 {
    // SAFETY: `geteuid` takes no arguments and returns the
    // caller's effective uid; it cannot fail and touches no memory.
    unsafe { libc::geteuid() }
}

/// Parse `loginctl show-user -p Linger --value`: `yes` means the
/// user manager lingers past logout. Pure over the command output
/// so tests pin the mapping without touching a manager. Compiled
/// on Linux (where the session-bound probe uses it) and in tests
/// everywhere.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
pub(super) fn parse_linger_state(output: &str) -> Result<bool, String> {
    match output.trim().to_ascii_lowercase().as_str() {
        "yes" => Ok(true),
        "no" => Ok(false),
        other => Err(format!(
            "loginctl answered {other:?}: expected \"yes\" or \"no\""
        )),
    }
}

/// Linux session bound: lingering keeps the user manager (and its
/// units) alive after logout, which would let an owner outlive the
/// login session. Refuses where lingering is enabled or its state
/// cannot be determined — never silently disabling it, never
/// treating Unknown as safe.
///
/// Checked at every enable and start; a change while the owner
/// runs is caught at the next manager interaction, not monitored
/// in between (no D-Bus polling in the owner loop). Install-only
/// and disable paths skip it: nothing persists at login there.
/// Non-systemd hosts skip it: no user manager exists to linger,
/// and `systemctl` reports its own errors downstream. macOS needs
/// no probe: installation is user-agent-only by construction.
///
/// Scope: Wyrd-managed service definitions are user-scoped
/// desktop lifecycle infrastructure (launchd user agents, systemd
/// user units) — Wyrd ships no system-scope service. Server
/// deployments register with `--no-service` and supervise
/// `wyrd owner start` independently; they never pass through the
/// managed enable/start paths, so this refusal does not apply to
/// them. The boundary follows the supervision path, never the
/// credential backend: file-backed desktop drives share it.
#[cfg(all(unix, not(target_os = "macos")))]
fn check_session_bound() -> Result<(), BootstrapError> {
    if !std::path::Path::new("/run/systemd/system").exists() {
        return Ok(());
    }
    let uid = current_uid();
    let uid_text = uid.to_string();
    match probe_success_output(
        "loginctl",
        &["show-user", &uid_text, "-p", "Linger", "--value"],
    ) {
        None => Err(BootstrapError::LingerUnknown {
            uid,
            // No cause is claimed beyond "no usable answer": the
            // probe collapses a missing tool, a failed command, and
            // a wedged-manager overrun into one refusal, and the
            // bound derives from PROBE_TIMEOUT rather than a
            // literal so the two cannot drift.
            detail: format!(
                "loginctl gave no usable answer within {:?} (missing, failed, or timed out)",
                PROBE_TIMEOUT
            ),
        }),
        Some(out) => match parse_linger_state(&out) {
            Ok(true) => Err(BootstrapError::LingeringEnabled { uid }),
            Ok(false) => Ok(()),
            Err(detail) => Err(BootstrapError::LingerUnknown { uid, detail }),
        },
    }
}

/// How long one manager output probe may run. A status answer
/// is instant; the bound only fires on a wedged manager, and
/// expiry reads as no answer — never an enablement state.
#[cfg(unix)]
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Run one manager probe and capture its stdout. The exit status is
/// ignored here: `is-enabled` exits non-zero for a disabled unit
/// and `print-disabled` callers parse the record, so only a spawn
/// failure (missing tool) or an overrun past [`PROBE_TIMEOUT`] is
/// no output. Callers map unparseable output to `Unknown`, never
/// to a lifecycle state.
#[cfg(unix)]
pub(super) fn probe_output(program: &str, args: &[&str]) -> Option<String> {
    use std::time::Instant;
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let mut stdout = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    use std::io::Read as _;
                    let _ = pipe.read_to_string(&mut stdout);
                }
                let _ = child.wait();
                return Some(stdout);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Run one manager probe and capture stdout only on exit success.
/// Unlike [`probe_output`] — which deliberately ignores exit status
/// because `is-enabled` exits non-zero for a disabled unit — a
/// failed `loginctl` with plausible stdout must never parse as a
/// safe answer. Spawn failure, non-zero exit, wedged-manager
/// overrun past [`PROBE_TIMEOUT`], and wait errors all read as no
/// answer; only exit-success stdout reaches `parse_linger_state`.
#[cfg(all(unix, not(target_os = "macos")))]
fn probe_success_output(program: &str, args: &[&str]) -> Option<String> {
    use std::time::Instant;
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    drop(child.stdout.take());
                    let _ = child.wait();
                    return None;
                }
                let mut stdout = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    use std::io::Read as _;
                    let _ = pipe.read_to_string(&mut stdout);
                }
                let _ = child.wait();
                return Some(stdout);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Query one drive's persistent login-start preference without
/// changing anything: no load, no unload, no record write. macOS
/// reads the `print-disabled` record for the label (a present
/// record maps to its value; an absent record is `Unknown` —
/// managed drives provisioned by `load -w` always carry one, so
/// absence means the record was never written or was externally
/// removed, never a license to claim `Disabled`). Linux reads the
/// `is-enabled` word (`enabled`/`disabled`/`masked` decide; a bus
/// failure prints no word and reads `Unknown`, never `Disabled`).
#[cfg(unix)]
pub fn service_enabled(entry: &RegistryEntry) -> ServiceEnabled {
    #[cfg(target_os = "macos")]
    {
        let domain = format!("user/{}", current_uid());
        match probe_output("launchctl", &["print-disabled", &domain]) {
            Some(output) => parse_print_disabled(&output, &entry.service),
            None => ServiceEnabled::Unknown,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = format!("wyrd-{}.service", entry.service);
        match probe_output("systemctl", &["--user", "is-enabled", &unit]) {
            Some(output) => parse_is_enabled(&output),
            None => ServiceEnabled::Unknown,
        }
    }
}

/// Map a `launchctl print-disabled` dump onto one drive's record:
/// a present `"label" => enabled|disabled` line decides; an absent
/// record is `Unknown` (managed drives provisioned by `load -w`
/// always carry one, so absence means the record was never written
/// or was externally removed — never a license to claim
/// `Disabled`). The label match includes the closing quote so
/// `drive-foo` never matches a `drive-foo-bar` record.
///
/// Mirror: the desktop holds the same parser beside its own probe
/// (`wyrd-desktop/src/service.rs`) — the desktop cannot depend on
/// this crate, so the two copies evolve together by contract.
/// The Unknown-on-absent/garbage rule is the shared invariant.
#[cfg(any(test, target_os = "macos"))]
pub(super) fn parse_print_disabled(output: &str, service: &str) -> ServiceEnabled {
    let prefix = format!("\"md.enesis.wyrd.{service}\" => ");
    for line in output.lines() {
        if let Some(value) = line.trim().strip_prefix(prefix.as_str()) {
            return match value.trim() {
                "enabled" => ServiceEnabled::Enabled,
                "disabled" => ServiceEnabled::Disabled,
                _ => ServiceEnabled::Unknown,
            };
        }
    }
    ServiceEnabled::Unknown
}

/// Map a `systemctl is-enabled` word onto the preference:
/// `enabled`/`disabled`/`masked` decide; any other word (static,
/// alias, indirect) or a bus failure printing no word reads
/// `Unknown`, never `Disabled`.
///
/// Mirror: the desktop holds the same word map beside its own probe
/// (`wyrd-desktop/src/service.rs`) — same layering reason as above;
/// the word map is the shared invariant.
#[cfg(any(test, not(target_os = "macos")))]
pub(super) fn parse_is_enabled(output: &str) -> ServiceEnabled {
    match output.trim() {
        "enabled" => ServiceEnabled::Enabled,
        "disabled" | "masked" => ServiceEnabled::Disabled,
        _ => ServiceEnabled::Unknown,
    }
}

/// No manager verdict off Unix: the probe cannot answer where no
/// Unix manager exists.
#[cfg(not(unix))]
pub fn service_enabled(_entry: &RegistryEntry) -> ServiceEnabled {
    ServiceEnabled::Unknown
}

/// Confirm the persisted preference after a toggle: the record
/// must read back the requested value. Exit codes lie on no-op
/// manager paths (`load` on a disabled job exits 0 without
/// loading; `unload -w` on a stopped job exits 0 with an error on
/// stderr), so the record decides, never the exit code alone.
#[cfg(unix)]
fn verify_service_enabled(entry: &RegistryEntry, enabled: bool) -> Result<(), BootstrapError> {
    let want = if enabled {
        ServiceEnabled::Enabled
    } else {
        ServiceEnabled::Disabled
    };
    if service_enabled(entry) != want {
        let detail = format!(
            "login-start record still reads the old value after the toggle (want {want:?})"
        );
        let probe = if cfg!(target_os = "macos") {
            ("launchctl", "print-disabled")
        } else {
            ("systemctl", "is-enabled")
        };
        let wrap = |source: BootstrapError| {
            if enabled {
                BootstrapError::EnableService {
                    name: entry.service.clone(),
                    source: Box::new(source),
                }
            } else {
                BootstrapError::StopService {
                    name: entry.service.clone(),
                    source: Box::new(source),
                }
            }
        };
        return Err(wrap(BootstrapError::CommandFailed {
            program: probe.0.to_string(),
            args: probe.1.to_string(),
            detail,
        }));
    }
    Ok(())
}

/// Persist one drive's login-start preference without rewriting
/// the definition: enable records login start without starting
/// anything; disable stops the unit now and records no login
/// start. The definition file stays either way — unlike
/// [`disable_service`], which clears it for reconfiguration.
/// Verifies the record (and, for disable, the stopped state)
/// before returning: the toggle reports the verified preference,
/// never the manager's exit code.
#[cfg(unix)]
pub fn set_service_enabled(
    entry: &RegistryEntry,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] definition: &Path,
    enabled: bool,
) -> Result<(), BootstrapError> {
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{}", entry.service);
        if enabled {
            // Persist-only: `enable` writes the record without
            // loading — a stopped job stays stopped, a running job
            // keeps running. Enablement never starts anything.
            run(
                "launchctl",
                &["enable", &format!("user/{}/{}", current_uid(), label)],
            )
            .map_err(|source| BootstrapError::EnableService {
                name: label.clone(),
                source: Box::new(source),
            })?;
        } else {
            // One op stops and persists: `unload -w` unloads the
            // job (a no-op on an already-stopped one — exit 0 with
            // an error on stderr, which the verify below absorbs)
            // and records disabled, keeping the plist file.
            run(
                "launchctl",
                &["unload", "-w", &definition.to_string_lossy()],
            )
            .map_err(|source| BootstrapError::StopService {
                name: label.clone(),
                source: Box::new(source),
            })?;
        }
        verify_service_enabled(entry, enabled)?;
        if !enabled && run("launchctl", &["list", &label]).is_ok() {
            // Disable stops: a succeeding `list` means the unit is
            // still loaded — the record alone does not prove the
            // stop stuck.
            return Err(BootstrapError::StopService {
                name: label.clone(),
                source: Box::new(BootstrapError::CommandFailed {
                    program: "launchctl".to_string(),
                    args: format!("list {label}"),
                    detail: "unit still loaded after disable".to_string(),
                }),
            });
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = format!("wyrd-{}.service", entry.service);
        if enabled {
            // The definition is unchanged, so no reload: enable
            // only records login start, never starts the unit.
            // The session bound applies to recording login start
            // exactly as to enabling at provision time.
            check_session_bound()?;
            run("systemctl", &["--user", "enable", &unit]).map_err(|source| {
                BootstrapError::EnableService {
                    name: unit.clone(),
                    source: Box::new(source),
                }
            })?;
        } else {
            // `disable --now` stops a running unit and persists
            // disabled in one op; the definition file stays.
            run("systemctl", &["--user", "disable", "--now", &unit]).map_err(|source| {
                BootstrapError::StopService {
                    name: unit.clone(),
                    source: Box::new(source),
                }
            })?;
        }
        verify_service_enabled(entry, enabled)?;
        if !enabled {
            // Disable stops: confirm the unit is inactive.
            if run("systemctl", &["--user", "is-active", "--quiet", &unit]).is_ok() {
                return Err(BootstrapError::StopService {
                    name: unit.clone(),
                    source: Box::new(BootstrapError::CommandFailed {
                        program: "systemctl".to_string(),
                        args: format!("is-active {unit}"),
                        detail: "unit still active after disable".to_string(),
                    }),
                });
            }
        }
        Ok(())
    }
}

/// Persisting login start needs a Unix manager.
#[cfg(not(unix))]
pub fn set_service_enabled(
    _entry: &RegistryEntry,
    _definition: &Path,
    _enabled: bool,
) -> Result<(), BootstrapError> {
    Err(BootstrapError::ServicesUnsupported)
}

/// Start the installed service now without touching its
/// login-start preference: the manual-Start path. An already
/// loaded job verifies in place; an enabled-or-unrecorded job
/// loads without touching the record; a disabled job loads
/// through a temporary enable that is re-persisted immediately
/// after — verified loaded AND disabled before returning, so a
/// completed start never flips the toggle. Linux restarts (a
/// re-run with a changed definition brings the new owner live
/// instead of verifying the still-running old one).
#[cfg(unix)]
pub fn start_service_only(
    entry: &RegistryEntry,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] definition: &Path,
) -> Result<(), BootstrapError> {
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{}", entry.service);
        if run("launchctl", &["list", &label]).is_ok() {
            return Ok(());
        }
        if service_enabled(entry) == ServiceEnabled::Disabled {
            // Plain `load` refuses a disabled job, so the record is
            // lifted for the load and restored right after — the
            // running job survives the re-disable, verified below.
            // Best-effort ordering (disabled restored last): the
            // completed command always leaves the toggle as found.
            run("launchctl", &["load", "-w", &definition.to_string_lossy()]).map_err(|source| {
                BootstrapError::AgentNotLoaded {
                    label: label.clone(),
                    source: Box::new(source),
                }
            })?;
            verify_loaded_macos(&label)?;
            run(
                "launchctl",
                &["disable", &format!("user/{}/{}", current_uid(), label)],
            )
            .map_err(|source| BootstrapError::AgentNotLoaded {
                label: label.clone(),
                source: Box::new(source),
            })?;
            verify_loaded_macos(&label)?;
            verify_service_enabled(entry, false).map_err(|source| {
                BootstrapError::AgentNotLoaded {
                    label: label.clone(),
                    source: Box::new(source),
                }
            })?;
            return Ok(());
        }
        run("launchctl", &["load", &definition.to_string_lossy()]).map_err(|source| {
            BootstrapError::AgentNotLoaded {
                label: label.clone(),
                source: Box::new(source),
            }
        })?;
        verify_loaded_macos(&label)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Session bound on every start, not just enable: lingering
        // enabled after the fact is caught here, at the next
        // manager interaction — never monitored in between.
        check_session_bound()?;
        start_service(entry)
    }
}

/// Confirm a macOS agent is loaded after a start: `list` exiting
/// non-zero is the not-loaded verdict (a missing job and a
/// failed load read the same — neither is running).
#[cfg(all(unix, target_os = "macos"))]
fn verify_loaded_macos(label: &str) -> Result<(), BootstrapError> {
    run("launchctl", &["list", label]).map_err(|source| BootstrapError::AgentNotLoaded {
        label: label.to_string(),
        source: Box::new(source),
    })?;
    Ok(())
}

/// Starting needs a Unix manager.
#[cfg(not(unix))]
pub fn start_service_only(
    _entry: &RegistryEntry,
    _definition: &Path,
) -> Result<(), BootstrapError> {
    Err(BootstrapError::ServicesUnsupported)
}

/// Stop one drive's managed service without changing its login-start
/// preference. Returns whether a running unit was actually stopped, so
/// the caller can resume exactly what it stopped when the operation is
/// refused. Unlike [`set_service_enabled`] with `enabled = false`, this
/// uses a plain unload/stop: no disabled record is persisted, and
/// [`start_service_only`] restores the running state as found.
#[cfg(unix)]
pub fn stop_service_only(
    entry: &RegistryEntry,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] definition: &Path,
) -> Result<bool, BootstrapError> {
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{}", entry.service);
        if run("launchctl", &["list", &label]).is_err() {
            return Ok(false);
        }
        run("launchctl", &["unload", &definition.to_string_lossy()]).map_err(|source| {
            BootstrapError::StopService {
                name: label.clone(),
                source: Box::new(source),
            }
        })?;
        if run("launchctl", &["list", &label]).is_ok() {
            let args = format!("list {label}");
            return Err(BootstrapError::StopService {
                name: label,
                source: Box::new(BootstrapError::CommandFailed {
                    program: "launchctl".to_string(),
                    args,
                    detail: "unit still loaded after stop".to_string(),
                }),
            });
        }
        Ok(true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = format!("wyrd-{}.service", entry.service);
        if run("systemctl", &["--user", "is-active", "--quiet", &unit]).is_err() {
            return Ok(false);
        }
        run("systemctl", &["--user", "stop", &unit]).map_err(|source| {
            BootstrapError::StopService {
                name: unit.clone(),
                source: Box::new(source),
            }
        })?;
        if run("systemctl", &["--user", "is-active", "--quiet", &unit]).is_ok() {
            let args = format!("--user is-active --quiet {unit}");
            return Err(BootstrapError::StopService {
                name: unit,
                source: Box::new(BootstrapError::CommandFailed {
                    program: "systemctl".to_string(),
                    args,
                    detail: "unit still active after stop".to_string(),
                }),
            });
        }
        Ok(true)
    }
}

/// Stopping needs a Unix manager.
#[cfg(not(unix))]
pub fn stop_service_only(
    _entry: &RegistryEntry,
    _definition: &Path,
) -> Result<bool, BootstrapError> {
    Err(BootstrapError::ServicesUnsupported)
}

/// Stop and clear the per-drive service definition before a
/// reconfiguration: disable the manager unit (or unload the agent)
/// so a restart cannot bring the old definition back over the new
/// registry entry, then remove the definition file so no login or
/// reload resurrects it. A missing definition file means the drive
/// was never managed on this machine — nothing to clear, not an
/// error. Runs before the registry rewrite, so a failure stops with
/// the old entry still standing; every later failure is retried by
/// re-running, which converges from the stopped-and-cleared state.
///
/// `definition_override` names the definition file directly
/// (tests); `None` resolves the platform path. It mirrors the
/// registry/binary overrides the provisioning config already
/// carries, so hermetic tests never touch a developer's real
/// service definitions.
#[cfg(unix)]
pub fn disable_service(
    entry: &RegistryEntry,
    definition_override: Option<&PathBuf>,
) -> Result<(), BootstrapError> {
    let owned;
    let definition = match definition_override {
        Some(path) => path.as_path(),
        None => {
            owned = service_definition_path(entry);
            &owned
        }
    };
    if !definition.exists() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{}", entry.service);
        // Not loaded means nothing to stop: the unload below would
        // fail on the absent job, and that absence is expected (a
        // never-managed drive, or this operation's own earlier
        // pass), never a stop failure.
        if run("launchctl", &["list", &label]).is_ok() {
            // `-w` persists the disable like the `load -w` the
            // enable path uses; without it the `RunAtLoad` plist
            // would load again at the next login.
            run(
                "launchctl",
                &["unload", "-w", &definition.to_string_lossy()],
            )
            .map_err(|source| BootstrapError::StopService {
                name: label.clone(),
                source: Box::new(source),
            })?;
        }
        clear_definition(definition, &label)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = format!("wyrd-{}.service", entry.service);
        // Only an enabled unit needs the manager's disable, and only
        // an active unit needs its stop: on a never-enabled,
        // never-active unit both would fail on the unknown name, and
        // that absence is expected (a never-managed drive, or this
        // operation's own earlier pass), never a stop failure. An
        // active-but-disabled unit is stopped but not disabled. The
        // definition file below is removed either way.
        let enabled = run("systemctl", &["--user", "is-enabled", "--quiet", &unit]).is_ok();
        let active = run("systemctl", &["--user", "is-active", "--quiet", &unit]).is_ok();
        if active {
            // Stop the running unit; the enablement symlink is
            // handled separately below.
            run("systemctl", &["--user", "stop", &unit]).map_err(|source| {
                BootstrapError::StopService {
                    name: unit.clone(),
                    source: Box::new(source),
                }
            })?;
        }
        if enabled {
            run("systemctl", &["--user", "disable", &unit]).map_err(|source| {
                BootstrapError::StopService {
                    name: unit.clone(),
                    source: Box::new(source),
                }
            })?;
        }
        clear_definition(definition, &unit)?;
        if enabled || active {
            // The manager drops the removed unit on its next reload;
            // reload after the removal (not before) so a stale
            // in-memory unit cannot outlive the registry rewrite below.
            run("systemctl", &["--user", "daemon-reload"]).map_err(|source| {
                BootstrapError::StopService {
                    name: unit,
                    source: Box::new(source),
                }
            })?;
        }
        Ok(())
    }
}

/// Remove the service definition file after a successful stop. A
/// missing file is not an error (already cleared, or a concurrent
/// pass got there first — the re-run converges either way).
#[cfg(unix)]
fn clear_definition(definition: &Path, name: &str) -> Result<(), BootstrapError> {
    match std::fs::remove_file(definition) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(BootstrapError::StopService {
            name: name.to_string(),
            source: Box::new(BootstrapError::RemoveFile {
                path: definition.to_path_buf(),
                source,
            }),
        }),
    }
}

#[cfg(not(unix))]
pub fn disable_service(
    _entry: &RegistryEntry,
    definition_override: Option<&PathBuf>,
) -> Result<(), BootstrapError> {
    // Mirror the Unix early-out so a never-managed drive
    // reconfigures anywhere; a present definition needs a manager
    // this platform does not have.
    if let Some(path) = definition_override {
        if !path.exists() {
            return Ok(());
        }
    }
    Err(BootstrapError::ServicesUnsupported)
}

#[cfg(unix)]
pub(super) fn run(program: &str, args: &[&str]) -> Result<(), BootstrapError> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|source| BootstrapError::Spawn {
            program: program.to_string(),
            source,
        })?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    Err(BootstrapError::CommandFailed {
        program: program.to_string(),
        args: args.join(" "),
        detail: detail.trim().to_string(),
    })
}
