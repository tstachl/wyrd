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

/**
 * Base dir for suggested drive dirs: two levels up from a
 * `<base>/keys/<leaf>` credential path (leaf starting with
 * `identity` or `passphrase`), else null. Parents are the store's
 * business (it creates the chain, hardening only the leaf), so a
 * nested `<base>/drives/<name>` suggestion is safe. Pure over the
 * input so the provisioning form pins without a filesystem.
 */
export function baseDirFromIdentity(path: string): string | null {
  const trimmed = path.trim();
  if (trimmed === "") return null;
  const stripped = trimmed.endsWith("/") ? trimmed.slice(0, -1) : trimmed;
  const parts = stripped.split("/");
  if (parts.length < 4) return null;
  const leaf = parts[parts.length - 1] ?? "";
  const parent = parts[parts.length - 2] ?? "";
  if (parent !== "keys") return null;
  if (!(leaf.startsWith("identity") || leaf.startsWith("passphrase"))) return null;
  return parts.slice(0, -2).join("/") || "/";
}

/** Suggested drive dir `<base>/drives/<name>` (`alpha` where the name is blank), else null. */
export function suggestedDriveDirFor(nameValue: string, identityPath: string): string | null {
  const base = baseDirFromIdentity(identityPath);
  if (base === null) return null;
  const safeName = nameValue.trim() === "" ? "alpha" : nameValue.trim();
  return `${base}/drives/${safeName}`;
}

/**
 * Missing-CLI hint for version-mismatch detail: the desktop resolves
 * the version-matched `wyrd` binary as a sibling of its own binary
 * (never `PATH`), so a fresh dev checkout without `cargo build -p
 * wyrd-cli` reports `cannot execute … No such file`. Append the
 * one-line build pointer where the reason names that case; otherwise
 * return the reason unchanged. Pure over the input, pinned below.
 */
export function withDevCliHint(reason: string): string {
  if (reason.includes("cannot execute") && reason.includes("No such file")) {
    return `${reason} — in development, build the CLI (cargo build -p wyrd-cli) so wyrd sits beside the desktop binary`;
  }
  return reason;
}
