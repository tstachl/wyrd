//! Application settings surface: the budgets a person may configure
//! and the running values the node was composed with.
//!
//! This module is the library wiring behind the settings screen, not
//! a preferences toggle. Every bound here is read once at
//! composition ([`LiveConfig`] docs): a change applies on the next
//! start, never to the running node, and the surface says so. The
//! relay count renders read-only — v0.3 owns relay configuration —
//! and the quota row carries its refusal scope in the same sentence
//! (DG-5), never as a bare number.
//!
//! File IO lives in `wyrd-daemon` (`settings.rs` there owns the
//! versioned JSON schema, the per-user path, and crash-safe writes)
//! so this crate gains no serde edge: this module holds the override
//! values, their application onto a [`LiveConfig`], and the running
//! surface. The settings file may never construct a quota without
//! its accountant — [`apply_budget_settings`] wires both from one
//! call, so the composition refusal
//! ([`CompositionError::QuotaWithoutAccountant`]) is unreachable
//! through this path, never merely documented.
//!
//! [`CompositionError::QuotaWithoutAccountant`]: crate::live::CompositionError::QuotaWithoutAccountant

use std::sync::Arc;
use std::time::Duration;

use wyrd_format::RetainedBytes;

use crate::budgets::ResourceBudgets;
use crate::live::{LiveConfig, LiveParts};
use crate::policy::StorageFigure;
use crate::view::NamespaceView;

/// The five bounds the settings surface exposes (OD-25-B,
/// recommendation B): the retention quota plus the four bounds a
/// person waits on — the fetch pass budget, `max_admit_per_pass`,
/// the mutation wait, and the open timeout. Nothing else from
/// [`ResourceBudgets`]: `max_parent_tokens` fails `ESTALE` and
/// `max_pending_wants` fails registration `EIO`, and neither is a
/// thing a person diagnoses from a settings page.
///
/// Every field is an override: `None` leaves the base config
/// untouched, so an absent settings file and an empty section both
/// compose exactly [`LiveConfig::for_local_sync`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BudgetOverrides {
    /// Ceiling on retained object bytes (`None` — the default — is
    /// unlimited). Always wires its accountant when set.
    pub retained_bytes_quota: Option<u64>,
    /// Wants admitted per sync pass.
    pub max_admit_per_pass: Option<usize>,
    /// Wall-clock budget for one pass's fetch phase.
    pub fetch_pass_budget: Option<Duration>,
    /// Wall-clock deadline for a held mutation before `TimedOut`.
    pub max_mutation_wait: Option<Duration>,
    /// Wall-clock budget for one serving-mirror readiness wait
    /// (`open` blocking on demand).
    pub open_timeout: Option<Duration>,
}

impl BudgetOverrides {
    /// True when every field is `None`: composing with this changes
    /// nothing.
    pub fn is_empty(&self) -> bool {
        self.retained_bytes_quota.is_none()
            && self.max_admit_per_pass.is_none()
            && self.fetch_pass_budget.is_none()
            && self.max_mutation_wait.is_none()
            && self.open_timeout.is_none()
    }
}

/// A [`LiveConfig`] with the settings overrides applied, plus the
/// open timeout for [`LiveNode::split`] and the accountant the
/// store constructor shares when a quota is set.
///
/// [`LiveNode::split`]: crate::live::LiveNode::split
pub struct AppliedSettings {
    /// The config to compose with (and to run the loop with — the
    /// supervision halves read from the same value).
    pub config: LiveConfig,
    /// The timeout to pass alongside the config at composition.
    pub open_timeout: Duration,
    /// The tally the store shares when a quota is configured.
    pub retained_bytes: Option<Arc<RetainedBytes>>,
}

/// Durations past this are refused: no pass budget or wait is a
/// day-long stall.
const MAX_SETTINGS_DURATION: Duration = Duration::from_secs(86_400);

/// Admission past this is refused: the intake family sizes the
/// bound at 1024, and a million-wide pass is a misconfiguration,
/// not tuning.
const MAX_SETTINGS_ADMIT_PER_PASS: usize = 1_000_000;

fn check_duration(name: &str, value: Duration) -> Result<(), String> {
    if value.is_zero() {
        return Err(format!("settings {name} must be positive (got zero)"));
    }
    if value > MAX_SETTINGS_DURATION {
        return Err(format!(
            "settings {name} exceeds 86400s (got {}s)",
            value.as_secs()
        ));
    }
    Ok(())
}

