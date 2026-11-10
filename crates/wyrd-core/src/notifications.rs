//! Person notifications: the states Wyrd can actually observe,
//! as an in-application activity list (OD-26-A/B/C, all decided as
//! recommended).
//!
//! Every notification below is a state transition this process
//! observed, never a poll result and never a claim about remote
//! history. The set is small on purpose: the drive could not be
//! opened, a write was refused, the membership epoch froze, pinned
//! bytes have not arrived, a relay's subscription closed and will
//! not heal, a device was removed, this device was removed
//! elsewhere, or a fetch pass left work outstanding. Anything else
//! the mechanism reports — intake deferrals, recovery attempts,
//! acceptance counts, an empty outbox — is observed and
//! deliberately silent ([`SILENT_OBSERVATIONS`]), because a
//! notification for "still working" trains people to dismiss the
//! notifications that mean "wrong".
//!
//! Three rules shape every renderer here:
//!
//! 1. No convergence or freshness claim, ever. The forbidden list
//!    ([`FORBIDDEN_FRESHNESS_WORDS`]) is issue 18's status-surface
//!    list extended, and the two lists live in exactly one place —
//!    here — so they cannot drift. The status surface tests pin the
//!    same constant.
//! 2. No payload carries a time. [`Notification`] fields are counts,
//!    epochs, and identities the engine produced; there is no
//!    duration, no millisecond count, and no clock read anywhere in
//!    this module. The application's own check-time lives in the
//!    settings file beside the dedup record (OD-26-C), labelled as
//!    the application's, never as Wyrd's.
//! 3. A notification never fires after teardown begins. The center
//!    models admission close ([`NotificationCenter::begin_teardown`]);
//!    after it, proposals return `None`, because a process that is
//!    no longer reporting anything must not announce.
//!
//! Naming distinction this issue owns: File Provider change
//! notifications (`docs/mobile.md:1092`) are a different mechanism
//! entirely — the OS being told a folder changed, not a person
//! being told anything. Nothing here emits those, and nothing
//! there is this capability.
//!
//! When a host presenter exists to own person-visible copy, the
//! rendering below migrates there and this module keeps the state
//! vocabulary and predicates; until then the copy lives next to
//! the mapping its tests pin (same arrangement as `relay_posture`
//! and `status_surface`). Any future host adapter (OD-26-B option
//! B) carries the same rendered string verbatim, never a shorter
//! variant — a shorter variant is where "Synced" appears.

use std::collections::BTreeSet;

use wyrd_format::{DeviceId, StoreFailure};
use wyrd_sync::durable::DurableError;
use wyrd_sync::runtime::{DrainReport, ExecuteReport};

use crate::mailbox::MailboxHealth;
use crate::member_surface::{HISTORY_NEVER_REENCRYPTED, REMOVAL_BOUNDARY, SOLE_OWNER_TERMINAL};
use crate::mutation::MutationError;

/// Issue 18's forbidden-string list, extended for notifications,
/// living in exactly one place. Both the status surface tests and
/// the notification tests pin this constant, so "up to date" cannot
/// appear in one surface while the other forbids it. Matching is
/// lowercase-substring; [`contains_interval_rendering`] covers
/// the interval renderings no literal list can enumerate.
pub const FORBIDDEN_FRESHNESS_WORDS: &[&str] = &[
    // Issue 18's list, kept verbatim.
    "in sync",
    "up to date",
    "up-to-date",
    "up to-date",
    "all files synced",
    "fully synchronised",
    "fully synchronized",
    "connected",
    "delivered",
    // Issue 26's extension.
    "backup complete",
    "last synced",
    "minutes ago",
    "seconds ago",
    "hours ago",
    "days ago",
    "weeks ago",
    "months ago",
    "years ago",
];

