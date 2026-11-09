//! First-drive provisioning: the per-user drive registry, the
//! OS service handoff, and the fixed initialize-then-register order.
//!
//! The contract lives in `docs/design/desktop-shell.md` (bootstrap
//! section): a registry file the installer creates and the desktop
//! registration flow normally writes — never a manager daemon, never
//! a discovery protocol. This module is the registry as code plus
//! the provisioning orchestration the `wyrd bootstrap` command
//! drives. The later desktop backend reads the same file through
//! this module, never through the CLI.
//!
//! Three rules hold throughout:
//!
//! - The registry holds drive facts, never secrets. Entries name a
//!   directory, a relay set, and a service; key material stays in
//!   the OS keychain and credential files (settings contract).
//! - Registration and service enablement happen only after a
//!   successful initialization. A drive is reported ready only when
//!   its socket verifies this session — never because its registry
//!   entry exists.
//! - Entries are untrusted configuration. Every read validates and
//!   skips what does not parse; every connection still
//!   authenticates through the owner interface's own checks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wyrd_sync::keys::DeviceIdentitySecret;
use wyrd_sync::runtime::{Engine, EngineError};

use crate::owner::client::{ClientError, OwnerClient};
use crate::owner::wire::WireErrorCode;

/// Registry schema version. Bump only with a migration that reads
/// the previous version; readers refuse newer versions loudly
/// rather than guessing.
pub const REGISTRY_VERSION: u32 = 1;

/// Registry filename, fixed so installer, command, and desktop read
/// the same path derivation.
pub const REGISTRY_FILENAME: &str = "drives.json";

/// Supervision mode for one registered drive. Only headless owner
/// mode exists today: mounting through the registry waits on the P2
/// owner-socket decision, which either moves the server into the
/// shared composition or marks mount non-desktop. Unknown modes
/// from newer writers are skipped per entry with a reason — a
/// future mode must never fail the whole file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceMode {
    Vault,
    Unknown(String),
}

impl Serialize for ServiceMode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ServiceMode::Vault => serializer.serialize_str("vault"),
            ServiceMode::Unknown(mode) => serializer.serialize_str(mode),
        }
    }
}

impl<'de> Deserialize<'de> for ServiceMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mode = String::deserialize(deserializer)?;
        match mode.as_str() {
            "vault" => Ok(ServiceMode::Vault),
            other => Ok(ServiceMode::Unknown(other.to_string())),
        }
    }
}

/// One registered drive: facts for discovery and supervision, never
/// secrets. `drive_dir` must be absolute; `name` and `service` are
/// `[a-z0-9-]` so service labels and unit names derive without
/// quoting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub name: String,
    pub drive_dir: PathBuf,
    pub relays: Vec<String>,
    pub service: String,
    pub mode: ServiceMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RegistryFile {
    version: u32,
    drives: BTreeMap<String, RegistryEntry>,
}

/// What reading the registry reports: the valid entries plus one
/// reason per skipped entry. Skips keep discovery working when one
/// entry rots; the reasons surface so a rotten entry is repaired,
/// not silently forgotten.
#[derive(Debug, Default)]
pub struct Registry {
    pub drives: BTreeMap<String, RegistryEntry>,
    pub skipped: Vec<String>,
}

/// Where the registry lives when no explicit path is given:
/// per-user state, resolved through platform conventions. The
/// installer creates it with owner-only permissions; the
/// registration flow below is its normal writer.
pub fn default_registry_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library/Application Support/Wyrd")
                .join(REGISTRY_FILENAME);
        }
    }
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("wyrd").join(REGISTRY_FILENAME);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local/state/wyrd")
            .join(REGISTRY_FILENAME);
    }
    PathBuf::from(REGISTRY_FILENAME)
}

