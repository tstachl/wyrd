// Copy and contract pins for the app-wide diagnostics control
// (`./DiagnosticsCard.svelte`, `SettingsView.svelte`). T20 in
// `docs/contracts/trust.md` fixes the wording obligations: opt-in,
// local-only, never transmitted, detailed records may be sensitive,
// subsequent operations only, and default as the genuine absence
// state. The card must show the persisted level, never an optimistic
// override.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const card = await Bun.file(`${import.meta.dir}/DiagnosticsCard.svelte`).text();
const settings = await Bun.file(`${import.meta.dir}/SettingsView.svelte`).text();
const errors = await Bun.file(`${import.meta.dir}/../ipc/types.ts`).text();

test("the card states opt-in, local-only, and no transmission", () => {
  expect(card).toContain("This setting is opt-in and local-only.");
  expect(card).toContain("Wyrd does not send diagnostic records anywhere and");
  expect(card).toContain("does not upload them.");
  expect(card).toContain("Detailed records may be sensitive, so export needs a deliberate");
});

test("the card states subsequent-operations scope and keeps default errors", () => {
  expect(card).toContain("They apply to subsequent operations, not to");
  expect(card).toContain("Default error reporting keeps working when this is off.");
  expect(card).toContain("Default is the genuine absence state — no detailed records are kept.");
});

test("the card never claims an optimistic applied state", () => {
  expect(card).toContain("if (!result.ok) {");
  expect(card).toContain("if (result.value !== next) {");
  expect(card).toContain("if (savedLevel(observed) !== next) {");
  expect(card).toContain("const observed = await onRefresh();");
  expect(card).not.toContain('controlChecked = next === "detailed"');
});

test("the level comes from the persisted observation, not a live answer", () => {
  expect(card).toContain('persisted.level');
  expect(card).toContain('persisted.state === "nofile"');
  expect(card).toContain("snapshot: Stamped<Diagnostics> | null");
  expect(settings).toContain("snapshot={diagnosticsSnapshot}");
  expect(settings).toContain("onSet={onDiagnosticsSet}");
});

test("export is local, temporary, and distinct from sharing", () => {
  expect(card).toContain("Export a local bundle");
  expect(card).toContain("Collect the current local diagnostic records into one temporary file.");
  expect(card).toContain("Collect the current local diagnostic records into one temporary file. Wyrd does not send it");
  expect(card).toContain("anywhere. Sharing the file is a separate action you take yourself.");
  expect(card).toContain("onExport: () => Promise<DiagnosticsExport>");
});

test("the card names the signing cause instead of Finder's generic banner", () => {
  expect(card).toContain("Finder integration needs an App Group container");
  expect(card).toContain('finderIntegration === "unprovisioned"');
  expect(card).toContain("This build has no appex provisioning profile");
  expect(card).toContain("An ad-hoc extension cannot resolve the");
  expect(card).toContain("group container, so Finder can show a server or network error for a drive whose owner");
  expect(card).toContain("is running; the drive still works in Wyrd. The packaging README lists the signed-build");
  expect(card).toContain("options, with and without a profile. Reinstalling also restarts the drive agents for");
  expect(card).toContain('finderIntegration === "team-signed"');
  expect(card).toContain("The File Provider extension is team-signed without a provisioning profile");
  expect(settings).toContain("{finderIntegration}");
});

test("the four diagnostics refusals stay in the typed code set", () => {
  for (const code of [
    "InvalidDiagnosticLevel",
    "DiagnosticsSettings",
    "DiagnosticsMirror",
    "DiagnosticsFilter",
  ]) {
    expect(errors).toContain(`"${code}"`);
  }
});