/// True when the text renders a span of time in any form: the word
/// "ago", or a number beside a time unit. Counts without units
/// ("3 outstanding", "epoch 5") are not times and do not match —
/// the scan requires a digit run followed, after optional space,
/// by a time-unit word.
pub fn contains_interval_rendering(text: &str) -> bool {
    const UNITS: &[&str] = &[
        "second", "minute", "hour", "day", "week", "month", "year", "sec", "min", "hr",
    ];
    let lower = text.to_lowercase();
    if lower.contains("ago") {
        return true;
    }
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            let rest = &lower[j..];
            for unit in UNITS {
                let Some(after) = rest.strip_prefix(unit) else {
                    continue;
                };
                // Plural "s" is still a unit; anything else
                // joining the stem (as in "minutes") is covered
                // by the same prefix, while "mind" or "secular"
                // must not match.
                if after.is_empty()
                    || after.starts_with(' ')
                    || after.starts_with('\n')
                    || after.starts_with('s')
                    || after.starts_with(',')
                    || after.starts_with('.')
                {
                    // "min" prefixes "mind"/"minor": require the
                    // plural-or-boundary above, and for the
                    // three-letter stems require it strictly.
                    if *unit == "min" || *unit == "sec" || *unit == "hr" {
                        let next = after.chars().next();
                        if !matches!(
                            next,
                            None | Some(' ') | Some('\n') | Some('s') | Some(',') | Some('.')
                        ) {
                            continue;
                        }
                    }
                    return true;
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    false
}

/// The closed notification vocabulary (OD-26-A, recommendation A):
/// one variant per observed transition worth interrupting a person
/// for. A new variant without a producing observation fails
/// [`tests_notifications::notifications_only_carry_observed_transitions`],
/// whose match over [`NotificationKind::all`] is exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    /// The drive could not be opened: another process holds it.
    DriveOpenRefused,
    /// A write was refused: more than one live head.
    WriteRefusedConflict,
    /// A write was refused: the store reported no space.
    WriteRefusedStorageFull,
    /// The membership epoch froze: rival transitions need an owner.
    EpochFrozen,
    /// Pinned bytes have not arrived and the read is failing.
    PinnedBytesAbsent,
    /// A relay closed our subscription: never heals without action.
    SubscriptionClosed,
    /// A device was removed from a drive this device administers.
    DeviceRemoved,
    /// This device was removed from someone else's drive.
    SelfRemoved,
    /// A fetch pass left work outstanding: still working, not wrong.
    FetchOutstanding,
}

impl NotificationKind {
    /// Every kind, for the completeness audit. A new variant must
    /// appear here and in the test's exhaustive match together.
    pub fn all() -> [NotificationKind; 9] {
        [
            NotificationKind::DriveOpenRefused,
            NotificationKind::WriteRefusedConflict,
            NotificationKind::WriteRefusedStorageFull,
            NotificationKind::EpochFrozen,
            NotificationKind::PinnedBytesAbsent,
            NotificationKind::SubscriptionClosed,
            NotificationKind::DeviceRemoved,
            NotificationKind::SelfRemoved,
            NotificationKind::FetchOutstanding,
        ]
    }

    /// The dedup-key slug, shared with the settings-file record
    /// (OD-26-C): the same string keys the in-process set and the
    /// persisted "notified about X".
    pub fn slug(&self) -> &'static str {
        match self {
            NotificationKind::DriveOpenRefused => "drive-open-refused",
            NotificationKind::WriteRefusedConflict => "write-refused-conflict",
            NotificationKind::WriteRefusedStorageFull => "write-refused-storage-full",
            NotificationKind::EpochFrozen => "epoch-frozen",
            NotificationKind::PinnedBytesAbsent => "pinned-bytes-absent",
            NotificationKind::SubscriptionClosed => "subscription-closed",
            NotificationKind::DeviceRemoved => "device-removed",
            NotificationKind::SelfRemoved => "self-removed",
            NotificationKind::FetchOutstanding => "fetch-outstanding",
        }
    }

    /// The named observation behind the kind: the producer a new
    /// kind must name. Empty for no kind — a kind without an
    /// observation behind it is the failure the audit exists for.
    pub fn observation_source(&self) -> &'static str {
        match self {
            NotificationKind::DriveOpenRefused => {
                "DurableError::StoreLocked on open (wyrd_sync::durable)"
            }
            NotificationKind::WriteRefusedConflict => {
                "MutationError::Conflicted (wyrd_core::mutation)"
            }
            NotificationKind::WriteRefusedStorageFull => {
                "MutationError::Store(StoreFailure::StorageFull) refusing a commit (wyrd_core::mutation, via wyrd_core::live)"
            }
            NotificationKind::EpochFrozen => {
                "contested-transition classification, member-surface frozen-notice inputs (epoch plus rivals)"
            }
            NotificationKind::PinnedBytesAbsent => {
                "pinned-but-not-local read failure (materialization)"
            }
            NotificationKind::SubscriptionClosed => {
                "MailboxHealth.closed_subscriptions rising past zero (wyrd_core::mailbox)"
            }
            NotificationKind::DeviceRemoved => {
                "membership transition removing a device (membership log)"
            }
            NotificationKind::SelfRemoved => {
                "membership transition removing this device (membership log)"
            }
            NotificationKind::FetchOutstanding => {
                "ExecuteReport.unfulfilled above zero (wyrd_sync::runtime)"
            }
        }
    }
}

