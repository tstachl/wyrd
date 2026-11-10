// Presentation cache, never a second drive model.
//
// The frontend displays what the owner last reported and marks it
// as such; when the reports change, the display follows. What may
// live here: selected view, open dialogs, form input,
// pending-request indicators, last observed response. What must
// not: drive membership, lifecycle, materialization policy,
// durable status, mutation outcomes — those live in the owner.
// See `docs/design/desktop-shell.md`, "Frontend: three views and
// the unavailable state".
//
// Cached-state semantics (normative for this shell):
//
// - A last successful observation displays as historical/stale
//   information, stamped with when it was observed. It never
//   becomes evidence of current owner state.
// - Current availability derives only from live socket outcomes:
//   a fresh terminal success marks available, an `Unavailable`
//   answer marks unavailable. An `Unconfirmed` outcome changes
//   nothing — the prior state stands, and the view marks its data
//   stale. Cached data, including freshly re-rendered cached data,
//   never marks available.
// - Owner unavailable does not mean the drive is empty; a failed
//   observation does not mean membership is absent; cached posture
//   is not current posture; a past success never proves the next
//   operation succeeds. Components read these rules from this
//   module, not from their own judgment.

import type { IpcResult } from "$lib/ipc/types.js";
import type {
  DriveSettingsState,
  RegistryState,
  WireFreshness,
  WireLiveness,
  WireSyncStatus,
} from "$lib/ipc/results.js";

export type OwnerAvailability = "unknown" | "available" | "unavailable";

/** The three shipped views. The unavailable state overlays content; it is not a selection. */
export type ShellView = "status" | "lifecycle" | "settings";

/**
 * One view's last successful answer, stamped at observation. The
 * stamp is display metadata (the shell's own clock, for staleness
 * wording), never owner state. A snapshot whose availability was
 * `unknown` or whose age the view shows as stale stays visibly
 * qualified — never presented as current.
 */
export interface Stamped<T> {
  data: T;
  observedAtMs: number;
}

export interface StatusSnapshot {
  observe: WireSyncStatus;
  liveness: WireLiveness;
  freshness: WireFreshness;
}

let availability = $state<OwnerAvailability>("unknown");
let selectedView = $state<ShellView>("status");
let pendingRequests = $state(0);
let lastResult = $state<IpcResult<unknown> | null>(null);
/** A local query (registry/settings) failed outside the taxonomy — transport or shell bug. Not an owner answer. */
let transportNotice = $state<string | null>(null);
let statusSnapshot = $state<Stamped<StatusSnapshot> | null>(null);
let lifecycleSnapshot = $state<Stamped<RegistryState> | null>(null);
let settingsSnapshot = $state<Stamped<DriveSettingsState> | null>(null);

function stamp<T>(data: T): Stamped<T> {
  return { data, observedAtMs: Date.now() };
}

export const presentation = {
  get availability(): OwnerAvailability {
    return availability;
  },
  get selectedView(): ShellView {
    return selectedView;
  },
  get pendingRequests(): number {
    return pendingRequests;
  },
  get lastResult(): IpcResult<unknown> | null {
    return lastResult;
  },
  get transportNotice(): string | null {
    return transportNotice;
  },
  get statusSnapshot(): Stamped<StatusSnapshot> | null {
    return statusSnapshot;
  },
  get lifecycleSnapshot(): Stamped<RegistryState> | null {
    return lifecycleSnapshot;
  },
  get settingsSnapshot(): Stamped<DriveSettingsState> | null {
    return settingsSnapshot;
  },
  selectView(view: ShellView): void {
    selectedView = view;
  },
  /**
   * Call only after a fresh terminal owner success. Cached answers
   * — including cache hits re-rendered now — must never call this.
   */
  markAvailable(): void {
    availability = "available";
  },
  /** An `Unavailable` answer is the only path here: no transport, owner state unknown. */
  markUnavailable(): void {
    availability = "unavailable";
  },
  clearTransportNotice(): void {
    transportNotice = null;
  },
  noteTransportFailure(detail: string): void {
    transportNotice = detail;
  },
  storeStatus(data: StatusSnapshot): void {
    statusSnapshot = stamp(data);
  },
  storeLifecycle(data: RegistryState): void {
    lifecycleSnapshot = stamp(data);
  },
  storeSettings(data: DriveSettingsState): void {
    settingsSnapshot = stamp(data);
  },
  trackRequest(): void {
    pendingRequests += 1;
  },
  /**
   * Settle one local-query dispatch (registry/settings): no owner
   * taxonomy is involved, so there is no answer to record and no
   * availability to derive. Transport failures surface through
   * `noteTransportFailure`, never here.
   */
  finishRequest(): void {
    pendingRequests = Math.max(0, pendingRequests - 1);
  },
  settleRequest(result: IpcResult<unknown>): void {
    pendingRequests = Math.max(0, pendingRequests - 1);
    lastResult = result;
    if (!result.ok && result.error.kind === "unavailable") {
      availability = "unavailable";
    }
    // Every other outcome leaves availability untouched: success
    // paths call `markAvailable` explicitly once their terminal
    // answer lands, and an `Unconfirmed` outcome is unknown — the
    // prior state stands, and views mark their data stale.
  },
};
