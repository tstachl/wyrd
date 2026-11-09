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
/// shared composition or marks mount non-desktop. A `mount` entry is
/// refused with that pointer, never silently run headless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceMode {
    Vault,
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
    if entry.relays.iter().any(|relay| relay.is_empty()) {
        return Err("relays must not contain empty entries".to_string());
    }
    Ok(())
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
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &bytes).map_err(|error| format!("write {}: {error}", tmp.display()))?;
    sync_file(&tmp)?;
    std::fs::rename(&tmp, path).map_err(|error| format!("replace {}: {error}", path.display()))?;
    sync_dir(path);
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

/// Render the per-drive launchd agent property list. The service runs
/// the owner binary against the drive directory, keeps it alive on
/// crash (never on the terminal held-drive status — that contract
/// belongs to the supervisor wrapper), and throttles restarts.
pub fn render_launchd_plist(label: &str, owner_bin: &Path, entry: &RegistryEntry) -> String {
    let mut argv = format!(
        "    <string>{}</string>\n    <string>{}</string>\n",
        owner_bin.display(),
        entry.drive_dir.display()
    );
    for relay in &entry.relays {
        argv.push_str(&format!(
            "    <string>--relay</string>\n    <string>{relay}</string>\n"
        ));
    }
    if entry.relays.is_empty() {
        argv.push_str("    <string>--loopback</string>\n");
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
            "        <false/>\n",
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
        label = label,
        argv = argv,
        log = entry.drive_dir.join("service.log").display(),
    )
}

