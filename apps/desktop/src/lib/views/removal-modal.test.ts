// Structure and copy pins for the Remove this drive modal
// (`SettingsView.svelte`). The strong consequence copy stays verbatim;
// it is reorganised into two alternatives with three short lines each,
// and the destructive alternative adds type-the-name confirmation.
// No sole-owner claim is shown as a warning: that fact is not observed.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const source = await Bun.file(`${import.meta.dir}/SettingsView.svelte`).text();
/** Rendered copy only: dev comments cannot satisfy a wording pin. */
const view = source
  .replace(/<!--[\s\S]*?-->/g, "")
  .replace(/^\s*\/\/.*$/gm, "");

test("the modal offers two alternatives with three consequence lines each", () => {
  expect(view).toContain("Keep local data");
  expect(view).toContain("Remove local data too");
  for (const line of [
    "<strong class=\"text-foreground\">On this device.</strong>",
    "<strong class=\"text-foreground\">For other devices.</strong>",
    "<strong class=\"text-foreground\">Can it be undone.</strong>",
  ]) {
    expect(view.match(new RegExp(line.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "g"))?.length).toBe(2);
  }
});

test("the checkbox paragraph is replaced by type-the-name confirmation", () => {
  expect(view).not.toContain('id="removal-acknowledged"');
  expect(view).not.toContain("I understand:");
  expect(view).toContain("Type the drive name to confirm");
  expect(view).toContain('id="remove-confirm-name"');
  expect(view).toContain('disabled={busy || driveName === null || deleteConfirmName !== driveName}');
  expect(view).toContain('disabled={busy || driveName === null}');
  expect(view).toMatch(/Remove local data\s*<\/AlertDialog\.Action>/);
  expect(view).toContain("deleteConfirmName = \"\"");
});

test("the two alternatives are not a sequence", () => {
  expect(view).toContain("void remove(true)");
  expect(view).toContain("void remove(false)");
  expect(view).toContain("acknowledged: true");
});

test("internal store wording is gone from the removal card", () => {
  expect(view).not.toContain("Reads this drive's store item");
  expect(view).not.toContain("derive its store item");
  expect(view).not.toContain("Operator deployments only: reads the verified identity pair below.");
});

test("the sole-owner consequence stays conditional, not a warning claim", () => {
  expect(view).toContain("If this device is the drive's only owner, the owner set becomes empty");
  const titles =
    view.match(/<(?:Alert|AlertDialog)\.Title>[\s\S]*?<\/(?:Alert|AlertDialog)\.Title>/g) ?? [];
  for (const title of titles) {
    expect(title).not.toContain("only owner");
  }
});

test("the strong consequence clauses survive the restructure", () => {
  for (const clause of [
    "removing this drive ends this device's membership going forward",
    "It does not erase copies this device already obtained",
    "takes the drive's local data off the device",
    "registry entry, service definition, credential item and group socket and label",
    "No other member's copy changes",
    "blocks stay recoverable from this device until overwritten",
    "the drive's service is stopped for this action and restarted if the removal is",
  ]) {
    expect(view).toContain(clause);
  }
});
