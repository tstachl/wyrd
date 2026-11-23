//! Explicit, local-only diagnostics bundle export (T20 in
//! `docs/contracts/trust.md`).
//!
//! A user action builds a temporary JSON artifact from a deliberately
//! narrow set of local records. The extension writes structured
//! records from the fields its line builder already marked
//! `exportable`; this module applies a second, explicit key allowlist
//! before any field enters the bundle. Raw logs are never copied:
//! they can carry `ContentId`s and identities that the export
//! allowlist forbids.
//!
//! There is no network client in this module and no upload path.
//! Collection is not sharing: the artifact stays in the local
//! temporary directory until the user copies or sends it themselves.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::diagnostics_view::{self, Diagnostics};
use wyrd_settings::app_group_identifier;

/// The extension writes this file inside its app-group `Diagnostics/`
/// leaf. The group id comes from the one shared accessor (env override
/// with the team-prefixed default); it is never taken from front-end
/// input.
const RECORD_RELATIVE_PATH: &str = "Diagnostics/records.jsonl";

/// The two gates every field must pass: the Swift builder marked it
/// `exportable`, and this allowlist names it explicitly. Anything
/// else — file contents, credentials, keys, identities, content ids,
/// the wire — is dropped even if it appeared in a source record.
pub const ALLOWED_FIELDS: &[&str] = &[
    "session",
    "filename",
    "content-type",
    "parent",
    "symlink-target",
    "options",
    "staged-contents",
    "store-error",
    "error-domain",
    "error-code",
    "error-detail",
];

/// Bounds on what one export may read and emit. A hand-edited or
/// oversized sink cannot turn a small local artifact into an
/// unbounded copy of unrelated data.
/// Read at most this many bytes from the extension's sink.
pub const MAX_SOURCE_BYTES: usize = 512 * 1024;
/// Emit at most this many records in one bundle.
pub const MAX_RECORDS: usize = 200;
/// Clamp any single field value to this many characters.
pub const MAX_FIELD_CHARS: usize = 4096;

/// One allowlisted record in the bundle. `source` is a closed local
/// label, `session` an ephemeral correlation id, never a protocol
/// identity.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BundleRecord {
    /// When the source observed the event, as supplied by the source.
    pub timestamp: String,
    /// Closed local label: `fileprovider` or `desktop`.
    pub source: &'static str,
    /// Ephemeral local correlation id, never a protocol identity.
    pub session: String,
    /// Safe operation/stage summary from the source.
    pub summary: String,
    /// Only allowlisted field names and bounded values.
    pub fields: BTreeMap<String, String>,
}

/// The temporary artifact's shape. Versioned so a future field can
/// be added without guessing what an older bundle contained.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Bundle {
    /// Schema version of the bundle document.
    pub version: u32,
    /// Unix seconds at generation time.
    pub generated_at: u64,
    /// Random local id for this export, shared by its desktop record.
    pub export_session: String,
    /// The persisted app-wide level at generation time.
    pub app_level: String,
    /// Allowlisted records from the local sinks.
    pub records: Vec<BundleRecord>,
}

/// Result of an explicit export. Like the other local queries, the
/// failure is a state inside the payload, never a rejection the
/// frontend could mistake for an owner answer.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum DiagnosticsExport {
    /// The temporary artifact was created and written.
    Written {
        /// Absolute path of the new local file.
        path: String,
        /// How many records the file holds.
        records: usize,
        /// Unix seconds at generation time.
        generated_at: u64,
    },
    /// The artifact could not be created or written.
    Failed {
        /// Local reason, never an owner answer.
        reason: String,
    },
}

/// Random, ephemeral local correlation id. The same audited leaf the
/// owner uses: no protocol identity, never entered into a network
/// message. The clock/pid fallback only separates runs; nothing
/// protocol-deciding reads this value.
fn new_session_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_ok() {
        return bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    }
    let nanos = now_secs().saturating_mul(1_000_000_000);
    format!("{:016x}{:016x}", std::process::id(), nanos)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|age| age.as_secs())
        .unwrap_or(0)
}