/// Read and validate the registry. A missing file is an empty
/// registry, not an error — a fresh installation has registered
/// nothing yet. A newer schema version refuses loudly. Invalid
/// entries are skipped with reasons; valid ones are unaffected.
pub fn read_registry(path: &Path) -> Result<Registry, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Registry::default());
        }
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let file: RegistryFile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    if file.version != REGISTRY_VERSION {
        return Err(format!(
            "registry {} is version {}, this build reads {}",
            path.display(),
            file.version,
            REGISTRY_VERSION
        ));
    }
    let mut registry = Registry::default();
    for (key, entry) in file.drives {
        match validate_entry(&key, &entry) {
            Ok(()) => {
                registry.drives.insert(key, entry);
            }
            Err(reason) => registry.skipped.push(format!("entry {key:?}: {reason}")),
        }
    }
    Ok(registry)
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn validate_entry(key: &str, entry: &RegistryEntry) -> Result<(), String> {
    if key != entry.name {
        return Err(format!(
            "key {key:?} does not match entry name {:?}",
            entry.name
        ));
    }
    if !valid_token(&entry.name) {
        return Err(format!(
            "name {:?} must be 1-64 chars of [a-z0-9-]",
            entry.name
        ));
    }
    if !valid_token(&entry.service) {
        return Err(format!(
            "service {:?} must be 1-64 chars of [a-z0-9-]",
            entry.service
        ));
    }
    if !entry.drive_dir.is_absolute() {
        return Err(format!(
            "drive_dir {} must be absolute",
            entry.drive_dir.display()
        ));
    }
    if has_dotdot(&entry.drive_dir) {
        return Err(format!(
            "drive_dir {} must not contain ..",
            entry.drive_dir.display()
        ));
    }
    if !matches!(entry.mode, ServiceMode::Vault) {
        return Err(
            "unknown service mode: mount and later modes wait on the P2 owner-socket decision"
                .to_string(),
        );
    }
    if entry.relays.iter().any(|relay| {
        relay.is_empty()
            || relay
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte < 0x20)
    }) {
        return Err("relays must be non-empty URLs without whitespace".to_string());
    }
    Ok(())
}

fn has_dotdot(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}

