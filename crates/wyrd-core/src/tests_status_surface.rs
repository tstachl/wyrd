//! Verification tests for the status surface (OD-18-A/B/C). Each
//! test name matches the issue's Verifications list; the CLI parity
//! leg lives in `wyrd-cli/src/tests_sync.rs` next to the renderer it
//! agrees with.

use super::{
    render_status_detail, render_status_summary, IntakeDeferral, CANNOT_TELL_MISSING_HISTORY,
    DELIVERY_IS_RELAY_ACCEPTANCE,
};
use crate::status::observe;
use wyrd_format::{DeviceId, Entry, ObjectKind, ObjectStore, Tree};
use wyrd_sync::keys::{DeviceEncryptionSecret, DeviceIdentitySecret};
use wyrd_sync::runtime::{DrainReport, Engine};

static SCRATCH_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn scratch_dir(prefix: &str) -> std::path::PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "wyrd-core-surface-{prefix}-{}-{seq}",
        std::process::id()
    ))
}

fn scratch_authored() -> (Engine, std::path::PathBuf) {
    let dir = scratch_dir("authored");
    std::fs::create_dir_all(&dir).unwrap();
    let identity = DeviceIdentitySecret::generate().unwrap();
    let mut engine = Engine::create(dir.clone(), "core-test-pass", identity).unwrap();
    let mut store = wyrd_format::FsObjectStore::open(dir.clone()).unwrap();
    let chunk = store.insert(ObjectKind::Chunk, b"surface-bytes").unwrap();
    let root = Tree::from_entries(vec![Entry::file("f", 13, false, vec![chunk]).unwrap()])
        .unwrap()
        .insert_into(&mut store)
        .unwrap();
    engine.author_snapshot(&store, root).unwrap();
    let peer = DeviceIdentitySecret::generate().unwrap();
    let encryption = DeviceEncryptionSecret::generate().unwrap();
    engine.stage_carry_heads().unwrap();
    engine
        .admit_device(peer.device_id(), encryption.encryption_key())
        .unwrap();
    let store = wyrd_format::FsObjectStore::open(dir.clone()).unwrap();
    engine.carry_pending(&store).unwrap();
    (engine, dir)
}

fn dir_snapshot(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in walkdir(dir) {
        out.push(entry);
    }
    out.sort();
    out
}

