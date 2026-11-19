// Wording pins for the frozen status terminology
// (`docs/design/product-terminology.md`, R-D2 and R-D6).
// Card and row labels are a contract with the acceptance criterion
// — a person reads these dimensions — so the approved names pin
// here against `StatusView.svelte`, and retired names pin absent.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const view = await Bun.file(`${import.meta.dir}/StatusView.svelte`).text();

test("status cards carry the frozen dimension names", () => {
  for (const title of ["Sync activity", "Drive activity", "Drive data"]) {
    expect(view).toContain(title);
  }
  expect(view).not.toContain("Durable observation");
  expect(view).not.toContain(">Liveness<");
  expect(view).not.toContain(">Freshness<");
});

test("status rows carry the frozen row names", () => {
  for (const label of [
    'label="Pending work"',
    'label="Local files"',
    'label="Published on last pass"',
  ]) {
    expect(view).toContain(label);
  }
  expect(view).not.toContain('label="Queue"');
  expect(view).not.toContain('label="Materialized"');
  expect(view).not.toContain('label="Last publish"');
});

test("paused names the state without the quiet/down gloss", () => {
  expect(view).toContain("Synchronization is paused.");
  expect(view).toContain("The drive is still running and can answer requests.");
  expect(view).not.toContain("reachable and quiet");
  expect(view).not.toContain("quiet, not down");
});

test("technical detail lives behind a disclosure", () => {
  expect(view).toContain("Show technical detail");
  expect(view).toContain("Hide technical detail");
});