fn level_name(view: &Diagnostics) -> String {
    match view {
        Diagnostics::Configured { level, .. } => level.as_str().to_string(),
        Diagnostics::Nofile { .. } => "default".to_string(),
        Diagnostics::Unreadable { .. } => "unknown".to_string(),
    }
}

/// Parse the extension's JSONL sink into allowlisted records. Unknown
/// keys, malformed lines, missing sessions, and fields that are not
/// strings are dropped, never carried through as raw text.
fn records_from_jsonl(bytes: &[u8], export_session: &str) -> Vec<BundleRecord> {
    let text = String::from_utf8_lossy(bytes);
    let mut used = 0usize;
    let mut records = Vec::new();
    for line in text.lines() {
        if records.len() >= MAX_RECORDS || used > MAX_SOURCE_BYTES {
            break;
        }
        used = used.saturating_add(line.len().saturating_add(1));
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(object) = value.as_object() else {
            continue;
        };
        if object.get("version").and_then(|v| v.as_u64()) != Some(1) {
            continue;
        }
        let Some(timestamp) = object.get("timestamp").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(summary) = object.get("summary").and_then(|v| v.as_str()) else {
            continue;
        };
        // These two are the only fields outside `fields`, and the
        // trusted builder controls their shape. Validate it anyway:
        // a hand-edited sink must not become a free-text surface.
        if !valid_timestamp(timestamp) || !valid_summary(summary) {
            continue;
        }
        let Some(source_fields) = object.get("fields").and_then(|v| v.as_object()) else {
            continue;
        };
        let mut fields = BTreeMap::new();
        for key in ALLOWED_FIELDS {
            if let Some(value) = source_fields.get(*key).and_then(|v| v.as_str()) {
                fields.insert((*key).to_string(), truncate(value));
            }
        }
        let Some(session) = fields.get("session").cloned() else {
            continue;
        };
        if session.is_empty() {
            continue;
        }
        records.push(BundleRecord {
            timestamp: truncate(timestamp),
            source: "fileprovider",
            session,
            summary: truncate(summary),
            fields,
        });
    }
    // A session is always attached so records from one export can be
    // correlated without borrowing a protocol identity.
    debug_assert!(!export_session.is_empty());
    records
}

/// RFC 3339 timestamp from `ISO8601DateFormatter`, with no free text.
fn valid_timestamp(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | ':' | '.' | 'T' | 'Z' | '+' | ' '))
}

/// Closed operation names mirrored from the Swift `DiagnosticOperation`
/// enum. The compile-time parity test keeps the two in step.
const OPERATIONS: &[&str] = &["createItem", "modifyItem", "deleteItem"];

/// Closed stage names mirrored from the Swift `DiagnosticStage` enum.
const STAGES: &[&str] = &[
    "cancelled",
    "excluded",
    "symlinkTemplate",
    "trashRefused",
    "stagedContentsRead",
    "mutation",
    "itemLookup",
    "baseVersionMismatch",
    "rename",
    "contentOnNonFile",
    "missingStagedContents",
    "contentWrite",
    "completed",
];

/// The builder emits exactly `operation outcome stage=<closed stage>`.
/// A closed shape keeps this field from carrying unrelated user text.
fn valid_summary(value: &str) -> bool {
    let mut parts = value.split(' ');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(op), Some(outcome), Some(stage), None) => {
            OPERATIONS.contains(&op)
                && matches!(outcome, "ok" | "failed")
                && stage
                    .strip_prefix("stage=")
                    .is_some_and(|name| STAGES.contains(&name))
        }
        _ => false,
    }
}

fn truncate(value: &str) -> String {
    let mut out = value.chars().take(MAX_FIELD_CHARS).collect::<String>();
    if value.chars().count() > MAX_FIELD_CHARS {
        out.push('…');
    }
    out
}

