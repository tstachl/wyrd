// Pure display helpers for the shell views. No runes, no IPC, no
// owner facts — components call these so the wording pins in
// `format.test.ts` instead of drifting per view.
//
// Load-bearing rule repeated here because helpers are where it
// would rot: every label derived from cached data must read as
// historical ("last observed …"), and every unknown state must read
// as unknown — never as a negative, never as a default.

import type { WireFreshnessState } from "$lib/ipc/results.js";
import type { DriveAvailability, SessionState } from "$lib/ipc/results.js";
import type { ProvisionReport } from "$lib/ipc/results.js";

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
 * One-line session gate for definition-less drives. Null where a
 * service definition exists — managed recovery owns that drive, so
 * no gate renders. Observation only, never supervision.
 */
export function sessionLabel(session: SessionState | null): string | null {
  if (session === null) return null;
  switch (session.state) {
    case "unlocked":
      return "Unlocked for this session";
    case "locked":
      return "Locked — needs passphrase";
  }
}

/**
 * Locked drives keep their local settings surfaces: configured
 * bounds read from the settings file, and the identity-pair Leave
 * card both work without an owner — so the settings tab renders
 * them instead of the unavailable overlay. Owner-backed Status
 * stays gated on unlock; managed-down drives (no session gate)
 * keep the overlay with their Start/Reinstall recovery.
 */
export function settingsSurviveLock(session: SessionState | null): boolean {
  return session !== null && session.state === "locked";
}

/**
 * How saved bounds take effect for one drive, from its registry
 * posture: `service` where no session gate exists (managed — the
 * shell restarts the service), `session` where the app holds an
 * unlock session (lock, then unlock with the typed passphrase),
 * `null` where no owner runs (locked or stopped — saved values
 * apply at the next start). The Settings view renders from this;
 * it never decides posture itself.
 */
export function restartKindFor(
  session: SessionState | null,
): "service" | "session" | null {
  if (session === null) return "service";
  return session.state === "unlocked" ? "session" : null;
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

/** One editable bound: the key the file carries plus its UI contract. */
export interface SettingBound {
  /** The settings-file key (and sparse-update member). */
  key: string;
  /** The row label the editor shows. */
  label: string;
  /** The unit the value carries (display and input hint). */
  unit: string;
  /** Smallest acceptable whole number. Zero only where zero is meaningful (the quota: refuse-all). */
  min: number;
  /** Largest acceptable whole number. */
  max: number;
}

/**
 * The five bounds the editor owns, in file order. The validation
 * ranges mirror the shared contract (`wyrd-settings`): the backend
 * enforces them again, so this is fast feedback, never authority.
 */
export const SETTING_BOUNDS: SettingBound[] = [
  { key: "retained_bytes_quota", label: "Retained bytes quota", unit: "bytes", min: 0, max: Number.MAX_SAFE_INTEGER },
  { key: "max_admit_per_pass", label: "Items admitted per sync pass", unit: "items", min: 1, max: 1000000 },
  { key: "fetch_pass_budget_secs", label: "Fetch time per pass", unit: "seconds", min: 1, max: 86400 },
  { key: "max_mutation_wait_secs", label: "Mutation wait limit", unit: "seconds", min: 1, max: 86400 },
  { key: "open_timeout_secs", label: "Open wait limit", unit: "seconds", min: 1, max: 86400 },
];

/**
 * Validate one bound's raw input text: whole numbers in the key's
 * range, naming the key on refusal. Returns the error message, or
 * null when the text is a committable value. Empty text is not an
 * error here — the editor treats it as "no change", never as zero.
 */
export function validateBoundInput(key: string, text: string): string | null {
  const bound = SETTING_BOUNDS.find((candidate) => candidate.key === key) ?? null;
  if (bound === null) return `Unknown settings key "${key}"`;
  const trimmed = text.trim();
  if (!/^[0-9]+$/.test(trimmed)) {
    return `${key}: enter a whole number of ${bound.unit}`;
  }
  const value = Number(trimmed);
  if (!Number.isSafeInteger(value) || value < bound.min || value > bound.max) {
    return `${key}: enter ${bound.min}–${bound.max} ${bound.unit}`;
  }
  return null;
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
 * Next drive-dir value from the form's state (pure, pinned below):
 * while untouched, follow the suggestion; otherwise keep the
 * operator's value. An explicit clear marks the field touched
 * upstream, so it sticks.
 */
export function nextDriveDir(
  touched: boolean,
  current: string,
  suggested: string | null,
): string {
  if (!touched && suggested !== null && current !== suggested) return suggested;
  return current;
}

/**
 * Missing-CLI hint for version-mismatch detail: the desktop resolves
 * the version-matched `wyrd` binary as a sibling of its own binary
 * (never `PATH`), so a checkout without a built CLI reports
 * `cannot execute … No such file`. Append the build pointer where
 * the reason names that case — in a development checkout that is the
 * fix; in a packaged install the same sentence degrades to the
 * reinstall line above it. Otherwise return the reason unchanged.
 * Pure over the input, pinned below.
 */
export function withDevCliHint(reason: string): string {
  if (reason.includes("cannot execute") && reason.includes("No such file")) {
    return `${reason} — if this is a development checkout, build the CLI (cargo build -p wyrd-cli) so wyrd sits beside the desktop binary; in a packaged install, reinstall the matched pair`;
  }
  return reason;
}

/**
 * One lifecycle-operation outcome line. The exit code says what the
 * adapter ran, never what exists; the observation below carries the
 * state. Unknown and held-drive outcomes name themselves instead of
 * collapsing into failure.
 */
export function describeProvisionResult(report: ProvisionReport): string {
  const result = report.result;
  switch (result.result) {
    case "completed":
      return `Exit 0 — the CLI reported no error. Only the observation below says what happened: ${result.detail}`;
    case "expectedfailure":
      return `Exit 2 — expected failure. Only the observation below says what actually exists: ${result.detail}`;
    case "helddrive":
      return `Held drive (another process holds it — not retried): ${result.detail}`;
    case "versionmismatch":
      return `Version mismatch (reinstall the matched pair): ${withDevCliHint(result.reason)}`;
    case "busy":
      return `Another operation is already in flight on ${result.drive_dir} — not queued.`;
    case "unknown":
      return `Outcome unknown: ${result.detail}. The observation below decides.`;
  }
}

/** Post-run registry summary. Never a claim about the operated drive's membership. */
export function observedRegistrySummary(report: ProvisionReport): string {
  const observed = report.observed;
  if (observed.status !== "ready") return `Registry: ${observed.status}.`;
  const names = observed.drives.map((drive) => drive.name).join(", ");
  return `Registry ready: ${observed.drives.length} drive(s)${names.length > 0 ? ` (${names})` : ""}.`;
}

/** Post-run membership tip for the operated drive; absent owner answer stays unknown. */
export function membershipSummary(report: ProvisionReport): string {
  const membership = report.membership;
  if (membership === null) return "Membership: unknown (the owner did not answer).";
  const epoch = membership.epoch === null ? "no tip yet" : `epoch ${membership.epoch}`;
  return `Membership: ${membership.known_members} member(s), ${epoch}.`;
}
