//! Verification tests for the relay-posture state vocabulary
//! (OD-20-A/B/C, all option A). Each test name matches the issue's
//! Verifications list; the blind-mailbox leg that needs a live relay
//! lives next to its fixture in `mailbox/tests_shutdown.rs` instead.

use super::{
    render_posture, RelayPosture, RunningHealth, ACCEPTANCE_IS_NOT_RECEIPT, RELAY_IDLE_LINE,
};
use crate::mailbox::MailboxHealth;

fn health(
    stream_alive: bool,
    connected: usize,
    total: usize,
    closed: u64,
    stream_attempts: u64,
    relay_attempts: u64,
) -> MailboxHealth {
    MailboxHealth {
        stream_alive,
        connected_relays: connected,
        total_relays: total,
        saturation_recoveries: 0,
        supervisor_ticks: 1,
        relay_attached: connected > 0,
        stream_recovery_attempts: stream_attempts,
        relay_recovery_attempts: relay_attempts,
        closed_subscriptions: closed,
    }
}

fn live_health() -> MailboxHealth {
    health(true, 2, 2, 0, 0, 0)
}

/// OD-20-B made executable: every posture the surface can observe
/// renders to exactly one state, with the zero-relay guard before
/// any `is_live` reading and the unavailable reading for the
/// configured-but-not-operational case the three-state vocabulary
/// alone would leave homeless.
#[test]
fn relay_posture_has_exactly_three_states() {
    // Zero relays render local-only and never running, even though
    // `is_live` is true for a zero-relay mailbox by construction.
    let zero = health(true, 0, 0, 0, 0, 0);
    assert!(zero.is_live());
    assert_eq!(
        RelayPosture::of(false, 0, Some(&zero)),
        RelayPosture::NoRelayConfigured
    );
    // The operator's switch wins over a live mailbox and pending work.
    assert_eq!(
        RelayPosture::of(true, 2, Some(&live_health())),
        RelayPosture::OfflineByChoice
    );
    // Relays with a live mailbox and a zero close count run.
    assert_eq!(
        RelayPosture::of(false, 2, Some(&live_health())),
        RelayPosture::Running(RunningHealth::Live)
    );
    // A non-zero close count qualifies the run as attached but blind.
    let blind = health(true, 1, 1, 1, 0, 0);
    assert_eq!(
        RelayPosture::of(false, 1, Some(&blind)),
        RelayPosture::Running(RunningHealth::AttachedButBlind)
    );
    // Relays configured, not offline, mailbox unable to carry
    // deliveries: unavailable — never folded into either operator
    // state. Covers no observation yet, startup, and failed
    // attachment with one reading.
    assert_eq!(RelayPosture::of(false, 2, None), RelayPosture::Unavailable);
    let down = health(false, 0, 2, 0, 0, 0);
    assert_eq!(
        RelayPosture::of(false, 2, Some(&down)),
        RelayPosture::Unavailable
    );
    // The mapping is total: every cell below names its state, so a
    // future input shape fails here instead of rendering silently.
    let recovery = health(true, 1, 2, 0, 3, 0);
    assert_eq!(
        RelayPosture::of(false, 2, Some(&recovery)),
        RelayPosture::Running(RunningHealth::Degraded)
    );
}

/// OD-20-A's negative invariant: no rendering path from mailbox
/// health produces the word "delivered", and no path composes a
/// connected count with an obligation count into a single verdict.
/// The signature enforces the second half — it takes no delivered
/// or pending count except the offline scope line — and this test
/// enforces the first half over every state.
#[test]
fn live_mailbox_is_never_rendered_as_delivered() {
    let states = [
        render_posture(false, 2, Some(&live_health()), 3),
        render_posture(false, 1, Some(&health(true, 1, 1, 1, 0, 0)), 0),
        render_posture(false, 2, Some(&health(true, 1, 2, 0, 2, 1)), 5),
        render_posture(true, 2, Some(&live_health()), 4),
        render_posture(false, 0, Some(&health(true, 0, 0, 0, 0, 0)), 0),
        render_posture(false, 2, None, 1),
        render_posture(false, 2, Some(&health(false, 0, 2, 0, 0, 0)), 0),
    ];
    for rendering in &states {
        assert!(
            !rendering.contains("delivered"),
            "posture must never claim delivery:\n{rendering}"
        );
    }
}

