//! Evidence of drives set up outside this shell, for the first-run gate.
//!
//! The shell's credential defaults name only files it created or picked.
//! A drive created by the CLI with explicit credential files therefore
//! leaves the gate asking a person to create a second identity even
//! while an owner serves that drive and a File Provider domain resolves
//! it. This module reads the two discovery inputs the registrar and the
//! lifecycle view already use — the drive registry and the App Group
//! `Sockets/by-drive/*.label` aliases — and probes owner reachability.
//! It never opens a drive, reads a credential, generates a key, or
//! writes anything.
//!
//! Fail closed: an input that cannot be read is an explicit `Unreadable`
//! state with its reason, never an empty list that would open the gate
//! as if the device were fresh. A missing registry or a missing App
//! Group container is not unreadable — it is the ordinary shape of a
//! device that has no drives yet.

use std::path::{Path, PathBuf};
use std::time::Duration;

use wyrd_owner_client::{ClientError, OwnerClient};

use crate::registry;

/// Bound on one alias probe, shared with the registry's reachability
/// probe. A wedged owner reports `reachable: false`, never a hang.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Where a discovered drive came from. Display provenance only: the
/// shell never treats an alias as a drive it can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalDriveSource {
    /// A validated entry in the per-user drive registry.
    Registry,
    /// A live App Group alias published by an owner.
    Alias,
}

/// One discovered drive: discovery facts, never secrets and never a
/// writable drive identity unless the registry supplied one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExternalDriveView {
    /// Registry entry name, or the alias label a live owner published.
    pub name: String,
    /// Which input named this drive.
    pub source: ExternalDriveSource,
    /// Absolute drive directory when the registry named one; `None`
    /// for an alias-only drive, whose owner publishes no directory.
    pub drive_dir: Option<String>,
    /// Published App Group drive id when the source is an alias.
    pub drive_id: Option<String>,
    /// Whether an owner answered a terminal probe this session.
    pub reachable: bool,
}

/// How the discovery inputs read. Flattened into the state payload so
/// the gate reads without stutter, like `RegistryStatus`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ExternalDriveStatus {
    /// Inputs read (including the empty case): candidates follow.
    Ready,
    /// An input could not be read: the gate must not open as fresh.
    Unreadable {
        /// Which input failed and why.
        reason: String,
    },
}

/// The discovery answer: a status, then the candidates it read. A
/// state struct, never the owner taxonomy — transport failures on
/// individual drives ride as `reachable: false`, never as a throw.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExternalDrivesState {
    /// How the inputs read.
    #[serde(flatten)]
    pub status: ExternalDriveStatus,
    /// One candidate per discovered drive, registry entries first.
    pub drives: Vec<ExternalDriveView>,
}

impl ExternalDrivesState {
    fn unreadable(reason: String) -> Self {
        Self {
            status: ExternalDriveStatus::Unreadable { reason },
            drives: Vec::new(),
        }
    }
}

/// Container path for one group id and home: pure so the derivation
/// pins on every platform. The group id comes from the shared
/// resolver, which prefers the runtime override and otherwise uses
/// the value baked at build time — the same id the owner, the appex
/// and the registrar use.
fn group_container_with_home(group: &str, home: &Path) -> Option<PathBuf> {
    // A group id must stay one directory component inside Group
    // Containers, mirroring the registrar's own refusal.
    if group.is_empty() || group.contains('/') || group == "." || group == ".." {
        return None;
    }
    Some(home.join("Library/Group Containers").join(group))
}

/// The production App Group container, where the owner publishes
/// `Sockets/by-drive/`. `None` off macOS and where no home resolves —
/// both mean there is no alias input to read, not that discovery
/// failed. Hand-created containers are not treated specially: only a
/// published, live alias is evidence of a drive.
fn production_group_container() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let home = std::env::var_os("HOME").filter(|home| !home.is_empty())?;
    group_container_with_home(&wyrd_settings::app_group_identifier(), &PathBuf::from(home))
}

/// Read the default registry and the production App Group aliases.
pub fn discover_external_drives() -> ExternalDrivesState {
    discover_at(
        &registry::default_registry_path(),
        production_group_container().as_deref(),
    )
}

