//! Relay posture: the person-visible state vocabulary for the relay
//! line (OD-20-A/B/C, all option A).
//!
//! Two questions stay separate by design. The relay line reports
//! transport — can this owner currently communicate with its
//! configured relays — in the vocabulary of [`MailboxHealth`]. The
//! outbox line reports obligations and delivery progress in the
//! wording of `docs/contracts/sync-and-peers.md`. Combining them
//! into one verdict would imply an end-to-end guarantee the
//! protocol does not provide: the sender's obligation retires
//! permanently at relay acceptance, so no later mechanism could
//! correct an optimistic verdict.
//!
//! The mapping below is exhaustive with explicit precedence, so no
//! observation falls through into the wrong state:
//!
//! 1. The operator's offline switch wins over everything. Offline
//!    is a deliberate mode, never a failure reading.
//! 2. Zero relays render "no relay configured" before anything
//!    reads liveness. `is_live` is true for a zero-relay mailbox
//!    by construction, so a surface that maps it to "running" is
//!    wrong on exactly the local-only drive.
//! 3. Relays configured, not offline, no health observed yet (or
//!    the mailbox cannot carry deliveries): "unavailable". This is
//!    its own non-running reading — startup, failed attachment,
//!    or another unhealthy condition — never folded into either
//!    operator state above.
//! 4. Otherwise the mailbox is running, qualified by health:
//!    blind (a relay closed our subscription) outranks degraded
//!    (a recovery episode ran), which outranks live. Blind is the
//!    operator-actionable fact — policy closes never heal without
//!    operator action — so it reads first even when attempts are
//!    also non-zero.
//!
//! The degraded predicate mirrors `mailbox_degraded` in
//! `wyrd-cli/src/commands/sync.rs` field for field (same inputs,
//! same truth table, saturation replays excluded); the posture
//! splits its one boolean into words while the CLI keeps its copy
//! as the run-verdict oracle, and the table test below pins their
//! agreement cell by cell.
//!
//! Reads are observations, not instantaneous facts: the supervisor
//! refreshes health every tick, so a rendering right after a
//! change may be briefly stale. The copy labels that rather than
//! hiding it.

use crate::mailbox::MailboxHealth;

/// The health qualifier inside the running state: connection
/// quality, never connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunningHealth {
    /// Transport carries deliveries and nothing observed says otherwise.
    Live,
    /// Attached but blind: at least one relay closed our subscription
    /// while the socket stayed up. Never heals without operator action.
    AttachedButBlind,
    /// A recovery episode ran during this mailbox's life (stream or
    /// relay attempts non-zero) without a blind close on record.
    Degraded,
}

/// The relay line's state vocabulary: three operator-facing states
/// plus the non-running reading the third state must not absorb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayPosture {
    /// Relays configured and the mailbox operational, qualified.
    Running(RunningHealth),
    /// The operator chose offline. Deliberate mode, never an error.
    OfflineByChoice,
    /// No relay configured. Supported local-only mode, never an
    /// error, warning, or failure affordance.
    NoRelayConfigured,
    /// Relays configured and explicitly not offline, but the mailbox
    /// cannot carry deliveries (not yet observed, starting up,
    /// failed attachment, or another unhealthy condition).
    Unavailable,
}

/// Recovery-attempt total behind the degraded qualifier: stream
/// plus relay loop iterations, matching the run verdict's unit —
/// loop iterations, not episodes. Saturation replays deliberately
/// do not count (a replay redelivers through dedupe, so intake is
/// whole again afterwards). Mirrors `recovery_attempts` in
/// `wyrd-cli/src/commands/sync.rs`.
pub fn recovery_attempts(health: &MailboxHealth) -> u64 {
    health
        .stream_recovery_attempts
        .saturating_add(health.relay_recovery_attempts)
}

impl RelayPosture {
    /// Map one observation to exactly one state. `offline` is the
    /// operator's switch (`--offline` today); `configured_relays`
    /// is the owner's startup relay count ("Configured relays: N
    /// (current owner configuration)" — process-local, not a
    /// durability promise); `health` is the latest supervisor
    /// sample, or `None` before any observation exists.
    pub fn of(
        offline: bool,
        configured_relays: usize,
        health: Option<&MailboxHealth>,
    ) -> RelayPosture {
        if offline {
            return RelayPosture::OfflineByChoice;
        }
        let observed_relays = health.map_or(configured_relays, |h| h.total_relays);
        if observed_relays == 0 {
            return RelayPosture::NoRelayConfigured;
        }
        let Some(health) = health else {
            return RelayPosture::Unavailable;
        };
        if !health.is_live() {
            return RelayPosture::Unavailable;
        }
        if health.closed_subscriptions > 0 {
            return RelayPosture::Running(RunningHealth::AttachedButBlind);
        }
        if recovery_attempts(health) > 0 {
            return RelayPosture::Running(RunningHealth::Degraded);
        }
        RelayPosture::Running(RunningHealth::Live)
    }
}

