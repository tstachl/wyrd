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
import type { CredentialReference } from "$lib/ipc/results.js";
import type { CredentialMode } from "$lib/ipc/provisioning-args.js";

/**
 * Relative age of the shell's own observation stamp. It uses the local
 * clock, never UTC, and is explicitly "Checked …" — the shell's
 * observation, never a synchronization time. `nowMs` is injected so the
 * wording pins without waiting, and the view refreshes it on a timer.
 */
export function formatObservedAge(observedAtMs: number, nowMs: number): string {
  const seconds = Math.max(0, Math.floor((nowMs - observedAtMs) / 1000));
  if (seconds < 5) return "Checked just now";
  if (seconds < 60) return `Checked ${seconds} seconds ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `Checked ${minutes} minute${minutes === 1 ? "" : "s"} ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `Checked ${hours} hour${hours === 1 ? "" : "s"} ago`;
  const days = Math.floor(hours / 24);
  return `Checked ${days} day${days === 1 ? "" : "s"} ago`;
}

/**
 * Age of the last local projection publication (D-11). The value is a
 * unix-seconds stamp from the owner, not the shell's observation stamp:
 * never a sync time, never a heartbeat, never UTC. Zero or absent means
 * unknown and reads as such — never as a 1970 date. `nowMs` is injected
 * so the wording pins without waiting; the view refreshes it on a timer.
 */
export function formatPublicationAge(publishedUnixSecs: number | undefined, nowMs: number): string {
  if (
    publishedUnixSecs === undefined ||
    !Number.isFinite(publishedUnixSecs) ||
    publishedUnixSecs <= 0
  ) {
    return "not yet updated on this device";
  }
  const seconds = Math.max(0, Math.floor((nowMs - publishedUnixSecs * 1000) / 1000));
  if (seconds < 5) return "Updated just now";
  if (seconds < 60) return `Updated ${seconds} seconds ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `Updated ${minutes} minute${minutes === 1 ? "" : "s"} ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `Updated ${hours} hour${hours === 1 ? "" : "s"} ago`;
  const days = Math.floor(hours / 24);
  return `Updated ${days} day${days === 1 ? "" : "s"} ago`;
}

/** Shorten a content id for display. The full value stays in the snapshot; this is presentation only. */
export function shortId(id: string): string {
  return id.length <= 12 ? id : `${id.slice(0, 8)}…`;
}

/** One-line freshness judgment, interim (D-11): "Current" answers
 * whether the served data is current for this device. A retained
 * last-good names its staleness, never current health — and never a
 * convergence claim. */
