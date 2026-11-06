//! Receive-path retention admission gate (DG-4 A).
//!
//! Local authorship is quota-checked, but remote admission was not: a
//! remote member's content was pulled under the materialization policy
//! and retained locally with no gate. This module closes that gap:
//! every fetched representation is admitted against the same retention
//! ceiling as the authoring path before it reaches durable storage.
//!
//! Two counters, one ceiling:
//!
//! - `RetainedBytes` (`wyrd-format`): plaintext object-store bytes,
//!   maintained by the store itself;
//! - `VaultSeen` (`serving::mirror`, beside mirror accounting where
//!   ciphertext already lives): sealed vault bytes observed through
//!   replication, seeded by the open-time vault-dir walk so torn-
//!   authoring orphans with no naming fact still charge the gate.
//!
//! Refused bytes are never stored and charged nowhere. A refusal never
//! reads as successful replication (no fact commits, no provider claim,
//! no serving request satisfied, no announcement advances on its
//! account), never enters loss/repair as lost, and survives restart via
//! the durable ledger below — a restart cannot turn "refused" into
//! "promised".
//!
//! No wire change: refusal is local policy, operator-visible locally
//! (`policy budgets` names the ceiling, the sync report counts
//! refusals). No durable `Fact`: the ledger is a sidecar file in the
//! drive directory, alongside committed facts, never in the commit log
//! (retention-constraints.md forbids a refusal `Fact` in v0.3).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use wyrd_format::RetainedBytes;

use crate::serving::VaultSeen;

/// File holding the durable refusal record, beside the drive's
/// commits and vault — alongside committed facts, never in them.
const REFUSAL_FILE: &str = "retention-refusals";

/// Version line for the refusal file. Bumped if the unit encoding
/// ever changes; unknown versions fail closed on load.
const REFUSAL_VERSION: &str = "v1";

/// Why one representation was refused: the ceiling and the numbers it
/// was compared against. Rendered in the trace log and the operator
/// surface; never on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionRefusal {
    /// The configured ceiling, in bytes.
    pub quota: u64,
    /// Plaintext bytes already retained.
    pub retained: u64,
    /// Vault bytes already observed.
    pub vault_seen: u64,
    /// Incoming vault bytes that were refused.
    pub incoming_vault: u64,
    /// Incoming plaintext bytes that were refused.
    pub incoming_retained: u64,
}

impl std::fmt::Display for RetentionRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "retention ceiling {} refused admission (retained {} + vault-seen {} + incoming vault {} + incoming retained {} would exceed it): \
             bytes not stored, nothing committed, retry after raising the ceiling or freeing retained bytes",
            self.quota, self.retained, self.vault_seen, self.incoming_vault, self.incoming_retained
        )
    }
}

impl std::error::Error for RetentionRefusal {}

/// The admission gate: one ceiling over two counters. `None` quota is
/// unlimited (dogfood default) and always admits. A quota without a
/// plaintext accountant cannot be evaluated — composition refuses that
/// pairing before the gate is ever consulted, so the gate treats it as
/// unlimited rather than refusing everything.
#[derive(Debug, Clone)]
pub struct RetentionGate {
    quota: Option<u64>,
    retained: Option<Arc<RetainedBytes>>,
    seen: Arc<VaultSeen>,
}

impl RetentionGate {
    /// Build the gate the fetch legs consult. Cheap clones: the
    /// counters are shared, never copied.
    pub fn new(
        quota: Option<u64>,
        retained: Option<Arc<RetainedBytes>>,
        seen: Arc<VaultSeen>,
    ) -> Self {
        RetentionGate {
            quota,
            retained,
            seen,
        }
    }

    /// Whether any ceiling is configured.
    pub fn has_quota(&self) -> bool {
        self.quota.is_some()
    }

    /// The configured ceiling, if any.
    pub fn quota(&self) -> Option<u64> {
        self.quota
    }