/// Write the registry crash-safely: temporary file in the same
/// directory, sync it, atomic replace, sync the directory. An
/// interrupted write can never leave a truncated registry that
/// makes every drive undiscoverable.
pub fn write_registry(path: &Path, drives: &BTreeMap<String, RegistryEntry>) -> Result<(), String> {
    for (key, entry) in drives {
        validate_entry(key, entry).map_err(|reason| format!("refusing to write: {reason}"))?;
    }
    let file = RegistryFile {
        version: REGISTRY_VERSION,
        drives: drives.clone(),
    };
    let bytes =
        serde_json::to_vec_pretty(&file).map_err(|error| format!("encode registry: {error}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    // Unique temporary sibling per writer: the rename stays atomic,
    // and concurrent writers fail on their own file instead of
    // clobbering each other's. Registration is still single-writer by
    // contract (one registration flow per user); the unique name
    // turns a violation into an error, not silent loss.
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, &bytes).map_err(|error| format!("write {}: {error}", tmp.display()))?;
    restrict_owner_only(&tmp)?;
    sync_file(&tmp)?;
    std::fs::rename(&tmp, path).map_err(|error| format!("replace {}: {error}", path.display()))?;
    restrict_owner_only(path)?;
    sync_dir(path);
    Ok(())
}

/// Owner-only file permissions (Unix): the registry holds drive
/// facts, not secrets, but it is still per-user state — no group or
/// other access, ever.
#[cfg(unix)]
fn restrict_owner_only(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("chmod {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn restrict_owner_only(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn sync_file(path: &Path) -> Result<(), String> {
    let file =
        std::fs::File::open(path).map_err(|error| format!("sync {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("sync {}: {error}", path.display()))
}

fn sync_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

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
/// double-quoted with backslash escapes.
fn systemd_quote(arg: &str) -> String {
    if arg
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_./:=,".contains(&byte))
    {
        return arg.to_string();
    }
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    for ch in arg.chars() {
        if ch == '"' || ch == '\\' {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

/// The supervised owner command: the `wyrd-supervise` wrapper from
/// `nix/drive` (OD-34-C) around `wyrd vault` with file credentials.
/// Managers cannot distinguish the binary's collapsed exit codes on
/// their own — every CLI failure is 2 except the terminal held-drive
/// 78 — so the wrapper maps the wait status onto the exit the
/// supervisor's policy wants (restart-worthy per manager, 78 never).
/// Routing through the wrapper is what makes the restart policy
/// below truthful; a bare owner binary cannot express it.
#[derive(Debug, Clone)]
pub struct SupervisedOwner {
    /// `wyrd-supervise` (`nix/drive/supervise.py`): same installer
    /// layout as the `wyrd` binary, overridable per command.
    pub supervise_bin: PathBuf,
    /// The `wyrd` binary whose `vault` subcommand serves the drive
    /// and binds the interface socket.
    pub wyrd_bin: PathBuf,
    /// Hardened credential files, resolved the way `load_credentials`
    /// resolves them. Paths, not secrets, travel here — the service
    /// definition names the same files the operator already keeps.
    pub identity_file: PathBuf,
    pub passphrase_file: PathBuf,
}

impl SupervisedOwner {
    /// The full supervised argv: wrapper, per-manager restart
    /// status, posture path, `--` separator, then the vault command.
    pub fn argv(&self, entry: &RegistryEntry, restart_status: &str) -> Vec<String> {
        let mut argv = vec![
            self.supervise_bin.to_string_lossy().into_owned(),
            "--restart-status".to_string(),
            restart_status.to_string(),
            "--posture-path".to_string(),
            entry
                .drive_dir
                .join("vault.posture")
                .to_string_lossy()
                .into_owned(),
            "--".to_string(),
            self.wyrd_bin.to_string_lossy().into_owned(),
            "vault".to_string(),
            entry.drive_dir.to_string_lossy().into_owned(),
            "--identity-file".to_string(),
            self.identity_file.to_string_lossy().into_owned(),
            "--passphrase-file".to_string(),
            self.passphrase_file.to_string_lossy().into_owned(),
        ];
        for relay in &entry.relays {
            argv.push("--relay".to_string());
            argv.push(relay.clone());
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

/// Install the per-drive service definition for this platform and
/// return its path. Writes atomically with owner-only permissions:
/// a half-written unit must never be what the manager loads.
/// Installation writes the definition; enabling and starting are
/// separate steps so registration can report exactly which one
/// failed.
#[cfg(unix)]
pub fn install_service(entry: &RegistryEntry, owner: &SupervisedOwner) -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    {
        let dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("Library/LaunchAgents");
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("create {}: {error}", dir.display()))?;
        let label = format!("md.enesis.wyrd.{}", entry.service);
        let path = dir.join(format!("{label}.plist"));
        write_service_file(&path, &render_launchd_plist(&label, owner, entry))?;
        Ok(path)
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
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("create {}: {error}", dir.display()))?;
        let path = dir.join(format!("wyrd-{}.service", entry.service));
        write_service_file(
            &path,
            &render_systemd_unit(&format!("Wyrd drive {}", entry.name), owner, entry),
        )?;
        return Ok(path);
    }
}

/// Atomic owner-only service write: temporary sibling, sync,
/// replace — the manager never loads a half-written definition.
#[cfg(unix)]
fn write_service_file(path: &Path, contents: &str) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, contents).map_err(|error| format!("write {}: {error}", tmp.display()))?;
    restrict_owner_only(&tmp)?;
    sync_file(&tmp)?;
    std::fs::rename(&tmp, path).map_err(|error| format!("replace {}: {error}", path.display()))?;
    restrict_owner_only(path)
}

#[cfg(not(unix))]
pub fn install_service(
    _entry: &RegistryEntry,
    _owner: &SupervisedOwner,
) -> Result<PathBuf, String> {
    Err("per-drive services are implemented for Unix (launchd agent, systemd user unit)".into())
}

/// Enable the installed service for startup-at-login. Separate
/// from installation and from starting, so each failure reports its
/// own stage. On macOS `load -w` loads now and starts the agent, so
/// enablement is the start; re-enabling unloads first, which
/// restarts a running service rather than duplicating it. Disabling
/// later is `launchctl unload` / `systemctl --user disable --now` —
/// the documented stop path, distinct from a crash the manager
/// retries.
#[cfg(unix)]
pub fn enable_service(entry: &RegistryEntry, definition: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{}", entry.service);
        // Unload-then-load: a disabled leftover re-enables rather
        // than duplicating, and a running service restarts on the
        // new definition. Errors from the unload are ignored — the
        // service may never have loaded — but the load must succeed.
        let _ = run("launchctl", &["unload", &definition.to_string_lossy()]);
        run("launchctl", &["load", "-w", &definition.to_string_lossy()])
            .map_err(|error| format!("enable {label}: {error}"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = format!("wyrd-{}.service", entry.service);
        run("systemctl", &["--user", "daemon-reload"])
            .map_err(|error| format!("reload user units: {error}"))?;
        run("systemctl", &["--user", "enable", &unit])
            .map_err(|error| format!("enable {unit}: {error}"))?;
        return Ok(());
    }
}

/// Start the enabled service now. On macOS enablement already
/// started the agent, so this only verifies it is loaded — a second
/// start path would double-start. On systemd enablement persists
/// without starting, so this starts explicitly.
#[cfg(unix)]
pub fn start_service(entry: &RegistryEntry) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{}", entry.service);
        run("launchctl", &["list", &label])
            .map_err(|error| format!("start {label}: agent not loaded: {error}"))?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = format!("wyrd-{}.service", entry.service);
        run("systemctl", &["--user", "start", &unit])
            .map_err(|error| format!("start {unit}: {error}"))?;
        Ok(())
    }
}

#[cfg(unix)]
fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("spawn {program}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "{program} {} failed: {}",
        args.join(" "),
        detail.trim()
    ))
}

/// How to initialize the drive before anything is registered.
pub enum Init {
    /// `Engine::create`: a new single-device drive.
    Create,
    /// `Engine::join`: join as an invited device from a sealed
    /// invitation file. The invitation exchange that produced the
    /// file stays out of band; joining consumes it.
    Join { sealed_invitation: Vec<u8> },
}

/// What provisioning this drive needs beyond initialization.
pub struct ProvisionConfig {
    pub name: String,
    pub drive_dir: PathBuf,
    pub relays: Vec<String>,
    pub identity: DeviceIdentitySecret,
    pub passphrase: String,
    pub init: Init,
    pub registry_path: PathBuf,
    /// The `wyrd` binary whose `vault` subcommand the service runs.
    /// Defaults to the `wyrd` beside the provisioning command; the
    /// installer ships both together.
    pub wyrd_bin: Option<PathBuf>,
    /// `wyrd-supervise` (`nix/drive/supervise.py`): the wait-status
    /// mapper the restart policy rests on. Defaults to the sibling
    /// of the `wyrd` binary. Managing a service without it is
    /// refused — a bare owner binary cannot express the terminal
    /// distinction — so pass an explicit path wherever the layout
    /// differs instead of running unmanaged.
    pub supervise_bin: Option<PathBuf>,
    /// Hardened credential files for the service definition,
    /// resolved the way `load_credentials` resolves them. Paths,
    /// never secrets, travel here.
    pub identity_file: PathBuf,
    pub passphrase_file: PathBuf,
    /// Install, enable, and start the per-drive service. Off skips
    /// all three: registration only, for operators who supervise
    /// the owner themselves.
    pub manage_service: bool,
    /// How long one socket verification waits for a terminal answer.
    /// An expiry is `Unconfirmed`, never success.
    pub verify_timeout: Duration,
}

/// Where one provisioning run ended. Ready is reported only as
/// `VerifiedAvailable` — a socket answer this session. Every other
/// variant names the stage that stopped, so partial completion is
/// explicit and resumable, never mistaken for ready.
#[derive(Debug)]
pub enum ProvisionState {
    VerifiedAvailable {
        entry: RegistryEntry,
    },
    RegisteredButUnavailable {
        entry: RegistryEntry,
        reason: String,
    },
    ProvisioningIncomplete {
        stage: &'static str,
        reason: String,
    },
}

/// The fixed handoff order: initialize → verify the open → write the
/// registry → install, enable, and start the service → verify the
/// socket. Registration and service steps happen only after a
/// successful initialization, never speculatively.
pub fn provision(config: ProvisionConfig) -> ProvisionState {
    let entry = RegistryEntry {
        service: format!("drive-{}", config.name),
        name: config.name.clone(),
        drive_dir: config.drive_dir.clone(),
        relays: config.relays.clone(),
        mode: ServiceMode::Vault,
    };
    if let Err(reason) = validate_entry(&entry.name, &entry) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            reason,
        };
    }
    // A headless `wyrd vault` refuses a relay-less drive (it would
    // serve nothing peers can discover), so managing a service
    // without relays is refused before anything is created or
    // registered — with that pointer, not a definition that can
    // never verify.
    if config.manage_service && config.relays.is_empty() {
        return ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            reason: "managing a service needs at least one --relay: \
                a relay-less vault serves nothing peers can discover; \
                register with --no-service instead"
                .to_string(),
        };
    }

    // 1. Initialize, or reconcile an existing drive. `DriveExists`
    // means a previous run (or another operator) already created
    // it: fall through and re-verify the open instead of re-creating.
    match &config.init {
        Init::Create => match Engine::create(
            config.drive_dir.clone(),
            &config.passphrase,
            config.identity.clone(),
        ) {
            Ok(_) | Err(EngineError::DriveExists) => {}
            Err(error) => {
                return ProvisionState::ProvisioningIncomplete {
                    stage: "initialize",
                    reason: error.to_string(),
                };
            }
        },
        Init::Join { sealed_invitation } => {
            let sealed = match wyrd_sync::control::SealedBootstrap::decode(sealed_invitation) {
                Ok(sealed) => sealed,
                Err(error) => {
                    return ProvisionState::ProvisioningIncomplete {
                        stage: "initialize",
                        reason: format!("decode invitation: {error}"),
                    };
                }
            };
            match Engine::join(
                config.drive_dir.clone(),
                &config.passphrase,
                config.identity.clone(),
                &sealed,
            ) {
                Ok(_) | Err(EngineError::DriveExists) => {}
                Err(error) => {
                    return ProvisionState::ProvisioningIncomplete {
                        stage: "initialize",
                        reason: error.to_string(),
                    };
                }
            }
        }
    };

    // 2. Verify the open before registering anything.
    if let Err(error) = Engine::open_keystore(
        config.drive_dir.clone(),
        &config.passphrase,
        config.identity.clone(),
    ) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "verify-open",
            reason: error.to_string(),
        };
    }

    // 3. Write the registry entry.
    let mut registry = match read_registry(&config.registry_path) {
        Ok(registry) => registry,
        Err(reason) => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "read-registry",
                reason,
            };
        }
    };
    registry.drives.insert(entry.name.clone(), entry.clone());
    if let Err(reason) = write_registry(&config.registry_path, &registry.drives) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "write-registry",
            reason,
        };
    }

    // 4. Install, enable, and start the per-drive service.
    if config.manage_service {
        let wyrd_bin = match &config.wyrd_bin {
            Some(path) => path.clone(),
            None => match sibling_bin("wyrd") {
                Some(path) => path,
                None => {
                    return ProvisionState::RegisteredButUnavailable {
                        entry,
                        reason: "no wyrd binary: pass --owner-bin (no wyrd beside this command)"
                            .to_string(),
                    };
                }
            },
        };
        let supervise_bin = match &config.supervise_bin {
            Some(path) => path.clone(),
            None => match sibling_bin("wyrd-supervise") {
                Some(path) => path,
                None => {
                    return ProvisionState::RegisteredButUnavailable {
                        entry,
                        reason: "no wyrd-supervise: pass --supervise-bin \
                            (the restart policy needs the wrapper; \
                            a bare owner binary cannot express it)"
                            .to_string(),
                    };
                }
            },
        };
        let owner = SupervisedOwner {
            supervise_bin,
            wyrd_bin,
            identity_file: config.identity_file.clone(),
            passphrase_file: config.passphrase_file.clone(),
        };
        let definition = match install_service(&entry, &owner) {
            Ok(path) => path,
            Err(reason) => {
                return ProvisionState::RegisteredButUnavailable {
                    entry,
                    reason: format!("install-service: {reason}"),
                };
            }
        };
        if let Err(reason) = enable_service(&entry, &definition) {
            return ProvisionState::RegisteredButUnavailable {
                entry,
                reason: format!("enable-service: {reason}"),
            };
        }
        if let Err(reason) = start_service(&entry) {
            return ProvisionState::RegisteredButUnavailable {
                entry,
                reason: format!("start-service: {reason}"),
            };
        }
    }

    // 5. Verify the socket: the owner must answer terminally. Ready
    // means `Ok` or the pre-first-pass `Unobserved` refusal — every
    // other refusal shape is unexpected from this operation and
    // stays unavailable, never verified. `Unconfirmed` and
    // `Unavailable` likewise never verify: the outcome is unknown
    // or the owner is down.
    if !wait_for_socket(&config.drive_dir, config.verify_timeout) {
        return ProvisionState::RegisteredButUnavailable {
            entry,
            reason: "verify-socket: owner.sock never appeared".to_string(),
        };
    }
    match OwnerClient::connect_drive(&config.drive_dir)
        .and_then(|client| client.with_timeout(config.verify_timeout))
    {
        Ok(mut client) => match client.observe() {
            Ok(_) => ProvisionState::VerifiedAvailable { entry },
            Err(ClientError::Refused(error)) if error.code == WireErrorCode::Unobserved => {
                ProvisionState::VerifiedAvailable { entry }
            }
            Err(error) => ProvisionState::RegisteredButUnavailable {
                entry,
                reason: format!("verify-socket: {error}"),
            },
        },
        Err(error) => ProvisionState::RegisteredButUnavailable {
            entry,
            reason: format!("verify-socket: {error}"),
        },
    }
}

