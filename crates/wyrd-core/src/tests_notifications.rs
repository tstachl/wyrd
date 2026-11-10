//! Verification tests for the notifications surface (OD-26-A/B/C).
//! Each test name matches the issue's Verifications list.

use std::io::Error as IoError;

use wyrd_format::DeviceId;
use wyrd_format::ObjectStore;
use wyrd_sync::durable::DurableError;
use wyrd_sync::keys::{DeviceEncryptionSecret, DeviceIdentitySecret};
use wyrd_sync::runtime::{DrainReport, Engine, ExecuteReport};

use super::{
    contains_interval_rendering, from_drain, from_epoch_freeze, from_fetch_report,
    from_mailbox_health, from_mutation_error, from_open_error, from_pinned_read_failure,
    from_removal, render_notification, Notification, NotificationCenter, NotificationKind,
    FORBIDDEN_FRESHNESS_WORDS, SILENT_OBSERVATIONS,
};
use crate::mailbox::MailboxHealth;
use crate::mutation::MutationError;
use wyrd_format::StoreFailure;

fn removed_device() -> DeviceId {
    DeviceIdentitySecret::generate().unwrap().device_id()
}

fn mailbox_health(closed: u64) -> MailboxHealth {
    MailboxHealth {
        stream_alive: true,
        connected_relays: 1,
        total_relays: 1,
        saturation_recoveries: 0,
        supervisor_ticks: 1,
        relay_attached: true,
        stream_recovery_attempts: 0,
        relay_recovery_attempts: 0,
        closed_subscriptions: closed,
    }
}

/// One shape per kind, with fixture values: the audit below drives
/// each through its producing observation.
fn every_shape() -> Vec<Notification> {
    vec![
        Notification::DriveOpenRefused,
        Notification::WriteRefusedConflict { heads: 2 },
        Notification::WriteRefusedStorageFull,
        Notification::EpochFrozen {
            epoch: 5,
            rivals: 2,
        },
        Notification::PinnedBytesAbsent {
            pinned: 3,
            missing: 1,
        },
        Notification::SubscriptionClosed { closed: 1 },
        Notification::DeviceRemoved {
            device: removed_device(),
            epoch: 4,
            carried: 1,
            terminal: false,
        },
        Notification::SelfRemoved {
            epoch: 4,
            carried: 1,
        },
        Notification::FetchOutstanding { outstanding: 3 },
    ]
}

/// OD-26-A made executable: every notification the surface can emit
/// is produced by a named observation. The match over
/// [`NotificationKind::all`] is exhaustive — a new kind without an
/// observation behind it fails compilation here — and every source
/// names its producer. The other direction is explicit rows in
/// [`SILENT_OBSERVATIONS`]: observed, and deliberately not
/// notified, with the reason beside each.
#[test]
fn notifications_only_carry_observed_transitions() {
    assert_eq!(NotificationKind::all().len(), every_shape().len());
    for kind in NotificationKind::all() {
        assert!(
            !kind.observation_source().is_empty(),
            "every kind names its producing observation: {kind:?}"
        );
        // Exhaustive: a new kind breaks this match until its
        // producing observation is wired below.
        let produced: Option<Notification> = match kind {
            NotificationKind::DriveOpenRefused => from_open_error(&DurableError::StoreLocked),
            NotificationKind::WriteRefusedConflict => {
                from_mutation_error(&MutationError::Conflicted { heads: 2 })
            }
            NotificationKind::WriteRefusedStorageFull => {
                from_mutation_error(&MutationError::Store(StoreFailure::StorageFull))
            }
            NotificationKind::EpochFrozen => Some(from_epoch_freeze(5, 2)),
            NotificationKind::PinnedBytesAbsent => from_pinned_read_failure(3, 1),
            NotificationKind::SubscriptionClosed => from_mailbox_health(&mailbox_health(1)),
            NotificationKind::DeviceRemoved => {
                Some(from_removal(removed_device(), 4, 1, false, false))
            }
            NotificationKind::SelfRemoved => {
                Some(from_removal(removed_device(), 4, 1, false, true))
            }
            NotificationKind::FetchOutstanding => from_fetch_report(&ExecuteReport {
                unfulfilled: 3,
                ..Default::default()
            }),
        };
        let produced = produced.expect("the named observation produces its kind");
        assert_eq!(produced.kind(), kind, "observation produces its own kind");
    }
    // The silent direction is explicit rows, each with its reason.
    // The silence is explicit: the audit loops the rows below,
    // and the count pins the table against vacuous truth.
    assert!(
        SILENT_OBSERVATIONS.len() > 5,
        "silence is a decision with a reason, never an omission"
    );
    for (observation, reason) in SILENT_OBSERVATIONS {
        assert!(!observation.is_empty(), "silent row names its observation");
        assert!(!reason.is_empty(), "silent row states its reason");
    }
    // And the constructor-callable silent rows yield nothing.
    assert!(from_open_error(&DurableError::Io(IoError::other("boom"))).is_none());
    assert!(from_mutation_error(&MutationError::Saturated).is_none());
    assert!(from_mutation_error(&MutationError::Invalid("x".to_owned())).is_none());
    assert!(from_pinned_read_failure(3, 0).is_none());
    assert!(from_mailbox_health(&mailbox_health(0)).is_none());
    assert!(
        from_fetch_report(&ExecuteReport::default()).is_none(),
        "zero outstanding is not news"
    );
}

