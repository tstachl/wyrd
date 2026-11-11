// Presentation cache, never a second drive model.
//
// The frontend displays what the owner last reported and marks it
// as such; when the reports change, the display follows. What may
// live here: selected view, selected drive, open dialogs, form
// input, pending-request indicators, last observed response. What
// must not: drive membership, lifecycle, materialization policy,
// durable status, mutation outcomes — those live in the owner.
// See `docs/design/desktop-shell.md`, "Frontend: three views and
// the unavailable state".
//
// The selected drive is presentation state, never an authorization
// boundary: every drive-scoped command carries the absolute drive
// directory as an exact string, and the backend resolves the
// connection from it. Selecting a drive never authorizes access to
// another drive's connection.
//
// Cached-state semantics (normative for this shell):
//
// - A last successful observation displays as historical/stale
//   information, stamped with when it was observed. It never
//   becomes evidence of current owner state.
// - Current availability derives only from live socket outcomes,
//   per drive: a fresh terminal success marks that drive
//   available, an `Unavailable` answer marks that drive
//   unavailable. An `Unconfirmed` outcome changes nothing — the
//   prior state stands, and the view marks its data stale. Cached
//   data, including freshly re-rendered cached data, never marks
//   available. One drive's outcome never changes another drive's
//   availability.
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

let selectedView = $state<ShellView>("status");
/** The explicitly selected drive, by absolute directory. Null before the first selection. */
let selectedDriveDir = $state<string | null>(null);
let pendingRequests = $state(0);
/** The last owner answer per drive, keyed by absolute drive
 * directory. One drive's error never renders on another drive's
 * view: derivations read only the selected drive's slot. */
let lastResults = $state<Record<string, IpcResult<unknown>>>({});
/** A local query (registry/settings) failed outside the taxonomy — transport or shell bug. Not an owner answer. */
let transportNotice = $state<string | null>(null);
/** Per-drive availability, keyed by absolute drive directory. Absent means unknown (never probed this session). */
let availabilityByDrive = $state<Record<string, OwnerAvailability>>({});
let lifecycleSnapshot = $state<Stamped<RegistryState> | null>(null);
/** Per-drive status snapshots, keyed by absolute drive directory. */
let statusSnapshots = $state<Record<string, Stamped<StatusSnapshot>>>({});
/** Per-drive settings snapshots, keyed by absolute drive directory. */
let settingsSnapshots = $state<Record<string, Stamped<DriveSettingsState>>>({});

function stamp<T>(data: T): Stamped<T> {
  return { data, observedAtMs: Date.now() };
}

/** Module-level so no caller can lose `this`: mark one drive available. */
function setAvailable(driveDir: string): void {
  availabilityByDrive[driveDir] = "available";
}

/** Module-level so no caller can lose `this`: mark one drive unavailable. */
function setUnavailable(driveDir: string): void {
  availabilityByDrive[driveDir] = "unavailable";
}

export const presentation = {
  get selectedView(): ShellView {
    return selectedView;
  },
  get selectedDriveDir(): string | null {
    return selectedDriveDir;
  },
  get pendingRequests(): number {
    return pendingRequests;
  },
  /**
   * One drive's last owner answer — null where that drive has no
   * terminal answer this session. Views and overlays read only
   * their own drive's slot, so concurrent per-drive refreshes
   * never bleed one drive's error onto another's view.
   */
  lastResultFor(driveDir: string): IpcResult<unknown> | null {
    return lastResults[driveDir] ?? null;
  },
  get transportNotice(): string | null {
    return transportNotice;
  },
  get lifecycleSnapshot(): Stamped<RegistryState> | null {
    return lifecycleSnapshot;
  },
  /** This drive's last status answer, stamped — null where this drive never answered this session. */
  statusSnapshotFor(driveDir: string): Stamped<StatusSnapshot> | null {
    return statusSnapshots[driveDir] ?? null;
  },
  /** This drive's last settings answer, stamped — null where this drive never answered this session. */
  settingsSnapshotFor(driveDir: string): Stamped<DriveSettingsState> | null {
    return settingsSnapshots[driveDir] ?? null;
  },
  /** This drive's availability: `unknown` where never probed this session. */
  availabilityFor(driveDir: string): OwnerAvailability {
    return availabilityByDrive[driveDir] ?? "unknown";
  },
  selectView(view: ShellView): void {
    selectedView = view;
  },
  selectDrive(driveDir: string): void {
    selectedDriveDir = driveDir;
  },
  /**
   * Call only after a fresh terminal owner success for that drive.
   * Cached answers — including cache hits re-rendered now — must
   * never call this. Never touches another drive's availability.
   */
  markAvailable(driveDir: string): void {
    setAvailable(driveDir);
  },
  clearTransportNotice(): void {
    transportNotice = null;
  },
  noteTransportFailure(detail: string): void {
    transportNotice = detail;
  },
  storeLifecycle(data: RegistryState): void {
    lifecycleSnapshot = stamp(data);
  },
  storeStatus(driveDir: string, data: StatusSnapshot): void {
    statusSnapshots[driveDir] = stamp(data);
  },
  storeSettings(driveDir: string, data: DriveSettingsState): void {
    settingsSnapshots[driveDir] = stamp(data);
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
  settleRequest(driveDir: string, result: IpcResult<unknown>): void {
    pendingRequests = Math.max(0, pendingRequests - 1);
    lastResults[driveDir] = result;
    if (!result.ok && result.error.kind === "unavailable") {
      setUnavailable(driveDir);
    }
    // Every other outcome leaves availability untouched: success
    // paths call `markAvailable` explicitly once their terminal
    // answer lands, and an `Unconfirmed` outcome is unknown — the
    // prior state stands, and views mark their data stale.
  },
};