/// The G19 trap: an offline rendering always carries its scope, at
/// zero pending as well as with a backlog, because "offline"
/// beside a bare zero is the conflation the offline trap warns
/// automation cannot distinguish.
#[test]
fn an_offline_run_states_the_scope_it_converged() {
    for pending in [0, 7] {
        let rendering = render_posture(true, 2, Some(&live_health()), pending);
        assert!(
            rendering.contains("relay: offline, by your choice"),
            "offline state names the choice:\n{rendering}"
        );
        assert!(
            rendering.contains("scope: local obligations only"),
            "offline rendering carries its scope at {pending} pending:\n{rendering}"
        );
    }
}

/// `docs/cli.md`: with no relay, intake stays idle and the drive is
/// local-only — a supported mode, not an error. No rendering for
/// that state carries an error, warning, or failure affordance,
/// and the run path's refusal (a usage error for `--offline` with
/// `--relay`) never leaks into the surface.
#[test]
fn no_relay_configured_is_local_only_not_an_error() {
    let rendering = render_posture(false, 0, None, 0);
    assert!(
        rendering.contains("(local-only operation)"),
        "zero relays render the supported local-only mode:\n{rendering}"
    );
    for word in ["error", "warning", "failed", "failure", "usage", "invalid"] {
        assert!(
            !rendering.to_lowercase().contains(word),
            "local-only mode must not read as a failure ({word}):\n{rendering}"
        );
    }
}

/// The surface's word agrees with `mailbox_degraded`
/// (`wyrd-cli/src/commands/sync.rs`) in every cell: same fields,
/// same truth table — zero relays never degraded, otherwise a
/// down transport, a blind close, or a recovery episode degrades.
/// Saturation replays do not count on either side. The CLI keeps
/// its copy as the run-verdict oracle; this table pins the
/// agreement the issue requires without importing the binary.
#[test]
fn degraded_posture_matches_the_run_verdict_cell_by_cell() {
    // (total, live, closed, attempts) -> degraded?
    let cells = [
        (0, true, 0, 0, false),
        (0, false, 0, 0, false),
        (1, true, 0, 0, false),
        (1, false, 0, 0, true),
        (1, true, 1, 0, true),
        (1, true, 0, 1, true),
        (1, true, 2, 3, true),
        (2, true, 0, 0, false),
        (2, false, 0, 5, true),
    ];
    for (total, live, closed, attempts, degraded) in cells {
        let h = health(live, usize::from(live), total, closed, attempts, 0);
        let posture = RelayPosture::of(false, total, Some(&h));
        let word = render_posture(false, total, Some(&h), 0);
        if total == 0 {
            assert_eq!(posture, RelayPosture::NoRelayConfigured);
            assert!(
                !word.contains("degraded"),
                "cell {total}/{live}/{closed}/{attempts}"
            );
        } else if degraded {
            assert!(
                matches!(
                    posture,
                    RelayPosture::Running(
                        RunningHealth::AttachedButBlind | RunningHealth::Degraded
                    ) | RelayPosture::Unavailable
                ),
                "cell {total}/{live}/{closed}/{attempts} degrades, got {posture:?}"
            );
            assert!(
                word.contains("attached but blind")
                    || word.contains("degraded")
                    || word.contains("unavailable"),
                "cell {total}/{live}/{closed}/{attempts} says so:\n{word}"
            );
        } else {
            assert_eq!(
                posture,
                RelayPosture::Running(RunningHealth::Live),
                "cell {total}/{live}/{closed}/{attempts}"
            );
            assert!(word.contains("relay: running ("), "cell:\n{word}");
        }
    }
}

