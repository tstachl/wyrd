// Wording pins for the per-session unlock card
// (`./UnlockView.svelte`). The locked state is stable and
// first-class — never a failure — so its copy pins here: the
// passphrase travels once and is cleared after every attempt.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const view = await Bun.file(`${import.meta.dir}/UnlockView.svelte`).text();

test("locked state names the posture and both recoveries", () => {
  for (const line of [
    "Locked for this session",
    "no owner serves and no service",
    "Unlock it with its passphrase, or reinstall the managed definition",
  ]) {
    expect(view).toContain(line);
  }
});

test("passphrase entry is typed-only and short-lived", () => {
  for (const line of [
    "Drive passphrase",
    "Typed only — never stored.",
    "quitting the app locks it",
  ]) {
    expect(view).toContain(line);
  }
  expect(view).not.toContain("remember");
});

test("identity gate blocks the form before it can run", () => {
  expect(view).toContain("finish the identity check first");
});
