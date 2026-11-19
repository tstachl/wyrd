// Wording pins for the frozen error and recovery wording
// (`docs/design/product-terminology.md`, R-D7). Error copy names the
// failure source — local refusal, wire skew, malformed answer — and
// never collapses unlike failures into one explanation, so the
// approved sentences pin here against the views and helpers that
// render them, and the retired strings pin absent.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const dir = `${import.meta.dir}`;
const app = await Bun.file(`${dir}/../../App.svelte`).text();
const errors = await Bun.file(`${dir}/../ipc/errors.ts`).text();
const format = await Bun.file(`${dir}/format.ts`).text();
const lifecycle = await Bun.file(`${dir}/LifecycleView.svelte`).text();
const status = await Bun.file(`${dir}/StatusView.svelte`).text();
// Cross-language pins: the wording matcher keys on Rust detail
// substrings, so read the producers (repo root is five levels up
// from this file). If backend wording drifts, the pin fails here
// instead of silently misrendering.
const repoRoot = `${dir}/../../../../../`;
const rustClient = await Bun.file(
  `${repoRoot}crates/wyrd-owner-client/src/client.rs`,
).text();
const rustDesktop = await Bun.file(
  `${repoRoot}crates/wyrd-desktop/src/lib.rs`,
).text();

test("credential and registry wording uses the approved sentences", () => {
  expect(app).toContain("Missing credentials:");
  expect(app).not.toContain("Incomplete credentials");
  expect(lifecycle).toContain("Some drives could not be loaded");
  expect(lifecycle).not.toContain("Skipped registry entries");
});

test("busy wording names running work, never the flight", () => {
  expect(app).toContain(
    "Another operation is already running on this drive. Wait for it to finish and try again.",
  );
  expect(format).toContain(
    "Another operation is already running on ${result.drive_dir}. Wait for it to finish and try again.",
  );
  expect(app).not.toContain("already in flight on this drive");
  expect(format).not.toContain("in flight on");
});

test("pending work carries the approved explanation", () => {
  // Whitespace-normalized: the sentence wraps across source lines.
  const flat = status.replace(/\s+/g, " ");
  expect(flat).toContain("Wyrd still has work to do for this drive.");
  expect(flat).toContain(
    "updates to share with other devices and content waiting to be transferred.",
  );
  // The explanation establishes no residency fact.
  expect(flat).not.toMatch(/work to do[^.]*missing/i);
  expect(flat).not.toContain("obtainable");
});

test("taxonomy rendering keeps unlike failures distinct", () => {  expect(errors).toContain("Wyrd refused this path:");
  expect(errors).toContain("The response used a different wire version:");
  expect(errors).toContain("The answer did not match the request:");
  expect(errors).toContain("The answer could not be read:");
  expect(errors).not.toContain("Incompatible owner");
  expect(errors).not.toContain("Incompatible drive service version");
});

test("rust detail markers still feed the wording matcher", () => {
  // `describeIncompatible` keys on these substrings, so the pin
  // reads the producers: if backend wording drifts, this fails
  // here instead of silently falling through to "could not be
  // read" for a local refusal. Synchronous reads — Bun.file(...).text()
  // above is module-level await; keep this test sync over cached text.
  expect(rustClient).toContain("owner answered wire version ");
  expect(rustClient).toContain("owner echoed id ");
  expect(rustClient).toContain("unrelated result");
  expect(rustClient).toContain("non-hex read bytes:");
  // The local refusal travels through the command boundary noun.
  expect(rustDesktop).toContain('"drive_dir"');
});