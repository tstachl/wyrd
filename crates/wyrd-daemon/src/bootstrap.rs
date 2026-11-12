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
/// mode exists today: mounting through the registry waits on the
/// installer follow-up (P2 landed — `wyrd mount` serves the socket
/// through the shared `OwnerService` when run manually — but the
/// registry's managed `mount` mode, service template, and enablement
/// UX still travel with that issue). Unknown modes from newer writers
/// are skipped per entry with a reason — a future mode must never
/// fail the whole file.
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
    /// Decoded per entry by the reader: structurally corrupt values
    /// fail the whole file (as before), while entries that decode
    /// but fail validation skip per entry with a reason — and are
    /// carried forward verbatim by the next write instead of being
    /// deleted (see `write_registry`).
    drives: BTreeMap<String, serde_json::Value>,
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

/// Every way the registry and service-definition I/O in this
/// module can fail. Provisioning also fails at engine
/// initialization and socket verification with other error text;
/// those paths are outside this type. Variants carry
/// the failing identity — paths, names, labels — so the desktop
/// backend can branch on failure instead of parsing text.
/// `Display` reproduces the historical message text exactly: the CLI
/// renders `reason` verbatim and the desktop shell reads the same
/// text. Batch discipline: [`read_registry`] counts validation
/// failures as per-entry skip reasons and keeps going, while
/// [`write_registry`] raises them and writes nothing.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// The registry file could not be read.
    #[error("read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The registry file is not valid JSON.
    #[error("parse {}: {source}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// The registry schema version is newer than this build reads.
    #[error("registry {} is version {found}, this build reads {REGISTRY_VERSION}", path.display())]
    UnsupportedVersion { path: PathBuf, found: u32 },
    /// A registry key does not match its entry's name.
    #[error("key {key:?} does not match entry name {name:?}")]
    KeyMismatch { key: String, name: String },
    /// A drive name breaks the `[a-z0-9-]` token rule.
    #[error("name {name:?} must be 1-64 chars of [a-z0-9-]")]
    InvalidName { name: String },
    /// A drive name leaves no room for the `drive-` service prefix.
    #[error("name {name:?} leaves no room for the drive- service prefix (max 58 chars)")]
    NameTooLongForServicePrefix { name: String },
    /// A service name breaks the `[a-z0-9-]` token rule.
    #[error("service {service:?} must be 1-64 chars of [a-z0-9-]")]
    InvalidService { service: String },
    /// A drive directory is not absolute.
    #[error("drive_dir {} must be absolute", dir.display())]
    RelativeDriveDir { dir: PathBuf },
    /// A drive directory escapes through `..`.
    #[error("drive_dir {} must not contain ..", dir.display())]
    DriveDirHasDotDot { dir: PathBuf },
    /// The entry names a service mode this build does not manage.
    #[error("unknown service mode: registry-managed mount mode travels with the installer follow-up (P2 landed: manual `wyrd mount` serves the socket)")]
    UnknownServiceMode,
    /// A relay URL is empty or carries whitespace/control bytes.
    #[error("relays must be non-empty URLs without whitespace or control bytes")]
    InvalidRelays,
    /// A drive directory carries control characters.
    #[error("drive_dir {} must not contain control characters", dir.display())]
    DriveDirHasControlChars { dir: PathBuf },
    /// The candidate set fails validation: nothing was written.
    #[error("refusing to write: {source}")]
    RefusedWrite {
        #[source]
        source: Box<BootstrapError>,
    },
    /// The registry could not be encoded.
    #[error("encode registry: {source}")]
    Encode {
        #[source]
        source: serde_json::Error,
    },
    /// A directory for the registry or service file could not be created.
    #[error("create {}: {source}", path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A registry or service file could not be written.
    #[error("write {}: {source}", path.display())]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The atomic replace of a registry or service file failed.
    #[error("replace {}: {source}", path.display())]
    ReplaceFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Owner-only permissions could not be applied.
    #[error("chmod {}: {source}", path.display())]
    Chmod {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A written file could not be synced to disk.
    #[error("sync {}: {source}", path.display())]
    SyncFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A service definition file could not be removed when clearing
    /// a reconfigured drive.
    #[error("remove {}: {source}", path.display())]
    RemoveFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A service manager command could not be spawned.
    #[error("spawn {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    /// A service manager command ran and failed.
    #[error("{program} {args} failed: {detail}")]
    CommandFailed {
        program: String,
        args: String,
        detail: String,
    },
    /// Enabling the service through its manager failed.
    #[error("enable {name}: {source}")]
    EnableService {
        name: String,
        #[source]
        source: Box<BootstrapError>,
    },
    /// Reloading the user service manager failed.
    #[error("reload user units: {source}")]
    ReloadUnits {
        #[source]
        source: Box<BootstrapError>,
    },
    /// The managed service is not loaded after enablement.
    #[error("start {label}: agent not loaded: {source}")]
    AgentNotLoaded {
        label: String,
        #[source]
        source: Box<BootstrapError>,
    },
    /// Restarting the managed service failed.
    #[error("restart {unit}: {source}")]
    RestartService {
        unit: String,
        #[source]
        source: Box<BootstrapError>,
    },
    /// Stopping (disabling) or clearing the managed service before
    /// a reconfiguration failed.
    #[error("stop {name}: {source}")]
    StopService {
        name: String,
        #[source]
        source: Box<BootstrapError>,
    },
    /// Per-drive services exist only on Unix managers.
    #[error("per-drive services are implemented for Unix (launchd agent, systemd user unit)")]
    ServicesUnsupported,
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
pub fn read_registry(path: &Path) -> Result<Registry, BootstrapError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Registry::default());
        }
        Err(source) => {
            return Err(BootstrapError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let file: RegistryFile =
        serde_json::from_slice(&bytes).map_err(|source| BootstrapError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    if file.version != REGISTRY_VERSION {
        return Err(BootstrapError::UnsupportedVersion {
            path: path.to_path_buf(),
            found: file.version,
        });
    }
    let mut registry = Registry::default();
    for (key, value) in file.drives {
        let entry: RegistryEntry =
            serde_json::from_value(value).map_err(|source| BootstrapError::Parse {
                path: path.to_path_buf(),
                source,
            })?;
        match validate_entry(&key, &entry) {
            Ok(()) => {
                registry.drives.insert(key, entry);
            }
            Err(error) => registry.skipped.push(format!("entry {key:?}: {error}")),
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

/// Relay URLs are non-empty and carry no whitespace or control
/// bytes. The managed path additionally requires a non-empty set;
/// keeping the predicate separate lets reconfiguration validate the
/// candidate values before anything stops.
fn valid_relays(relays: &[String]) -> bool {
    !relays.iter().any(|relay| {
        relay.is_empty()
            || relay
                .bytes()
                .any(|byte| byte < 0x20 || byte == 0x7f || byte.is_ascii_whitespace())
    })
}

fn validate_entry(key: &str, entry: &RegistryEntry) -> Result<(), BootstrapError> {
    if key != entry.name {
        return Err(BootstrapError::KeyMismatch {
            key: key.to_string(),
            name: entry.name.clone(),
        });
    }
    if !valid_token(&entry.name) {
        return Err(BootstrapError::InvalidName {
            name: entry.name.clone(),
        });
    }
    if entry.name.len() + "drive-".len() > 64 {
        return Err(BootstrapError::NameTooLongForServicePrefix {
            name: entry.name.clone(),
        });
    }
    if !valid_token(&entry.service) {
        return Err(BootstrapError::InvalidService {
            service: entry.service.clone(),
        });
    }
    if !entry.drive_dir.is_absolute() {
        return Err(BootstrapError::RelativeDriveDir {
            dir: entry.drive_dir.clone(),
        });
    }
    if has_dotdot(&entry.drive_dir) {
        return Err(BootstrapError::DriveDirHasDotDot {
            dir: entry.drive_dir.clone(),
        });
    }
    if !matches!(entry.mode, ServiceMode::Vault) {
        return Err(BootstrapError::UnknownServiceMode);
    }
    if !valid_relays(&entry.relays) {
        return Err(BootstrapError::InvalidRelays);
    }
    if entry
        .drive_dir
        .to_string_lossy()
        .bytes()
        .any(|byte| byte < 0x20 || byte == 0x7f)
    {
        return Err(BootstrapError::DriveDirHasControlChars {
            dir: entry.drive_dir.clone(),
        });
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
///
/// Entries the reader skipped (unknown modes, hand edits that fail
/// validation) are carried forward verbatim from the existing file:
/// a rewrite must never delete what this build does not understand.
/// Keys the new map carries win; only absent keys merge back, so a
/// re-registration that repairs an entry takes effect. The merge
/// reads the file best-effort — a missing or unreadable file simply
/// contributes nothing — while the candidate map itself is fully
/// validated and refuses the write on any invalid entry.
pub fn write_registry(
    path: &Path,
    drives: &BTreeMap<String, RegistryEntry>,
) -> Result<(), BootstrapError> {
    for (key, entry) in drives {
        validate_entry(key, entry).map_err(|source| BootstrapError::RefusedWrite {
            source: Box::new(source),
        })?;
    }
    let mut merged: BTreeMap<String, serde_json::Value> = drives
        .iter()
        .map(|(key, entry)| {
            serde_json::to_value(entry)
                .map(|value| (key.clone(), value))
                .map_err(|source| BootstrapError::Encode { source })
        })
        .collect::<Result<_, _>>()?;
    if let Ok(existing) = std::fs::read(path) {
        if let Ok(file) = serde_json::from_slice::<RegistryFile>(&existing) {
            if file.version == REGISTRY_VERSION {
                for (key, value) in file.drives {
                    merged.entry(key).or_insert(value);
                }
            }
        }
    }
    let file = RegistryFile {
        version: REGISTRY_VERSION,
        drives: merged,
    };
    let bytes =
        serde_json::to_vec_pretty(&file).map_err(|source| BootstrapError::Encode { source })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| BootstrapError::CreateDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    // Unique temporary sibling per writer: the rename stays atomic,
    // and concurrent writers fail on their own file instead of
    // clobbering each other's. Registration is still single-writer by
    // contract (one registration flow per user); the unique name
    // turns a violation into an error, not silent loss.
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, &bytes).map_err(|source| BootstrapError::WriteFile {
        path: tmp.clone(),
        source,
    })?;
    restrict_owner_only(&tmp)?;
    sync_file(&tmp)?;
    std::fs::rename(&tmp, path).map_err(|source| BootstrapError::ReplaceFile {
        path: path.to_path_buf(),
        source,
    })?;
    restrict_owner_only(path)?;
    sync_dir(path);
    Ok(())
}

/// Owner-only file permissions (Unix): the registry holds drive
/// facts, not secrets, but it is still per-user state — no group or
/// other access, ever.
#[cfg(unix)]
fn restrict_owner_only(path: &Path) -> Result<(), BootstrapError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
        BootstrapError::Chmod {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn restrict_owner_only(_path: &Path) -> Result<(), BootstrapError> {
    Ok(())
}

fn sync_file(path: &Path) -> Result<(), BootstrapError> {
    let file = std::fs::File::open(path).map_err(|source| BootstrapError::SyncFile {
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| BootstrapError::SyncFile {
        path: path.to_path_buf(),
        source,
    })
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
fn service_definition_path(entry: &RegistryEntry) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("Library/LaunchAgents");
        let label = format!("md.enesis.wyrd.{}", entry.service);
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
        dir.join(format!("wyrd-{}.service", entry.service))
    }
}

/// Install the per-drive service definition for this platform and
/// return its path. Writes atomically with owner-only permissions:
/// a half-written unit must never be what the manager loads.
/// Installation writes the definition; enabling and starting are
/// separate steps so registration can report exactly which one
/// failed.
#[cfg(unix)]
pub fn install_service(
    entry: &RegistryEntry,
    owner: &SupervisedOwner,
) -> Result<PathBuf, BootstrapError> {
    let dir = service_definition_path(entry)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    std::fs::create_dir_all(&dir).map_err(|source| BootstrapError::CreateDir {
        path: dir.clone(),
        source,
    })?;
    let path = service_definition_path(entry);
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
fn write_service_file(path: &Path, contents: &str) -> Result<(), BootstrapError> {
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
        // Only an enabled unit needs the manager's disable: on a
        // never-enabled unit `disable --now` would fail on the
        // unknown name, and that absence is expected (a never-managed
        // drive, or this operation's own earlier pass), never a stop
        // failure. The definition file below is removed either way.
        if run("systemctl", &["--user", "is-enabled", "--quiet", &unit]).is_ok() {
            // `disable --now` stops a running unit and removes the
            // enablement symlink in one step.
            run("systemctl", &["--user", "disable", "--now", &unit]).map_err(|source| {
                BootstrapError::StopService {
                    name: unit.clone(),
                    source: Box::new(source),
                }
            })?;
            // The manager drops the removed unit on its next reload;
            // reload now so a stale in-memory unit cannot outlive the
            // registry rewrite below.
            run("systemctl", &["--user", "daemon-reload"]).map_err(|source| {
                BootstrapError::StopService {
                    name: unit,
                    source: Box::new(source),
                }
            })?;
        }
        clear_definition(definition, &unit)
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
fn run(program: &str, args: &[&str]) -> Result<(), BootstrapError> {
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
/// explicit and resumable, never mistaken for ready. The post-write
/// variants carry the registry entries the read skipped, so the
/// caller can surface them instead of silently discarding rotten
/// data on rewrite.
#[derive(Debug)]
pub enum ProvisionState {
    VerifiedAvailable {
        entry: RegistryEntry,
        skipped: Vec<String>,
    },
    RegisteredButUnavailable {
        entry: RegistryEntry,
        kind: ProvisionFailureKind,
        reason: String,
        skipped: Vec<String>,
    },
    ProvisioningIncomplete {
        stage: &'static str,
        kind: ProvisionFailureKind,
        reason: String,
    },
}

/// The machine-readable failure kind for a provisioning run that
/// did not verify: one variant per terminal failure class (16
/// return sites share 12 kinds), carried alongside (never instead
/// of) the human-readable `reason`. Direct Rust callers match on
/// this instead of parsing text; the rendered strings are
/// unchanged. A new terminal `return` in `provision()` requires a
/// variant here — the compiler enforces the field, only review
/// enforces the right tag. Deliberately coarse — the desktop adapter
/// still branches on exit codes plus independent re-observation
/// alone and never parses CLI output, so this kind stops at the
/// `ProvisionState` boundary by design. The intended consumer is the
/// CLI/desktop adapter after a desktop-shell amendment; until then
/// it serves direct Rust callers and pins classification in tests.
/// `non_exhaustive` so adding a kind later is not a breaking change
/// for downstream exhaustive matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProvisionFailureKind {
    /// Entry validation refused before anything was created.
    Validate,
    /// The sealed invitation did not decode.
    DecodeInvitation,
    /// Engine creation or join failed.
    Initialize,
    /// The fresh open of the created drive failed.
    VerifyOpen,
    /// Another process holds the drive's store lock: stop the live
    /// owner first, the same rule every other offline writer states.
    /// Distinct from other open failures so the CLI can return the
    /// terminal held-drive exit instead of a usage error.
    StoreLocked,
    /// The registry could not be read.
    ReadRegistry,
    /// The registry entry could not be written.
    WriteRegistry,
    /// No `wyrd` binary for the service definition.
    NoWyrdBinary,
    /// No `wyrd-supervise` wrapper for the restart policy.
    NoSuperviseBinary,
    /// Service definition installation failed.
    InstallService,
    /// Service enablement failed.
    EnableService,
    /// Service start (or load verification) failed.
    StartService,
    /// Service stop (disable) before a reconfiguration failed.
    StopService,
    /// The owner socket never verified this session.
    VerifySocket,
}

impl ProvisionFailureKind {
    /// The `stage` string the `ProvisioningIncomplete` sites report
    /// alongside this kind. `None` for the kinds that only occur
    /// post-registration in `provision()` (`RegisteredButUnavailable`
    /// carries no stage). Construction keeps the literal CLI strings;
    /// the kind test asserts they agree, so a future edit that
    /// changes one and not the other fails a test instead of
    /// splitting display from machine kind. `reconfigure()` fails
    /// some service kinds pre-write (binary lookup at validation,
    /// install before the rewrite), so those map to their stages
    /// even though `provision()` never reports them there.
    #[cfg(test)]
    fn stage(&self) -> Option<&'static str> {
        match self {
            ProvisionFailureKind::Validate
            | ProvisionFailureKind::NoWyrdBinary
            | ProvisionFailureKind::NoSuperviseBinary => Some("validate"),
            ProvisionFailureKind::DecodeInvitation => Some("initialize"),
            ProvisionFailureKind::Initialize => Some("initialize"),
            ProvisionFailureKind::VerifyOpen | ProvisionFailureKind::StoreLocked => {
                Some("verify-open")
            }
            ProvisionFailureKind::ReadRegistry => Some("read-registry"),
            ProvisionFailureKind::WriteRegistry => Some("write-registry"),
            ProvisionFailureKind::InstallService => Some("install-service"),
            ProvisionFailureKind::EnableService
            | ProvisionFailureKind::StartService
            | ProvisionFailureKind::VerifySocket => None,
            ProvisionFailureKind::StopService => Some("stop-service"),
        }
    }
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
    if let Err(error) = validate_entry(&entry.name, &entry) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: error.to_string(),
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
            kind: ProvisionFailureKind::Validate,
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
                    kind: ProvisionFailureKind::Initialize,
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
                        kind: ProvisionFailureKind::DecodeInvitation,
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
                        kind: ProvisionFailureKind::Initialize,
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
            kind: ProvisionFailureKind::VerifyOpen,
            reason: error.to_string(),
        };
    }

    // 3. Write the registry entry, carrying the skipped entries
    // forward so the caller surfaces them instead of silently
    // deleting rotten data on rewrite.
    let mut registry = match read_registry(&config.registry_path) {
        Ok(registry) => registry,
        Err(error) => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "read-registry",
                kind: ProvisionFailureKind::ReadRegistry,
                reason: error.to_string(),
            };
        }
    };
    registry.drives.insert(entry.name.clone(), entry.clone());
    if let Err(error) = write_registry(&config.registry_path, &registry.drives) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "write-registry",
            kind: ProvisionFailureKind::WriteRegistry,
            reason: error.to_string(),
        };
    }
    let skipped = registry.skipped;

    // 4. Install, enable, and start the per-drive service.
    if config.manage_service {
        let wyrd_bin = match &config.wyrd_bin {
            Some(path) => path.clone(),
            None => match sibling_bin("wyrd") {
                Some(path) => path,
                None => {
                    return ProvisionState::RegisteredButUnavailable {
                        entry,
                        skipped: skipped.clone(),
                        kind: ProvisionFailureKind::NoWyrdBinary,
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
                        skipped: skipped.clone(),
                        kind: ProvisionFailureKind::NoSuperviseBinary,
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
        let definition = match install_service_definition(&entry, &owner) {
            Ok(definition) => definition,
            Err((kind, reason)) => {
                return ProvisionState::RegisteredButUnavailable {
                    entry,
                    skipped: skipped.clone(),
                    kind,
                    reason,
                };
            }
        };
        if let Err((kind, reason)) = enable_and_start(&entry, &definition) {
            return ProvisionState::RegisteredButUnavailable {
                entry,
                skipped: skipped.clone(),
                kind,
                reason,
            };
        }
    }

    // 5. Verify the socket: the owner must answer terminally. Ready
    // means `Ok` or the pre-first-pass `Unobserved` refusal — every
    // other refusal shape is unexpected from this operation and
    // stays unavailable, never verified. `Unconfirmed` and
    // `Unavailable` likewise never verify: the outcome is unknown
    // or the owner is down.
    match verify_owner_socket(&config.drive_dir, config.verify_timeout, false) {
        Ok(()) => ProvisionState::VerifiedAvailable { entry, skipped },
        Err((kind, reason)) => ProvisionState::RegisteredButUnavailable {
            entry,
            skipped,
            kind,
            reason,
        },
    }
}

/// What a mode reconfiguration changes about one registered
/// drive. The entry is looked up by its drive directory — the one
/// identity the `drive` command already carries — and must resolve
/// to exactly one registry key, never a guess across duplicates.
pub struct ReconfigureConfig {
    pub drive_dir: PathBuf,
    /// Replacement relay set. `None` keeps the registered set; the
    /// CLI passes `Some` only when the operator named `--relay`
    /// flags, so a credential-path-only reconfigure never clears
    /// relays by accident.
    pub relays: Option<Vec<String>>,
    /// Target service mode, as the display string the registry
    /// carries. Only `vault` reconfigures today: any other value
    /// refuses with the installer-follow-up pointer before anything
    /// stops, exactly like provisioning — the mount template and
    /// enablement UX still travel with that follow-up.
    pub mode: String,
    pub identity: DeviceIdentitySecret,
    pub passphrase: String,
    pub registry_path: PathBuf,
    /// The `wyrd` binary whose `vault` subcommand the service runs.
    /// Defaults to the `wyrd` beside the reconfiguring command; the
    /// installer ships both together.
    pub wyrd_bin: Option<PathBuf>,
    /// `wyrd-supervise`: the wait-status mapper the restart policy
    /// rests on. Managing a service without it is refused — a bare
    /// owner binary cannot express the terminal distinction — so an
    /// explicit path is required wherever the layout differs.
    pub supervise_bin: Option<PathBuf>,
    /// Hardened credential files for the service definition,
    /// resolved the way `load_credentials` resolves them. Paths,
    /// never secrets, travel here.
    pub identity_file: PathBuf,
    pub passphrase_file: PathBuf,
    /// Explicit service definition path (tests); `None` resolves the
    /// platform path. Hermetic tests must never stop or clear a
    /// developer's real services, so they name a scratch path
    /// instead — mirroring the registry and binary overrides above.
    pub service_definition: Option<PathBuf>,
    /// Reinstall, enable, and restart the per-drive service after
    /// the rewrite. Off disables the old service, clears its
    /// definition, and rewrites the entry without installing a new
    /// one: registration only.
    pub manage_service: bool,
    /// How long one socket verification waits for a terminal answer.
    /// An expiry is `Unconfirmed`, never success.
    pub verify_timeout: Duration,
}

/// Reconfigure one registered drive in place: validate the target,
/// prove the keystore still opens (a live owner holding the store
/// refuses here — stop the service first, the same rule every other
/// offline writer states), read the registry, stop and clear the
/// old service definition, rewrite the entry carrying skipped
/// entries forward, reinstall and restart when managed, then verify
/// the socket. The fixed order keeps every refusal honest: the
/// registry is rewritten only after the old service is stopped,
/// and the drive is reported ready only when its socket verifies
/// this session — never because the entry was rewritten.
/// Install the per-drive service definition for a freshly
/// reconfigured (or newly provisioned) entry. Shared by both
/// handoffs so their install failures carry the same kind and
/// reason; the caller decides the state shape around it.
fn install_service_definition(
    entry: &RegistryEntry,
    owner: &SupervisedOwner,
) -> Result<PathBuf, (ProvisionFailureKind, String)> {
    install_service(entry, owner).map_err(|error| {
        (
            ProvisionFailureKind::InstallService,
            format!("install-service: {error}"),
        )
    })
}

/// Enable the installed definition and start it now. Shared by both
/// handoffs so their service failures carry the same kinds.
fn enable_and_start(
    entry: &RegistryEntry,
    definition: &Path,
) -> Result<(), (ProvisionFailureKind, String)> {
    if let Err(error) = enable_service(entry, definition) {
        return Err((
            ProvisionFailureKind::EnableService,
            format!("enable-service: {error}"),
        ));
    }
    if let Err(error) = start_service(entry) {
        return Err((
            ProvisionFailureKind::StartService,
            format!("start-service: {error}"),
        ));
    }
    Ok(())
}

/// Verify the owner socket answers terminally this session: `Ok` or
/// the pre-first-pass `Unobserved` refusal. Every other refusal
/// shape, `Unconfirmed`, and `Unavailable` never verify. When the
/// socket appeared while waiting (`retry_connect`), a freshly
/// started owner is still binding, so the connect retries until the
/// deadline; a stale pre-existing socket gets one terminal attempt,
/// like provisioning — the desktop's post-operation probe converges
/// the display from the observed state either way.
fn verify_owner_socket(
    drive_dir: &Path,
    verify_timeout: Duration,
    retry_connect: bool,
) -> Result<(), (ProvisionFailureKind, String)> {
    let missing = |reason: String| {
        (
            ProvisionFailureKind::VerifySocket,
            format!("verify-socket: {reason}"),
        )
    };
    let preexisting = crate::owner::socket_path_for(drive_dir).exists();
    if !wait_for_socket(drive_dir, verify_timeout) {
        return Err(missing("owner.sock never appeared".to_string()));
    }
    let retry = retry_connect && !preexisting;
    let deadline = std::time::Instant::now() + verify_timeout;
    loop {
        // Bound each attempt by the remaining budget so a wedged
        // owner cannot stretch one attempt past the deadline.
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let attempt_timeout = if retry {
            remaining.min(verify_timeout)
        } else {
            verify_timeout
        };
        let outcome = OwnerClient::connect_drive(drive_dir)
            .and_then(|client| client.with_timeout(attempt_timeout))
            .and_then(|mut client| client.observe());
        let refusal = match outcome {
            Ok(_) => return Ok(()),
            Err(ClientError::Refused(error)) if error.code == WireErrorCode::Unobserved => {
                return Ok(())
            }
            Err(error) => error.to_string(),
        };
        if !retry || std::time::Instant::now() >= deadline {
            return Err(missing(refusal));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Reconfigure one registered drive in place: validate the target,
/// read the registry and resolve the entry by directory, stop and
/// clear the old service definition, prove the keystore still opens,
/// install the new definition when managed, rewrite the entry, and
/// verify the socket. The fixed order keeps every refusal honest:
/// cheap checks precede the keystore open (no KDF on a refusal) and
/// the stop (a managed drive reaches its own stop instead of
/// refusing on the store lock); the registry is rewritten only after
/// the old service is stopped and the new definition installed; the
/// drive is reported ready only when its socket verifies this
/// session — never because the entry was rewritten. Every failure
/// past the stop is retried by re-running, which converges from the
/// stopped-and-cleared state (the operation is idempotent there).
pub fn reconfigure(config: ReconfigureConfig) -> ProvisionState {
    // 0. Validate the request before anything moves: no I/O, no
    // side effects, so a refusal touches nothing.
    if config.mode != "vault" {
        return ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: BootstrapError::UnknownServiceMode.to_string(),
        };
    }

    // 1. Read the registry and resolve the entry by directory. Zero
    // matches means the drive was never registered (or was
    // registered under another spelling — directories match by
    // exact path only); several means conflicting registrations
    // that only the operator can untangle, never this command.
    let mut registry = match read_registry(&config.registry_path) {
        Ok(registry) => registry,
        Err(error) => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "read-registry",
                kind: ProvisionFailureKind::ReadRegistry,
                reason: error.to_string(),
            };
        }
    };
    let mut matches: Vec<String> = registry
        .drives
        .iter()
        .filter(|(_, entry)| entry.drive_dir == config.drive_dir)
        .map(|(key, _)| key.clone())
        .collect();
    matches.sort();
    let key = match matches.len() {
        0 => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: format!(
                    "no registry entry for {}: register with `wyrd bootstrap` first",
                    config.drive_dir.display()
                ),
            };
        }
        1 => matches.pop().expect("one match"),
        _ => {
            return ProvisionState::ProvisioningIncomplete {
                stage: "validate",
                kind: ProvisionFailureKind::Validate,
                reason: format!(
                    "conflicting registry entries for {}: {} — resolve the duplicate by hand first",
                    config.drive_dir.display(),
                    matches.join(", ")
                ),
            };
        }
    };
    let mut entry = registry.drives.get(&key).expect("key resolved").clone();

    // 2. Candidate relays: flags replace the registered set, absence
    // keeps it — so a credential-path-only reconfigure never clears
    // relays by accident. Values validate before anything stops: a
    // bad relay refuses with the service still up.
    let relays = match config.relays {
        Some(relays) => relays,
        None => entry.relays.clone(),
    };
    if !valid_relays(&relays) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: BootstrapError::InvalidRelays.to_string(),
        };
    }
    if config.manage_service && relays.is_empty() {
        return ProvisionState::ProvisioningIncomplete {
            stage: "validate",
            kind: ProvisionFailureKind::Validate,
            reason: "managing a service needs at least one --relay: \
                a relay-less vault serves nothing peers can discover; \
                register with --no-service instead"
                .to_string(),
        };
    }

    // 3. Resolve the service binaries before stopping: a missing
    // binary refuses with the service still up, never with the drive
    // down. Same reason text as provisioning; the kind marks the
    // pre-stop validation site.
    let (wyrd_bin, supervise_bin) = if config.manage_service {
        let wyrd_bin = match &config.wyrd_bin {
            Some(path) => path.clone(),
            None => match sibling_bin("wyrd") {
                Some(path) => path,
                None => {
                    return ProvisionState::ProvisioningIncomplete {
                        stage: "validate",
                        kind: ProvisionFailureKind::NoWyrdBinary,
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
                    return ProvisionState::ProvisioningIncomplete {
                        stage: "validate",
                        kind: ProvisionFailureKind::NoSuperviseBinary,
                        reason: "no wyrd-supervise: pass --supervise-bin \
                            (the restart policy needs the wrapper; \
                            a bare owner binary cannot express it)"
                            .to_string(),
                    };
                }
            },
        };
        (Some(wyrd_bin), Some(supervise_bin))
    } else {
        (None, None)
    };

    // 4. Stop and clear the old service definition before touching
    // the store: a live managed owner stops here instead of failing
    // the open below on the store lock. A drive that was never
    // managed has no definition file — nothing to clear.
    if let Err(error) = disable_service(&entry, config.service_definition.as_ref()) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "stop-service",
            kind: ProvisionFailureKind::StopService,
            reason: format!("stop-service: {error}"),
        };
    }

    // 5. Prove the keystore opens. A manually supervised owner —
    // `wyrd vault` in a terminal, outside any manager — still holds
    // the lock here: stop it first, the same rule every other
    // offline writer states. Only the lock refusal carries the
    // terminal kind; every other open failure stays a usage error.
    if let Err(error) = Engine::open_keystore(
        config.drive_dir.clone(),
        &config.passphrase,
        config.identity.clone(),
    ) {
        let kind = if matches!(
            &error,
            EngineError::Durable(wyrd_sync::durable::DurableError::StoreLocked)
        ) {
            ProvisionFailureKind::StoreLocked
        } else {
            ProvisionFailureKind::VerifyOpen
        };
        return ProvisionState::ProvisioningIncomplete {
            stage: "verify-open",
            kind,
            reason: error.to_string(),
        };
    }

    // 6. Install the new definition when managed — before the
    // registry rewrite, so a crash between them leaves the new
    // definition beside the old entry (the next run reconciles
    // forward) rather than the old definition over the new entry.
    // Without management the definition is already cleared above.
    let definition = if config.manage_service {
        let owner = SupervisedOwner {
            supervise_bin: supervise_bin.expect("resolved when managed"),
            wyrd_bin: wyrd_bin.expect("resolved when managed"),
            identity_file: config.identity_file.clone(),
            passphrase_file: config.passphrase_file.clone(),
        };
        match install_service_definition(&entry, &owner) {
            Ok(definition) => Some(definition),
            Err((kind, reason)) => {
                return ProvisionState::ProvisioningIncomplete {
                    stage: "install-service",
                    kind,
                    reason,
                };
            }
        }
    } else {
        None
    };

    // 7. Rewrite the entry in place: the name, service label, and
    // directory stay put (identity never reconfigures); the relay
    // set follows the new configuration. Skipped entries carry
    // forward verbatim inside the write, so a future mode or a
    // rotten entry survives the rewrite and is surfaced by the
    // caller instead of silently discarded.
    entry.relays = relays;
    entry.mode = ServiceMode::Vault;
    registry.drives.insert(key, entry.clone());
    if let Err(error) = write_registry(&config.registry_path, &registry.drives) {
        return ProvisionState::ProvisioningIncomplete {
            stage: "write-registry",
            kind: ProvisionFailureKind::WriteRegistry,
            reason: error.to_string(),
        };
    }
    let skipped = registry.skipped;

    // 8. Enable, start, and verify when managed — the same handoff
    // provisioning runs. With `--no-service` and no manually
    // supervised owner, the socket never appears and readiness is
    // withheld — registered, never verified.
    if let Some(definition) = definition {
        if let Err((kind, reason)) = enable_and_start(&entry, &definition) {
            return ProvisionState::RegisteredButUnavailable {
                entry,
                skipped: skipped.clone(),
                kind,
                reason,
            };
        }
    }
    match verify_owner_socket(&config.drive_dir, config.verify_timeout, true) {
        Ok(()) => ProvisionState::VerifiedAvailable { entry, skipped },
        Err((kind, reason)) => ProvisionState::RegisteredButUnavailable {
            entry,
            skipped,
            kind,
            reason,
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
        assert!(refused.to_string().contains("version 999"), "{refused}");
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
        assert!(refused.to_string().contains("absolute"), "{refused}");
        assert!(!path.exists(), "no partial registry may remain");
    }

    /// Every reachable terminal provision site carries its
    /// machine-readable kind: direct callers match on `kind`, never
    /// on `reason` text. Rendered text is pinned where the CLI owns
    /// it (`crates/wyrd-cli/tests/bootstrap.rs`, contains-pins on
    /// the state lines); this test pins the mapping from site to
    /// kind, plus the `stage` agreement for the incomplete sites.
    #[test]
    fn provision_failures_carry_their_machine_readable_kind() {
        use ProvisionFailureKind as Kind;
        // Validation refuses before anything is created.
        let dir = scratch("kind-validate");
        let registry = dir.join("drives.json");
        let mut config = provision_no_service(&dir, &registry, "BAD NAME!");
        config.name = "BAD NAME!".to_string();
        match provision(config) {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, Kind::Validate);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("validation must stop first: {other:?}"),
        }
        // An undecodable invitation stops at initialization.
        let dir = scratch("kind-invitation");
        let registry = dir.join("drives.json");
        let mut config = provision_no_service(&dir, &registry, "demo");
        config.init = Init::Join {
            sealed_invitation: b"not a sealed invitation".to_vec(),
        };
        match provision(config) {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, Kind::DecodeInvitation);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("a bad invitation must stop at initialize: {other:?}"),
        }
        // A drive directory that is a regular file cannot be
        // initialized as a store.
        let dir = scratch("kind-initialize");
        let blocker = dir.join("file");
        std::fs::write(&blocker, b"x").unwrap();
        let registry = dir.join("drives.json");
        let mut config = provision_no_service(&dir, &registry, "demo");
        config.drive_dir = blocker;
        match provision(config) {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, Kind::Initialize);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("a file cannot be initialized: {other:?}"),
        }
        // A registry path that is a directory cannot be read.
        let dir = scratch("kind-read-registry");
        let config = provision_no_service(&dir, &dir, "demo");
        match provision(config) {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, Kind::ReadRegistry);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("an unreadable registry must stop: {other:?}"),
        }
        // Managed-service binary lookup stops before any service
        // operation: this is pinned by construction, not by test —
        // exercising it end to end would install a real service
        // definition whenever the test binary has version-matched
        // siblings, so `NoWyrdBinary` and `NoSuperviseBinary` stay
        // literal-adjacent to their reason strings like
        // `WriteRegistry` and `VerifyOpen` below. (`WriteRegistry`
        // needs a write failure after a successful read of the same
        // path, and `VerifyOpen` needs a drive that creates but does
        // not open; neither is reachable hermetically.)
        // No owner socket verifies nothing, with the socket kind.
        let dir = scratch("kind-socket");
        let registry = dir.join("drives.json");
        match provision(provision_no_service(&dir, &registry, "demo")) {
            ProvisionState::RegisteredButUnavailable { kind, reason, .. } => {
                assert_eq!(kind, Kind::VerifySocket);
                assert!(reason.contains("verify-socket"), "{reason}");
            }
            other => panic!("a missing owner must never verify: {other:?}"),
        }
    }

    /// `run` reports spawn and command failures with their program
    /// and detail: missing programs never spawn, failing programs
    /// report their own text. The service-stage `ProvisionFailureKind`
    /// variants (`NoWyrdBinary`, `NoSuperviseBinary`,
    /// `InstallService`, `EnableService`, `StartService`,
    /// `StopService`) and the service-manager `BootstrapError`
    /// variants beneath them (`Spawn`, `CommandFailed`,
    /// `ReloadUnits`, `AgentNotLoaded`, `RestartService`,
    /// `StopService`) stay literal-adjacent to their reason
    /// strings — reaching them end to end would touch a real
    /// manager, as the provision kind test documents.
    #[test]
    fn service_commands_report_spawn_and_command_failures() {
        // `run` itself: missing program spawns nothing, a failing
        // program reports its own text.
        match run("wyrd-definitely-missing-binary-xyz", &[]) {
            Err(BootstrapError::Spawn { program, .. }) => {
                assert_eq!(program, "wyrd-definitely-missing-binary-xyz");
            }
            other => panic!("a missing program must fail spawn: {other:?}"),
        }
        match run("sh", &["-c", "exit 3"]) {
            Err(BootstrapError::CommandFailed {
                program, detail, ..
            }) => {
                assert_eq!(program, "sh");
                assert!(detail.is_empty(), "exit 3 carries no stderr: {detail:?}");
            }
            other => panic!("a failing program must report: {other:?}"),
        }
    }

    #[test]
    fn registry_missing_file_is_an_empty_registry() {
        let dir = scratch("missing");
        let read = read_registry(&dir.join("absent.json")).unwrap();
        assert!(read.drives.is_empty() && read.skipped.is_empty());
    }

    /// Every `Display` string pinned exactly: a future edit that
    /// changes user-visible text fails here. Variants over foreign
    /// errors (`Parse`, `Encode`) pin the format around the live
    /// source error instead of the dependency's wording.
    #[test]
    fn bootstrap_display_reproduces_the_historical_messages_exactly() {
        use std::io::{Error as IoError, ErrorKind};
        let io = |kind: ErrorKind, message: &str| IoError::new(kind, message.to_owned());
        let parse_source = serde_json::from_slice::<serde_json::Value>(b"xx").unwrap_err();
        struct AlwaysFails;
        impl Serialize for AlwaysFails {
            fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("boom"))
            }
        }
        let encode_source = serde_json::to_vec(&AlwaysFails).unwrap_err();
        let command = BootstrapError::CommandFailed {
            program: "launchctl".to_string(),
            args: "list missing".to_string(),
            detail: "No such process".to_string(),
        };
        let cases: Vec<(BootstrapError, String)> = vec![
            (
                BootstrapError::Read {
                    path: "/reg/drives.json".into(),
                    source: io(ErrorKind::NotFound, "missing"),
                },
                "read /reg/drives.json: missing".to_string(),
            ),
            (
                BootstrapError::Parse {
                    path: "/reg/drives.json".into(),
                    source: parse_source,
                },
                format!(
                    "parse /reg/drives.json: {}",
                    serde_json::from_slice::<serde_json::Value>(b"xx").unwrap_err()
                ),
            ),
            (
                BootstrapError::UnsupportedVersion {
                    path: "/reg/drives.json".into(),
                    found: 999,
                },
                format!("registry /reg/drives.json is version 999, this build reads {REGISTRY_VERSION}"),
            ),
            (
                BootstrapError::KeyMismatch {
                    key: "a".to_string(),
                    name: "b".to_string(),
                },
                "key \"a\" does not match entry name \"b\"".to_string(),
            ),
            (
                BootstrapError::InvalidName {
                    name: "BAD NAME!".to_string(),
                },
                "name \"BAD NAME!\" must be 1-64 chars of [a-z0-9-]".to_string(),
            ),
            (
                BootstrapError::NameTooLongForServicePrefix {
                    name: "a-very-long-drive-name-that-leaves-no-room-at-all-xyz".to_string(),
                },
                "name \"a-very-long-drive-name-that-leaves-no-room-at-all-xyz\" leaves no room for the drive- service prefix (max 58 chars)".to_string(),
            ),
            (
                BootstrapError::InvalidService {
                    service: "BAD!".to_string(),
                },
                "service \"BAD!\" must be 1-64 chars of [a-z0-9-]".to_string(),
            ),
            (
                BootstrapError::RelativeDriveDir {
                    dir: "relative/path".into(),
                },
                "drive_dir relative/path must be absolute".to_string(),
            ),
            (
                BootstrapError::DriveDirHasDotDot {
                    dir: "/reg/../drive".into(),
                },
                "drive_dir /reg/../drive must not contain ..".to_string(),
            ),
            (BootstrapError::UnknownServiceMode, "unknown service mode: registry-managed mount mode travels with the installer follow-up (P2 landed: manual `wyrd mount` serves the socket)".to_string()),
            (
                BootstrapError::InvalidRelays,
                "relays must be non-empty URLs without whitespace or control bytes".to_string(),
            ),
            (
                BootstrapError::DriveDirHasControlChars {
                    dir: "/reg/drive".into(),
                },
                "drive_dir /reg/drive must not contain control characters".to_string(),
            ),
            (
                BootstrapError::RefusedWrite {
                    source: Box::new(BootstrapError::RelativeDriveDir {
                        dir: "relative/path".into(),
                    }),
                },
                "refusing to write: drive_dir relative/path must be absolute".to_string(),
            ),
            (
                BootstrapError::Encode {
                    source: encode_source,
                },
                "encode registry: boom".to_string(),
            ),
            (
                BootstrapError::CreateDir {
                    path: "/reg".into(),
                    source: io(ErrorKind::PermissionDenied, "denied"),
                },
                "create /reg: denied".to_string(),
            ),
            (
                BootstrapError::WriteFile {
                    path: "/reg/drives.json".into(),
                    source: io(ErrorKind::StorageFull, "full"),
                },
                "write /reg/drives.json: full".to_string(),
            ),
            (
                BootstrapError::ReplaceFile {
                    path: "/reg/drives.json".into(),
                    source: io(ErrorKind::PermissionDenied, "denied"),
                },
                "replace /reg/drives.json: denied".to_string(),
            ),
            (
                BootstrapError::Chmod {
                    path: "/reg/drives.json".into(),
                    source: io(ErrorKind::PermissionDenied, "denied"),
                },
                "chmod /reg/drives.json: denied".to_string(),
            ),
            (
                BootstrapError::SyncFile {
                    path: "/reg/drives.json".into(),
                    source: io(ErrorKind::Other, "sync lost"),
                },
                "sync /reg/drives.json: sync lost".to_string(),
            ),
            (
                BootstrapError::RemoveFile {
                    path: "/reg/drive-demo.plist".into(),
                    source: io(ErrorKind::PermissionDenied, "denied"),
                },
                "remove /reg/drive-demo.plist: denied".to_string(),
            ),
            (
                BootstrapError::Spawn {
                    program: "launchctl".to_string(),
                    source: io(ErrorKind::NotFound, "missing"),
                },
                "spawn launchctl: missing".to_string(),
            ),
            (
                BootstrapError::CommandFailed {
                    program: "launchctl".to_string(),
                    args: "list missing".to_string(),
                    detail: "No such process".to_string(),
                },
                "launchctl list missing failed: No such process".to_string(),
            ),
            (
                BootstrapError::EnableService {
                    name: "md.enesis.wyrd.drive-demo".to_string(),
                    source: Box::new(command),
                },
                "enable md.enesis.wyrd.drive-demo: launchctl list missing failed: No such process"
                    .to_string(),
            ),
            (
                BootstrapError::ReloadUnits {
                    source: Box::new(BootstrapError::Spawn {
                        program: "systemctl".to_string(),
                        source: io(ErrorKind::NotFound, "missing"),
                    }),
                },
                "reload user units: spawn systemctl: missing".to_string(),
            ),
            (
                BootstrapError::AgentNotLoaded {
                    label: "md.enesis.wyrd.drive-demo".to_string(),
                    source: Box::new(BootstrapError::CommandFailed {
                        program: "launchctl".to_string(),
                        args: "list md.enesis.wyrd.drive-demo".to_string(),
                        detail: "No such process".to_string(),
                    }),
                },
                "start md.enesis.wyrd.drive-demo: agent not loaded: launchctl list md.enesis.wyrd.drive-demo failed: No such process".to_string(),
            ),
            (
                BootstrapError::RestartService {
                    unit: "wyrd-drive-demo.service".to_string(),
                    source: Box::new(BootstrapError::Spawn {
                        program: "systemctl".to_string(),
                        source: io(ErrorKind::NotFound, "missing"),
                    }),
                },
                "restart wyrd-drive-demo.service: spawn systemctl: missing".to_string(),
            ),
            (
                BootstrapError::StopService {
                    name: "wyrd-drive-demo.service".to_string(),
                    source: Box::new(BootstrapError::Spawn {
                        program: "systemctl".to_string(),
                        source: io(ErrorKind::NotFound, "missing"),
                    }),
                },
                "stop wyrd-drive-demo.service: spawn systemctl: missing".to_string(),
            ),
            (
                BootstrapError::ServicesUnsupported,
                "per-drive services are implemented for Unix (launchd agent, systemd user unit)"
                    .to_string(),
            ),
        ];
        assert_eq!(cases.len(), 28, "every variant pins its message");
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected, "Display changed");
        }
    }

    #[test]
    fn bootstrap_failures_match_by_variant_not_by_text() {
        let dir = scratch("typed");
        let mut bad = entry("bad", &dir);
        bad.name = "BAD NAME!".to_string();
        let error = validate_entry("BAD NAME!", &bad).unwrap_err();
        assert!(
            matches!(error, BootstrapError::InvalidName { .. }),
            "callers branch on the variant, never parse the message: {error}"
        );
        // The text readers keep working: Display is unchanged.
        assert!(
            error
                .to_string()
                .contains("must be 1-64 chars of [a-z0-9-]"),
            "{error}"
        );
        let nested = BootstrapError::RefusedWrite {
            source: Box::new(error),
        };
        assert!(
            nested.to_string().starts_with("refusing to write: "),
            "{nested}"
        );
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
                    "v": crate::owner::WIRE_VERSION,
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
        let ProvisionState::VerifiedAvailable { entry, .. } = report else {
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
        let ProvisionState::RegisteredButUnavailable { entry, reason, .. } = report else {
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

    pub(super) fn reconfigure_no_service(
        dir: &Path,
        registry: &Path,
        relays: Option<Vec<String>>,
        mode: &str,
    ) -> ReconfigureConfig {
        ReconfigureConfig {
            drive_dir: dir.join("drive"),
            relays,
            mode: mode.to_string(),
            identity: identity(),
            passphrase: "bootstrap-test-pass".to_string(),
            registry_path: registry.to_path_buf(),
            wyrd_bin: Some(dir.join("wyrd")),
            supervise_bin: Some(dir.join("wyrd-supervise")),
            identity_file: dir.join("identity"),
            passphrase_file: dir.join("passphrase"),
            // A scratch definition that never exists: hermetic tests
            // must stop and clear nothing real, and the missing file
            // is the not-managed case — never a manager touch.
            service_definition: Some(dir.join("wyrd-test-definition")),
            manage_service: false,
            verify_timeout: Duration::from_secs(10),
        }
    }

    /// Reconfigure rewrites the entry and verifies the socket: the
    /// relay set is replaced, the stub owner's pre-first-pass
    /// refusal verifies like provisioning, and the registry carries
    /// the new set.
    #[test]
    fn reconfigure_rewrites_relays_and_verifies_the_socket() {
        let dir = scratch("reconfigure");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(
            matches!(first, ProvisionState::RegisteredButUnavailable { .. }),
            "no owner serves the fresh drive: {first:?}"
        );
        let sock = dir.join("drive").join("owner.sock");
        let server = stub_owner(sock, 1, "Unobserved");
        let report = reconfigure(reconfigure_no_service(
            &dir,
            &registry,
            Some(vec!["relay://new".to_string()]),
            "vault",
        ));
        let ProvisionState::VerifiedAvailable { entry, .. } = report else {
            panic!("a stubbed owner must verify: {report:?}");
        };
        assert_eq!(entry.relays, vec!["relay://new".to_string()]);
        assert!(matches!(entry.mode, ServiceMode::Vault));
        let read = read_registry(&registry).unwrap();
        assert_eq!(read.drives["demo"].relays, vec!["relay://new".to_string()]);
        server.join().unwrap();
    }

    /// `None` relays keep the registered set: a credential-path-only
    /// reconfigure never clears relays by accident.
    #[test]
    fn reconfigure_keeps_relays_when_none_given() {
        let dir = scratch("reconf-keep");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        // Hand the entry a relay set, then reconfigure without one.
        let mut read = read_registry(&registry).unwrap();
        read.drives.get_mut("demo").unwrap().relays = vec!["relay://kept".to_string()];
        write_registry(&registry, &read.drives).unwrap();
        let sock = dir.join("drive").join("owner.sock");
        let server = stub_owner(sock, 1, "Unobserved");
        let report = reconfigure(reconfigure_no_service(&dir, &registry, None, "vault"));
        let ProvisionState::VerifiedAvailable { entry, .. } = report else {
            panic!("a stubbed owner must verify: {report:?}");
        };
        assert_eq!(entry.relays, vec!["relay://kept".to_string()]);
        server.join().unwrap();
    }

    /// Non-vault modes refuse before anything stops: the registry
    /// keeps the old entry and no service state is touched — the
    /// mount template still travels with the installer follow-up.
    #[test]
    fn reconfigure_refuses_non_vault_modes_before_stopping() {
        let dir = scratch("reconfigure-mode");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let before = std::fs::read(&registry).unwrap();
        let report = reconfigure(reconfigure_no_service(&dir, &registry, None, "mount"));
        match report {
            ProvisionState::ProvisioningIncomplete {
                kind,
                stage,
                reason,
            } => {
                assert_eq!(kind, ProvisionFailureKind::Validate);
                assert_eq!(Some(stage), kind.stage());
                assert!(
                    reason.contains("installer follow-up"),
                    "refusal points at the follow-up: {reason}"
                );
            }
            other => panic!("a mount mode must refuse: {other:?}"),
        }
        assert_eq!(std::fs::read(&registry).unwrap(), before);
        assert_eq!(
            Some("stop-service"),
            ProvisionFailureKind::StopService.stage()
        );
    }

    /// No entry for the directory refuses without writing: the drive
    /// exists (verify-open passes) but was registered elsewhere —
    /// directories match by exact path only.
    #[test]
    fn reconfigure_refuses_an_unregistered_directory() {
        let dir = scratch("reconfigure-elsewhere");
        let registry = dir.join("drives.json");
        let other = dir.join("other.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let report = reconfigure(reconfigure_no_service(&dir, &other, None, "vault"));
        match report {
            ProvisionState::ProvisioningIncomplete {
                kind,
                stage,
                reason,
            } => {
                assert_eq!(kind, ProvisionFailureKind::Validate);
                assert_eq!(Some(stage), kind.stage());
                assert!(reason.contains("no registry entry"), "{reason}");
            }
            other => panic!("an unregistered directory must refuse: {other:?}"),
        }
        assert!(!other.exists(), "a refusal writes nothing");
    }

    /// Duplicate registrations for one directory refuse loudly with
    /// both key names: the command never guesses across conflicts.
    #[test]
    fn reconfigure_refuses_conflicting_registrations() {
        let dir = scratch("reconfigure-dupe");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let raw = serde_json::json!({
            "version": 1,
            "drives": {
                "demo": {
                    "name": "demo",
                    "drive_dir": dir.join("drive").to_string_lossy(),
                    "relays": [],
                    "service": "drive-demo",
                    "mode": "vault",
                },
                "alias": {
                    "name": "alias",
                    "drive_dir": dir.join("drive").to_string_lossy(),
                    "relays": [],
                    "service": "drive-alias",
                    "mode": "vault",
                },
            },
        });
        std::fs::write(&registry, serde_json::to_vec(&raw).unwrap()).unwrap();
        let report = reconfigure(reconfigure_no_service(&dir, &registry, None, "vault"));
        match report {
            ProvisionState::ProvisioningIncomplete { kind, reason, .. } => {
                assert_eq!(kind, ProvisionFailureKind::Validate);
                assert!(
                    reason.contains("demo") && reason.contains("alias"),
                    "conflict names both keys: {reason}"
                );
            }
            other => panic!("a duplicate registration must refuse: {other:?}"),
        }
        // Both entries survive the refusal.
        let read = read_registry(&registry).unwrap();
        assert!(read.drives.contains_key("demo") && read.drives.contains_key("alias"));
    }

    /// A wrong passphrase fails the open before the registry is
    /// read: the file keeps its bytes exactly.
    #[test]
    fn reconfigure_verify_open_failure_leaves_the_registry_untouched() {
        let dir = scratch("reconfigure-locked");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let before = std::fs::read(&registry).unwrap();
        let mut config = reconfigure_no_service(&dir, &registry, None, "vault");
        config.passphrase = "wrong-pass".to_string();
        match reconfigure(config) {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, ProvisionFailureKind::VerifyOpen);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("a wrong passphrase must stop at verify-open: {other:?}"),
        }
        assert_eq!(std::fs::read(&registry).unwrap(), before);
    }

    /// A managed reconfigure without relays refuses at validation,
    /// before any service state moves: reaching the stop/install
    /// steps end to end would touch a real manager, so the managed
    /// path past validation stays literal-adjacent like the
    /// provision kind test — hermetic tests stop here.
    #[test]
    fn reconfigure_refuses_a_relay_less_managed_service() {
        let dir = scratch("reconfigure-norelay");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let mut config = reconfigure_no_service(&dir, &registry, Some(Vec::new()), "vault");
        config.manage_service = true;
        match reconfigure(config) {
            ProvisionState::ProvisioningIncomplete {
                kind,
                stage,
                reason,
            } => {
                assert_eq!(kind, ProvisionFailureKind::Validate);
                assert_eq!(Some(stage), kind.stage());
                assert!(reason.contains("--no-service"), "{reason}");
            }
            other => panic!("a relay-less managed reconfigure must refuse: {other:?}"),
        }
    }

    /// A rewrite carries skipped entries forward verbatim: a future
    /// `mount` entry survives a vault reconfigure with its bytes
    /// intact, and the caller still warns about the skip instead of
    /// silently forgetting the entry.
    #[test]
    fn reconfigure_preserves_skipped_entries_verbatim() {
        let dir = scratch("rec-presv");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        // A future-mode entry beside the managed one, staged by hand
        // like a newer writer would leave it.
        let future_raw = serde_json::json!({
            "name": "future",
            "drive_dir": dir.join("drive").to_string_lossy(),
            "relays": ["relay://future"],
            "service": "drive-future",
            "mode": "mount",
        });
        let mut raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
        raw["drives"]["future"] = future_raw.clone();
        std::fs::write(&registry, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
        let sock = dir.join("drive").join("owner.sock");
        let server = stub_owner(sock, 1, "Unobserved");
        let report = reconfigure(reconfigure_no_service(
            &dir,
            &registry,
            Some(vec!["relay://new".to_string()]),
            "vault",
        ));
        let ProvisionState::VerifiedAvailable { entry, skipped } = report else {
            panic!("a stubbed owner must verify: {report:?}");
        };
        assert_eq!(entry.relays, vec!["relay://new".to_string()]);
        assert!(
            skipped.iter().any(|reason| reason.contains("future")),
            "the skip is still surfaced: {skipped:?}"
        );
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
        assert_eq!(
            after["drives"]["future"], future_raw,
            "the unknown entry survives byte-identical"
        );
        assert_eq!(
            after["drives"]["demo"]["relays"],
            serde_json::json!(["relay://new"])
        );
        server.join().unwrap();
    }

    /// `--no-service` disables and clears: the definition file staged
    /// at the override path is gone after the run, and the entry is
    /// rewritten. The override keeps the test hermetic — the
    /// list/is-enabled probes only read the manager, and removal
    /// touches the scratch file alone.
    #[test]
    fn reconfigure_no_service_removes_the_definition_file() {
        let dir = scratch("rec-clear");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        // A leftover definition from the managed past the entry
        // remembers, at the path the test override names.
        let definition = dir.join("wyrd-test-definition");
        std::fs::write(&definition, "leftover").unwrap();
        let sock = dir.join("drive").join("owner.sock");
        let server = stub_owner(sock, 1, "Unobserved");
        let report = reconfigure(reconfigure_no_service(&dir, &registry, None, "vault"));
        let ProvisionState::VerifiedAvailable { entry, .. } = report else {
            panic!("a stubbed owner must verify: {report:?}");
        };
        assert!(!definition.exists(), "the definition is cleared");
        assert!(entry.relays.is_empty());
        server.join().unwrap();
    }

    /// Invalid relay values refuse before anything stops: the staged
    /// definition file survives, and the registry keeps its bytes.
    #[test]
    fn reconfigure_validates_relays_before_stopping() {
        let dir = scratch("reconf-badrelay");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let definition = dir.join("wyrd-test-definition");
        std::fs::write(&definition, "leftover").unwrap();
        let before = std::fs::read(&registry).unwrap();
        let report = reconfigure(reconfigure_no_service(
            &dir,
            &registry,
            Some(vec!["not a relay".to_string()]),
            "vault",
        ));
        match report {
            ProvisionState::ProvisioningIncomplete {
                kind,
                stage,
                reason,
            } => {
                assert_eq!(kind, ProvisionFailureKind::Validate);
                assert_eq!(Some(stage), kind.stage());
                assert!(reason.contains("relays"), "{reason}");
            }
            other => panic!("a bad relay must refuse: {other:?}"),
        }
        assert!(definition.exists(), "the stop never ran");
        assert_eq!(std::fs::read(&registry).unwrap(), before);
    }

    /// A live owner holding the store fails the open with the
    /// terminal kind: the registry keeps its bytes, and the caller
    /// can return the held-drive exit instead of a usage error.
    #[test]
    fn reconfigure_reports_a_held_drive() {
        let dir = scratch("reconf-held");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let before = std::fs::read(&registry).unwrap();
        // Hold the store open: a second opener fails the lock, the
        // same contention the sync lock test pins.
        let held =
            Engine::open_keystore(dir.join("drive"), "bootstrap-test-pass", identity()).unwrap();
        let report = reconfigure(reconfigure_no_service(&dir, &registry, None, "vault"));
        match report {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, ProvisionFailureKind::StoreLocked);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("a held drive must stop at verify-open: {other:?}"),
        }
        assert_eq!(std::fs::read(&registry).unwrap(), before);
        drop(held);
    }

    /// A socket that appears while waiting retries the connect until
    /// the deadline: a freshly restarted owner still binding does not
    /// read as unavailable.
    #[test]
    fn reconfigure_waits_for_a_late_owner_socket() {
        let dir = scratch("reconf-late");
        let registry = dir.join("drives.json");
        let first = provision(provision_no_service(&dir, &registry, "demo"));
        assert!(matches!(
            first,
            ProvisionState::RegisteredButUnavailable { .. }
        ));
        let sock = dir.join("drive").join("owner.sock");
        let server = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            stub_owner(sock, 1, "Unobserved").join().unwrap();
        });
        let report = reconfigure(reconfigure_no_service(&dir, &registry, None, "vault"));
        let ProvisionState::VerifiedAvailable { .. } = report else {
            panic!("a late owner must still verify: {report:?}");
        };
        server.join().unwrap();
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
        owned.relays = vec!["relay://exa mple".to_string(), "relay://%i".to_string()];
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
        assert!(
            unit.contains("\"relay://%%i\""),
            "relay specifiers are doubled, got:\n{unit}"
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
        match report {
            ProvisionState::ProvisioningIncomplete { kind, stage, .. } => {
                assert_eq!(kind, ProvisionFailureKind::Validate);
                assert_eq!(Some(stage), kind.stage());
            }
            other => panic!("a relay-less managed service is refused, not installed: {other:?}"),
        }
        assert!(
            !registry.exists(),
            "refusal precedes registration side effects"
        );
    }
}
