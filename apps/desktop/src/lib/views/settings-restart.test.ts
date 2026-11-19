// Wording pins for the settings restart-to-apply banner
// (`./SettingsView.svelte`). Saving and applying stay separate
// outcomes: the banner names the per-posture next step, the
// managed restart runs behind a confirmation that states the
// interruption, and the restart verdict never claims application —
// the post-restart observation decides that.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const view = await Bun.file(`${import.meta.dir}/SettingsView.svelte`).text();

test("saved banner separates saving from applying", () => {
  for (const line of [
    "Bounds saved — restart to apply",
    "Bounds saved — lock and unlock to apply",
    "Bounds saved. They apply the next time the drive starts",
  ]) {
    expect(view).toContain(line);
  }
  expect(view).not.toContain("Restart the drive to apply them");
});

test("managed restart confirms the interruption first", () => {
  for (const line of [
    "Restart the service?",
    "briefly interrupts availability",
    "The bounds you saved apply when it comes back",
  ]) {
    expect(view).toContain(line);
  }
});

test("restart verdict never claims application", () => {
  for (const line of [
    "Service restarted and the owner is answering — the saved values apply from this start.",
    "Saved, but the restart failed:",
    "the saved values wait for the next start",
  ]) {
    expect(view).toContain(line);
  }
  expect(view).not.toContain("successfully applied");
});

test("session path never asks the shell to hold the passphrase", () => {
  expect(view).toContain("unlocking with the passphrase starts it again");
  expect(view).not.toContain("remember");
});
