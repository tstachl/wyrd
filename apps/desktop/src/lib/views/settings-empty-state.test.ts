// Copy and shape pins for the settings empty states
// (`SettingsView.svelte`). Both `nofile` and `nosection` name the
// resolved file path (with a copy button) and show the section
// shape to create — rendered from the live drive dir with every
// key optional — while keeping the editor entry point. The Rust
// payload (`DriveSettings::{Nofile,Nosection} { path }`) is the
// source of truth; these pins only hold the display end of it.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const view = await Bun.file(`${import.meta.dir}/SettingsView.svelte`).text();

test("both empty states name the resolved file path with a copy button", () => {
  // One pin per block: the path must render inside each branch,
  // not just somewhere in the file.
  const nofile = view.slice(
    view.indexOf('settings.state === "nofile"'),
    view.indexOf('settings.state === "nosection"'),
  );
  expect(nofile).toContain("{settings.path}");
  expect(nofile).toContain('label="settings file path"');
  const nosection = view.slice(view.indexOf('settings.state === "nosection"'));
  expect(nosection).toContain("{settings.path}");
  expect(nosection).toContain('label="settings file path"');
});

test("both empty states show the section shape with the live drive dir", () => {
  // The example is derived from the drive on screen, so the key
  // the file needs is the key shown — never a hardcoded drive.
  // (Quoted `"version"` only exists at runtime via JSON.stringify;
  // the source holds the object literal.) All five bound keys are
  // present: the issue scope asks for the schema, not one key.
  expect(view).toContain("[driveDir]");
  expect(view).toContain("version: 1");
  for (const key of [
    "retained_bytes_quota",
    "max_admit_per_pass",
    "fetch_pass_budget_secs",
    "max_mutation_wait_secs",
    "open_timeout_secs",
  ]) {
    expect(view).toContain(key);
  }
  expect(view).toContain("{emptyExample}");
  expect(view.match(/\{emptyExample\}/g)?.length).toBe(2);
});

test("the shape is honest about optionality and keeps the editor entry", () => {
  // Values read as starting points, never recommendations: the copy
  // says to keep only wanted lines with the operator's own values.
  expect(view).toContain("every key is optional");
  expect(view).toContain("put in your own values");
  expect(view).toContain("Set bounds for this drive");
});
