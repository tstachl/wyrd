// Decision pins for skipping the drive overview with a single
// configured drive (`single-drive-select.ts`, wired in
// `App.svelte`). The overview only earns its keep with a choice
// to make: first load with one drive lands on Status, two or
// more park on the overview, and zero stay driveless. Off the
// overview the header shows the way back at two or more drives
// and the Add drive route at or below one; the add page always
// keeps its way back.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";
import {
  autoOpenTarget,
  headerAffordance,
  soleDriveDir,
} from "./single-drive-select.js";

function drives(n: number): { drive_dir: string }[] {
  return Array.from({ length: n }, (_, i) => ({ drive_dir: `/drives/d${i}` }));
}

test("sole drive is named only when exactly one is configured", () => {
  expect(soleDriveDir(drives(0))).toBeNull();
  expect(soleDriveDir(drives(1))).toBe("/drives/d0");
  expect(soleDriveDir(drives(2))).toBeNull();
});

test("first load with one drive advances to Status", () => {
  const target = autoOpenTarget(drives(1), null, "status");
  expect(target).toEqual({ driveDir: "/drives/d0", advanceToStatus: true });
});

test("first load with one drive from the add page selects without yanking the report", () => {
  const target = autoOpenTarget(drives(1), null, "provision");
  expect(target).toEqual({ driveDir: "/drives/d0", advanceToStatus: false });
});

test("first load with zero or two drives earns no auto-open", () => {
  expect(autoOpenTarget(drives(0), null, "status")).toBeNull();
  expect(autoOpenTarget(drives(2), null, "status")).toBeNull();
});

test("collapse to one while on the overview advances to Status", () => {
  const target = autoOpenTarget(drives(1), "/drives/gone", "lifecycle");
  expect(target).toEqual({ driveDir: "/drives/d0", advanceToStatus: true });
});

test("collapse to one off the overview repairs the selection without yanking the screen", () => {
  const target = autoOpenTarget(drives(1), "/drives/gone", "provision");
  expect(target).toEqual({ driveDir: "/drives/d0", advanceToStatus: false });
});

test("already open sole drive stays put outside the overview", () => {
  expect(autoOpenTarget(drives(1), "/drives/d0", "status")).toBeNull();
  expect(autoOpenTarget(drives(1), "/drives/d0", "settings")).toBeNull();
});

test("parked on the overview with the sole drive selected advances", () => {
  const target = autoOpenTarget(drives(1), "/drives/d0", "lifecycle");
  expect(target).toEqual({ driveDir: "/drives/d0", advanceToStatus: true });
});

test("header shows the way back at two or more drives, Add at or below one", () => {
  expect(headerAffordance("lifecycle", 2)).toBe("none");
  expect(headerAffordance("lifecycle", 1)).toBe("none");
  expect(headerAffordance("status", 2)).toBe("back");
  expect(headerAffordance("status", 1)).toBe("add");
  expect(headerAffordance("status", 0)).toBe("add");
  expect(headerAffordance("settings", 2)).toBe("back");
  expect(headerAffordance("settings", 1)).toBe("add");
});

test("the add page always keeps its way back", () => {
  expect(headerAffordance("provision", 0)).toBe("back");
  expect(headerAffordance("provision", 1)).toBe("back");
  expect(headerAffordance("provision", 2)).toBe("back");
});

test("App wires the count-driven header and the sole-drive home", async () => {
  const dir = `${import.meta.dir}`;
  const app = await Bun.file(`${dir}/../../App.svelte`).text();
  // The header follows the helper, with the frozen names only.
  expect(app).toContain("headerAffordance(presentation.selectedView, drives.length)");
  expect(app).toContain('headerAction === "back"');
  expect(app).toContain('headerAction === "add"');
  expect(app).toContain("Add drive");
  expect(app).not.toContain("All drives");
  // Back from anywhere with a single drive opens it directly.
  expect(app).toContain("soleDriveDir(registryDrives())");
});
