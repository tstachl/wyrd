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

import type { IpcResult } from "$lib/ipc/types.js";

export type OwnerAvailability = "unknown" | "available" | "unavailable";

/** The only view this shell revision renders. Views land later. */
export type ShellView = "status";

let availability = $state<OwnerAvailability>("unknown");
let selectedView = $state<ShellView>("status");
let pendingRequests = $state(0);
let lastResult = $state<IpcResult<unknown> | null>(null);

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
  markAvailable(): void {
    availability = "available";
  },
  markUnavailable(): void {
    availability = "unavailable";
  },
  trackRequest(): void {
    pendingRequests += 1;
  },
  settleRequest(result: IpcResult<unknown>): void {
    pendingRequests = Math.max(0, pendingRequests - 1);
    lastResult = result;
    if (!result.ok && result.error.kind === "unavailable") {
      availability = "unavailable";
    }
  },
};
