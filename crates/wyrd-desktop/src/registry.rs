//! Read-only view of the per-user drive registry (`drives.json`).
//!
//! Observer only: this module parses the schema-version-1 file the
//! installer and `wyrd bootstrap` flow write, validates entries with
//! the same rules as the daemon's registry reader, and probes each
//! drive's owner socket. It never writes the registry — creation,
//! joining, and membership changes belong to the provisioning and
//! membership flows, not to this shell (OD: desktop lifecycle
//! authority, stage 2).
//!
//! Every answer is a fresh observation, never cached authority:
//!
//! - a missing registry file is an empty drive list (a fresh
//!   installation registered nothing yet), not an error;
//! - a newer schema version or a malformed file is an explicit
//!   `Unreadable` state with the reason — loud, never guessed, never
//!   silently defaulted;
//! - invalid entries are skipped with their reasons; valid ones are
//!   unaffected, exactly like the daemon's reader;
//! - per-drive reachability is verified this session (connect plus a
//!   terminal `liveness` answer): `Available` means the owner
//!   answered, `Unavailable` means no transport, `Unknown` means the
//!   transport exists but no terminal answer arrived (or the peer
//!   speaks another wire version). A refusal to the probe `liveness`
//!   still proves the owner answers, so it reports `Available` —
//!   availability is transport-level, never a judgment on the drive.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use wyrd_owner_client::{ClientError, OwnerClient, WireLiveness};

/// Registry schema version this shell reads. Newer files are
/// reported unreadable with their version, never guessed.
const REGISTRY_VERSION: u32 = 1;

/// Registry filename, fixed so installer, command, and desktop read
/// the same path derivation.
const REGISTRY_FILENAME: &str = "drives.json";

/// Bound on one drive's reachability probe. A status read answers in
/// milliseconds; the bound only fires on a wedged owner, and expiry
/// reports `Unknown` — never `Unavailable`, never success. It also
/// bounds the whole list: one hung drive must not wedge the view.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// One registry entry as parsed: discovery facts, never secrets.
/// Private: the public [`DriveView`] carries the validated subset
/// plus the live reachability verdict.
#[derive(Debug, Clone, serde::Deserialize)]
struct RegistryEntryParse {
    name: String,
    drive_dir: PathBuf,
    #[serde(default)]
    relays: Vec<String>,
    service: String,
    mode: String,
}

#[derive(Debug, serde::Deserialize)]
struct RegistryFileParse {
    version: u32,
    drives: BTreeMap<String, RegistryEntryParse>,
}

/// How the registry file itself read: ready with entries, absent
/// (fresh installation), or present but unreadable with the reason.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum RegistryStatus {
    Ready,
    Nofile,
    Unreadable { reason: String },
}

/// One drive's reachability, verified this session. Transport-level
/// only: `Available` never claims anything about the drive's
/// contents, and `Unavailable` never claims the drive is gone.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum DriveAvailability {
    Available,
    Unavailable { detail: String },
    Unknown { detail: String },
}

/// One registered drive for the lifecycle view: validated facts
/// plus the live verdict. `mode` is a display string end to end —
/// the shell shows the registry's service mode and never interprets
/// it. Drives match by exact path only.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DriveView {
    pub name: String,
    pub drive_dir: String,
    pub relays: Vec<String>,
    pub service: String,
    pub mode: String,
    pub availability: DriveAvailability,
}

/// The lifecycle view's registry answer: the file status, one view
/// per valid entry, and one reason per skipped entry. The status
/// flattens into the payload (`{"status": "ready", "drives": ...}`)
/// so the file verdict reads without stutter.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RegistryState {
    #[serde(flatten)]
    pub status: RegistryStatus,
    pub drives: Vec<DriveView>,
    pub skipped: Vec<String>,
}