/// The pure seam: both inputs are injected so the rule pins without
/// touching an operator's registry or a real App Group container.
pub fn discover_at(registry_path: &Path, container: Option<&Path>) -> ExternalDrivesState {
    let registered = registry::list_drives_at(registry_path);
    if let crate::registry::RegistryStatus::Unreadable { reason } = &registered.status {
        return ExternalDrivesState::unreadable(format!("drive registry: {reason}"));
    }
    let aliases = match container {
        None => Vec::new(),
        Some(container) => match aliases_in(container) {
            Ok(aliases) => aliases,
            Err(reason) => return ExternalDrivesState::unreadable(reason),
        },
    };

    let mut seen = std::collections::BTreeSet::new();
    let mut drives = Vec::new();
    for drive in registered
        .drives
        .into_iter()
        .map(|drive| ExternalDriveView {
            name: drive.name,
            source: ExternalDriveSource::Registry,
            drive_dir: Some(drive.drive_dir),
            drive_id: None,
            reachable: matches!(drive.availability, registry::DriveAvailability::Available),
        })
    {
        seen.insert(drive.name.clone());
        drives.push(drive);
    }
    for drive in aliases {
        // One drive may be both registered and published. The
        // registry entry wins: it is the one the lifecycle view can
        // open, while the alias only proves a live owner.
        if seen.insert(drive.name.clone()) {
            drives.push(drive);
        }
    }
    ExternalDrivesState {
        status: ExternalDriveStatus::Ready,
        drives,
    }
}

/// Probe one group socket. A terminal refusal still proves the owner
/// answers, exactly as the lifecycle view's reachability rule says;
/// everything else is an unreachable alias, never evidence of a drive.
fn reachable(socket: &Path) -> bool {
    let Ok(client) = OwnerClient::connect(socket) else {
        return false;
    };
    let Ok(mut client) = client.with_timeout(PROBE_TIMEOUT) else {
        return false;
    };
    matches!(client.liveness(), Ok(_) | Err(ClientError::Refused(_)))
}

/// A 64-character lowercase hex drive id, never the all-zero
/// placeholder the registrar also refuses.
fn valid_drive_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && value.bytes().any(|b| b != b'0')
}

/// Registry-style display name: the label is control metadata, so a
/// malformed value is ignored rather than trusted from a shared dir.
fn valid_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The socket a by-drive alias names, kept inside `Sockets/`. The
/// registrar publishes `../<fingerprint>.sock`; anything with another
/// path component is a decoy and is ignored.
fn alias_target(container: &Path, alias: &Path) -> Option<PathBuf> {
    let target = std::fs::read_link(alias).ok()?;
    let text = target.to_string_lossy();
    let file = text.strip_prefix("../").unwrap_or(&text);
    if file.is_empty() || file.contains('/') || file == ".." || file == "." {
        return None;
    }
    Some(container.join("Sockets").join(file))
}

