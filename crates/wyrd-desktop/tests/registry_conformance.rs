//! Registry conformance: one fixture, two readers and two writers.
//!
//! `drives.json` has two implementations that must agree: the
//! registration flow (`wyrd_daemon::bootstrap`) and the desktop shell
//! (`wyrd_desktop::registry`). The two crates cannot link in production
//! — the layer gate forbids the desktop→daemon edge — so they meet here,
//! in dev-dependencies, exactly as `registry_authority.rs` already does
//! for the write race. This file feeds identical fixtures to both and
//! pins the shared schema, validation, and write contract.
//!
//! The fixtures are deliberately about the *file*, not about a live
//! drive: the drive directories do not exist, so the shell's reachability
//! probe reports `Unavailable` without spawning a service manager, and
//! the comparison stays deterministic.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use wyrd_daemon::bootstrap as daemon;
use wyrd_desktop::registry as shell;

const RELAYS: &[&str] = &["wss://relay.example"];
const CANDIDATE_DIR: &str = "/wyrd-registry-conformance";
const LONG_NAME: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

static NEXT_CASE: AtomicU64 = AtomicU64::new(0);

/// One scratch directory per case, unique across tests running in parallel.
fn scratch_dir(case: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-registry-conformance-{case}-{}-{}",
        std::process::id(),
        NEXT_CASE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The facts both readers must derive from one file. Reachability and
/// service verdicts are deliberately excluded: they are live
/// observations, not registry facts.
#[derive(Debug, PartialEq, Eq)]
struct DriveFacts {
    name: String,
    drive_dir: PathBuf,
    relays: Vec<String>,
    service: String,
    mode: String,
    mountpoint: Option<PathBuf>,
}

fn daemon_facts(entry: &daemon::RegistryEntry) -> DriveFacts {
    DriveFacts {
        name: entry.name.clone(),
        drive_dir: entry.drive_dir.clone(),
        relays: entry.relays.clone(),
        service: entry.service.clone(),
        mode: match &entry.mode {
            daemon::ServiceMode::Vault => "vault".to_string(),
            daemon::ServiceMode::Mount => "mount".to_string(),
            daemon::ServiceMode::Unknown(mode) => mode.clone(),
        },
        mountpoint: entry.mountpoint.clone(),
    }
}

fn shell_facts(view: &shell::DriveView) -> DriveFacts {
    DriveFacts {
        name: view.name.clone(),
        drive_dir: PathBuf::from(&view.drive_dir),
        relays: view.relays.clone(),
        service: view.service.clone(),
        mode: view.mode.clone(),
        mountpoint: view.mountpoint.as_ref().map(PathBuf::from),
    }
}

/// Both readers phrase a skip as `entry "<key>": <reason>`. Recover the
/// key so the assertion compares *which* entries each side skipped, not
/// the prose of the reason (which each surface renders in its own voice).
fn skipped_keys(reasons: &[String]) -> BTreeSet<String> {
    reasons
        .iter()
        .map(|reason| {
            reason
                .strip_prefix("entry ")
                .and_then(|rest| rest.split_once(": "))
                .map(|(key, _)| key.trim_matches('"').to_string())
                .unwrap_or_else(|| panic!("unexpected skip reason: {reason}"))
        })
        .collect()
}

/// What a fixture expects. A missing file is empty on both sides; an
/// unreadable file is an error on one side and an explicit `Unreadable`
/// status on the other. `mentions` is the token both reason strings must
/// carry, so version skew and parse failures stay distinguishable.
#[derive(Clone, Copy)]
enum Verdict {
    Nofile,
    Unreadable {
        mentions: &'static str,
    },
    Ready {
        drives: &'static [&'static str],
        skipped: &'static [&'static str],
    },
}

/// One entry value. `mountpoint: null` keeps the wire shape identical to
/// what both writers emit for a vault entry.
fn entry(
    name: &str,
    dir: &str,
    mode: &str,
    mountpoint: Option<&str>,
    relays: &[&str],
) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "drive_dir": dir,
        "relays": relays,
        "service": format!("drive-{name}"),
        "mode": mode,
        "mountpoint": mountpoint,
    })
}

fn drives(
    entries: impl IntoIterator<Item = (String, serde_json::Value)>,
) -> serde_json::Map<String, serde_json::Value> {
    entries.into_iter().collect()
}