/// Observed, and deliberately not notified, because a row here must
/// say why. The audit test drives the constructor-callable rows and
/// asserts they yield nothing; the rest name observations no
/// constructor accepts.
pub const SILENT_OBSERVATIONS: &[(&str, &str)] = &[
    (
        "DrainReport.deferred aggregate",
        "mixes membership-unseen holds with budget and quota sheds: the aggregate cannot name a cause, so notifying on it attributes backpressure to a missing peer",
    ),
    (
        "DrainReport.deferred_unseen",
        "held on unseen membership resolves on arrival: still working, not wrong",
    ),
    (
        "DrainReport.deferred_status_blocked",
        "drains by itself on re-drive: diagnostic noise as an alarm",
    ),
    (
        "DrainReport.deferred_shed and sub-counters",
        "relay-retained and revalidated next pass: self-resolving, nothing to diagnose",
    ),
    (
        "ExecuteReport.unfulfilled at zero",
        "no outstanding work is not news",
    ),
    (
        "MutationError::Saturated",
        "admission backpressure: transient, the submitter retries at the point of action",
    ),
    (
        "point-of-action authoring refusals (Invalid, NotFound, AlreadyExists, Stale, TooLarge)",
        "the caller already holds the refusal: nothing to interrupt for",
    ),
    (
        "converged outbox / empty obligations",
        "must never render as a notification: outbox-empty is not current-with-peers",
    ),
    (
        "relay-less idle intake",
        "no observation to notify from: a relay-less sync exits idle by construction",
    ),
    (
        "recovery attempts without a blind close",
        "transient: resolves without operator action, and the blind close is the notifiable transition",
    ),
    (
        "relay acceptance counts",
        "the standing sentence covers delivery: acceptance is not receipt",
    ),
];

