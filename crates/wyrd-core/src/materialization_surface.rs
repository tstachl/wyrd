//! Materialization vocabulary: the person-visible words for what
//! this device intends to retain (OD-21-A/B/C, all decided as
//! recommended).
//!
//! This module owns the copy, and only the copy — no policy reads,
//! no commits, no traversal, no rendering. The CLI prints these
//! sentences; the application host (the window that consumes the
//! drive-owner interface) prints the same sentences. One source, two
//! surfaces.
//!
//! Two settled rules shape every renderer:
//!
//! 1. Two policy values (OD-21-A, option A): `PINNED` promises
//!    retention, `REMOTE_ONLY` promises nothing. Content fetched
//!    through access is still `REMOTE_ONLY` — its presence on this
//!    device does not constitute a retention promise.
//! 2. Evict changes intent only (OD-21-C, option A): the confirmation
//!    names the release and the bytes that stay, and `PinnedPresent`
//!    renders unchanged with no live byte figure.

/// The explicit retention promise: guaranteed retained by device policy.
pub const POLICY_PINNED_WORD: &str = "PINNED";

/// Not pinned: retained only while convenient, reclaimable under a
/// future GC. Covers both never-wanted and fetched-by-access content —
/// neither promises anything.
pub const POLICY_REMOTE_ONLY_WORD: &str = "REMOTE_ONLY";

/// What this device intends to retain for one content object, as a
/// word. Two values deliberately: `Cached` (the fetch loop's arrival
/// marking) carries no retention promise and reports as
/// `REMOTE_ONLY` — only an explicit pin promises retention.
pub fn policy_word(pinned: bool) -> &'static str {
    if pinned {
        POLICY_PINNED_WORD
    } else {
        POLICY_REMOTE_ONLY_WORD
    }
}

/// Whether the durable log records the bytes as local. Orthogonal to
/// policy: eviction changes intent, never deletes bytes.
pub const PRESENCE_PRESENT_WORD: &str = "PRESENT";

/// Bytes are not local.
pub const PRESENCE_ABSENT_WORD: &str = "ABSENT";

/// What fetched-but-unpinned content is: still `REMOTE_ONLY`. Its
/// presence on this device does not constitute a retention promise
/// (OD-21-A).
pub const FETCHED_STILL_REMOTE_ONLY: &str = "Content fetched through access is still REMOTE_ONLY. Its presence on this device does not constitute a retention promise.";

/// What evict asks: release the retention promise, keep the bytes.
/// The action does not delete the bytes currently stored on this
/// device (OD-21-C). Dialog copy for the application host's
/// confirmation — the CLI receipt below states the completed fact
/// instead of asking.
pub const EVICT_CONFIRM: &str = "Evict this content from the device's retention policy? This releases the device's retention promise. The action does not delete the bytes currently stored on this device.";

/// What an evict completed: the promise is released, the bytes stay.
/// Receipt copy for the CLI — declarative, past tense — beside the
/// dialog copy above, which asks.
pub const EVICT_RELEASED_NOTE: &str = "eviction released the retention promise only: no bytes deleted, files that stay fully local keep reading";

#[cfg(test)]
#[path = "tests_materialization_surface.rs"]
mod tests_materialization_surface;