export function freshnessLabel(state: WireFreshnessState): string {
  if (state === "FreshEmpty") return "Current (empty)";
  if (state === "FreshNonEmpty") return "Current";
  return `Last good copy — stale: ${state.RetainedLastGood.detail}`;
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

/** The drive folder's own name, not a truncated path. */
export function folderNameFromPath(path: string): string {
  const trimmed = path.trim().replace(/[/\\]+$/, "");
  const leaf = trimmed.split(/[/\\]/).pop() ?? "";
  return leaf === "" ? (path.trim() === "" ? "Drive folder" : path.trim()) : leaf;
}

/** Observable facts a drive card can state; null where it never answered this session. */
export interface OverviewDriveFacts {
  availability: "available" | "unavailable" | "unknown";
  paused: boolean;
  connectedRelays: number;
  totalRelays: number;
  peers: number;
  knownMembers: number;
  stale: boolean;
}

/** Observable run state for a card, never a single health verdict. */
export function driveRunState(facts: OverviewDriveFacts | null): string {
  if (facts === null) return "Not yet observed";
  if (facts.availability === "unavailable") return "Not running";
  if (facts.availability === "unknown") return "Not yet observed";
  return facts.paused ? "Paused" : "Running";
}

/** One line of separate facts beneath the card title. */
export function driveFactLine(facts: OverviewDriveFacts | null, configuredRelays: number): string {
  if (facts === null) {
    return `${configuredRelays} relay${configuredRelays === 1 ? "" : "s"} configured · Status not read this session`;
  }
  const relays =
    facts.totalRelays === 0
      ? "No relays connected at the last pass"
      : `Connected to ${facts.connectedRelays} of ${facts.totalRelays} relays`;
  const sharing =
    facts.knownMembers <= 1 && facts.peers === 0
      ? "No other devices yet"
      : facts.peers > 0
        ? `${facts.peers} other device${facts.peers === 1 ? "" : "s"} connected at the last pass`
        : `${facts.knownMembers - 1} other device${facts.knownMembers - 1 === 1 ? "" : "s"} in this drive, none connected at the last pass`;
  return `${relays} · ${sharing}`;
}

/** Only observed problems put a drive in the attention group. */
export function driveNeedsAttention(facts: OverviewDriveFacts | null): boolean {
  if (facts === null) return false;
  return (
    facts.availability === "unavailable" ||
    facts.paused ||
    facts.totalRelays === 0 ||
    facts.connectedRelays === 0 ||
    facts.stale
  );
}

/** Stable partition: drives needing attention first, original order within each group. */
export function sortDrivesByAttention<T>(
  drives: T[],
  factsFor: (drive: T) => OverviewDriveFacts | null,
): T[] {
  return [...drives].sort((a, b) => {
    const aNeeds = driveNeedsAttention(factsFor(a)) ? 1 : 0;
    const bNeeds = driveNeedsAttention(factsFor(b)) ? 1 : 0;
    return bNeeds - aNeeds;
  });
}

/**
 * The separate observable facts the Status screen states in plain
 * language. No field is a verdict: reachability, relay connectivity,
 * membership, freshness and convergence stay distinct. Every value
 * comes from the last terminal answer plus the shell's observation,
 * never from a fabricated default.
 */
export interface StatusFacts {
  availability: "available" | "unavailable" | "unknown";
  paused: boolean;
  connectedRelays: number;
  totalRelays: number;
  knownMembers: number;
  peers: number;
  /** Present only for a retained last-good copy; never a sync timestamp. */
  staleDetail: string | null;
  freshEmpty: boolean;
  localObjects: number;
}

/** Observable run state. Paused still answers; unavailable never claims the drive is gone. */
export function statusRunState(facts: StatusFacts): string {
  if (facts.availability === "unavailable") return "Not running";
  if (facts.availability === "unknown") return "State not yet observed";
  return facts.paused ? "Paused" : "Running";
}

/** One plain sentence of separate facts — not a single green verdict. */
export function statusSummaryLine(facts: StatusFacts): string {
  const relays =
    facts.totalRelays === 0
      ? "no relays connected at the last pass"
      : `${facts.connectedRelays} of ${facts.totalRelays} relays connected`;
  const sharing =
    facts.knownMembers <= 1
      ? "not shared with another device yet"
      : facts.peers === 0
        ? `${facts.knownMembers - 1} other device${facts.knownMembers - 1 === 1 ? "" : "s"} in this drive, none connected at the last pass`
        : `${facts.peers} other device${facts.peers === 1 ? "" : "s"} connected at the last pass`;
  return `${statusRunState(facts)} · ${relays} · ${sharing}`;
}

/**
 * Only conditions the shell actually observed. An empty list renders
 * no attention area at all — there is no "all good" state to show.
 */
export function statusAttention(facts: StatusFacts): string[] {
  const items: string[] = [];
  if (facts.availability === "unavailable") {
    items.push("The drive is not answering. Its last reported state is still shown below.");
  }
  if (facts.paused) {
    items.push("Synchronization is paused. The drive is still running and can answer requests.");
  }
  if (facts.totalRelays === 0 || facts.connectedRelays === 0) {
    items.push(
      "No relays connected at the last pass. Nothing can be exchanged with other devices until a relay is reachable.",
    );
  }
  if (facts.staleDetail !== null) {
    items.push(`The served data is stale: ${facts.staleDetail}.`);
  }
  return items;
}

/** One member, no peers and an empty served projection is a new drive, not a grid of zeros. */
export function isFirstRun(facts: StatusFacts): boolean {
  return facts.knownMembers === 1 && facts.peers === 0 && facts.freshEmpty;
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
 * the label says exactly that — "Using Wyrd's default" — and the
 * view carries the disclosure that the shell cannot see the value.
 */
export function boundLabel(value: number | null | undefined, unit: string): string {
  if (value === null || value === undefined) {
    return "Using Wyrd's default";
  }
  return `${value} ${unit}`;
}

/** One editable bound: the key the file carries plus its UI contract. */
export interface SettingBound {
  /** The settings-file key (and sparse-update member). */
  key: string;
  /** The row label the editor shows: what the setting means to the user. */
  label: string;
  /** One line beneath the label: precisely what the implementation enforces. */
  description: string;
  /** The unit the settings file stores (validation messages only). */
  unit: string;
  /** The human unit shown beside the input. Storage is never shown in raw bytes. */
  inputUnit: string;
  /** Raw units per human unit. Storage uses 1024³ bytes; the rest are 1:1. */
  inputScale: number;
  /** Smallest acceptable whole number of raw units. Zero only where zero is meaningful (the quota: refuse-all). */
  min: number;
  /** Largest acceptable whole number of raw units. */
  max: number;
}

/** Bytes in one storage unit as entered in the form (1024³, binary GB). */
const BYTES_PER_STORAGE_UNIT = 1024 * 1024 * 1024;

/**
 * The five bounds the editor owns, in file order. Labels say what
 * the setting means; descriptions say what the implementation
 * enforces — jargon (`quota`, `admitted`, `mutation`) lives here
 * in the description at most, never in the label. The validation
 * ranges mirror the shared contract (`wyrd-settings`): the backend
 * enforces them again, so this is fast feedback, never authority.
 *
 * The form shows storage in GB (1024³ bytes), never as a raw byte
 * count; the other four bound values already carry a human unit.
 * The Advanced disclosure states the byte definition so the display
 * cannot imply a different guarantee.
 */
export const SETTING_BOUNDS: SettingBound[] = [
  { key: "retained_bytes_quota", label: "Storage limit", description: "The most drive data Wyrd will keep on this device. When that limit is reached, Wyrd refuses new data before writing anything. This is not a limit on your disk's free space.", unit: "bytes", inputUnit: "GB", inputScale: BYTES_PER_STORAGE_UNIT, min: 0, max: Number.MAX_SAFE_INTEGER },
  { key: "max_admit_per_pass", label: "Sync work limit", description: "How many items of sync work Wyrd starts in each sync pass. Anything beyond this waits for the next pass; nothing is dropped.", unit: "items", inputUnit: "items", inputScale: 1, min: 1, max: 1000000 },
  { key: "fetch_pass_budget_secs", label: "Download time per sync pass", description: "How long Wyrd spends downloading in one sync pass. When that time runs out, unfinished downloads wait for the next pass.", unit: "seconds", inputUnit: "seconds", inputScale: 1, min: 1, max: 86400 },
  { key: "max_mutation_wait_secs", label: "File change wait limit", description: "How long a change you make can wait for the content it needs from other devices. If that content does not arrive in time, the change fails and you can retry it; nothing is saved.", unit: "seconds", inputUnit: "seconds", inputScale: 1, min: 1, max: 86400 },
  { key: "open_timeout_secs", label: "File open wait limit", description: "How long Wyrd waits for a file's content to become available on this device when you open or read it. If it takes longer, the open or read fails and you can try again while the download continues.", unit: "seconds", inputUnit: "seconds", inputScale: 1, min: 1, max: 86400 },
];

/** The parsed result of one input: a raw value for the sparse update, or a key-named error. */
export type ParsedBound = { ok: true; raw: number } | { ok: false; error: string };

/** The bound for a key, or null for an unknown key. */
export function boundFor(key: string): SettingBound | null {
  return SETTING_BOUNDS.find((candidate) => candidate.key === key) ?? null;
}

/**
 * Initial text for one input: the configured value in the bound's
 * human unit, or empty for an absent key (Wyrd's default). Scaled
 * values drop trailing zeroes so a stored 1024³ bytes reads "1".
 */
export function formatBoundValue(bound: SettingBound, raw: number | null | undefined): string {
  if (raw === null || raw === undefined) return "";
  const scaled = raw / bound.inputScale;
  return Number(scaled.toFixed(9)).toString();
}

/**
 * Parse one input's text into the raw value the settings file takes.
 * Storage accepts a decimal number of GB; the other bounds take a
 * whole number of their unit. Every failure names the key, and the
 * raw value is always a safe integer so the sparse update is exact.
 */
export function parseBoundInput(key: string, text: string): ParsedBound {
  const bound = boundFor(key);
  if (bound === null) return { ok: false, error: `Unknown settings key "${key}"` };
  const trimmed = text.trim();
  const pattern = bound.inputScale === 1 ? /^[0-9]+$/ : /^[0-9]+(?:\.[0-9]+)?$/;
  const wholeWord = bound.inputScale === 1 ? "whole number" : "number";
  if (!pattern.test(trimmed)) {
    return { ok: false, error: `${key}: enter a ${wholeWord} of ${bound.inputUnit}` };
  }
  const raw = Number(trimmed) * bound.inputScale;
  if (!Number.isSafeInteger(raw) || raw < bound.min || raw > bound.max) {
    return { ok: false, error: `${key}: enter a ${wholeWord} of ${bound.inputUnit} within the allowed range` };
  }
  return { ok: true, raw };
}

/**
 * Validate one bound's input text, naming the key on refusal. Kept
 * as the error-only face of `parseBoundInput` for callers that only
 * need a message. Empty text is not an error here — the editor
 * treats it as "no change", never as zero.
 */
export function validateBoundInput(key: string, text: string): string | null {
  const parsed = parseBoundInput(key, text);
  return parsed.ok ? null : parsed.error;
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

/**
 * Suggested relays pre-filled in the Add-drive form (create and join
 * tabs share the entry state, so both start with this set — the user
 * keeps, edits, or clears it, and the drive saves exactly what is
 * listed). Selection method, decided for this issue: live policy
 * probes from 2026-11-19, one fresh throwaway keypair per relay,
 * publishing then reading back a gift wrap shaped like Wyrd's wire
 * format with NIP-42 auth disabled. Hard gates: no auth on
 * gift-wrap reads (trust.md T16), no payment, no restricted writes,
 * write accepted, wrap returned — then 3/3 repeat consistency and
 * NIP-66 monitor breadth. `relay.primal.net` doubles as the relay
 * the opt-in interop test already proves. Vantage-dependent
 * (availability varies by network) and unmeasured on retention, so
 * re-check periodically — this is a starting suggestion, never a
 * standing endorsement.
 */
export const SUGGESTED_RELAYS: string[] = [
  "wss://relay.primal.net",
  "wss://relay.ditto.pub",
  "wss://purplerelay.com",
  "wss://relay.snort.social",
  "wss://offchain.pub",
];

/** Suggested drive dir `<base>/drives/<name>` (`alpha` where the name is blank), else null. */
export function suggestedDriveDirFor(nameValue: string, identityPath: string): string | null {
  const base = baseDirFromIdentity(identityPath);
  if (base === null) return null;
  const safeName = nameValue.trim() === "" ? "alpha" : nameValue.trim();
  return `${base}/drives/${safeName}`;
}

/**
 * Auto-format a typed drive name into the shape the registry
 * accepts: lowercase letters, digits and dashes, at most 58
 * characters (the registry reserves six for the service prefix).
 * Runs of other characters become one dash so "My Drive!" reads
 * "my-drive" as the user types. Pure over the input so the form
 * pins without a component.
 */
export function normalizeDriveName(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "")
    .slice(0, 58)
    .replace(/-$/g, "");
}

/** Whether a name is a committable registry token after formatting. */
export function driveNameValid(value: string): boolean {
  return value.length > 0 && value.length <= 58 && /^[a-z0-9-]+$/.test(value);
}

/**
 * OS credential-store account for one drive (`wyrd-drive-<name>`,
 * `alpha` where the name is blank so the label stays stable while
 * typing). One account per drive; the label is a reference, never a
 * secret. Registry names keep the label inside the account shape
 * (1–128 bytes, no control bytes, no surrounding whitespace); the
 * form gates only on non-empty, so anything outside the registry
 * alphabet refuses at the CLI with the account rule before anything
 * provisions.
 */
export function storeAccountFor(nameValue: string): string {
  const safeName = nameValue.trim() === "" ? "alpha" : nameValue.trim();
  return `wyrd-drive-${safeName}`;
}

/**
 * Credential mode from observation: a known store or file reference
 * fixes the mode; a missing or explicitly unknown observation keeps
 * the caller's fallback so the operator selector still renders. No
 * backend is inferred from a registry name.
 */
export function credentialModeFor(
  reference: CredentialReference | null,
  fallback: CredentialMode,
): CredentialMode {
  if (reference?.backend === "os-store") return "store";
  if (reference?.backend === "file") return "file";
  return fallback;
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
      return `Another operation is already running on ${result.drive_dir}. Wait for it to finish and try again.`;
    case "unknown":
      return `Outcome unknown: ${result.detail}. The observation below decides.`;
  }
}

/** Post-run drive-list summary. Never a claim about the operated drive's membership. */
export function observedRegistrySummary(report: ProvisionReport): string {
  const observed = report.observed;
  if (observed.status !== "ready") return `The drive list is not ready (${observed.status}).`;
  const names = observed.drives.map((drive) => drive.name).join(", ");
  return `Drives on this device: ${observed.drives.length}${names.length > 0 ? ` (${names})` : ""}.`;
}

/**
 * Whether one provision should leave the form: the CLI reported no
 * error (`completed`) and the post-run observation names the drive
 * dir (the registry read ready and lists it). Both halves are
 * required — a clean exit alone never advances (the registry has
 * the last word), and a refused or uncertain outcome never
 * advances even where the observation still names the drive (its
 * reason stays on the form). An unreadable or missing registry
 * proves nothing and stays false.
 */
export function shouldAdvanceAfterProvision(report: ProvisionReport, driveDir: string): boolean {
  if (report.result.result !== "completed") return false;
  const observed = report.observed;
  return (
    observed.status === "ready" &&
    observed.drives.some((drive) => drive.drive_dir === driveDir)
  );
}

/**
 * Whether one removal has been confirmed by its post-run observation.
 * The inverse of the add rule: a clean exit alone never advances; the
 * observed state has the last word. A full delete is confirmed only
 * when a ready registry no longer names the drive dir. A keep-local
 * leave keeps the drive registered on purpose, so its confirmation is
 * the owner's terminal membership tip answering *alongside* the still
 * listed drive. An unreadable registry, an absent tip, or any refusal
 * or held outcome proves nothing and keeps the report on screen with
 * a way back.
 *
 * Known limit: a non-owner leave authors nothing yet also exits 0 with
 * the tip still answering — the shell cannot tell it from a real leave
 * through this observation. The overview banner carries the full
 * report on the advance path so that refusal text stays visible and a
 * no-op is never presented as a finished removal.
 */
export function shouldAdvanceAfterRemoval(
  report: ProvisionReport,
  driveDir: string,
  keepLocalData: boolean,
): boolean {
  if (report.result.result !== "completed") return false;
  if (report.observed.status !== "ready") return false;
  const listed = report.observed.drives.some((drive) => drive.drive_dir === driveDir);
  return keepLocalData ? listed && report.membership !== null : !listed;
}

/** Post-run membership tip for the operated drive; absent owner answer stays unknown. */
export function membershipSummary(report: ProvisionReport): string {
  const membership = report.membership;
  if (membership === null) return "Membership: unknown (the owner did not answer).";
  const epoch = membership.epoch === null ? "no tip yet" : `epoch ${membership.epoch}`;
  return `Membership: ${membership.known_members} member(s), ${epoch}.`;
}
