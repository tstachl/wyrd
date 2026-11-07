//! Storage screen: the person-visible storage vocabulary (OD-31-A/B/C/D,
//! all option A).
//!
//! One screen answers one user question — how much of the drive is
//! here, and how much is intended to be — with the census figures
//! above and the quota half below. Every figure name comes from the
//! DG-5 closed vocabulary ([`StorageFigure::label`]); a figure
//! without a variant has no label to print, so the surface cannot
//! emit it. The forbidden words ([`FORBIDDEN_STORAGE_WORDS`]) never
//! render, in any casing.
//!
//! Two halves with different cadences (OD-31-A): the quota half is
//! standing — the retained count is an atomic load on the counter
//! the store already maintains, so the figure that predicts a
//! refused write is always current — while the census half is
//! on demand (a full uncached fact-log replay plus a reachable
//! walk per read). Each half carries its own observation time: the
//! timestamp is the boundary of the claim, and a value observed
//! yesterday is evidence about yesterday. Nothing here is durable
//! (OD-31-D): figures are computed on read, and the UI may retain
//! them for its view lifetime but never as signed state.
//!
//! Refusals (OD-31-B): three conditions reach one `ENOSPC` and only
//! one is distinguishable from a number the screen can see. The
//! screen states the ceiling refusal with its scope, or says
//! plainly that the available measurements cannot distinguish the
//! host causes — and never turns a negative ceiling check into a
//! positive host diagnosis. Deletion is a state change that
//! reclaims nothing, so no refusal copy suggests freeing space.
//!
//! When a host presenter exists to own person-visible copy, the
//! rendering below migrates there and this module keeps the state
//! vocabulary and predicates; until then the copy lives next to
//! the mapping its tests pin (same arrangement as `relay_posture`
//! and `status_surface`).

use std::time::{SystemTime, UNIX_EPOCH};

use wyrd_format::RetainedBytes;

use super::policy::{LocalPresence, RetentionPolicy};
use super::policy::{ResidencyCensus, StorageFigure};

/// The standing count: the atomic retained-bytes figure plus when it
/// was read. Never a census, never a replay — the observation is the
/// counter load itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageReading {
    /// No accountant is wired (no ceiling configured): explicitly
    /// unmeasured. Renders as unmeasured, never zero — a zero would
    /// read as "no bytes retained", the opposite of the truth on any
    /// drive with content.
    Unmeasured,
    /// The owner or observation mechanism is unreachable: no old
    /// observation is shown as current.
    Unavailable,
    /// The counter's value at the observation time.
    Measured { bytes: u64, observed_unix_secs: u64 },
}

impl StorageReading {
    /// Read the standing figure through the owner's handle
    /// (`LiveParts::retained_bytes`), stamping the observation time.
    /// Never opens the store: only the owner holds it.
    pub fn read(accountant: Option<&RetainedBytes>) -> StorageReading {
        match accountant {
            None => StorageReading::Unmeasured,
            Some(counter) => StorageReading::Measured {
                bytes: counter.get(),
                observed_unix_secs: unix_now(),
            },
        }
    }
}

/// Seconds since the Unix epoch: the observation-time stamp. std-only;
/// no calendar crate for a boundary marker.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// The standing quota half: the count plus the ceiling it is compared
/// against. Both arrive from the live composition (`LiveParts`); the
/// composition refuses a quota without its accountant, so a `Some`
/// ceiling always has a comparable count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StandingStorage {
    pub reading: StorageReading,
    /// The configured ceiling (`budgets.retained_bytes_quota`).
    /// `None` is a fact about this device, never "unlimited" — that
    /// word is a ceiling claim by another name.
    pub ceiling: Option<u64>,
}

impl StandingStorage {
    pub fn read(accountant: Option<&RetainedBytes>, ceiling: Option<u64>) -> StandingStorage {
        StandingStorage {
            reading: StorageReading::read(accountant),
            ceiling,
        }
    }