/// Where the registry lives when no explicit path is given: the
/// same per-user derivation the installer and the daemon use, so
/// all three read the same file.
fn default_registry_path() -> PathBuf {
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

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn has_dotdot(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}

/// The daemon's entry rules, mirrored: the shell must agree with
/// the writer about which entries are valid, or the lifecycle view
/// would show drives the owner tooling refuses (and vice versa).
fn validate_entry(key: &str, entry: &RegistryEntryParse) -> Result<(), String> {
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
    if entry.name.len() + "drive-".len() > 64 {
        return Err(format!(
            "name {:?} leaves no room for the drive- service prefix (max 58 chars)",
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
    if entry.mode != "vault" {
        return Err(
            "unknown service mode: registry-managed mount mode travels with the installer follow-up"
                .to_string(),
        );
    }
    if entry.relays.iter().any(|relay| {
        relay.is_empty()
            || relay
                .bytes()
                .any(|byte| byte < 0x20 || byte == 0x7f || byte.is_ascii_whitespace())
    }) {
        return Err(
            "relays must be non-empty URLs without whitespace or control bytes".to_string(),
        );
    }
    if entry
        .drive_dir
        .to_string_lossy()
        .bytes()
        .any(|byte| byte < 0x20 || byte == 0x7f)
    {
        return Err(format!(
            "drive_dir {} must not contain control characters",
            entry.drive_dir.display()
        ));
    }
    Ok(())
}

/// Map one probe exchange onto the reachability verdict. Pure, so
/// every taxonomy arm pins without staging an owner.
fn availability_of(result: Result<WireLiveness, ClientError>) -> DriveAvailability {
    match result {
        Ok(_) => DriveAvailability::Available,
        // A terminal refusal still proves the owner answers: the
        // socket serves, whatever it thought of the probe.
        Err(ClientError::Refused(_)) => DriveAvailability::Available,
        Err(ClientError::Unavailable(detail)) => DriveAvailability::Unavailable { detail },
        Err(ClientError::Unconfirmed { detail, .. }) => DriveAvailability::Unknown { detail },
        Err(ClientError::Incompatible(detail)) => DriveAvailability::Unknown { detail },
    }
}

/// Probe one registered drive: connect plus a terminal `liveness`
/// answer under the probe bound. Connecting alone proves nothing —
/// the socket may accept while the owner never answers — so only a
/// terminal answer reports `Available`.
fn probe_drive(drive_dir: &Path) -> DriveAvailability {
    let client = match OwnerClient::connect_drive(drive_dir) {
        Ok(client) => client,
        Err(error) => {
            return match error {
                ClientError::Unavailable(detail) => DriveAvailability::Unavailable { detail },
                other => availability_of(Err(other)),
            };
        }
    };
    let mut client = match client.with_timeout(PROBE_TIMEOUT) {
        Ok(bounded) => bounded,
        Err(error) => return availability_of(Err(error)),
    };
    availability_of(client.liveness())
}

/// Read the registry at an explicit path and probe every valid
/// entry. The explicit path exists for tests; production passes the
/// default derivation.
pub fn list_drives_at(registry_path: &Path) -> RegistryState {
    let bytes = match std::fs::read(registry_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RegistryState {
                status: RegistryStatus::Nofile,
                drives: Vec::new(),
                skipped: Vec::new(),
            };
        }
        Err(error) => {
            return RegistryState {
                status: RegistryStatus::Unreadable {
                    reason: format!("read {}: {error}", registry_path.display()),
                },
                drives: Vec::new(),
                skipped: Vec::new(),
            };
        }
    };
    let file: RegistryFileParse = match serde_json::from_slice(&bytes) {
        Ok(file) => file,
        Err(error) => {
            return RegistryState {
                status: RegistryStatus::Unreadable {
                    reason: format!("parse {}: {error}", registry_path.display()),
                },
                drives: Vec::new(),
                skipped: Vec::new(),
            };
        }
    };
    if file.version != REGISTRY_VERSION {
        return RegistryState {
            status: RegistryStatus::Unreadable {
                reason: format!(
                    "registry {} is version {}, this shell reads {}",
                    registry_path.display(),
                    file.version,
                    REGISTRY_VERSION
                ),
            },
            drives: Vec::new(),
            skipped: Vec::new(),
        };
    }
    let mut drives = Vec::new();
    let mut skipped = Vec::new();
    for (key, entry) in file.drives {
        match validate_entry(&key, &entry) {
            Ok(()) => drives.push(DriveView {
                name: entry.name.clone(),
                drive_dir: entry.drive_dir.to_string_lossy().into_owned(),
                relays: entry.relays.clone(),
                service: entry.service.clone(),
                mode: entry.mode.clone(),
                availability: probe_drive(&entry.drive_dir),
            }),
            Err(reason) => skipped.push(format!("entry {key:?}: {reason}")),
        }
    }
    RegistryState {
        status: RegistryStatus::Ready,
        drives,
        skipped,
    }
}

/// Read the default registry and probe every valid entry.
pub fn list_drives() -> RegistryState {
    list_drives_at(&default_registry_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The taxonomy-to-reachability map pins without an owner: every
    /// client outcome lands on exactly one verdict, and only a
    /// terminal owner answer (success or refusal) reports available.
    #[test]
    fn probe_mapping_covers_the_taxonomy() {
        assert_eq!(
            availability_of(Err(ClientError::Unavailable("down".to_string()))),
            DriveAvailability::Unavailable {
                detail: "down".to_string()
            }
        );
        assert_eq!(
            availability_of(Err(ClientError::Unconfirmed {
                op: "liveness".to_string(),
                detail: "torn".to_string(),
            })),
            DriveAvailability::Unknown {
                detail: "torn".to_string()
            }
        );
        assert_eq!(
            availability_of(Err(ClientError::Incompatible("v9".to_string()))),
            DriveAvailability::Unknown {
                detail: "v9".to_string()
            }
        );
        // A refusal proves the owner answers: transport-level
        // availability, not a judgment on the probe.
        let refused = ClientError::Refused(wyrd_owner_client::WireError {
            code: wyrd_owner_client::WireErrorCode::Store,
            message: "locked".to_string(),
        });
        assert_eq!(availability_of(Err(refused)), DriveAvailability::Available);
    }

    /// A missing registry file is an empty drive list, not an
    /// error: a fresh installation registered nothing yet.
    #[test]
    fn missing_registry_is_empty_not_error() {
        let path = std::env::temp_dir().join(format!(
            "wyrd-desktop-noreg-{}-{}",
            std::process::id(),
            "missing.json"
        ));
        let _ = std::fs::remove_file(&path);
        let state = list_drives_at(&path);
        assert_eq!(state.status, RegistryStatus::Nofile);
        assert!(state.drives.is_empty());
        assert!(state.skipped.is_empty());
    }

    /// Version skew and malformed files surface as explicit
    /// unreadable states with the reason — loud, never guessed.
    #[test]
    fn version_skew_and_malformed_files_are_unreadable() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-reg-{}-skew", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let newer = dir.join("newer.json");
        std::fs::write(&newer, r#"{"version": 2, "drives": {}}"#).unwrap();
        match list_drives_at(&newer).status {
            RegistryStatus::Unreadable { reason } => {
                assert!(reason.contains("version 2"), "reason: {reason}");
                assert!(reason.contains("reads 1"), "reason: {reason}");
            }
            status => panic!("a newer registry refuses loudly: {status:?}"),
        }
        let broken = dir.join("broken.json");
        std::fs::write(&broken, r#"{"version": 1, "drives": {"#).unwrap();
        assert!(
            matches!(
                list_drives_at(&broken).status,
                RegistryStatus::Unreadable { .. }
            ),
            "a malformed registry is unreadable, never guessed"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Invalid entries skip with reasons while valid ones verify:
    /// entries pointing at no owner probe `Unavailable`, which says
    /// nothing about any drive.
    #[test]
    fn invalid_entries_skip_while_valid_ones_probe() {
        let dir =
            std::env::temp_dir().join(format!("wyrd-desktop-reg-{}-entries", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let drive = dir.join("drive");
        std::fs::create_dir_all(&drive).unwrap();
        let path = dir.join("drives.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "drives": {
                    "good": {
                        "name": "good",
                        "drive_dir": drive.to_string_lossy(),
                        "relays": [],
                        "service": "drive-good",
                        "mode": "vault",
                    },
                    "bad": {
                        "name": "other",
                        "drive_dir": "/absent",
                        "relays": [],
                        "service": "drive-bad",
                        "mode": "vault",
                    },
                },
            })
            .to_string(),
        )
        .unwrap();
        let state = list_drives_at(&path);
        assert_eq!(state.status, RegistryStatus::Ready);
        assert_eq!(state.drives.len(), 1, "only the valid entry verifies");
        assert_eq!(state.drives[0].name, "good");
        assert!(
            matches!(
                state.drives[0].availability,
                DriveAvailability::Unavailable { .. }
            ),
            "no owner serves the scratch drive: {:?}",
            state.drives[0].availability
        );
        assert_eq!(state.skipped.len(), 1, "the invalid entry skips loudly");
        assert!(
            state.skipped[0].contains("does not match"),
            "skip reason: {}",
            state.skipped[0]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