/// Read live, labelled aliases in one App Group container. A missing
/// directory is the ordinary no-alias case; any other read failure is
/// explicit unreadability so the gate never guesses from a partial
/// listing.
fn aliases_in(container: &Path) -> Result<Vec<ExternalDriveView>, String> {
    let by_drive = container.join("Sockets").join("by-drive");
    let entries = match std::fs::read_dir(&by_drive) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("read {}: {error}", by_drive.display())),
    };
    let mut aliases = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read {}: {error}", by_drive.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_suffix(".sock") else {
            continue;
        };
        if !valid_drive_id(id) {
            continue;
        }
        // No label yet means no published drive; any other read
        // failure is an unreadable input, not an empty list that
        // would open the gate as a fresh device.
        let label_path = by_drive.join(format!("{id}.label"));
        let label = match std::fs::read_to_string(&label_path) {
            Ok(label) => label,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("read {}: {error}", label_path.display())),
        };
        let label = label.trim().to_string();
        if !valid_label(&label) {
            continue;
        }
        let Some(socket) = alias_target(container, &entry.path()) else {
            continue;
        };
        if !reachable(&socket) {
            continue;
        }
        aliases.push(ExternalDriveView {
            name: label,
            source: ExternalDriveSource::Alias,
            drive_dir: None,
            drive_id: Some(id.to_string()),
            reachable: true,
        });
    }
    aliases.sort_by(|a, b| a.drive_id.cmp(&b.drive_id));
    Ok(aliases)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "wyrd-external-drives-{}-{}-{}",
                tag,
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An owner that refuses the probe: a terminal answer still proves
    /// the socket answers, so the alias stays evidence of a live drive.
    fn refusing_owner(socket: &Path) -> UnixListener {
        let listener = UnixListener::bind(socket).unwrap();
        let version = wyrd_owner_client::CLIENT_WIRE_VERSION;
        let id = 1u64;
        let body = serde_json::json!({
            "v": version,
            "id": id,
            "result": {"err": {"code": "Malformed", "message": "probe refusal"}},
        })
        .to_string();
        let listener_clone = listener.try_clone().unwrap();
        std::thread::spawn(move || {
            let Ok((mut peer, _)) = listener_clone.accept() else {
                return;
            };
            let mut len = [0u8; 4];
            if peer.read_exact(&mut len).is_err() {
                return;
            }
            let requested = u32::from_be_bytes(len) as usize;
            let mut body_buf = vec![0u8; requested.min(4096)];
            let _ = peer.read_exact(&mut body_buf);
            let bytes = body.as_bytes();
            let frame = u32::try_from(bytes.len()).unwrap().to_be_bytes();
            let _ = peer.write_all(&frame);
            let _ = peer.write_all(bytes);
        });
        listener
    }

    fn registry(path: &Path, drive_dir: &Path, name: &str) {
        std::fs::write(
            path,
            serde_json::json!({
                "version": 1,
                "drives": {
                    name: {
                        "name": name,
                        "drive_dir": drive_dir.to_string_lossy(),
                        "relays": [],
                        "service": format!("drive-{name}"),
                        "mode": "vault",
                    },
                },
            })
            .to_string(),
        )
        .unwrap();
    }

    /// A fresh device reads ready with no candidates: neither a
    /// registry nor a group container exists, and neither is an error.
    #[test]
    fn fresh_device_is_ready_and_empty() {
        let scratch = Scratch::new("fresh");
        let state = discover_at(&scratch.0.join("drives.json"), Some(&scratch.0));
        assert_eq!(state.status, ExternalDriveStatus::Ready);
        assert!(state.drives.is_empty());
    }

    /// A registered drive is a candidate even when its owner is down:
    /// the drive exists on this device, so the welcome invitation to
    /// create a second identity would still be wrong.
    #[test]
    fn registered_drive_is_a_candidate_without_a_live_owner() {
        let scratch = Scratch::new("registered");
        let dir = scratch.0.join("drive");
        std::fs::create_dir_all(&dir).unwrap();
        let path = scratch.0.join("drives.json");
        registry(&path, &dir, "demo");
        let state = discover_at(&path, Some(&scratch.0));
        assert_eq!(state.status, ExternalDriveStatus::Ready);
        assert_eq!(state.drives.len(), 1);
        assert_eq!(state.drives[0].name, "demo");
        assert_eq!(state.drives[0].source, ExternalDriveSource::Registry);
        assert_eq!(
            state.drives[0].drive_dir.as_deref(),
            Some(dir.to_str().unwrap())
        );
        assert!(!state.drives[0].reachable);
    }

    /// A live, labelled App Group alias is a candidate even with no
    /// registry entry: this is the CLI-created drive case.
    #[test]
    fn live_alias_is_a_candidate_without_a_registry() {
        let scratch = Scratch::new("alias");
        let sockets = scratch.0.join("Sockets");
        let by_drive = sockets.join("by-drive");
        std::fs::create_dir_all(&by_drive).unwrap();
        let id = "ab".repeat(32);
        let socket = sockets.join("owner.sock");
        let _listener = refusing_owner(&socket);
        std::os::unix::fs::symlink("../owner.sock", by_drive.join(format!("{id}.sock"))).unwrap();
        std::fs::write(by_drive.join(format!("{id}.label")), b"outside\n").unwrap();
        let state = discover_at(&scratch.0.join("drives.json"), Some(&scratch.0));
        assert_eq!(state.status, ExternalDriveStatus::Ready);
        assert_eq!(state.drives.len(), 1);
        assert_eq!(state.drives[0].name, "outside");
        assert_eq!(state.drives[0].source, ExternalDriveSource::Alias);
        assert_eq!(state.drives[0].drive_dir, None);
        assert_eq!(state.drives[0].drive_id.as_deref(), Some(id.as_str()));
        assert!(state.drives[0].reachable);
    }

    /// A dead alias is not evidence of a drive, and neither is a
    /// decoy symlink escaping `Sockets/` or a malformed id.
    #[test]
    fn dead_and_decoy_aliases_are_ignored() {
        let scratch = Scratch::new("decoy");
        let sockets = scratch.0.join("Sockets");
        let by_drive = sockets.join("by-drive");
        std::fs::create_dir_all(&by_drive).unwrap();
        let dead = "cd".repeat(32);
        std::os::unix::fs::symlink("../gone.sock", by_drive.join(format!("{dead}.sock"))).unwrap();
        std::fs::write(by_drive.join(format!("{dead}.label")), b"dead").unwrap();
        let escaping = "ef".repeat(32);
        std::os::unix::fs::symlink("../../elsewhere", by_drive.join(format!("{escaping}.sock")))
            .unwrap();
        std::fs::write(by_drive.join(format!("{escaping}.label")), b"decoy").unwrap();
        std::fs::write(by_drive.join("short.label"), b"short").unwrap();
        let state = discover_at(&scratch.0.join("drives.json"), Some(&scratch.0));
        assert_eq!(state.status, ExternalDriveStatus::Ready);
        assert!(state.drives.is_empty());
    }

    /// An unreadable alias label is not "no label yet": it fails
    /// closed like the other discovery inputs instead of dropping a
    /// live drive's only evidence.
    #[test]
    fn unreadable_alias_label_fails_closed() {
        let scratch = Scratch::new("bad-label");
        let by_drive = scratch.0.join("Sockets").join("by-drive");
        std::fs::create_dir_all(&by_drive).unwrap();
        let id = "de".repeat(32);
        std::fs::write(by_drive.join(format!("{id}.sock")), b"not-a-symlink").unwrap();
        std::fs::create_dir(by_drive.join(format!("{id}.label"))).unwrap();
        let state = discover_at(&scratch.0.join("drives.json"), Some(&scratch.0));
        assert!(matches!(
            &state.status,
            ExternalDriveStatus::Unreadable { reason } if reason.contains(".label")
        ));
        assert!(state.drives.is_empty());
    }

    /// An unreadable registry stays unreadable: the gate must show a
    /// retry state, never a fresh-device invitation.
    #[test]
    fn unreadable_registry_fails_closed() {
        let scratch = Scratch::new("unreadable");
        let path = scratch.0.join("drives.json");
        std::fs::write(&path, b"not json").unwrap();
        let state = discover_at(&path, Some(&scratch.0));
        assert!(matches!(
            &state.status,
            ExternalDriveStatus::Unreadable { reason } if reason.contains("drive registry")
        ));
        assert!(state.drives.is_empty());
    }

    /// A named drive and its published alias are one candidate: the
    /// registry entry wins so the lifecycle view can open it.
    #[test]
    fn registry_entry_wins_over_its_alias() {
        let scratch = Scratch::new("both");
        let dir = scratch.0.join("drive");
        std::fs::create_dir_all(&dir).unwrap();
        let path = scratch.0.join("drives.json");
        registry(&path, &dir, "outside");
        let sockets = scratch.0.join("Sockets");
        let by_drive = sockets.join("by-drive");
        std::fs::create_dir_all(&by_drive).unwrap();
        let id = "ab".repeat(32);
        let _listener = refusing_owner(&sockets.join("owner.sock"));
        std::os::unix::fs::symlink("../owner.sock", by_drive.join(format!("{id}.sock"))).unwrap();
        std::fs::write(by_drive.join(format!("{id}.label")), b"outside").unwrap();
        let state = discover_at(&path, Some(&scratch.0));
        assert_eq!(state.drives.len(), 1);
        assert_eq!(state.drives[0].source, ExternalDriveSource::Registry);
        assert_eq!(
            state.drives[0].drive_dir.as_deref(),
            Some(dir.to_str().unwrap())
        );
    }

    /// The serialization is the IPC contract the frontend mirrors.
    #[test]
    fn discovery_shapes_serialize_verbatim() {
        let ready = ExternalDrivesState {
            status: ExternalDriveStatus::Ready,
            drives: vec![ExternalDriveView {
                name: "outside".to_string(),
                source: ExternalDriveSource::Alias,
                drive_dir: None,
                drive_id: Some("ab".repeat(32)),
                reachable: true,
            }],
        };
        assert_eq!(
            serde_json::to_value(&ready).unwrap(),
            serde_json::json!({
                "status": "ready",
                "drives": [{
                    "name": "outside",
                    "source": "alias",
                    "drive_dir": null,
                    "drive_id": "ab".repeat(32),
                    "reachable": true,
                }],
            })
        );
        let broken = ExternalDrivesState::unreadable("read ...: denied".to_string());
        assert_eq!(
            serde_json::to_value(&broken).unwrap(),
            serde_json::json!({
                "status": "unreadable",
                "reason": "read ...: denied",
                "drives": [],
            })
        );
    }

    /// The container derivation uses the shared group-id resolver and
    /// refuses a group id that would escape Group Containers.
    #[test]
    fn group_container_uses_the_shared_group_id() {
        let home = Path::new("/home/operator");
        let group = wyrd_settings::app_group_identifier();
        assert_eq!(
            group_container_with_home(&group, home),
            Some(home.join("Library/Group Containers").join(&group))
        );
        assert!(group_container_with_home("team/group", home).is_none());
        assert!(group_container_with_home("..", home).is_none());
    }
}
