// Single-drive selection decisions (pure, no Svelte state).
//
// With exactly one configured drive the overview earns no keep, so
// first load lands on that drive's Status view and a mid-session
// collapse back to one drive while parked on the overview advances
// too. Two or more drives keep the overview; zero drives stay
// driveless (the provisioning empty state). Selection stays
// presentation state only: every drive-scoped command still carries
// the directory explicitly.
//
// Run: `bun run test` from `apps/desktop/`.

import type { ShellView } from "$lib/state/presentation.svelte.js";

export interface NamedDrive {
  drive_dir: string;
}

/** The sole drive's directory iff exactly one drive is configured. */
export function soleDriveDir<D extends NamedDrive>(drives: D[]): string | null {
  return drives.length === 1 && drives[0] !== undefined ? drives[0].drive_dir : null;
}

export interface AutoOpen {
  driveDir: string;
  advanceToStatus: boolean;
}

/**
 * Where a registry read leaves the selection when exactly one
 * drive is configured, or null where the count earns no
 * auto-open (zero, or two or more). The add page keeps the
 * screen where it owns it: the selection still repairs behind
 * it, but the view never yanks off an in-progress report.
 */
export function autoOpenTarget<D extends NamedDrive>(
  drives: D[],
  selectedDriveDir: string | null,
  selectedView: ShellView,
): AutoOpen | null {
  if (drives.length !== 1 || drives[0] === undefined) return null;
  const sole = drives[0].drive_dir;
  if (selectedDriveDir === null) {
    return { driveDir: sole, advanceToStatus: selectedView !== "provision" };
  }
  if (selectedDriveDir !== sole) {
    return { driveDir: sole, advanceToStatus: selectedView === "lifecycle" };
  }
  if (selectedView === "lifecycle") {
    return { driveDir: sole, advanceToStatus: true };
  }
  return null;
}

export type HeaderAffordance = "back" | "add" | "none";

/**
 * The header affordance off the overview. The overview is the
 * device home; with two or more drives the way back earns its
 * keep, while at or below one drive it navigates nowhere useful
 * and becomes the Add drive route instead. The add page always
 * keeps its way back — it is the exit from an in-progress form,
 * never a drive count statement.
 */
export function headerAffordance(
  selectedView: ShellView,
  driveCount: number,
): HeaderAffordance {
  if (selectedView === "lifecycle") return "none";
  if (selectedView === "provision") return "back";
  return driveCount >= 2 ? "back" : "add";
}
