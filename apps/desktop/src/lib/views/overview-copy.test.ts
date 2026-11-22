// Copy and structure pins for the Drives overview
// (`LifecycleView.svelte` plus its extracted `drive-card.svelte`).
// The card leads with observable run state, one line of separate facts
// and the folder name; operator mode and service chips, a relay URL
// list and the repeated "Open to inspect" sentence are gone.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const view = await Bun.file(`${import.meta.dir}/LifecycleView.svelte`).text();
const card = await Bun.file(`${import.meta.dir}/drive-card.svelte`).text();
const app = await Bun.file(`${import.meta.dir}/../../App.svelte`).text();

test("the card leads with observable run state, not an Available pill", () => {
  expect(card).toContain("{driveRunState(facts)}");
  expect(view + card).not.toContain("AvailabilityBadge");
  expect(card).not.toContain("{drive.mode}");
  expect(card).not.toContain("service {drive.service}");
});

test("one line of facts replaces the chip wall", () => {
  expect(card).toContain("{driveFactLine(facts, drive.relays.length)}");
  expect(card).toContain("{drive.relays.length === 1 ? \"1 relay\" : `${drive.relays.length} relays`}");
  expect(card).not.toContain("{#each drive.relays as relay (relay)}");
});

test("the folder name and a reveal link replace the truncated path", () => {
  expect(card).toContain("{folderNameFromPath(drive.drive_dir)}");
  expect(card).toContain("Show in Finder");
  expect(card).not.toContain("{drive.drive_dir}</span>");
  expect(view + card).not.toContain('label="drive path"');
  expect(app).toContain("onReveal={reveal}");
});

test("the whole card opens the drive; the button names the action", () => {
  expect(card).toContain('role="button"');
  expect(card).toContain("onclick={onOpen}");
  expect(view + card).not.toContain("Open to inspect this drive.");
});

test("attention drives sort first", () => {
  expect(view).toContain("sortDrivesByAttention(drives, factsFor)");
  expect(view).toContain("{#each orderedDrives as drive (drive.drive_dir)}");
});

test("the App supplies the last Status answer per drive", () => {
  expect(app).toContain("statusFor={(driveDir) => presentation.statusSnapshotFor(driveDir)}");
});