/// One person-visible notification: the kind plus the
/// engine-produced facts that distinguish it. Counts, epochs, and
/// identities only — no duration, no millisecond count, no clock
/// read. Adding a field means updating the payload audit in the
/// tests, whose patterns are exhaustive on purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notification {
    /// The drive could not be opened: another process holds it.
    DriveOpenRefused,
    /// A write was refused: more than one live head.
    WriteRefusedConflict {
        /// Live-head count behind the refusal.
        heads: usize,
    },
    /// A write was refused: the store reported no space.
    WriteRefusedStorageFull,
    /// The membership epoch froze.
    EpochFrozen {
        /// The frozen epoch.
        epoch: u64,
        /// Rival transition count needing the owner decision.
        rivals: usize,
    },
    /// Pinned bytes have not arrived and the read is failing.
    PinnedBytesAbsent {
        /// Pinned-object count the read needed.
        pinned: usize,
        /// How many are absent locally.
        missing: usize,
    },
    /// A relay closed our subscription.
    SubscriptionClosed {
        /// Lifetime close count at observation.
        closed: usize,
    },
    /// A device was removed from an administered drive.
    DeviceRemoved {
        /// The removed device, named: the owner already knows members.
        device: DeviceId,
        /// The removal epoch.
        epoch: u64,
        /// Carried heads at removal.
        carried: usize,
        /// The owner set emptied with the author.
        terminal: bool,
    },
    /// This device was removed from someone else's drive.
    SelfRemoved {
        /// The removal epoch.
        epoch: u64,
        /// Carried heads at removal.
        carried: usize,
    },
    /// A fetch pass left work outstanding.
    FetchOutstanding {
        /// Unfulfilled plan items.
        outstanding: usize,
    },
}

impl Notification {
    /// The discriminant vocabulary for the completeness audit.
    pub fn kind(&self) -> NotificationKind {
        match self {
            Notification::DriveOpenRefused => NotificationKind::DriveOpenRefused,
            Notification::WriteRefusedConflict { .. } => NotificationKind::WriteRefusedConflict,
            Notification::WriteRefusedStorageFull => NotificationKind::WriteRefusedStorageFull,
            Notification::EpochFrozen { .. } => NotificationKind::EpochFrozen,
            Notification::PinnedBytesAbsent { .. } => NotificationKind::PinnedBytesAbsent,
            Notification::SubscriptionClosed { .. } => NotificationKind::SubscriptionClosed,
            Notification::DeviceRemoved { .. } => NotificationKind::DeviceRemoved,
            Notification::SelfRemoved { .. } => NotificationKind::SelfRemoved,
            Notification::FetchOutstanding { .. } => NotificationKind::FetchOutstanding,
        }
    }

    /// The dedup key: kind slug plus the distinguishing facts, so
    /// two observations of an unchanged drive key identically and
    /// the second proposes nothing. The same string is the
    /// settings-file record (OD-26-C).
    pub fn key(&self) -> String {
        let slug = self.kind().slug();
        match self {
            Notification::DriveOpenRefused => slug.to_owned(),
            Notification::WriteRefusedConflict { heads } => format!("{slug}:{heads}"),
            Notification::WriteRefusedStorageFull => slug.to_owned(),
            Notification::EpochFrozen { epoch, .. } => format!("{slug}:{epoch}"),
            Notification::PinnedBytesAbsent { pinned, missing } => {
                format!("{slug}:{pinned}:{missing}")
            }
            Notification::SubscriptionClosed { closed } => format!("{slug}:{closed}"),
            Notification::DeviceRemoved { device, epoch, .. } => {
                format!("{slug}:{device}:{epoch}")
            }
            Notification::SelfRemoved { epoch, .. } => format!("{slug}:{epoch}"),
            Notification::FetchOutstanding { outstanding } => format!("{slug}:{outstanding}"),
        }
    }
}

/// Map an open failure to its notification: only the locked store
/// is person-actionable (close the other owner); every other open
/// failure already carries its own diagnostic at the point of
/// action.
pub fn from_open_error(error: &DurableError) -> Option<Notification> {
    match error {
        DurableError::StoreLocked => Some(Notification::DriveOpenRefused),
        _ => None,
    }
}

/// Map a refused mutation to its notification: the two refusals a
/// person was not waiting for (conflict freeze, no space).
/// Point-of-action authoring errors (`Invalid`, `NotFound`,
/// saturation, stale handles) return `None` — the caller already
/// holds those refusals (see [`SILENT_OBSERVATIONS`]).
pub fn from_mutation_error(error: &MutationError) -> Option<Notification> {
    match error {
        MutationError::Conflicted { heads } => {
            Some(Notification::WriteRefusedConflict { heads: *heads })
        }
        MutationError::Store(StoreFailure::StorageFull) => {
            Some(Notification::WriteRefusedStorageFull)
        }
        _ => None,
    }
}