    /// Admit `incoming_vault` sealed bytes plus `incoming_retained`
    /// plaintext bytes against the ceiling. Refused bytes must never
    /// reach either store — callers check before `vault.import` and
    /// before `objects.insert_verified`.
    pub fn check(
        &self,
        incoming_vault: u64,
        incoming_retained: u64,
    ) -> Result<(), RetentionRefusal> {
        let (Some(quota), Some(retained)) = (self.quota, self.retained.as_ref()) else {
            return Ok(());
        };
        let held = retained.get();
        let seen = self.seen.get();
        if held
            .saturating_add(seen)
            .saturating_add(incoming_vault)
            .saturating_add(incoming_retained)
            >= quota
        {
            return Err(RetentionRefusal {
                quota,
                retained: held,
                vault_seen: seen,
                incoming_vault,
                incoming_retained,
            });
        }
        Ok(())
    }
}

/// Durable refusal record: the refused retention units plus the
/// ceiling that refused each one, sufficient for a reboot to preserve
/// the same refusal decision and for the operator to read which
/// ceiling caused it. The unit names the refused representation,
/// never the bytes:
///
/// - `snapshot-body:<snapshot-hex>` — a snapshot body;
/// - `root:<snapshot-hex>` — a root manifest;
/// - `manifest:<content-hex>` — a child manifest;
/// - `object:<content-hex>` — a file-content object.
///
/// Content is content-addressed, so the id is the unit: re-admission
/// of the same id under the same ceiling decides identically. Raising
/// the ceiling (or freeing retained bytes) re-admits on retry — the
/// ledger records what was refused under which ceiling, it never
/// pins a permanent ban.
pub struct RefusalLedger {
    path: PathBuf,
    refused: Mutex<HashMap<String, u64>>,
}

