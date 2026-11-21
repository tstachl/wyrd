// Wording pins for the per-session unlock card
// (`./UnlockView.svelte`). The locked state is stable and
// first-class — never a failure — so its copy pins here: the
// passphrase travels once and is cleared after every attempt.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const view = await Bun.file(`${import.meta.dir}/UnlockView.svelte`).text();
const toggle = await Bun.file(`${import.meta.dir}/credential-storage-toggle.svelte`).text();

test("locked state names the posture and both recoveries", () => {
  for (const line of [
    "Locked for this session",
    "this drive isn't running",
    "with its passphrase for this session, or use the recovery below",
  ]) {
    expect(view).toContain(line);
  }
  expect(view).not.toContain("no owner serves");
});

test("passphrase entry is typed-only and short-lived", () => {
  for (const line of [
    "Drive passphrase",
    "Typed only — never stored.",
    "Quitting the app leaves the drive running",
  ]) {
    expect(view).toContain(line);
  }
  expect(view).not.toContain("remember");
});

test("identity gate blocks the file fallback before it can run", () => {
  expect(view).toContain("finish the identity check first");
});

test("store unlock names the item reference, never a secret", () => {
  for (const line of ["OS credential store", "never a secret"]) {
    expect(view + toggle).toContain(line);
  }
  expect(view).not.toContain("passphrase file");
});