/// Map a pinned read failure to its notification: pinned bytes
/// absent locally while the read fails. Zero missing is not a
/// failure and notifies nothing.
pub fn from_pinned_read_failure(pinned: usize, missing: usize) -> Option<Notification> {
    if missing == 0 {
        return None;
    }
    Some(Notification::PinnedBytesAbsent { pinned, missing })
}

/// Map a frozen epoch to its notification: the epoch plus the rival
/// count needing the owner decision. The member surface owns the
/// resolve wording; this names the transition and points there.
pub fn from_epoch_freeze(epoch: u64, rivals: usize) -> Notification {
    Notification::EpochFrozen { epoch, rivals }
}

/// Map a mailbox-health sample to its notification: the blind
/// close, which never heals without operator action. Any sample
/// without a close on record notifies nothing — degraded-but-
/// attached is transient (see [`SILENT_OBSERVATIONS`]).
pub fn from_mailbox_health(health: &MailboxHealth) -> Option<Notification> {
    if health.closed_subscriptions == 0 {
        return None;
    }
    Some(Notification::SubscriptionClosed {
        closed: health.closed_subscriptions.min(usize::MAX as u64) as usize,
    })
}

/// Map a membership removal to its notification: `self_removed`
/// distinguishes this device removed elsewhere (no device to name
/// — it is the reader) from a device removed from an administered
/// drive. Both carry the boundary sentences verbatim.
pub fn from_removal(
    device: DeviceId,
    epoch: u64,
    carried: usize,
    terminal: bool,
    self_removed: bool,
) -> Notification {
    if self_removed {
        Notification::SelfRemoved { epoch, carried }
    } else {
        Notification::DeviceRemoved {
            device,
            epoch,
            carried,
            terminal,
        }
    }
}

/// Map a fetch pass to its notification: outstanding work is "still
/// fetching", never "missing peer". Zero outstanding notifies
/// nothing.
pub fn from_fetch_report(report: &ExecuteReport) -> Option<Notification> {
    if report.unfulfilled == 0 {
        return None;
    }
    Some(Notification::FetchOutstanding {
        outstanding: report.unfulfilled,
    })
}

/// Map one drain pass plus its fetch report to notifications. A
/// report carrying only `deferred` — in any cause split — notifies
/// nothing: deferral is local intake state, never a staleness
/// signal. The drain observation is taken (the caller proves a pass
/// was observed) and consciously unread; outstanding fetch work
/// notifies exactly once, as "still fetching".
pub fn from_drain(_report: &DrainReport, fetch: &ExecuteReport) -> Vec<Notification> {
    let mut out = Vec::new();
    if let Some(fetching) = from_fetch_report(fetch) {
        out.push(fetching);
    }
    out
}

