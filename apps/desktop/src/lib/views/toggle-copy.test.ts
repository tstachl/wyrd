// Wording pins for the per-drive auto-start toggle
// (`./UnavailableView.svelte`, `./SettingsView.svelte`). The toggle
// records a login-start preference and never implies the owner
// answered; the managed stop states keep their explicit Start.
// Terminology is frozen: both surfaces use "Start drive service at
// login" for a drive with a definition (desktop-lifecycle.md, R-D1).
// The Settings card additionally describes the outcome; the label
// itself may not drift.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const unavailable = await Bun.file(`${import.meta.dir}/UnavailableView.svelte`).text();
const settings = await Bun.file(`${import.meta.dir}/SettingsView.svelte`).text();

test("both toggle surfaces use the frozen label", () => {
  for (const view of [unavailable, settings]) {
    expect(view).toContain("Start drive service at login");
  }
});

test("the Settings card describes the outcome, not the mechanism", () => {
  // The issue asked to rewrite the description carrying bare
  // owner/manager and the login-start record detail. The label above
  // stays frozen; only the prose beneath it changes.
  expect(settings).toContain(
    "Start this drive automatically when you log in. Turning this off doesn't delete anything.",
  );
  expect(settings).not.toContain("The per-drive owner service the manager starts at login");
  expect(settings).not.toContain("login-start record for this drive reads");
});

test("the toggle never claims to start anything", () => {
  for (const view of [unavailable, settings]) {
    expect(view).not.toContain("will start the service");
    expect(view).not.toContain("starts now");
  }
  expect(unavailable).toContain("a disabled drive stays stopped");
  expect(unavailable).toContain("until started manually");
});

test("the unavailable toggle hides on definition-less and unanswered states", () => {
  expect(unavailable).toContain(
    '(serviceState.state === "stopped" || serviceState.state === "wedged") &&\n      onToggleEnablement !== undefined',
  );
  expect(unavailable).toContain("a session-posture drive has no unit to");
});

test("the settings toggle renders only for managed drives", () => {
  expect(settings).toContain("session === null && onToggleEnablement !== undefined");
});
