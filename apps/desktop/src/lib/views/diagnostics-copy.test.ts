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