/// The relay-less mailbox posture, as its own equivalent of the
/// CLI's `MAILBOX_IDLE_LINE` (`wyrd-cli/src/commands/sync.rs`):
/// identical text, never a paraphrase, so the status, now, and
/// posture surfaces cannot drift apart. Pinned by the literal
/// test below.
pub const RELAY_IDLE_LINE: &str = "mailbox: idle (no --relay given)\n";

/// Standing qualifier, unconditional: it states a protocol
/// guarantee, not a transient condition, so it reads the same
/// with zero pending obligations as with a full outbox.
pub const ACCEPTANCE_IS_NOT_RECEIPT: &str =
    "note: relay acceptance does not guarantee recipient receipt\n";

/// Render the full posture block: the state line, the configured
/// count in OD-20-C's words, the unconditional standing
/// qualifier, and the observation label on health-derived states
/// only (Running and Unavailable with a sample behind them — the
/// supervisor tick refreshes them, so they may be briefly stale).
/// The offline switch and the relay counts are construction facts
/// and carry no stale label.
///
/// `pending_obligations` feeds the offline scope line only: an
/// offline rendering always carries its scope, even at zero
/// pending, because "offline" beside a bare zero is the exact
/// conflation the offline trap warns about. No other state takes
/// a delivered or pending count — rendering one here would be the
/// OD-20-A overstatement.
///
/// When a host presenter exists to own person-visible copy, the
/// rendering below migrates there and this module keeps the state
/// vocabulary and predicates; until then the copy lives next to
/// the mapping its tests pin.
pub fn render_posture(
    offline: bool,
    configured_relays: usize,
    health: Option<&MailboxHealth>,
    pending_obligations: u64,
) -> String {
    let mut out = String::new();
    match RelayPosture::of(offline, configured_relays, health) {
        RelayPosture::Running(RunningHealth::Live) => {
            let health = health.expect("live posture derives from observed health");
            out.push_str(&format!(
                "relay: running ({} of {} relays connected)\n",
                health.connected_relays, health.total_relays
            ));
        }
        RelayPosture::Running(RunningHealth::AttachedButBlind) => {
            let health = health.expect("blind posture derives from observed health");
            let closed = health.closed_subscriptions;
            out.push_str(&format!(
                "relay: running, attached but blind ({} subscription{} closed by relay)\n",
                closed,
                if closed == 1 { "" } else { "s" }
            ));
        }
        RelayPosture::Running(RunningHealth::Degraded) => {
            let health = health.expect("degraded posture derives from observed health");
            let attempts = recovery_attempts(health);
            out.push_str(&format!(
                "relay: running, degraded ({} recovery attempt{} during the run)\n",
                attempts,
                if attempts == 1 { "" } else { "s" }
            ));
        }
        RelayPosture::OfflineByChoice => {
            out.push_str("relay: offline, by your choice\n");
            out.push_str(&format!(
                "scope: local obligations only ({pending_obligations} pending)\n"
            ));
        }
        RelayPosture::NoRelayConfigured => {
            out.push_str(RELAY_IDLE_LINE);
            out.push_str("relay: no relay configured (local-only operation)\n");
        }
        RelayPosture::Unavailable => {
            out.push_str(&format!(
                "relay: unavailable ({configured_relays} relays configured, mailbox not operational)\n"
            ));
        }
    }
    out.push_str(&format!(
        "configured relays: {configured_relays} (current owner configuration)\n"
    ));
    out.push_str(ACCEPTANCE_IS_NOT_RECEIPT);
    let health_derived = matches!(
        RelayPosture::of(offline, configured_relays, health),
        RelayPosture::Running(_) | RelayPosture::Unavailable
    );
    if health_derived && health.is_some() {
        out.push_str("note: health readings are observed and may be briefly stale\n");
    }
    out
}

#[cfg(test)]
#[path = "tests_relay_posture.rs"]
mod tests_relay_posture;