/// Render the per-drive systemd user unit. Restart on failure with
/// backoff and a start limit, mirroring the `nix/drive` supervision
/// contract: terminal statuses never restart, crashes do.
pub fn render_systemd_unit(description: &str, owner_bin: &Path, entry: &RegistryEntry) -> String {
    let mut exec = format!(
        "ExecStart={} {}",
        owner_bin.display(),
        entry.drive_dir.display()
    );
    for relay in &entry.relays {
        exec.push_str(&format!(" --relay {relay}"));
    }
    if entry.relays.is_empty() {
        exec.push_str(" --loopback");
    }
    format!(
        concat!(
            "[Unit]\n",
            "Description={description}\n",
            "After=network-online.target\n",
            "Wants=network-online.target\n\n",
            "[Service]\n",
            "Type=simple\n",
            "{exec}\n",
            "Restart=on-failure\n",
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
/// return its path. Installation writes the definition; enabling
/// and starting are separate steps so registration can report
/// exactly which one failed.
#[cfg(unix)]
pub fn install_service(entry: &RegistryEntry, owner_bin: &Path) -> Result<PathBuf, String> {
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
        std::fs::write(&path, render_launchd_plist(&label, owner_bin, entry))
            .map_err(|error| format!("write {}: {error}", path.display()))?;
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
        std::fs::write(
            &path,
            render_systemd_unit(&format!("Wyrd drive {}", entry.name), owner_bin, entry),
        )
        .map_err(|error| format!("write {}: {error}", path.display()))?;
        return Ok(path);
    }
}

#[cfg(not(unix))]
pub fn install_service(_entry: &RegistryEntry, _owner_bin: &Path) -> Result<PathBuf, String> {
    Err("per-drive services are implemented for Unix (launchd agent, systemd user unit)".into())
}

/// Enable the installed service for startup-at-login. Separate from
/// installation and from starting, so each failure reports its own
/// stage.
#[cfg(unix)]
pub fn enable_service(entry: &RegistryEntry, definition: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{}", entry.service);
        // `load -w` both loads now and persists across login; a
        // disabled leftover re-enables rather than duplicating.
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

/// Start the enabled service now.
#[cfg(unix)]
pub fn start_service(entry: &RegistryEntry) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let label = format!("md.enesis.wyrd.{}", entry.service);
        // `load -w` above already starts persistent agents; kickstart
        // covers the already-loaded case without erroring the flow.
        let uid = user_id()?;
        match run("launchctl", &["kickstart", &format!("gui/{uid}/{label}")]) {
            Ok(()) => Ok(()),
            Err(error) => Err(format!("start {label}: {error}")),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = format!("wyrd-{}.service", entry.service);
        run("systemctl", &["--user", "start", &unit])
            .map_err(|error| format!("start {unit}: {error}"))?;
        Ok(())
    }
}

#[cfg(all(unix, target_os = "macos"))]
fn user_id() -> Result<String, String> {
    // uid of the invoking user: the agent domain bootstrap runs in.
    // Read from `id`, not from process introspection, so this
    // module carries no unsafe code.
    let output = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map_err(|error| format!("spawn id: {error}"))?;
    if !output.status.success() {
        return Err("id -u failed".to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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
    pub owner_bin: Option<PathBuf>,
    /// Install and enable the OS service. Off skips both (and the
    /// start below): registration only, for operators who supervise
    /// the owner themselves.
    pub manage_service: bool,
    /// Start the service after enabling. Off registers and enables
    /// without starting now.
    pub start_now: bool,
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

    // 1. Initialize, or reconcile an existing drive. `DriveExists`
    // means a previous run (or another operator) already created
    // it: re-verify the open instead of re-creating.
    let initialized = match &config.init {
        Init::Create => match Engine::create(
            config.drive_dir.clone(),
            &config.passphrase,
            config.identity.clone(),
        ) {
            Ok(_) => true,
            Err(EngineError::DriveExists) => false,
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
                Ok(_) => true,
                Err(EngineError::DriveExists) => false,
                Err(error) => {
                    return ProvisionState::ProvisioningIncomplete {
                        stage: "initialize",
                        reason: error.to_string(),
                    };
                }
            }
        }
    };
    let _ = initialized;

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
        let owner_bin = match &config.owner_bin {
            Some(path) => path.clone(),
            None => match sibling_owner_bin() {
                Some(path) => path,
                None => {
                    return ProvisionState::RegisteredButUnavailable {
                        entry,
                        reason: "no owner binary: pass --owner-bin (no wyrd-host beside wyrd)"
                            .to_string(),
                    };
                }
            },
        };
        let definition = match install_service(&entry, &owner_bin) {
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
        if config.start_now {
            if let Err(reason) = start_service(&entry) {
                return ProvisionState::RegisteredButUnavailable {
                    entry,
                    reason: format!("start-service: {reason}"),
                };
            }
        } else {
            return ProvisionState::RegisteredButUnavailable {
                entry,
                reason: "service installed and enabled, not started (--no-start)".to_string(),
            };
        }
    }

    // 5. Verify the socket: connect and demand a terminal answer.
    // Ok and Refused both prove the owner serves; only a terminal
    // refusal counts as verified — Unconfirmed and Unavailable mean
    // the outcome is unknown or the owner is down.
    match OwnerClient::connect_drive(&config.drive_dir)
        .and_then(|client| client.with_timeout(config.verify_timeout))
    {
        Ok(mut client) => match client.observe() {
            Ok(_) => ProvisionState::VerifiedAvailable { entry },
            Err(ClientError::Refused(_)) => ProvisionState::VerifiedAvailable { entry },
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

/// Locate `wyrd-host` beside the running executable: the installer
/// ships both binaries together, so the sibling is the default
/// owner. Overridable with an explicit path wherever the layout
/// differs.
fn sibling_owner_bin() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let dir = current.parent()?;
    let candidate = dir.join("wyrd-host");
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

    fn scratch(case: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("wyrd-bootstrap-{case}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(name: &str, dir: &Path) -> RegistryEntry {
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
        let owner = PathBuf::from("/opt/wyrd/bin/wyrd-host");
        let owned = entry("demo", &dir);
        let plist = render_launchd_plist("md.enesis.wyrd.drive-demo", &owner, &owned);
        assert!(plist.contains("<string>md.enesis.wyrd.drive-demo</string>"));
        assert!(plist.contains(&*owner.display().to_string()));
        assert!(plist.contains(&*owned.drive_dir.display().to_string()));
        assert!(plist.contains("--relay"));
        let unit = render_systemd_unit("Wyrd drive demo", &owner, &owned);
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains(&*owned.drive_dir.display().to_string()));
    }

    /// A stub owner socket: answers every request with the terminal
    /// `Unobserved` refusal. It speaks the real framed protocol, so
    /// the handoff test exercises the real client, the real framing,
    /// and the real refusal mapping — only the owner is fake. The
    /// accept loop is deadline-bounded so the thread always returns
    /// for `join`, even when the caller makes fewer connections than
    /// the cap.
    fn stub_owner(sock: PathBuf, max_requests: usize) -> std::thread::JoinHandle<()> {
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
                    "result": {"err": {"code": "Unobserved", "message": "no pass yet"}},
                });
                let body = serde_json::to_vec(&reply).unwrap();
                let _ = stream.write_all(&(body.len() as u32).to_be_bytes());
                let _ = stream.write_all(&body);
            }
        })
    }

    fn identity() -> DeviceIdentitySecret {
        DeviceIdentitySecret::from_bytes([7u8; 32]).unwrap()
    }

    fn provision_no_service(dir: &Path, registry: &Path, name: &str) -> ProvisionConfig {
        ProvisionConfig {
            name: name.to_string(),
            drive_dir: dir.join("drive"),
            relays: vec![],
            identity: identity(),
            passphrase: "bootstrap-test-pass".to_string(),
            init: Init::Create,
            registry_path: registry.to_path_buf(),
            owner_bin: None,
            manage_service: false,
            start_now: false,
            verify_timeout: Duration::from_secs(10),
        }
    }

    #[test]
    fn provision_verifies_the_socket_and_registers_the_drive() {
        let dir = scratch("provision");
        let registry = dir.join("drives.json");
        let sock = dir.join("drive").join("owner.sock");
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let server = stub_owner(sock, 1);

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