fn walkdir(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(top) = stack.pop() {
        let entries = std::fs::read_dir(&top).unwrap();
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// OD-18-A's negative invariant, executable: no string the surface
/// can render claims remote freshness. Every rendering — summary and
/// detail, over genesis, backlog, and converged fixtures — is
/// matched against the forbidden list and asserted to carry both
/// standing sentences verbatim.
#[test]
fn status_surface_states_its_own_blind_spot() {
    let fresh_dir = scratch_dir("fresh");
    std::fs::create_dir_all(&fresh_dir).unwrap();
    let fresh = Engine::create(
        fresh_dir.clone(),
        "core-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let fresh_status = observe(&fresh, 0).unwrap();
    let (backlog, backlog_dir) = scratch_authored();
    let backlog_status = observe(&backlog, 2).unwrap();

    let mut renderings = vec![
        render_status_summary(&fresh_status, IntakeDeferral::NotObserved),
        render_status_detail(&fresh_status, IntakeDeferral::NotObserved),
        render_status_summary(&backlog_status, IntakeDeferral::NotObserved),
        render_status_detail(
            &backlog_status,
            IntakeDeferral::Observed {
                membership_unseen: 3,
            },
        ),
    ];
    // A converged rendering must carry the sentences too: the limit
    // is permanent, not a transient warning that success clears.
    let mut converged = fresh_status.clone();
    renderings.push(render_status_summary(
        &converged,
        IntakeDeferral::NotObserved,
    ));
    converged.totals.announcements_delivered = 2;
    renderings.push(render_status_summary(
        &converged,
        IntakeDeferral::Observed {
            membership_unseen: 0,
        },
    ));

    for rendering in &renderings {
        let lower = rendering.to_lowercase();
        for forbidden in [
            "in sync",
            "up to date",
            "up-to-date",
            "up to-date",
            "all files synced",
            "fully synchronised",
            "fully synchronized",
            "connected",
            "delivered",
        ] {
            assert!(
                !lower.contains(forbidden),
                "surface must never claim remote freshness ({forbidden}):\n{rendering}"
            );
        }
        // Peer handles name pending work, never a connection.
        assert!(
            !lower.contains("peer connected") && !lower.contains("connected to peer"),
            "no peer is ever rendered as connected:\n{rendering}"
        );
        assert!(
            rendering.contains(DELIVERY_IS_RELAY_ACCEPTANCE.trim_end()),
            "standing sentence one, verbatim:\n{rendering}"
        );
        assert!(
            rendering.contains(CANNOT_TELL_MISSING_HISTORY.trim_end()),
            "standing sentence two, verbatim:\n{rendering}"
        );
    }
    std::fs::remove_dir_all(fresh_dir).unwrap();
    std::fs::remove_dir_all(backlog_dir).unwrap();
}

/// `docs/contracts/sync-and-peers.md:80-96`: relay acceptance is the
/// maximum guarantee, so a status whose outbox shows acceptances
/// renders the fact with relay-accepted wording and never with
/// recipient-received wording.
#[test]
fn delivered_is_rendered_as_relay_accepted() {
    let (engine, dir) = scratch_authored();
    let mut status = observe(&engine, 2).unwrap();
    status.totals.announcements_delivered = 2;
    status.totals.transitions_delivered = 1;
    status.totals.capabilities_delivered = 1;
    for rendering in [
        render_status_summary(&status, IntakeDeferral::NotObserved),
        render_status_detail(&status, IntakeDeferral::NotObserved),
    ] {
        assert!(
            rendering.contains("relay-accepted"),
            "acceptances render as relay acceptance:\n{rendering}"
        );
        assert!(
            !rendering.to_lowercase().contains("delivered"),
            "no bare delivered, no recipient receipt:\n{rendering}"
        );
        // "Statements received" (reconciliation) is a local durable
        // fact and stays; a recipient-receipt claim never renders.
        // (The standing sentence's own denial — "does not mean the
        // recipient has it" — is the refusal, not a claim.)
        for forbidden in ["recipient received", "recipient-received"] {
            assert!(
                !rendering.to_lowercase().contains(forbidden),
                "acceptance is not receipt ({forbidden}):\n{rendering}"
            );
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// The surface introduces no non-determinism: the same observation
/// renders byte-identically twice, summary and detail.
#[test]
fn observation_is_repeatable() {
    let (engine, dir) = scratch_authored();
    let status = observe(&engine, 0).unwrap();
    assert_eq!(
        render_status_summary(&status, IntakeDeferral::NotObserved),
        render_status_summary(&status, IntakeDeferral::NotObserved)
    );
    assert_eq!(
        render_status_detail(
            &status,
            IntakeDeferral::Observed {
                membership_unseen: 1
            }
        ),
        render_status_detail(
            &status,
            IntakeDeferral::Observed {
                membership_unseen: 1
            }
        )
    );
    assert_eq!(observe(&engine, 0).unwrap(), observe(&engine, 0).unwrap());
    std::fs::remove_dir_all(dir).unwrap();
}

/// The inherited limit: the surface's read path leaves the drive
/// exactly as found — no mailbox construction, no seen-store side
/// effect, no connection. Liveness belongs to run reports.
#[test]
fn status_never_constructs_a_mailbox() {
    let dir = scratch_dir("untouched");
    std::fs::create_dir_all(&dir).unwrap();
    let engine = Engine::create(
        dir.clone(),
        "core-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let before = dir_snapshot(&dir);
    let status = observe(&engine, 0).unwrap();
    let _ = render_status_summary(&status, IntakeDeferral::NotObserved);
    let _ = render_status_detail(&status, IntakeDeferral::NotObserved);
    assert_eq!(before, dir_snapshot(&dir), "observing commits nothing");
    assert!(
        !dir.join("mailbox.seen").exists(),
        "no seen store from a read path"
    );
    assert_eq!(status.mailbox.configured_relays, 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// The genesis figures the summary must reproduce exactly: epoch-one
/// tip, its secret held, nothing owed, converged.
#[test]
fn fresh_drive_status_is_genesis_with_empty_outbox() {
    let dir = scratch_dir("genesis");
    std::fs::create_dir_all(&dir).unwrap();
    let engine = Engine::create(
        dir.clone(),
        "core-test-pass",
        DeviceIdentitySecret::generate().unwrap(),
    )
    .unwrap();
    let status = observe(&engine, 0).unwrap();
    let summary = render_status_summary(&status, IntakeDeferral::NotObserved);
    assert!(
        summary.contains("sync: converged"),
        "empty backlog converges:\n{summary}"
    );
    assert!(
        summary.contains("epoch 1 tip"),
        "genesis tip in the head line:\n{summary}"
    );
    assert!(summary.contains("0 pending"), "nothing owed:\n{summary}");
    assert!(
        summary.contains("0 relay-accepted"),
        "nothing accepted:\n{summary}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// The admission figures the summary must reproduce exactly: two
/// announcements, one transition, one capability owed.
#[test]
fn admission_queues_all_three_obligation_classes() {
    let (engine, dir) = scratch_authored();
    let status = observe(&engine, 2).unwrap();
    let summary = render_status_summary(&status, IntakeDeferral::NotObserved);
    assert!(
        summary.contains("sync: active (4 outstanding)"),
        "the fixture owes four:\n{summary}"
    );
    assert!(
        summary.contains("2 announcements, 1 transitions, 1 capabilities"),
        "all three classes itemized:\n{summary}"
    );
    let detail = render_status_detail(&status, IntakeDeferral::NotObserved);
    assert!(
        detail.contains("2 pending"),
        "itemized lines keep the count:\n{detail}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// OD-18-B's three requirements: only the unseen reason renders,
/// absence renders as "Not observed" (distinct from zero), and the
/// field stays local — never named stale, missing, or behind.
#[test]
fn intake_deferral_is_reason_labelled_and_explicitly_absent() {
    let report = DrainReport {
        deferred: 41,
        deferred_unseen: 12,
        deferred_status_blocked: 9,
        deferred_shed: 20,
        deferred_shed_unseen: 5,
        deferred_shed_status_blocked: 6,
        ..DrainReport::default()
    };
    // Sibling causes are not re-aggregated: the surface reads the
    // unseen counter only, so budget sheds and status blocks can
    // never inflate the freshness signal.
    let observed = IntakeDeferral::from_drain(&report);
    assert_eq!(
        observed,
        IntakeDeferral::Observed {
            membership_unseen: 12
        }
    );

    let (engine, dir) = scratch_authored();
    let status = observe(&engine, 0).unwrap();
    let rendered = render_status_summary(&status, observed);
    assert!(
        rendered.contains("intake deferred: 12 (membership not observed)"),
        "reason-labelled, locally named:\n{rendered}"
    );
    // The reason word lives on the intake line only: the standing
    // sentence names the blind spot in its own words ("missing
    // history"), but the field itself is local intake, never a
    // remote verdict.
    let intake_line = observed.render();
    for word in ["stale", "missing", "behind"] {
        assert!(
            !intake_line.to_lowercase().contains(word),
            "the field is local intake, never a remote verdict ({word}):\n{intake_line}"
        );
    }
    // Absence is explicit and distinct from zero.
    let absent = render_status_summary(&status, IntakeDeferral::NotObserved);
    assert!(
        absent.contains("intake deferred: Not observed"),
        "absent says so:\n{absent}"
    );
    let zero = render_status_summary(
        &status,
        IntakeDeferral::Observed {
            membership_unseen: 0,
        },
    );
    assert!(
        zero.contains("intake deferred: 0 (membership not observed)"),
        "observed zero is an observation, not an absence:\n{zero}"
    );
    assert_ne!(absent, zero);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Field-absence audit, executable: every `SyncStatus` field names
/// either its row in the detail rendering or the explicit reason it
/// has none. The destructure is exhaustive on purpose — a new field
/// fails compilation here until this audit accounts for it.
#[test]
fn every_status_field_is_accounted() {
    let (engine, dir) = scratch_authored();
    let status = observe(&engine, 2).unwrap();
    let detail = render_status_detail(&status, IntakeDeferral::NotObserved);
    let summary = render_status_summary(&status, IntakeDeferral::NotObserved);

    let crate::status::SyncStatus {
        tip: _,
        held_epochs: _,
        obligations: _,
        totals: _,
        live_heads: _,
        head_classes: _,
        mailbox: _,
        known_members: _,
        peers: _,
        queue: _,
        reconciliation: _,
        convergence: _,
        materialization: _,
        durability: _,
    } = &status;

    // tip + held_epochs: membership rows.
    assert!(detail.contains("tip"), "tip row:\n{detail}");
    assert!(detail.contains("held secrets"), "held-epoch row:\n{detail}");
    // obligations + totals: itemized outbox with relay-accepted word.
    assert!(
        detail.contains("outbox announcements"),
        "obligation rows:\n{detail}"
    );
    assert!(detail.contains("relay-accepted"), "totals row:\n{detail}");
    // live_heads + peers: opaque handles, never identities.
    assert!(detail.contains("live heads"), "head rows:\n{detail}");
    assert!(detail.contains("peer-"), "peer rows:\n{detail}");
    for entry in &status.peers {
        assert!(
            !detail.contains(&entry.peer.to_string()),
            "handles, never identities"
        );
    }
    // head_classes: all seven counters.
    assert!(detail.contains("heads classified"), "class rows:\n{detail}");
    // mailbox: configuration only, never liveness.
    assert!(
        detail.contains("relays configured"),
        "mailbox row:\n{detail}"
    );
    assert!(
        !detail.contains("connected"),
        "no liveness claim:\n{detail}"
    );
    // known_members: the denominator beside the handles.
    assert!(
        detail.contains(&status.known_members.to_string()),
        "member denominator:\n{detail}"
    );
    // queue + convergence + reconciliation + materialization +
    // durability: every durable projection has its row.
    assert!(detail.contains("outstanding"), "queue row:\n{detail}");
    assert!(detail.contains("convergence"), "convergence row:\n{detail}");
    assert!(
        detail.contains("reconciliation"),
        "reconciliation row:\n{detail}"
    );
    assert!(
        detail.contains("materialization"),
        "materialization row:\n{detail}"
    );
    assert!(detail.contains("durability"), "durability row:\n{detail}");
    // The summary carries the same facts in five groups, never by
    // paraphrasing away the standing sentences.
    for marker in [
        "sync:",
        "heads:",
        "obligations:",
        "intake deferred:",
        "peers:",
    ] {
        assert!(
            summary.contains(marker),
            "summary group {marker}:\n{summary}"
        );
    }
    let _ = DeviceId::from_bytes([0u8; 32]);
    std::fs::remove_dir_all(dir).unwrap();
}

/// N1 follow-up: the tip-less path renders the live-head count
/// rather than collapsing to "none", so summary and detail agree
/// even in the impossible-but-unproven state.
#[test]
fn tipless_summary_names_live_heads_like_detail() {
    let (engine, dir) = scratch_authored();
    let mut status = observe(&engine, 0).unwrap();
    assert!(!status.live_heads.is_empty());
    status.tip = None;
    let summary = render_status_summary(&status, IntakeDeferral::NotObserved);
    let detail = render_status_detail(&status, IntakeDeferral::NotObserved);
    assert!(
        summary.contains("no membership tip"),
        "tip-less state named:\n{summary}"
    );
    assert!(
        summary.contains(&status.live_heads.len().to_string()),
        "heads not collapsed to none:\n{summary}"
    );
    assert!(detail.contains("membership: none observed"));
    assert!(detail.contains("live heads: 1"));

    // And the reachable tip-less shape (no tip, no heads) agrees too.
    status.live_heads.clear();
    let summary = render_status_summary(&status, IntakeDeferral::NotObserved);
    assert!(summary.contains("heads: none observed (no membership tip)"));
    std::fs::remove_dir_all(dir).unwrap();
}

/// N2 follow-up: the surface renders the shared idle reading
/// (`RELAY_IDLE_LINE`, identical to the CLI's `MAILBOX_IDLE_LINE`),
/// never a paraphrase, so the status, now, and posture surfaces
/// cannot drift apart.
#[test]
fn idle_line_is_the_shared_reading() {
    use crate::relay_posture::RELAY_IDLE_LINE;

    assert_eq!(RELAY_IDLE_LINE, "mailbox: idle (no --relay given)\n");
    let (engine, dir) = scratch_authored();
    let status = observe(&engine, 0).unwrap();
    for rendering in [
        render_status_summary(&status, IntakeDeferral::NotObserved),
        render_status_detail(&status, IntakeDeferral::NotObserved),
    ] {
        assert!(
            rendering.contains(RELAY_IDLE_LINE),
            "shared idle reading, never a paraphrase:\n{rendering}"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