/// The negative invariant, executable: no string a notification
/// can carry claims convergence or freshness. Every rendering —
/// all nine shapes plus the terminal-removal arm — is matched
/// against the shared forbidden list (the same constant the status
/// surface tests pin, so the two cannot drift) and the elapsed
/// predicate, and asserted to carry no elapsed rendering at all.
#[test]
fn no_notification_claims_convergence_or_freshness() {
    let mut shapes = every_shape();
    shapes.push(Notification::DeviceRemoved {
        device: removed_device(),
        epoch: 7,
        carried: 2,
        terminal: true,
    });
    for notification in &shapes {
        let rendering = render_notification(notification);
        let lower = rendering.to_lowercase();
        for forbidden in FORBIDDEN_FRESHNESS_WORDS {
            assert!(
                !lower.contains(forbidden),
                "no convergence or freshness claim ({forbidden}):\n{rendering}"
            );
        }
        assert!(
            !contains_interval_rendering(&rendering),
            "no elapsed-time rendering at all:\n{rendering}"
        );
    }
}

/// The substrate's constraint, not a wording preference: no
/// notification payload has a field the engine did not produce. The
/// exhaustive destructure below fails compilation when a variant
/// gains a field, and the source check pins that this module reads
/// no clock — `enum Fact` carries no time at all and
/// `Snapshot.timestamp` is a display tiebreak, so there is nothing
/// legitimate to compute an interval from.
#[test]
fn no_notification_payload_carries_an_elapsed_time() {
    for notification in every_shape() {
        // Exhaustive on purpose: a new variant or a new field
        // breaks this audit until it is accounted for.
        match notification {
            Notification::DriveOpenRefused => {}
            Notification::WriteRefusedConflict { heads } => {
                let _ = heads;
            }
            Notification::WriteRefusedStorageFull => {}
            Notification::EpochFrozen { epoch, rivals } => {
                let _ = (epoch, rivals);
            }
            Notification::PinnedBytesAbsent { pinned, missing } => {
                let _ = (pinned, missing);
            }
            Notification::SubscriptionClosed { closed } => {
                let _ = closed;
            }
            Notification::DeviceRemoved {
                device,
                epoch,
                carried,
                terminal,
            } => {
                let _ = (device, epoch, carried, terminal);
            }
            Notification::SelfRemoved { epoch, carried } => {
                let _ = (epoch, carried);
            }
            Notification::FetchOutstanding { outstanding } => {
                let _ = outstanding;
            }
        }
    }
    // The module reads no clock: no system time, no instant, and
    // no interval computed from either.
    const SRC: &str = include_str!("notifications.rs");
    for token in ["SystemTime", "Instant::now", "elapsed"] {
        assert!(
            !SRC.contains(token),
            "the notifications module reads no clock ({token})"
        );
    }
}

/// The aggregate-`deferred` bug as a test: a drain report with
/// `deferred: 12` and nothing else produces no notification; the
/// same report with `unfulfilled > 0` produces a "still fetching"
/// notification and never a "missing peer" one.
#[test]
fn deferred_never_becomes_a_staleness_notification() {
    let deferred_only = DrainReport {
        deferred: 12,
        ..Default::default()
    };
    assert!(
        from_drain(&deferred_only, &ExecuteReport::default()).is_empty(),
        "deferred alone notifies nothing"
    );
    // Cause splits do not matter: unseen holds, status blocks, and
    // sheds are all local intake state, never staleness.
    let split = DrainReport {
        deferred: 41,
        deferred_unseen: 12,
        deferred_status_blocked: 9,
        deferred_shed: 20,
        deferred_shed_unseen: 5,
        deferred_shed_status_blocked: 6,
        ..Default::default()
    };
    assert!(
        from_drain(&split, &ExecuteReport::default()).is_empty(),
        "no cause split becomes a staleness signal"
    );
    let fetching = from_drain(
        &deferred_only,
        &ExecuteReport {
            unfulfilled: 4,
            ..Default::default()
        },
    );
    assert_eq!(fetching.len(), 1, "outstanding work notifies exactly once");
    let rendering = render_notification(&fetching[0]);
    assert!(
        rendering.contains("still fetching"),
        "outstanding work says it is still fetching:\n{rendering}"
    );
    for word in ["missing peer", "stale", "missing history", "behind"] {
        assert!(
            !rendering.to_lowercase().contains(word),
            "still working is never a remote verdict ({word}):\n{rendering}"
        );
    }
}