impl std::fmt::Debug for RefusalLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefusalLedger")
            .field("path", &self.path)
            .field(
                "refused",
                &self.refused.lock().map(|set| set.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl RefusalLedger {
    /// Load the ledger for a drive directory. A missing file is an
    /// empty ledger; a torn file fails closed — refusing to run on an
    /// unreadable refusal record is safer than forgetting refusals.
    pub fn load(drive_dir: &Path) -> Result<Self, String> {
        let path = drive_dir.join(REFUSAL_FILE);
        let refused = match std::fs::read_to_string(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => {
                return Err(format!(
                    "refusal ledger unreadable {}: {error}",
                    path.display()
                ));
            }
            Ok(text) => parse_ledger(&text)?,
        };
        Ok(RefusalLedger {
            path,
            refused: Mutex::new(refused),
        })
    }

    /// The ledger file path, for diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether `unit` was refused. Lock-poisoned reads fail closed
    /// (report refused rather than forgetting a refusal).
    pub fn contains(&self, unit: &str) -> bool {
        self.refused
            .lock()
            .map(|map| map.contains_key(unit))
            .unwrap_or(true)
    }

    /// How many distinct units were refused.
    pub fn len(&self) -> usize {
        self.refused.lock().map(|map| map.len()).unwrap_or(0)
    }

    /// Whether any refusal is recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Record a refusal durably under the ceiling that caused it.
    /// Idempotent per ceiling: re-recording a known unit under the
    /// same ceiling returns `false` and rewrites nothing; a new unit
    /// — or a known unit refused under a different ceiling — rewrites
    /// the file atomically (temp + fsync + rename + dir fsync), so a
    /// crash leaves the previous ledger or the full new one — never a
    /// torn half. Lock poisoning or I/O failure returns an error and
    /// the in-memory map keeps the entry for this lifetime regardless.
    /// Cost: one whole-file rewrite per newly refused unit — O(N)
    /// fsyncs on a first wave of many distinct over-quota
    /// representations. Retries under the same ceiling are free, and
    /// single-writer (store lock) ordering makes the rewrite correct;
    /// batch per-pass writes only if dogfood ever shows this biting.
    pub fn record(&self, unit: &str, quota: u64) -> Result<bool, String> {
        let mut map = self
            .refused
            .lock()
            .map_err(|_| "refusal ledger lock poisoned".to_string())?;
        if map.get(unit) == Some(&quota) {
            return Ok(false);
        }
        map.insert(unit.to_string(), quota);
        let snapshot: Vec<(String, u64)> = map
            .iter()
            .map(|(unit, quota)| (unit.clone(), *quota))
            .collect();
        drop(map);
        // The in-memory map already holds the entry, so this lifetime
        // still refuses it when durability fails below; the error
        // tells the caller durability did not follow.
        write_ledger(&self.path, &snapshot)?;
        Ok(true)
    }

    /// Units refused, sorted, for the operator surface. Lock-poisoned
    /// reads report nothing rather than blocking the surface.
    pub fn units(&self) -> Vec<String> {
        let mut units: Vec<String> = self
            .refused
            .lock()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default();
        units.sort();
        units
    }

    /// Distinct ceilings that caused refusals, sorted, for the
    /// operator surface (`policy budgets` names the ceiling behind
    /// its refusal counts). Empty when nothing was refused.
    pub fn ceilings(&self) -> Vec<u64> {
        let mut ceilings: Vec<u64> = self
            .refused
            .lock()
            .map(|map| {
                let mut ceilings: Vec<u64> = map.values().copied().collect();
                ceilings.sort_unstable();
                ceilings.dedup();
                ceilings
            })
            .unwrap_or_default();
        ceilings.sort_unstable();
        ceilings.dedup();
        ceilings
    }

    /// Refused units with the ceiling that refused each, sorted by
    /// unit, for the operator surface.
    pub fn entries(&self) -> Vec<(String, u64)> {
        let mut entries: Vec<(String, u64)> = self
            .refused
            .lock()
            .map(|map| {
                map.iter()
                    .map(|(unit, quota)| (unit.clone(), *quota))
                    .collect()
            })
            .unwrap_or_default();
        entries.sort();
        entries
    }
}

/// Parse the ledger file: a `v1` version line, then one `unit quota`
/// pair per line (quota in bytes, decimal). Blank lines are ignored.
/// Anything else fails closed. Units without a quota (pre-ceiling
/// ledgers) never shipped — the ledger is new in the gate's PR — so
/// no migration arm exists.
fn parse_ledger(text: &str) -> Result<HashMap<String, u64>, String> {
    let mut lines = text.lines();
    match lines.next() {
        Some(REFUSAL_VERSION) => {}
        Some(other) => {
            return Err(format!("unknown refusal ledger version: {other}"));
        }
        None => return Err("refusal ledger is empty".to_string()),
    }
    let mut map = HashMap::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((unit, quota)) = line.split_once(' ') else {
            return Err(format!("malformed refusal ledger entry: {line}"));
        };
        if !is_unit(unit) {
            return Err(format!("malformed refusal ledger entry: {line}"));
        }
        let quota: u64 = quota
            .parse()
            .map_err(|_| format!("malformed refusal ledger entry: {line}"))?;
        map.insert(unit.to_string(), quota);
    }
    Ok(map)
}

/// Whether `unit` is a well-formed ledger entry.
fn is_unit(unit: &str) -> bool {
    let Some((kind, hex)) = unit.split_once(':') else {
        return false;
    };
    if !matches!(kind, "snapshot-body" | "root" | "manifest" | "object") {
        return false;
    }
    hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Rewrite the ledger file atomically. Single-writer (the store lock
/// serializes engines), so a whole-file rewrite cannot race a peer.
fn write_ledger(path: &Path, entries: &[(String, u64)]) -> Result<(), String> {
    let mut sorted = entries.to_vec();
    sorted.sort();
    let mut text = String::from(REFUSAL_VERSION);
    text.push('\n');
    for (unit, quota) in &sorted {
        text.push_str(unit);
        text.push(' ');
        text.push_str(&quota.to_string());
        text.push('\n');
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text.as_bytes())
        .map_err(|error| format!("refusal ledger write failed {}: {error}", tmp.display()))?;
    let file = std::fs::File::open(&tmp)
        .map_err(|error| format!("refusal ledger fsync failed {}: {error}", tmp.display()))?;
    file.sync_all()
        .map_err(|error| format!("refusal ledger fsync failed {}: {error}", tmp.display()))?;
    drop(file);
    std::fs::rename(&tmp, path)
        .map_err(|error| format!("refusal ledger rename failed {}: {error}", path.display()))?;
    let parent = path.parent().unwrap_or(Path::new("."));
    match std::fs::File::open(parent) {
        Ok(dir) => dir
            .sync_all()
            .map_err(|error| format!("refusal ledger dir sync failed: {error}"))?,
        Err(error) => return Err(format!("refusal ledger dir open failed: {error}")),
    }
    Ok(())
}

/// Refusal unit constructors: one per admission leg.
pub fn snapshot_body_unit(snapshot: &wyrd_format::SnapshotId) -> String {
    format!("snapshot-body:{snapshot}")
}

/// Refusal unit constructors: one per admission leg.
pub fn root_unit(snapshot: &wyrd_format::SnapshotId) -> String {
    format!("root:{snapshot}")
}

/// Refusal unit constructors: one per admission leg.
pub fn manifest_unit(id: &wyrd_format::ContentId) -> String {
    format!("manifest:{id}")
}

/// Refusal unit constructors: one per admission leg.
pub fn object_unit(id: &wyrd_format::ContentId) -> String {
    format!("object:{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_gate_admits_everything() {
        let seen = VaultSeen::new();
        let gate = RetentionGate::new(None, None, seen);
        assert!(gate.check(u64::MAX / 2, u64::MAX / 2).is_ok());
        assert!(!gate.has_quota());
    }

    #[test]
    fn quota_without_accountant_admits() {
        // Composition refuses this pairing before the gate runs; the
        // gate itself must not refuse everything on a half-wired config.
        let seen = VaultSeen::new();
        seen.add(10);
        let gate = RetentionGate::new(Some(5), None, seen);
        assert!(gate.check(1_000_000, 1_000_000).is_ok());
    }

    #[test]
    fn gate_covers_both_counters_and_incoming() {
        let seen = VaultSeen::new();
        seen.add(40);
        let retained = RetainedBytes::new();
        retained.add(40);
        let gate = RetentionGate::new(Some(100), Some(Arc::clone(&retained)), Arc::clone(&seen));
        assert!(gate.check(10, 9).is_ok());
        let refusal = gate.check(10, 10).unwrap_err();
        assert_eq!(refusal.quota, 100);
        assert_eq!(refusal.retained, 40);
        assert_eq!(refusal.vault_seen, 40);
    }

    #[test]
    fn ledger_round_trips_and_rejects_garbage() {
        let dir = std::env::temp_dir().join(format!(
            "wyrd-refusals-{}-{}",
            std::process::id(),
            std::sync::atomic::AtomicU64::new(0).fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ledger = RefusalLedger::load(&dir).unwrap();
        assert!(ledger.is_empty());
        assert!(ledger
            .record(
                "object:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                100
            )
            .unwrap());
        assert!(!ledger
            .record(
                "object:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                100
            )
            .unwrap());
        // Same unit under a new ceiling rewrites (the ceiling that
        // caused the refusal is operator data, not just the unit).
        assert!(ledger
            .record(
                "object:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                50
            )
            .unwrap());
        assert!(ledger
            .contains("object:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"));
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger.ceilings(), vec![50]);
        let reloaded = RefusalLedger::load(&dir).unwrap();
        assert!(reloaded
            .contains("object:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"));
        assert_eq!(reloaded.ceilings(), vec![50]);
        assert_eq!(
            reloaded.entries(),
            vec![(
                "object:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                    .to_string(),
                50
            )]
        );
        assert!(parse_ledger("v2\n").is_err());
        assert!(parse_ledger("v1\nbogus\n").is_err());
        assert!(parse_ledger(
            "v1\nobject:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n"
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
