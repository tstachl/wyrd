// Pure display helpers for the three views. No runes, no IPC, no
// owner facts — components call these so the wording pins in
// `format.test.ts` instead of drifting per view.
//
// Load-bearing rule repeated here because helpers are where it
// would rot: every label derived from cached data must read as
// historical ("last observed …"), and every unknown state must read
// as unknown — never as a negative, never as a default.

import type { WireFreshnessState } from "$lib/ipc/results.js";
import type { DriveAvailability } from "$lib/ipc/results.js";

/** Clock time of an observation stamp, UTC. The stamp is display metadata, never owner state. */
export function formatObservedAt(observedAtMs: number): string {
  return `${new Date(observedAtMs).toISOString().slice(11, 19)} UTC`;
}

/** Shorten a content id for display. The full value stays in the snapshot; this is presentation only. */
export function shortId(id: string): string {
  return id.length <= 12 ? id : `${id.slice(0, 8)}…`;
}

/** One-line freshness judgment. A retained last-good names its staleness, never current health. */
export function freshnessLabel(state: WireFreshnessState): string {
  if (state === "FreshEmpty") return "Fresh (empty)";
  if (state === "FreshNonEmpty") return "Fresh";
  return `Last good, stale: ${state.RetainedLastGood.detail}`;
}

/** One-line reachability. Unavailable never claims the drive is gone; unknown never claims it is down. */
export function availabilityLabel(availability: DriveAvailability): string {
  switch (availability.state) {
    case "available":
      return "Available";
    case "unavailable":
      return `Unavailable — ${availability.detail}`;
    case "unknown":
      return `Unknown — ${availability.detail}`;
  }
}

/**
 * One configured bound for the settings view. Absent stays absent:
 * the compiled default applies and the shell does not know it, so
 * the label says exactly that instead of filling a number in.
 */
export function boundLabel(value: number | null | undefined, unit: string): string {
  if (value === null || value === undefined) {
    return "Not configured — compiled default applies (unknown to the shell)";
  }
  return `${value} ${unit}`;
}
