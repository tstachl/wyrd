// Copy and shape pins for the settings empty states
// (`SettingsView.svelte`). Every observation state now renders the
// same five-field form: an unconfigured drive shows "Using Wyrd's
// default" per field rather than an edit-this-JSON prompt. The file
// shape and path live behind an Advanced disclosure and carry real
// configured values only — never sample numbers that look like a
// recommended default. The Rust payload
// (`DriveSettings::{Configured,Nofile,Nosection}`) is the source of
// truth; these pins only hold the display end of it.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const view = await Bun.file(`${import.meta.dir}/SettingsView.svelte`).text();

test("the unread empty state keeps the original visual rhythm", () => {
  expect(view).toContain("<Empty.Root");
  expect(view).toContain("bg-card/50 px-6 py-14");
  expect(view).toContain('<Empty.Media variant="icon" class="size-10">');
});

test("one form serves every observation state", () => {
  // No per-state editor toggles: configured, nofile and nosection
  // all fall through the same fields, and the old JSON prompt is gone.
  expect(view).toContain("{#each SETTING_BOUNDS as bound (bound.key)}");
  expect(view).not.toContain("No settings file yet");
  expect(view).not.toContain("No section for this drive");
  expect(view).not.toContain("Set bounds for this drive");
  expect(view).not.toContain("Edit bounds");
});

test("absent values read as Wyrd's default with a per-field reset", () => {
  expect(view).toContain('placeholder="Using Wyrd\'s default"');
  expect(view).toContain("Using Wyrd's default</Badge>");
  expect(view).toContain("Use Wyrd's default");
  expect(view).not.toContain("Using default</Badge>");
  expect(view).not.toContain("compiled defaults");
});

test("when changes take effect is stated on the form", () => {
  expect(view).toContain(
    "Changes take effect the next time this drive starts — they don't change a running drive.",
  );
});

test("the file JSON is behind an Advanced disclosure with no sample values", () => {
  expect(view).toContain("Advanced: edit the settings file directly");
  expect(view).toContain("<details");
  expect(view).toContain("{advancedJson}");
  // The disclosure shows only keys this drive actually configured,
  // filtered from the live observation — the old literal sample
  // object (and its 64/30/120/45 values) is gone.
  expect(view).not.toContain("emptyExample");
  expect(view).not.toContain("1073741824");
  expect(view).not.toContain("120,");
  expect(view).toContain(".filter(([, value]) => value !== null)");
  expect(view).toContain("Every key is optional — leave one out to use Wyrd's default.");
});

test("the resolved file path stays visible where the observation carries it", () => {
  expect(view).toContain('const settingsPath = $derived(');
  expect(view).toContain('"path" in settings ? settings.path : null');
  expect(view).toContain("{settingsPath}");
  expect(view).toContain('label="settings file path"');
});