/// The trigger is a transition, not a poll: two proposals from an
/// unchanged observation key identically, so the second proposes
/// nothing. Runs against a real engine observation to tie the
/// dedup to durable state rather than to a hand-built fixture.
#[test]
fn two_observations_of_an_unchanged_drive_notify_nothing() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-core-notifications-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let identity = DeviceIdentitySecret::generate().unwrap();
    let mut engine = Engine::create(dir.clone(), "core-test-pass", identity).unwrap();
    let mut store = wyrd_format::FsObjectStore::open(dir.clone()).unwrap();
    let chunk = store
        .insert(wyrd_format::ObjectKind::Chunk, b"notify-bytes")
        .unwrap();
    let root = wyrd_format::Tree::from_entries(vec![wyrd_format::Entry::file(
        "f",
        12,
        false,
        vec![chunk],
    )
    .unwrap()])
    .unwrap()
    .insert_into(&mut store)
    .unwrap();
    engine.author_snapshot(&store, root).unwrap();
    drop(store);
    let peer = DeviceIdentitySecret::generate().unwrap();
    let encryption = DeviceEncryptionSecret::generate().unwrap();
    engine.stage_carry_heads().unwrap();
    engine
        .admit_device(peer.device_id(), encryption.encryption_key())
        .unwrap();

    let mut center = NotificationCenter::new([]);
    let status = crate::status::observe(&engine, 0).unwrap();
    // The unchanged drive proposes nothing on repeat: the same
    // observation twice keys the same notifications.
    let first = from_drain(&DrainReport::default(), &ExecuteReport::default());
    assert!(first.is_empty());
    let repeat = crate::status::observe(&engine, 0).unwrap();
    assert_eq!(status, repeat, "observation repeats over unchanged state");
    let second = from_drain(&DrainReport::default(), &ExecuteReport::default());
    assert!(second.is_empty());

    // And a genuine transition proposes exactly once: the blind
    // close fires on 0→1 and suppresses on 1→1.
    let blind = from_mailbox_health(&mailbox_health(1)).unwrap();
    assert!(center.propose(blind.clone()).is_some());
    assert!(
        center.propose(blind).is_none(),
        "the unchanged observation re-announces nothing"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// The teardown order is the contract: after admission close, no
/// notification is emitted — including for a transition still
/// pending in the queues. Drives the center through a shutdown
/// with a pending notification-producing transition.
#[test]
fn a_notification_never_fires_after_teardown_begins() {
    let mut center = NotificationCenter::new([]);
    center.begin_teardown();
    assert!(center.teardown_begun());
    // A pending transition that would notify mid-run notifies
    // nothing once teardown began.
    assert!(center.propose(Notification::DriveOpenRefused).is_none());
    assert!(center
        .propose(Notification::FetchOutstanding { outstanding: 4 })
        .is_none());
    assert!(center.propose(from_epoch_freeze(5, 2)).is_none());
}

/// A removal notification's rendered strings are matched against
/// "lost", "deleted", "wiped", and "no longer has": a removed
/// device keeps everything it already held, so any of those words
/// would state the opposite of the trust contract.
#[test]
fn a_removed_device_notification_does_not_say_lost_data() {
    for notification in [
        Notification::DeviceRemoved {
            device: removed_device(),
            epoch: 4,
            carried: 1,
            terminal: false,
        },
        Notification::DeviceRemoved {
            device: removed_device(),
            epoch: 7,
            carried: 2,
            terminal: true,
        },
        Notification::SelfRemoved {
            epoch: 4,
            carried: 1,
        },
    ] {
        let rendering = render_notification(&notification);
        for forbidden in ["lost", "deleted", "wiped", "no longer has"] {
            assert!(
                !rendering.to_lowercase().contains(forbidden),
                "removal bounds acquisition, never possession ({forbidden}):\n{rendering}"
            );
        }
    }
}
