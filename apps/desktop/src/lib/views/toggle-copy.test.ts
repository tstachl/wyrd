// Wording pins for the per-drive auto-start toggle
// (`./UnavailableView.svelte`, `./SettingsView.svelte`). The toggle
// records a login-start preference and never implies the owner
// answered; the managed stop states keep their explicit Start.
// Terminology is frozen: "Start drive service at login" (R-D1).
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
