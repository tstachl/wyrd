// First-run gate classification (`./App.svelte`).
//
// The gate used to read only the shell's credential defaults, so a
// drive set up outside the GUI looked like a fresh device and invited
// a person to create a second identity. Discovery facts are the other
// input. This module keeps the decision pure so every branch —
// including the fail-closed branches — pins without mounting a Tauri
// app. It never verifies a credential or writes anything: `verify`
// means the caller still has to run the production hardened reader.

import type { ExternalDriveView, ExternalDrivesState, KeyDefaults } from "$lib/ipc/results.js";

export type FirstRunClass =
  | { state: "welcome" }
  | { state: "adopt"; drive: ExternalDriveView }
  | { state: "repair" }
  | { state: "unknown"; detail: string }
  | { state: "verify" };

/**
 * Classify the first-run gate from credential paths and discovery.
 * `unknown` is the fail-closed answer: no home directory, or a
 * discovery input that could not be read, never opens the gate as
 * `welcome` or `done`. A discovered drive replaces the welcome
 * invitation with an explicit adoption choice.
 */
export function classifyFirstRun(
  defaults: KeyDefaults,
  discovery: ExternalDrivesState,
): FirstRunClass {
  if (!defaults.usable) {
    return {
      state: "unknown",
      detail:
        "Could not determine this device's credential locations: no home directory resolves. Fix the environment and retry — nothing was written.",
    };
  }
  if (defaults.identity_exists && defaults.passphrase_exists) {
    return { state: "verify" };
  }
  if (discovery.status === "unreadable") {
    return {
      state: "unknown",
      detail: `Could not check for drives already on this device: ${discovery.reason}. Retry instead of creating a second identity.`,
    };
  }
  if (defaults.identity_exists || defaults.passphrase_exists) {
    return { state: "repair" };
  }
  // A live owner is the strongest evidence; a registered but stopped
  // drive still rules out the fresh-device invitation.
  const drive =
    discovery.drives.find((candidate) => candidate.reachable) ?? discovery.drives[0] ?? null;
  return drive === null ? { state: "welcome" } : { state: "adopt", drive };
}

/** Display text for the adoption banner; provenance stays explicit. */
export function describeAdoptedDrive(drive: ExternalDriveView): string {
  const origin =
    drive.source === "registry"
      ? `the drive registry (${drive.drive_dir ?? "path unavailable"})`
      : `a live File Provider alias (${drive.drive_id ?? "id unavailable"})`;
  return `Wyrd found "${drive.name}" through ${origin}. Choose the identity files that drive uses — no new identity is generated.`;
}