    /// Render the standing half: figure, ceiling with its refusal
    /// scope, distance to refusal, and observation time. Every figure
    /// name is a [`StorageFigure`] label.
    pub fn render(&self) -> String {
        let mut out = String::new();
        match self.reading {
            StorageReading::Measured {
                bytes,
                observed_unix_secs,
            } => {
                out.push_str(&format!(
                    "{}: {bytes} bytes (observed unix {observed_unix_secs})\n",
                    StorageFigure::RetainedContent.label(),
                ));
                match self.ceiling {
                    Some(ceiling) => {
                        // No bare quota number: DG-5 forbids a quota
                        // rendering that omits its refusal scope, so
                        // the scope rides the same sentence.
                        out.push_str(&format!(
                            "  {}: {ceiling} (operator-selected refusal boundary over retained content only; not a ceiling on the device)\n",
                            StorageFigure::RetainedQuota.label()
                        ));
                        if bytes >= ceiling {
                            out.push_str(
                                "  distance to refusal: at or above the ceiling — writes refuse\n",
                            );
                        } else {
                            out.push_str(&format!(
                                "  distance to refusal: {} bytes below the ceiling\n",
                                ceiling - bytes
                            ));
                        }
                    }
                    None => {
                        out.push_str(&format!(
                            "  {}: no retention ceiling is set on this device (unset: local writes and fetched content admit without a retention refusal)\n",
                            StorageFigure::RetainedQuota.label()
                        ));
                    }
                }
            }
            StorageReading::Unmeasured => {
                out.push_str(&format!(
                    "{}: not measured (no retention ceiling is configured on this device)\n",
                    StorageFigure::RetainedContent.label(),
                ));
                out.push_str(&format!(
                    "  {}: no retention ceiling is set on this device (unset: local writes and fetched content admit without a retention refusal)\n",
                    StorageFigure::RetainedQuota.label()
                ));
            }
            StorageReading::Unavailable => {
                out.push_str(&format!(
                    "{}: unavailable (the owner or observation mechanism is unreachable; no old observation is shown as current)\n",
                    StorageFigure::RetainedContent.label(),
                ));
            }
        }
        out
    }
}

/// What the available measurements establish about a refused write
/// (OD-31-B): exactly one arm is the identified ceiling refusal;
/// every other arm says what cannot be established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalAssessment {
    /// Ceiling configured and the measured count at or above it: the
    /// one case the screen can identify.
    CeilingRefused { bytes: u64, ceiling: u64 },
    /// No ceiling configured: Wyrd cannot attribute the refusal to
    /// its own ceiling.
    Unattributable,
    /// Ceiling configured and the measured count below it: the
    /// ceiling does not explain the refusal. Never a positive claim
    /// about the host.
    CeilingExonerated { bytes: u64, ceiling: u64 },
    /// Ceiling configured but the count unmeasured or unreachable:
    /// no comparison is possible either way.
    CannotCompare,
}

/// Classify a refused write from the reading at refusal time. Pure:
/// the caller supplies the observation, the type supplies the words.
/// Call this when a write was actually refused — the standing render
/// above never claims a refusal on its own.
pub fn assess_refusal(reading: StorageReading, ceiling: Option<u64>) -> RefusalAssessment {
    match (reading, ceiling) {
        (
            StorageReading::Measured {
                bytes,
                observed_unix_secs: _,
            },
            Some(ceiling),
        ) if bytes >= ceiling => RefusalAssessment::CeilingRefused { bytes, ceiling },
        (_, None) => RefusalAssessment::Unattributable,
        (
            StorageReading::Measured {
                bytes,
                observed_unix_secs: _,
            },
            Some(ceiling),
        ) => RefusalAssessment::CeilingExonerated { bytes, ceiling },
        _ => RefusalAssessment::CannotCompare,
    }
}