/// Resolve the extension's sink from the running user's home. A
/// missing home is not an error: the bundle simply has no extension
/// records and says so through the desktop record it still contains.
fn default_container() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let home = home.to_string_lossy();
    if home.is_empty() {
        return None;
    }
    Some(
        Path::new(&*home)
            .join("Library/Group Containers")
            .join(app_group_identifier()),
    )
}

/// Build one bundle from explicit local paths. Kept pure over its
/// inputs so the exclusion tests can stage fixtures without touching
/// the user's home.
pub fn build_bundle_at(container: Option<&Path>, settings_path: Option<&Path>) -> Bundle {
    let export_session = new_session_id();
    let generated_at = now_secs();
    let mut records = Vec::new();

    if let Some(container) = container {
        let sink = container.join(RECORD_RELATIVE_PATH);
        if let Ok(mut file) = std::fs::File::open(&sink) {
            // Bound the read itself, not just the parse: a hand-edited
            // sink must never drive an unbounded allocation.
            let mut bytes = Vec::new();
            if file
                .by_ref()
                .take(MAX_SOURCE_BYTES as u64)
                .read_to_end(&mut bytes)
                .is_ok()
            {
                records.extend(records_from_jsonl(&bytes, &export_session));
            }
        }
    }

    let persisted = match settings_path {
        Some(path) => diagnostics_view::diagnostics_at(path),
        None => diagnostics_view::diagnostics(),
    };
    records.push(BundleRecord {
        timestamp: generated_at.to_string(),
        source: "desktop",
        session: export_session.clone(),
        summary: format!("bundle exported at level {}", level_name(&persisted)),
        fields: BTreeMap::new(),
    });

    Bundle {
        version: 1,
        generated_at,
        export_session,
        app_level: level_name(&persisted),
        records,
    }
}

/// Build and write one temporary local artifact. The file is private
/// to this user and created new — an existing bundle is never
/// overwritten. No network call exists on this path.
pub fn export_bundle() -> DiagnosticsExport {
    let out_dir = std::env::temp_dir();
    export_bundle_at(default_container().as_deref(), None, &out_dir)
}