/// Apply settings overrides onto a base config. Starts from
/// `base` (usually [`LiveConfig::for_local_sync`]) so unknown
/// fields keep their pinned defaults; only the five surfaced
/// bounds move. A quota always wires a fresh accountant shared
/// with the store constructor ([`LiveConfig::with_retained_quota`]
/// shape), so the result never holds the mismatch composition
/// refuses.
pub fn apply_budget_settings(
    base: &LiveConfig,
    base_open_timeout: Duration,
    overrides: &BudgetOverrides,
) -> Result<AppliedSettings, String> {
    // Clone field by field: `LiveConfig` holds an `Arc` (not
    // `Clone`-free to rebuild), and spelling each carried field
    // keeps a future field from silently resetting to default.
    let mut config = LiveConfig {
        interval: base.interval,
        error_base_delay: base.error_base_delay,
        error_max_delay: base.error_max_delay,
        max_consecutive_errors: base.max_consecutive_errors,
        budgets: base.budgets,
        max_mutation_wait: base.max_mutation_wait,
        serving_flush_budget: base.serving_flush_budget,
        fetch_pass_budget: base.fetch_pass_budget,
        publish_pass_budget: base.publish_pass_budget,
        retained_bytes: None,
    };
    let mut open_timeout = base_open_timeout;
    let mut retained_bytes = base.retained_bytes.clone();

    if let Some(quota) = overrides.retained_bytes_quota {
        let retained = RetainedBytes::new();
        config.budgets.retained_bytes_quota = Some(quota);
        config.retained_bytes = Some(Arc::clone(&retained));
        retained_bytes = Some(retained);
    } else {
        config.budgets.retained_bytes_quota = base.budgets.retained_bytes_quota;
        config.retained_bytes = base.retained_bytes.clone();
    }
    if let Some(admit) = overrides.max_admit_per_pass {
        if admit == 0 || admit > MAX_SETTINGS_ADMIT_PER_PASS {
            return Err(format!(
                "settings max_admit_per_pass must be 1..={MAX_SETTINGS_ADMIT_PER_PASS} (got {admit})"
            ));
        }
        config.budgets.max_admit_per_pass = admit;
    }
    if let Some(budget) = overrides.fetch_pass_budget {
        check_duration("fetch_pass_budget", budget)?;
        config.fetch_pass_budget = budget;
    }
    if let Some(wait) = overrides.max_mutation_wait {
        check_duration("max_mutation_wait", wait)?;
        config.max_mutation_wait = wait;
    }
    if let Some(timeout) = overrides.open_timeout {
        check_duration("open_timeout", timeout)?;
        open_timeout = timeout;
    }
    Ok(AppliedSettings {
        config,
        open_timeout,
        retained_bytes,
    })
}

/// The running settings: the values the node was actually composed
/// with. Built from [`LiveParts`] plus the loop-held halves — never
/// from [`LiveConfig::for_local_sync`] — so the surface cannot show
/// a default beside a configured drive without either being wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunningSettings {
    /// Resource bounds fixed at composition.
    pub budgets: ResourceBudgets,
    /// Per-pass fetch budget the loop paces against.
    pub fetch_pass_budget: Duration,
    /// Held-mutation deadline the loop enforces.
    pub max_mutation_wait: Duration,
    /// Read-side demand wait the backend enforces.
    pub open_timeout: Duration,
    /// Relays configured for this session (read-only here).
    pub configured_relays: usize,
}

impl RunningSettings {
    /// Snapshot the running composition. Every value is read off
    /// the composed node, never off a fresh default.
    pub fn snapshot<V: NamespaceView>(
        parts: &LiveParts<V>,
        fetch_pass_budget: Duration,
        max_mutation_wait: Duration,
        configured_relays: usize,
    ) -> Self {
        RunningSettings {
            budgets: parts.budgets,
            fetch_pass_budget,
            max_mutation_wait,
            open_timeout: parts.open_timeout,
            configured_relays,
        }
    }
}

/// Render the settings surface over the running composition: every
/// configured value plus whether it applies now or on the next
/// start. All five budget bounds are composition-read, so all five
/// say "applies on the next start" — "now" is the narrow claim and
/// nothing here earns it. The relay row is read-only with its
/// pointer, never an editor.
pub fn render_settings_surface(running: &RunningSettings) -> String {
    let mut out =
        String::from("settings (running composition — values the node was composed with):\n");
    match running.budgets.retained_bytes_quota {
        // No bare quota number: DG-5 forbids a quota rendering that
        // omits its refusal scope, so the scope rides the same
        // sentence.
        Some(quota) => out.push_str(&format!(
            "  {}: {quota} (operator-selected refusal boundary over retained content only; not a ceiling on the device) — applies on the next start\n",
            StorageFigure::RetainedQuota.label()
        )),
        None => out.push_str(&format!(
            "  {}: no retention ceiling is set on this device (unset: local writes and fetched content admit without a retention refusal) — applies on the next start\n",
            StorageFigure::RetainedQuota.label()
        )),
    }
    out.push_str(&format!(
        "  max_admit_per_pass: {} — applies on the next start\n",
        running.budgets.max_admit_per_pass
    ));
    out.push_str(&format!(
        "  fetch_pass_budget: {}s — applies on the next start\n",
        running.fetch_pass_budget.as_secs()
    ));
    out.push_str(&format!(
        "  max_mutation_wait: {}s — applies on the next start\n",
        running.max_mutation_wait.as_secs()
    ));
    out.push_str(&format!(
        "  open_timeout: {}s — applies on the next start\n",
        running.open_timeout.as_secs()
    ));
    out.push_str(&format!(
        "  configured relays: {} (read-only; relay configuration lives in the drive configuration, not here)\n",
        running.configured_relays
    ));
    out
}

#[cfg(test)]
#[path = "tests_settings.rs"]
mod tests_settings;