/// Render the refusal explanation: the identified case names
/// itself with count, ceiling, and scope; every other case says
/// what the measurements cannot distinguish instead of choosing.
pub fn explain_refusal(assessment: RefusalAssessment) -> String {
    match assessment {
        RefusalAssessment::CeilingRefused { bytes, ceiling } => format!(
            "the configured Wyrd ceiling refused the write: {} {bytes} bytes against a ceiling of {ceiling} bytes (operator-selected refusal boundary over retained content only; not a ceiling on the device)\n",
            StorageFigure::RetainedContent.label(),
        ),
        RefusalAssessment::Unattributable => "Wyrd cannot attribute this refusal to its own ceiling: no retention ceiling is configured on this device. The refusal may be due to host filesystem capacity or a host filesystem quota; the available measurements cannot distinguish those causes.\n".to_owned(),
        RefusalAssessment::CeilingExonerated { bytes, ceiling } => format!(
            "the configured ceiling does not explain this refusal: {bytes} bytes is below the ceiling of {ceiling} bytes. The underlying cause is not established by the available measurements.\n",
        ),
        RefusalAssessment::CannotCompare => "the configured ceiling may or may not explain this refusal: the retained count was not measured on this device, so no comparison is possible.\n".to_owned(),
    }
}

/// A census figure with its own observation time: explicitly
/// on demand, never confused with the standing count.
pub struct CensusReading<'a> {
    pub census: &'a ResidencyCensus,
    pub observed_unix_secs: u64,
}

impl<'a> CensusReading<'a> {
    /// Stamp an on-demand census just taken. The caller ran the walk;
    /// this only names when.
    pub fn observed(census: &'a ResidencyCensus) -> Self {
        CensusReading {
            census,
            observed_unix_secs: unix_now(),
        }
    }
}

/// Render the census half: reachable-content figures only (the
/// object store as a whole is never scanned), each naming its
/// population, with the conflicts the totals skipped named beside
/// them rather than silently dropped.
pub fn render_census(reading: Option<CensusReading<'_>>) -> String {
    let mut out = String::new();
    let Some(reading) = reading else {
        out.push_str("census (reachable content): not requested in this view — per-path figures refresh on demand, with their own observation time\n");
        return out;
    };
    let census = reading.census;
    let pinned_files = census.quadrant(RetentionPolicy::Pinned, LocalPresence::Present)
        + census.quadrant(RetentionPolicy::Pinned, LocalPresence::Absent);
    out.push_str(&format!(
        "{}: {pinned_files} (observed unix {})\n",
        StorageFigure::PinnedFiles.label(),
        reading.observed_unix_secs,
    ));
    out.push_str(&format!(
        "{}: {} (deduplicated across files that share chunks)\n",
        StorageFigure::PinnedChunks.label(),
        census.pinned_chunk_union().len(),
    ));
    out.push_str(&format!(
        "{}: {} pinned present, {} pinned absent, {} remote-only present, {} remote-only absent (reachable content only; observed unix {})\n",
        StorageFigure::QuadrantFiles.label(),
        census.quadrant(RetentionPolicy::Pinned, LocalPresence::Present),
        census.quadrant(RetentionPolicy::Pinned, LocalPresence::Absent),
        census.quadrant(RetentionPolicy::RemoteOnly, LocalPresence::Present),
        census.quadrant(RetentionPolicy::RemoteOnly, LocalPresence::Absent),
        reading.observed_unix_secs,
    ));
    if !census.conflicts.is_empty() {
        out.push_str(&format!(
            "{}: {} skipped (never walked)\n",
            StorageFigure::ConflictsSkipped.label(),
            census.conflicts.len()
        ));
    }
    if census.symlinks_skipped > 0 {
        out.push_str(&format!(
            "{}: {}\n",
            StorageFigure::SymlinksSkipped.label(),
            census.symlinks_skipped
        ));
    }
    out
}

/// Render the whole screen (OD-31-C option A): census above, quota
/// half below. `refusal` carries the assessment only when a write
/// was actually refused — the standing half never claims a refusal
/// on its own.
pub fn render_storage_screen(
    standing: &StandingStorage,
    census: Option<CensusReading<'_>>,
    refusal: Option<RefusalAssessment>,
) -> String {
    let mut out = String::new();
    out.push_str("storage (reachable content above, quota half below)\n");
    out.push_str(&render_census(census));
    out.push_str(&standing.render());
    if let Some(assessment) = refusal {
        out.push_str(&explain_refusal(assessment));
    }
    out
}

#[cfg(test)]
#[path = "tests_storage_surface.rs"]
mod tests_storage_surface;