/// Render the in-application activity-list row: the exact string a
/// future host adapter must carry verbatim (OD-26-B), never a
/// shorter variant. Every line below is pinned against
/// [`FORBIDDEN_FRESHNESS_WORDS`] and
/// [`contains_interval_rendering`].
pub fn render_notification(notification: &Notification) -> String {
    match notification {
        Notification::DriveOpenRefused => {
            "the drive could not be opened: another process holds this drive (store locked) — close the other owner and try again\n"
                .to_owned()
        }
        Notification::WriteRefusedConflict { heads } => {
            format!(
                "a write was refused: the drive has {heads} live heads and a path write needs one tree — the conflict surface carries the decision wording\n"
            )
        }
        // Never a diagnosis: only the storage screen, with its
        // measurements, can attribute a refusal to the ceiling or
        // the host. Deletion reclaims nothing, so no refusal copy
        // suggests freeing space.
        Notification::WriteRefusedStorageFull => {
            "a write was refused: the store reported no space — the storage screen states what the measurements establish, and deletion reclaims nothing\n"
                .to_owned()
        }
        Notification::EpochFrozen { epoch, rivals } => {
            format!(
                "membership is frozen at epoch {epoch}: {rivals} rival transitions need an owner decision — resolve with: member resolve (only an owner resolves; nothing else authors while frozen)\n"
            )
        }
        Notification::PinnedBytesAbsent { pinned, missing } => {
            format!(
                "a pinned file's bytes have not arrived and the read is failing ({pinned} pinned, {missing} absent) — fetching continues; this is bytes outstanding, not a verdict about any peer\n"
            )
        }
        Notification::SubscriptionClosed { closed } => {
            format!(
                "a relay closed our subscription ({closed} closed) — it will not heal without operator action; nothing arrives until it is fixed, and re-requesting earns another close\n"
            )
        }
        Notification::DeviceRemoved {
            device,
            epoch,
            carried,
            terminal,
        } => {
            let mut out = format!("removed {device} at epoch {epoch} ({carried} carried)\n");
            out.push_str(REMOVAL_BOUNDARY);
            out.push('\n');
            out.push_str(HISTORY_NEVER_REENCRYPTED);
            out.push('\n');
            if *terminal {
                out.push_str(SOLE_OWNER_TERMINAL);
                out.push('\n');
            }
            out
        }
        Notification::SelfRemoved { epoch, carried } => {
            let mut out = format!(
                "this device was removed by another drive's owner at epoch {epoch} ({carried} carried) — this device keeps what it already holds and can obtain nothing later\n"
            );
            out.push_str(REMOVAL_BOUNDARY);
            out.push('\n');
            out.push_str(HISTORY_NEVER_REENCRYPTED);
            out.push('\n');
            out
        }
        Notification::FetchOutstanding { outstanding } => {
            format!(
                "still fetching ({outstanding} outstanding) — bytes announced but not yet local; this is work in progress, not a verdict about any peer\n"
            )
        }
    }
}

/// The in-application activity list (OD-26-B, recommendation A):
/// transition-triggered proposals with repeat suppression and the
/// teardown gate. Seed with the persisted keys from the settings
/// file so a notification recorded before a restart does not
/// re-announce; after [`begin_teardown`](Self::begin_teardown)
/// every proposal returns `None`, modelling admission close in the
/// mount teardown order (unmount and session join, then admission
/// close and loop join, then transport).
#[derive(Debug, Default)]
pub struct NotificationCenter {
    seen: BTreeSet<String>,
    teardown_begun: bool,
}

impl NotificationCenter {
    /// A center over the keys already notified (the settings-file
    /// record, or empty on first run).
    pub fn new(previously_notified: impl IntoIterator<Item = String>) -> Self {
        NotificationCenter {
            seen: previously_notified.into_iter().collect(),
            teardown_begun: false,
        }
    }

    /// Model admission close: from here on the process is no longer
    /// reporting anything, so no notification fires — including a
    /// notification for a transition still pending in the queues.
    pub fn begin_teardown(&mut self) {
        self.teardown_begun = true;
    }

    /// Whether teardown began (admission close modelled).
    pub fn teardown_begun(&self) -> bool {
        self.teardown_begun
    }

    /// True when the key already notified, in this process or a
    /// previous one (via the seeded record).
    pub fn already_notified(&self, key: &str) -> bool {
        self.seen.contains(key)
    }

    /// Propose a notification from an observed transition. Returns
    /// `None` after teardown began, or when the key already
    /// notified; otherwise records the key and returns the
    /// notification. The trigger is the transition — two proposals
    /// from an unchanged observation key identically, so the second
    /// proposes nothing.
    pub fn propose(&mut self, notification: Notification) -> Option<Notification> {
        if self.teardown_begun {
            return None;
        }
        if !self.seen.insert(notification.key()) {
            return None;
        }
        Some(notification)
    }
}

#[cfg(test)]
#[path = "tests_notifications.rs"]
mod tests_notifications;