/// Run one fixture through both readers and assert they agree.
fn check_reader(case: &str, contents: Option<&str>, verdict: Verdict) {
    let dir = scratch_dir(case);
    let path = dir.join("drives.json");
    if let Some(contents) = contents {
        std::fs::write(&path, contents).unwrap();
    }

    let daemon_read = daemon::read_registry(&path);
    let shell_state = shell::list_drives_at(&path);

    match verdict {
        Verdict::Nofile => {
            let registry = daemon_read.unwrap();
            assert!(registry.drives.is_empty() && registry.skipped.is_empty());
            assert_eq!(shell_state.status, shell::RegistryStatus::Nofile);
            assert!(shell_state.drives.is_empty() && shell_state.skipped.is_empty());
        }
        Verdict::Unreadable { mentions } => {
            let daemon_reason = daemon_read
                .expect_err("daemon must refuse this file")
                .to_string();
            let shell_reason = match &shell_state.status {
                shell::RegistryStatus::Unreadable { reason } => reason.clone(),
                other => panic!("shell must refuse this file too: {other:?}"),
            };
            assert!(daemon_reason.contains(mentions), "daemon: {daemon_reason}");
            assert!(shell_reason.contains(mentions), "shell: {shell_reason}");
        }
        Verdict::Ready {
            drives: expected_drives,
            skipped: expected_skipped,
        } => {
            let registry = daemon_read.unwrap();
            assert_eq!(shell_state.status, shell::RegistryStatus::Ready);
            let expected_skipped: BTreeSet<String> = expected_skipped
                .iter()
                .map(|key| (*key).to_string())
                .collect();
            assert_eq!(
                skipped_keys(&registry.skipped),
                expected_skipped,
                "daemon skipped: {:?}",
                registry.skipped
            );
            assert_eq!(
                skipped_keys(&shell_state.skipped),
                expected_skipped,
                "shell skipped: {:?}",
                shell_state.skipped
            );

            let daemon_map: BTreeMap<_, _> = registry
                .drives
                .iter()
                .map(|(key, value)| (key.clone(), daemon_facts(value)))
                .collect();
            let shell_map: BTreeMap<_, _> = shell_state
                .drives
                .iter()
                .map(|view| (view.name.clone(), shell_facts(view)))
                .collect();
            assert_eq!(
                daemon_map, shell_map,
                "{case}: readers disagree on the valid entries"
            );
            assert_eq!(
                daemon_map
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>(),
                expected_drives.iter().copied().collect::<BTreeSet<_>>()
            );
        }
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

fn daemon_candidate(name: &str) -> daemon::RegistryEntry {
    daemon::RegistryEntry {
        name: name.to_string(),
        drive_dir: Path::new(CANDIDATE_DIR).join(format!("drive-{name}")),
        relays: RELAYS.iter().map(|relay| (*relay).to_string()).collect(),
        service: format!("drive-{name}"),
        mode: daemon::ServiceMode::Vault,
        mountpoint: None,
    }
}

fn shell_candidate(name: &str) -> shell::RegistryEntryWrite {
    shell::RegistryEntryWrite {
        name: name.to_string(),
        drive_dir: Path::new(CANDIDATE_DIR).join(format!("drive-{name}")),
        relays: RELAYS.iter().map(|relay| (*relay).to_string()).collect(),
        service: format!("drive-{name}"),
        mode: "vault".to_string(),
        mountpoint: None,
    }
}

#[test]
fn readers_agree_on_registry_fixtures() {
    let valid = |name: &str| {
        (
            name.to_string(),
            entry(name, "/wyrd/valid", "vault", None, RELAYS),
        )
    };

    check_reader("nofile", None, Verdict::Nofile);
    check_reader(
        "empty",
        Some(&serde_json::json!({ "version": 1, "drives": {} }).to_string()),
        Verdict::Ready {
            drives: &[],
            skipped: &[],
        },
    );
    check_reader(
        "version-skew",
        Some(&serde_json::json!({ "version": 2, "drives": {} }).to_string()),
        Verdict::Unreadable {
            mentions: "version 2",
        },
    );
    check_reader(
        "malformed",
        Some("{not json"),
        Verdict::Unreadable { mentions: "parse" },
    );
    check_reader(
        "corrupt-entry",
        Some(
            &serde_json::json!({
                "version": 1,
                "drives": {
                    "broken": { "name": "broken", "drive_dir": 42, "service": "drive-broken", "mode": "vault", "relays": [], "mountpoint": null }
                }
            })
            .to_string(),
        ),
        Verdict::Unreadable { mentions: "parse" },
    );
    check_reader(
        "valid-vault",
        Some(&serde_json::json!({ "version": 1, "drives": drives([valid("vault")]) }).to_string()),
        Verdict::Ready {
            drives: &["vault"],
            skipped: &[],
        },
    );
    check_reader(
        "valid-mount",
        Some(
            &serde_json::json!({
                "version": 1,
                "drives": drives([(
                    "mounted".to_string(),
                    entry("mounted", "/wyrd/mounted", "mount", Some("/mnt/mounted"), RELAYS),
                )])
            })
            .to_string(),
        ),
        Verdict::Ready {
            drives: &["mounted"],
            skipped: &[],
        },
    );
    check_reader(
        "pre-mount-file",
        Some(
            &serde_json::json!({
                "version": 1,
                "drives": drives([(
                    "old".to_string(),
                    serde_json::json!({
                        "name": "old",
                        "drive_dir": "/wyrd/old",
                        "service": "drive-old",
                        "mode": "vault",
                        "relays": [],
                    }),
                )])
            })
            .to_string(),
        ),
        Verdict::Ready {
            drives: &["old"],
            skipped: &[],
        },
    );
    check_reader(
        "invalid-entries",
        Some(
            &serde_json::json!({
                "version": 1,
                "drives": drives([
                    ("wrong-key".to_string(), entry("other", "/wyrd/other", "vault", None, RELAYS)),
                    ("Upper".to_string(), entry("Upper", "/wyrd/upper", "vault", None, RELAYS)),
                    ("relative".to_string(), entry("relative", "relative/dir", "vault", None, RELAYS)),
                    ("dotdot".to_string(), entry("dotdot", "/wyrd/../dir", "vault", None, RELAYS)),
                    ("future".to_string(), entry("future", "/wyrd/future", "fuse", None, RELAYS)),
                    ("vaultmp".to_string(), entry("vaultmp", "/wyrd/vaultmp", "vault", Some("/mnt/x"), RELAYS)),
                    ("mountless".to_string(), entry("mountless", "/wyrd/mountless", "mount", None, RELAYS)),
                    ("badrelay".to_string(), entry("badrelay", "/wyrd/badrelay", "vault", None, &["wss://bad relay"])),
                    (LONG_NAME.to_string(), entry(LONG_NAME, "/wyrd/long", "vault", None, RELAYS)),
                ])
            })
            .to_string(),
        ),
        Verdict::Ready {
            drives: &[],
            skipped: &[
                "Upper",
                LONG_NAME,
                "badrelay",
                "dotdot",
                "future",
                "mountless",
                "relative",
                "vaultmp",
                "wrong-key",
            ],
        },
    );
    check_reader(
        "mixed",
        Some(
            &serde_json::json!({
                "version": 1,
                "drives": drives([
                    valid("good"),
                    ("future".to_string(), entry("future", "/wyrd/future", "fuse", None, RELAYS)),
                ])
            })
            .to_string(),
        ),
        Verdict::Ready {
            drives: &["good"],
            skipped: &["future"],
        },
    );
}

/// A known schema divergence, recorded rather than silently ignored.
/// The daemon's entry requires `relays`, so a hand-edited file missing
/// that field is structurally corrupt and refuses the whole read; the
/// desktop's `#[serde(default)]` accepts it as an empty relay set and
/// lists the drive. Both writers always emit the field, so only a
/// hand-edited file reaches this state. This pins the current boundary
/// so it cannot widen unnoticed; unifying the two schemas is the
/// shared-crate follow-up, not a behavior change in this conformance PR.
#[test]
fn missing_relays_divergence_is_recorded() {
    let dir = scratch_dir("missing-relays");
    let path = dir.join("drives.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "drives": {
                "norelay": {
                    "name": "norelay",
                    "drive_dir": "/wyrd/norelay",
                    "service": "drive-norelay",
                    "mode": "vault",
                    "mountpoint": null
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let daemon_error = daemon::read_registry(&path)
        .expect_err("the daemon refuses an entry missing a required field")
        .to_string();
    assert!(
        daemon_error.contains("missing field `relays`"),
        "{daemon_error}"
    );
    let shell_state = shell::list_drives_at(&path);
    assert_eq!(shell_state.status, shell::RegistryStatus::Ready);
    assert_eq!(shell_state.drives.len(), 1);
    assert!(shell_state.drives[0].relays.is_empty());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn writers_produce_the_same_file_and_carry_skipped_entries() {
    let initial = serde_json::json!({
        "version": 1,
        "drives": {
            "keep": entry("keep", "/wyrd/keep", "vault", None, RELAYS),
            "legacy": entry("legacy", "/wyrd/legacy", "fuse", None, RELAYS),
        }
    })
    .to_string();

    let daemon_dir = scratch_dir("writer-daemon");
    let shell_dir = scratch_dir("writer-shell");
    let daemon_path = daemon_dir.join("drives.json");
    let shell_path = shell_dir.join("drives.json");
    std::fs::write(&daemon_path, &initial).unwrap();
    std::fs::write(&shell_path, &initial).unwrap();

    let mut daemon_candidates = BTreeMap::new();
    let candidate = daemon_candidate("new");
    daemon_candidates.insert(candidate.name.clone(), candidate);
    daemon::write_registry(&daemon_path, &daemon_candidates).unwrap();

    let mut shell_candidates = BTreeMap::new();
    let candidate = shell_candidate("new");
    shell_candidates.insert(candidate.name.clone(), candidate);
    shell::write_registry_at(&shell_path, &shell_candidates).unwrap();

    let daemon_file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&daemon_path).unwrap()).unwrap();
    let shell_file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&shell_path).unwrap()).unwrap();
    assert_eq!(
        daemon_file, shell_file,
        "the two writers must emit the same registry"
    );

    // The carried entry is still skipped by both readers, and the new
    // entry is visible to both — one file, no interpretation gap.
    let daemon_read = daemon::read_registry(&daemon_path).unwrap();
    let shell_read = shell::list_drives_at(&shell_path);
    assert_eq!(
        skipped_keys(&daemon_read.skipped),
        BTreeSet::from(["legacy".to_string()])
    );
    assert_eq!(
        skipped_keys(&shell_read.skipped),
        BTreeSet::from(["legacy".to_string()])
    );
    assert_eq!(daemon_read.drives.len(), 2);
    assert_eq!(shell_read.drives.len(), 2);

    std::fs::remove_dir_all(&daemon_dir).unwrap();
    std::fs::remove_dir_all(&shell_dir).unwrap();
}

#[test]
fn writers_both_refuse_invalid_candidates_without_writing() {
    let initial = serde_json::json!({ "version": 1, "drives": {} }).to_string();
    let daemon_dir = scratch_dir("refuse-daemon");
    let shell_dir = scratch_dir("refuse-shell");
    let daemon_path = daemon_dir.join("drives.json");
    let shell_path = shell_dir.join("drives.json");
    std::fs::write(&daemon_path, &initial).unwrap();
    std::fs::write(&shell_path, &initial).unwrap();

    let mut daemon_candidates = BTreeMap::new();
    let mut candidate = daemon_candidate("bad");
    candidate.drive_dir = PathBuf::from("relative/dir");
    daemon_candidates.insert(candidate.name.clone(), candidate);
    let mut shell_candidates = BTreeMap::new();
    let mut candidate = shell_candidate("bad");
    candidate.drive_dir = PathBuf::from("relative/dir");
    shell_candidates.insert(candidate.name.clone(), candidate);

    assert!(daemon::write_registry(&daemon_path, &daemon_candidates).is_err());
    assert!(shell::write_registry_at(&shell_path, &shell_candidates).is_err());
    assert_eq!(std::fs::read(&daemon_path).unwrap(), initial.as_bytes());
    assert_eq!(std::fs::read(&shell_path).unwrap(), initial.as_bytes());

    std::fs::remove_dir_all(&daemon_dir).unwrap();
    std::fs::remove_dir_all(&shell_dir).unwrap();
}

#[test]
fn writers_both_refuse_unreadable_files_without_writing() {
    for (case, fixture) in [
        (
            "skew",
            serde_json::json!({ "version": 2, "drives": {} }).to_string(),
        ),
        ("malformed", "{not json".to_string()),
    ] {
        let daemon_dir = scratch_dir(&format!("unreadable-{case}-daemon"));
        let shell_dir = scratch_dir(&format!("unreadable-{case}-shell"));
        let daemon_path = daemon_dir.join("drives.json");
        let shell_path = shell_dir.join("drives.json");
        std::fs::write(&daemon_path, &fixture).unwrap();
        std::fs::write(&shell_path, &fixture).unwrap();

        let mut daemon_candidates = BTreeMap::new();
        let candidate = daemon_candidate("new");
        daemon_candidates.insert(candidate.name.clone(), candidate);
        let mut shell_candidates = BTreeMap::new();
        let candidate = shell_candidate("new");
        shell_candidates.insert(candidate.name.clone(), candidate);

        assert!(
            daemon::write_registry(&daemon_path, &daemon_candidates).is_err(),
            "{case}"
        );
        assert!(
            shell::write_registry_at(&shell_path, &shell_candidates).is_err(),
            "{case}"
        );
        assert_eq!(
            std::fs::read(&daemon_path).unwrap(),
            fixture.as_bytes(),
            "{case}"
        );
        assert_eq!(
            std::fs::read(&shell_path).unwrap(),
            fixture.as_bytes(),
            "{case}"
        );

        std::fs::remove_dir_all(&daemon_dir).unwrap();
        std::fs::remove_dir_all(&shell_dir).unwrap();
    }
}