/// Testable half of [`export_bundle`] with explicit local paths.
pub fn export_bundle_at(
    container: Option<&Path>,
    settings_path: Option<&Path>,
    out_dir: &Path,
) -> DiagnosticsExport {
    let bundle = build_bundle_at(container, settings_path);
    let path = out_dir.join(format!("wyrd-diagnostics-{}.json", bundle.export_session));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        // Set the mode at creation: a umask-derived 0644 window would
        // briefly expose the artifact to other local users.
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(file) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Err(error) = file.set_permissions(std::fs::Permissions::from_mode(0o600)) {
                    return DiagnosticsExport::Failed {
                        reason: format!("secure bundle: {error}"),
                    };
                }
            }
            match serde_json::to_writer_pretty(file, &bundle) {
                Ok(()) => DiagnosticsExport::Written {
                    path: path.to_string_lossy().into_owned(),
                    records: bundle.records.len(),
                    generated_at: bundle.generated_at,
                },
                Err(error) => DiagnosticsExport::Failed {
                    reason: format!("write bundle: {error}"),
                },
            }
        }
        Err(error) => DiagnosticsExport::Failed {
            reason: format!("create bundle: {error}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Bundle {
        let dir = std::env::temp_dir().join(format!("wyrd-bundle-fixture-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let container = dir.join("container");
        let sink = container.join(RECORD_RELATIVE_PATH);
        std::fs::create_dir_all(sink.parent().unwrap()).unwrap();
        let settings = dir.join("settings.json");
        std::fs::write(&settings, r#"{"version":2,"drives":{}}"#).unwrap();
        let line = serde_json::json!({
            "version": 1,
            "timestamp": "2026-01-01T00:00:00Z",
            "summary": "createItem failed stage=symlinkTemplate",
            "fields": {
                "session": "ext-session",
                "filename": "report.pdf",
                "symlink-target": "/Users/me/iCloud Drive/report.pdf",
                "content-id": "must-not-appear",
                "file-contents": "secret bytes",
                "credentials": "secret",
                "identity": "device-key"
            }
        });
        std::fs::write(&sink, format!("{line}\n")).unwrap();
        let bundle = build_bundle_at(Some(&container), Some(&settings));
        std::fs::remove_dir_all(&dir).unwrap();
        bundle
    }

    #[test]
    fn only_allowlisted_fields_reach_the_bundle() {
        let bundle = fixture();
        let record = bundle
            .records
            .iter()
            .find(|record| record.source == "fileprovider")
            .expect("the extension record was collected");
        assert_eq!(record.fields.get("filename").unwrap(), "report.pdf");
        assert_eq!(
            record.fields.get("symlink-target").unwrap(),
            "/Users/me/iCloud Drive/report.pdf"
        );
        for forbidden in ["content-id", "file-contents", "credentials", "identity"] {
            assert!(
                !bundle_json(&bundle).contains(forbidden),
                "{forbidden} must not appear in the artifact"
            );
        }
    }

    fn bundle_json(bundle: &Bundle) -> String {
        serde_json::to_string(bundle).unwrap()
    }

    /// A hand-edited sink cannot smuggle unrelated records through:
    /// bad versions, non-object lines, missing sessions and unknown
    /// keys are all dropped, and the record bound holds.
    #[test]
    fn malformed_and_overlong_lines_are_dropped() {
        let mut lines = Vec::new();
        for version in [1, 2] {
            lines.push(format!(
                r#"{{"version":{version},"timestamp":"2026-01-01T00:00:00Z","summary":"createItem failed stage=symlinkTemplate","fields":{{"session":"x"}}}}"#
            ));
        }
        lines.push("not json".to_string());
        lines.push(
            r#"{"version":1,"timestamp":"2026-01-01T00:00:00Z","summary":"createItem failed stage=symlinkTemplate","fields":{}}"#
                .to_string(),
        );
        for index in 0..(MAX_RECORDS + 10) {
            lines.push(format!(
                r#"{{"version":1,"timestamp":"2026-01-01T00:00:00Z","summary":"createItem failed stage=symlinkTemplate","fields":{{"session":"s{index}"}}}}"#
            ));
        }
        let joined = lines.join("\n");
        let records = records_from_jsonl(joined.as_bytes(), "export");
        assert_eq!(records.len(), MAX_RECORDS);
        assert!(records.iter().all(|record| record.source == "fileprovider"));
    }

    /// Correlation ids are random and ephemeral: two exports differ,
    /// and the value is a plain hex string, never a DriveId, a Nostr
    /// key, or a protocol id with a prefix.
    #[test]
    fn export_session_is_random_and_ephemeral() {
        let first = new_session_id();
        let second = new_session_id();
        assert_ne!(first, second);
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Collection is not transmission. The module contains no
    /// networking client, upload, or endpoint; this source pin makes
    /// adding one a visible contract break.
    #[test]
    fn export_has_no_transmission_path() {
        let source = include_str!("diagnostics_bundle.rs");
        // Only the production half counts: the test names these
        // tokens deliberately, and the module comment says what is
        // absent. Look for code paths, not prose.
        let production = source
            .split("#[cfg(test)]")
            .next()
            .unwrap_or(source)
            .to_ascii_lowercase();
        for forbidden in [
            "std::net::",
            "reqwest::",
            "ureq::",
            "tcpstream",
            "httpclient",
        ] {
            assert!(
                !production.contains(forbidden),
                "the exporter must not gain a transmission path: {forbidden}"
            );
        }
    }

    /// The artifact is created new, private, and reports its record
    /// count; a second export never overwrites the first.
    #[test]
    fn export_writes_a_temporary_private_artifact() {
        let dir = std::env::temp_dir().join(format!("wyrd-bundle-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = export_bundle_at(None, None, &dir);
        let second = export_bundle_at(None, None, &dir);
        let (path, count) = match first {
            DiagnosticsExport::Written {
                path,
                records,
                generated_at: _,
            } => (path, records),
            other => panic!("export wrote a file: {other:?}"),
        };
        assert!(count >= 1, "the desktop record is always present");
        assert!(Path::new(&path).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the artifact is owner-only from creation");
        }
        assert!(
            matches!(second, DiagnosticsExport::Written { .. }),
            "a second export mints a new file"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The `case` lines of one Swift enum, read at compile time.
    fn swift_cases(source: &str, name: &str) -> Vec<String> {
        let start = source
            .find(&format!("enum {name}"))
            .unwrap_or_else(|| panic!("Swift enum {name} is missing"));
        let body = &source[start..];
        let end = body.find('}').expect("enum body closes");
        body[..end]
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("case ")
                    .map(|case| case.trim().trim_end_matches(',').to_string())
            })
            .collect()
    }

    /// Quoted keys in the Swift sink's `allowedKeys` array.
    fn swift_allowed_keys(source: &str) -> Vec<String> {
        let start = source
            .find("static let allowedKeys")
            .expect("the Swift sink allowlist exists");
        let body = &source[start..];
        let end = body.find(']').expect("allowlist array closes");
        body[..end]
            .lines()
            .filter_map(|line| line.trim().strip_prefix('"'))
            .filter_map(|line| line.split_once('"').map(|(key, _)| key.to_string()))
            .collect()
    }

    /// The closed enums and allowlists are duplicated across languages
    /// by design. Both directions are pinned: a Rust name missing in
    /// Swift would break collection, and a new Swift case or field
    /// missing here would silently drop every record that used it.
    #[test]
    fn cross_language_closed_lists_stay_in_step() {
        let swift_diagnostics =
            include_str!("../../../packaging/macos/fileprovider/Sources/Diagnostics.swift");
        let swift_sink = include_str!(
            "../../../packaging/macos/fileprovider/Sources/DiagnosticsRecordSink.swift"
        );

        let swift_operations = swift_cases(swift_diagnostics, "DiagnosticOperation");
        let swift_stages = swift_cases(swift_diagnostics, "DiagnosticStage");
        let swift_fields = swift_allowed_keys(swift_sink);

        for (rust, swift) in [
            (OPERATIONS, &swift_operations),
            (STAGES, &swift_stages),
            (ALLOWED_FIELDS, &swift_fields),
        ] {
            for name in rust {
                assert!(
                    swift.iter().any(|case| case == name),
                    "Rust name {name} is missing in the Swift source"
                );
            }
            for name in swift {
                assert!(
                    rust.contains(&name.as_str()),
                    "Swift added {name} without updating the Rust list"
                );
            }
        }
    }

    /// A hand-edited sink cannot turn the two source-controlled fields
    /// into a free-text surface. Their closed shapes are validated.
    #[test]
    fn free_text_timestamp_and_summary_are_dropped() {
        let valid =
            r#"{"version":1,"timestamp":"2026-01-01T00:00:00Z","summary":"createItem failed stage=symlinkTemplate","fields":{"session":"x"}}"#
                .to_string();
        let bad_stamp = valid.replace("2026-01-01T00:00:00Z", "private meeting notes");
        let bad_summary = valid.replace(
            "createItem failed stage=symlinkTemplate",
            "someone's unrelated document",
        );
        assert_eq!(records_from_jsonl(valid.as_bytes(), "export").len(), 1);
        assert!(records_from_jsonl(bad_stamp.as_bytes(), "export").is_empty());
        assert!(records_from_jsonl(bad_summary.as_bytes(), "export").is_empty());
    }
}