/// Wait for the owner socket to appear: the service was just
/// enabled and the owner may still be starting. Polls; never
/// invents an answer.
fn wait_for_socket(drive_dir: &Path, timeout: Duration) -> bool {
    let sock = crate::owner::socket_path_for(drive_dir);
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if sock.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    sock.exists()
}

/// Locate a sibling binary beside the running executable: the
/// installer ships the set together, so the sibling is the default.
/// Overridable with an explicit path wherever the layout differs.
fn sibling_bin(name: &str) -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let dir = current.parent()?;
    let candidate = dir.join(name);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(&candidate) {
            if metadata.permissions().mode() & 0o111 != 0 {
                return Some(candidate);
            }
        }
        None
    }
    #[cfg(not(unix))]
    {
        if candidate.exists() {
            return Some(candidate);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    pub(super) fn scratch(case: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("wyrd-bootstrap-{case}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(super) fn entry(name: &str, dir: &Path) -> RegistryEntry {
        RegistryEntry {
            name: name.to_string(),
            drive_dir: dir.join("drive"),
            relays: vec!["relay://example".to_string()],
            service: format!("drive-{name}"),
            mode: ServiceMode::Vault,
        }
    }

    #[test]
    fn registry_round_trips_and_rejects_a_newer_schema() {
        let dir = scratch("schema");
        let path = dir.join("drives.json");
        let mut drives = BTreeMap::new();
        drives.insert("demo".to_string(), entry("demo", &dir));
        write_registry(&path, &drives).unwrap();

        let read = read_registry(&path).unwrap();
        assert!(read.skipped.is_empty());
        assert_eq!(read.drives["demo"], drives["demo"]);

        std::fs::write(&path, r#"{"version": 999, "drives": {}}"#).unwrap();
        let refused = read_registry(&path).unwrap_err();
        assert!(refused.contains("version 999"), "{refused}");
    }

    #[test]
    fn registry_skips_invalid_entries_with_reasons_and_keeps_valid_ones() {
        let dir = scratch("invalid");
        let path = dir.join("drives.json");
        let good = entry("good", &dir);
        let mut bad_relative = entry("bad", &dir);
        bad_relative.drive_dir = PathBuf::from("relative/path");
        let mut bad_name = entry("BAD NAME!", &dir);
        bad_name.name = "BAD NAME!".to_string();
        let mut drives = BTreeMap::new();
        drives.insert("good".to_string(), good);
        // Written by hand, bypassing the validating writer.
        let raw = serde_json::json!({
            "version": 1,
            "drives": {
                "good": drives["good"],
                "bad": bad_relative,
                "BAD NAME!": bad_name,
            }
        });
        std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();

        let read = read_registry(&path).unwrap();
        assert_eq!(read.drives.len(), 1);
        assert!(read.drives.contains_key("good"));
        assert_eq!(read.skipped.len(), 2, "{:?}", read.skipped);
    }

    #[test]
    fn registry_write_refuses_invalid_entries_and_never_writes_partial() {
        let dir = scratch("nowrite");
        let path = dir.join("drives.json");
        let mut drives = BTreeMap::new();
        let mut bad = entry("bad", &dir);
        bad.drive_dir = PathBuf::from("relative/path");
        drives.insert("bad".to_string(), bad);
        let refused = write_registry(&path, &drives).unwrap_err();
        assert!(refused.contains("absolute"), "{refused}");
        assert!(!path.exists(), "no partial registry may remain");
    }

    #[test]
    fn registry_missing_file_is_an_empty_registry() {
        let dir = scratch("missing");
        let read = read_registry(&dir.join("absent.json")).unwrap();
        assert!(read.drives.is_empty() && read.skipped.is_empty());
    }

    #[test]
    fn service_templates_name_the_service_paths_and_relays() {
        let dir = scratch("templates");
        let owner = SupervisedOwner {
            supervise_bin: PathBuf::from("/opt/wyrd/bin/wyrd-supervise"),
            wyrd_bin: PathBuf::from("/opt/wyrd/bin/wyrd"),
            identity_file: dir.join("identity"),
            passphrase_file: dir.join("passphrase"),
        };
        let owned = entry("demo", &dir);
        let plist = render_launchd_plist("md.enesis.wyrd.drive-demo", &owner, &owned);
        assert!(plist.contains("<string>md.enesis.wyrd.drive-demo</string>"));
        assert!(plist.contains("wyrd-supervise"));
        assert!(plist.contains("--restart-status"));
        assert!(plist.contains("<string>0</string>"), "{plist}");
        assert!(plist.contains("vault.posture"));
        assert!(plist.contains(&*owned.drive_dir.display().to_string()));
        assert!(plist.contains("--identity-file"));
        assert!(plist.contains("--relay"));
        assert!(plist.contains("<key>SuccessfulExit</key>"));
        let unit = render_systemd_unit("Wyrd drive demo", &owner, &owned);
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("RestartPreventExitStatus=78"));
        assert!(unit.contains("wyrd-supervise"));
        assert!(unit.contains("--restart-status=1") || unit.contains("--restart-status 1"));
        assert!(unit.contains(&*owned.drive_dir.display().to_string()));
    }

    /// A stub owner socket: answers every request with the given
    /// refusal code. It speaks the real framed protocol, so the
    /// handoff test exercises the real client, the real framing, and
    /// the real refusal mapping — only the owner is fake. The
    /// accept loop is deadline-bounded so the thread always returns
    /// for `join`, even when the caller makes fewer connections than
    /// the cap.
    pub(super) fn stub_owner(
        sock: PathBuf,
        max_requests: usize,
        code: &'static str,
    ) -> std::thread::JoinHandle<()> {
        if sock.exists() {
            std::fs::remove_file(&sock).unwrap();
        }
        std::thread::spawn(move || {
            let listener = UnixListener::bind(&sock).unwrap();
            listener.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            let mut served = 0;
            while served < max_requests && std::time::Instant::now() < deadline {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                served += 1;
                stream
                    .set_read_timeout(Some(Duration::from_secs(30)))
                    .unwrap();
                let mut len = [0u8; 4];
                if stream.read_exact(&mut len).is_err() {
                    continue;
                }
                let len = u32::from_be_bytes(len) as usize;
                if len > 96 * 1024 * 1024 {
                    continue;
                }
                let mut body = vec![0u8; len];
                if stream.read_exact(&mut body).is_err() {
                    continue;
                }
                let id: u64 = serde_json::from_slice::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|value| value.get("id")?.as_u64())
                    .unwrap_or(0);
                let reply = serde_json::json!({
                    "v": 1,
                    "id": id,
                    "result": {"err": {"code": code, "message": "stub refusal"}},
                });
                let body = serde_json::to_vec(&reply).unwrap();
                let _ = stream.write_all(&(body.len() as u32).to_be_bytes());
                let _ = stream.write_all(&body);
            }
        })
    }

    pub(super) fn identity() -> DeviceIdentitySecret {
        DeviceIdentitySecret::from_bytes([7u8; 32]).unwrap()
    }

    pub(super) fn provision_no_service(dir: &Path, registry: &Path, name: &str) -> ProvisionConfig {
        ProvisionConfig {
            name: name.to_string(),
            drive_dir: dir.join("drive"),
            relays: vec![],
            identity: identity(),
            passphrase: "bootstrap-test-pass".to_string(),
            init: Init::Create,
            registry_path: registry.to_path_buf(),
            wyrd_bin: Some(dir.join("wyrd")),
            supervise_bin: Some(dir.join("wyrd-supervise")),
            identity_file: dir.join("identity"),
            passphrase_file: dir.join("passphrase"),
            manage_service: false,
            verify_timeout: Duration::from_secs(10),
        }
    }

    #[test]
    fn provision_verifies_the_socket_and_registers_the_drive() {
        let dir = scratch("provision");
        let registry = dir.join("drives.json");
        let sock = dir.join("drive").join("owner.sock");
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let server = stub_owner(sock, 1, "Unobserved");

        let report = provision(provision_no_service(&dir, &registry, "demo"));
        let ProvisionState::VerifiedAvailable { entry } = report else {
            panic!("expected verified available, got terminal state without success");
        };
        assert_eq!(entry.name, "demo");

        let read = read_registry(&registry).unwrap();
        assert!(read.skipped.is_empty());
        assert_eq!(read.drives["demo"].drive_dir, dir.join("drive"));
        server.join().unwrap();
    }

    #[test]
    fn provision_without_an_owner_registers_but_withholds_ready() {
        let dir = scratch("noowner");
        let registry = dir.join("drives.json");
        let report = provision(provision_no_service(&dir, &registry, "demo"));
        let ProvisionState::RegisteredButUnavailable { entry, reason } = report else {
            panic!("a missing owner must never verify");
        };
        assert!(reason.contains("verify-socket"), "{reason}");
        // Registered all the same: the next run reconciles instead of
        // starting over.
        let read = read_registry(&registry).unwrap();
        assert!(read.drives.contains_key(&entry.name));

        // Re-run reconciles the existing drive (create refuses,
        // the flow re-verifies) instead of failing on it.
        let again = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(
            matches!(again, ProvisionState::RegisteredButUnavailable { .. }),
            "re-run must reconcile, not fail on the existing drive"
        );
    }

    #[test]
    fn provision_rejects_bad_names_before_touching_anything() {
        let dir = scratch("badname");
        let registry = dir.join("drives.json");
        let mut config = provision_no_service(&dir, &registry, "BAD NAME!");
        config.name = "BAD NAME!".to_string();
        let report = provision(config);
        assert!(
            matches!(
                report,
                ProvisionState::ProvisioningIncomplete {
                    stage: "validate",
                    ..
                }
            ),
            "validation must precede initialization"
        );
        assert!(!dir.join("drive").exists());
        assert!(!registry.exists());
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::tests::{entry, provision_no_service, scratch, stub_owner};
    use super::*;

    #[test]
    fn registry_skips_unknown_modes_without_failing_the_file() {
        let dir = scratch("mode");
        let path = dir.join("drives.json");
        let raw = serde_json::json!({
            "version": 1,
            "drives": {
                "future": {
                    "name": "future",
                    "drive_dir": dir.join("drive").to_string_lossy(),
                    "relays": [],
                    "service": "drive-future",
                    "mode": "mount"
                }
            }
        });
        std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
        let read = read_registry(&path).unwrap();
        assert!(read.drives.is_empty());
        assert_eq!(read.skipped.len(), 1, "{:?}", read.skipped);
        assert!(read.skipped[0].contains("mount"), "{:?}", read.skipped);
    }

    #[test]
    fn registry_file_is_owner_only() {
        let dir = scratch("perms");
        let path = dir.join("drives.json");
        let mut drives = BTreeMap::new();
        drives.insert("demo".to_string(), entry("demo", &dir));
        write_registry(&path, &drives).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "registry is owner-only, got {mode:o}");
        }
    }

    #[test]
    fn service_files_write_atomically_and_owner_only() {
        let dir = scratch("svcfile");
        let path = dir.join("wyrd-drive-demo.service");
        let owner = SupervisedOwner {
            supervise_bin: PathBuf::from("/opt/wyrd/bin/wyrd-supervise"),
            wyrd_bin: PathBuf::from("/opt/wyrd/bin/wyrd"),
            identity_file: dir.join("identity"),
            passphrase_file: dir.join("passphrase"),
        };
        let owned = entry("demo", &dir);
        write_service_file(
            &path,
            &render_systemd_unit("Wyrd drive demo", &owner, &owned),
        )
        .unwrap();
        assert!(path.exists());
        assert!(
            !dir.join(format!("wyrd-drive-demo.tmp.{}", std::process::id()))
                .exists(),
            "no temporary sibling remains"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "service definition is owner-only, got {mode:o}"
            );
        }
    }

    #[test]
    fn templates_escape_operator_input() {
        let dir = scratch("escape");
        let mut owned = entry("demo", &dir);
        owned.relays = vec!["relay://exa mple".to_string()];
        let owner = SupervisedOwner {
            supervise_bin: PathBuf::from("/opt/wyrd/bin/with space/wyrd-supervise"),
            wyrd_bin: PathBuf::from("/opt/wyrd/bin/wyrd"),
            identity_file: dir.join("identity"),
            passphrase_file: dir.join("passphrase"),
        };
        let plist = render_launchd_plist("md.enesis.wyrd.drive-demo", &owner, &owned);
        assert!(plist.contains("with space"), "paths survive escaped");
        let unit = render_systemd_unit("Wyrd drive demo", &owner, &owned);
        assert!(
            unit.contains("\"/opt/wyrd/bin/with space/wyrd-supervise\""),
            "spaces are quoted, got:\n{unit}"
        );
        assert!(
            unit.contains("\"relay://exa mple\""),
            "relay spaces are quoted, got:\n{unit}"
        );
    }

    #[test]
    fn only_ok_and_unobserved_verify() {
        let dir = scratch("whitelist");
        let registry = dir.join("drives.json");
        let sock = dir.join("drive").join("owner.sock");
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let server = stub_owner(sock, 1, "Malformed");
        let report = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(
            matches!(report, ProvisionState::RegisteredButUnavailable { .. }),
            "an unexpected refusal shape must not verify"
        );
        server.join().unwrap();
    }

    #[test]
    fn managing_a_service_needs_a_relay() {
        let dir = scratch("norelay");
        let registry = dir.join("drives.json");
        let mut config = provision_no_service(&dir, &registry, "demo");
        config.manage_service = true;
        config.relays = vec![];
        let report = provision(config);
        assert!(
            matches!(
                report,
                ProvisionState::ProvisioningIncomplete {
                    stage: "validate",
                    ..
                }
            ),
            "a relay-less managed service is refused, not installed"
        );
        assert!(
            !registry.exists(),
            "refusal precedes registration side effects"
        );
    }
}