/// The inherited honesty constraint: the surface may not present a
/// supervisor sample as an instantaneous fact. Every health-derived
/// line carries the observation label; the label is absent only
/// where the reading is a construction-time guarantee (the offline
/// switch and the configured count with no sample behind them).
#[test]
fn relay_health_is_labelled_eventually_consistent() {
    let observed = render_posture(false, 2, Some(&live_health()), 0);
    assert!(
        observed.contains("observed and may be briefly stale"),
        "health-derived rendering labels the sample:\n{observed}"
    );
    let constructed = render_posture(true, 0, None, 0);
    assert!(
        !constructed.contains("briefly stale"),
        "construction-time readings carry no stale label:\n{constructed}"
    );
    assert!(
        constructed.contains(ACCEPTANCE_IS_NOT_RECEIPT.trim_end()),
        "the standing qualifier is unconditional, even offline at zero relays:\n{constructed}"
    );
}

/// The constant check: the third surface renders the same idle
/// reading the CLI's status and now surfaces share
/// (`MAILBOX_IDLE_LINE`), identical text, never a paraphrase.
#[test]
fn status_and_relay_surfaces_cannot_drift() {
    assert_eq!(RELAY_IDLE_LINE, "mailbox: idle (no --relay given)\n");
    let rendering = render_posture(false, 0, None, 0);
    assert!(
        rendering.starts_with(RELAY_IDLE_LINE),
        "the relay-less posture leads with the shared idle reading:\n{rendering}"
    );
}

/// The qualifier is unconditional: it states a protocol guarantee,
/// not a transient condition, so it reads identically with an
/// empty outbox, a backlog, and no mailbox at all.
#[test]
fn acceptance_qualifier_ignores_pending_counts() {
    for (offline, configured, pending) in [(false, 2, 0), (false, 2, 9), (true, 2, 5)] {
        let rendering = render_posture(offline, configured, Some(&live_health()), pending);
        assert!(
            rendering.contains(ACCEPTANCE_IS_NOT_RECEIPT.trim_end()),
            "qualifier present at pending={pending}:\n{rendering}"
        );
    }
}

/// Coverage around the edges the table test does not own:
/// saturation replays never degrade, the offline switch outranks a
/// zero relay count, partial connectivity names its fraction, and
/// mismatched state-vs-copy inputs render by the documented rule
/// (state follows the observation, copy states the configured
/// count) rather than failing or folding.
#[test]
fn posture_edge_cells_render_by_rule() {
    // Saturation replays are pressure signals, not a health
    // verdict: a live mailbox with only replays on record runs live.
    let mut replayed = live_health();
    replayed.saturation_recoveries = 41;
    assert_eq!(
        RelayPosture::of(false, 2, Some(&replayed)),
        RelayPosture::Running(RunningHealth::Live)
    );
    // The operator's switch outranks even a zero relay count.
    assert_eq!(
        RelayPosture::of(true, 0, None),
        RelayPosture::OfflineByChoice
    );
    // Partial connectivity names its fraction, never a bare live.
    let partial = health(true, 1, 2, 0, 0, 0);
    let rendering = render_posture(false, 2, Some(&partial), 0);
    assert!(
        rendering.contains("relay: running (1 of 2 relays connected)"),
        "partial connectivity names the fraction:\n{rendering}"
    );
    // Offline with a sample behind it carries no stale label: the
    // state derives from the switch, a construction fact.
    let offline_observed = render_posture(true, 2, Some(&live_health()), 0);
    assert!(
        !offline_observed.contains("briefly stale"),
        "offline state is a construction fact:\n{offline_observed}"
    );
    // Mismatched inputs (unreachable in production, where the
    // mailbox is built from the owner's count): the state follows
    // the observation, the copy states the configured count.
    let mismatch = health(true, 2, 2, 0, 0, 0);
    assert_eq!(
        RelayPosture::of(false, 0, Some(&mismatch)),
        RelayPosture::Running(RunningHealth::Live),
        "observation places the state"
    );
    let rendered = render_posture(false, 0, Some(&mismatch), 0);
    assert!(
        rendered.contains("configured relays: 0 (current owner configuration)"),
        "copy states the configured count:\n{rendered}"
    );
}
