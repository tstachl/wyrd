// Disclosure audit fixtures for first-run identity onboarding.
// The load-bearing strings are asserted verbatim against the view
// sources: sections 2-3 copy per the frozen v5 design, the either/or
// rule, the on-device / another-device split, and the security
// boundary (no copy may imply the remembered passphrase
// authenticates anyone or stops same-user attackers).
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const view = await Bun.file(`${import.meta.dir}/OnboardingView.svelte`).text();
const repair = await Bun.file(`${import.meta.dir}/RepairView.svelte`).text();

test("mental model states the two parts and the device split", () => {
  for (const line of [
    "This device's identity has two parts.",
    "Identity key",
    "Unlock passphrase",
    "You need both",
    "On this device:",
    "Wyrd remembers your passphrase",
    "accessible to your user account",
    "Full-disk encryption and your login are the actual protection here.",
    "On another device:",
  ]) {
    expect(view).toContain(line);
  }
});

test("identity stage names choices without a nudge", () => {
  for (const line of [
    "Welcome to Wyrd",
    "This device needs its own identity to join drives.",
    "Create a new identity for this device.",
    "Restore from backup",
  ]) {
    expect(view).toContain(line);
  }
  expect(view).not.toContain("Recommended");
});

test("restore branch names cloning as one device", () => {
  for (const line of [
    "Choose the files belonging to the identity",
    "A second device that stays in use needs its own identity",
  ]) {
    expect(view).toContain(line);
  }
});

test("passphrase stage states the floor and the remembered rule", () => {
  for (const line of [
    "Choose a passphrase",
    "Generate one for me",
    "Choose my own",
    "This device will remember it",
    "12 characters is the floor; longer is better.",
    "generated locally on this device",
    "your stored copy is the backup, not the clipboard",
  ]) {
    expect(view).toContain(line);
  }
});

test("recovery is two layers with the either/or rule", () => {
  for (const line of [
    "Plan for losing this device",
    "Wyrd cannot recover this device's identity for you.",
    "To restore it, you need a backup",
    "If you lose either one, Wyrd cannot restore your identity.",
    "Keep your identity files and passphrase in separate places.",
    "created when you finish",
    "what restore does and doesn't cover",
  ]) {
    expect(view).toContain(line);
  }
  // "Lose both" logic fails the fixture: losing *either* is fatal.
  expect(view).not.toContain("If you lose both");
});

test("acknowledgement is a single checkbox with the comprehension statement", () => {
  expect(view).toContain(
    "I've stored my passphrase somewhere safe, and I understand Wyrd cannot recover this device's identity if I lose my identity-file backup or my passphrase.",
  );
});

test("ready pairs create and join with actual file locations", () => {
  for (const line of [
    "This device is ready",
    "Your device has its own Wyrd identity.",
    "Start a private collection",
    "Join with an invitation",
    "Join a drive someone has shared",
    "Stored on this device",
  ]) {
    expect(view).toContain(line);
  }
  // No recovery/backup status is rendered — backup is acknowledged,
  // never verified.
  expect(view).not.toContain("Backup complete");
  expect(view).not.toContain("backup verified");
});

test("security boundary never overpromises the remembered credential", () => {
  for (const banned of [
    "only you can access",
    "protects against malware",
    "secure against other apps",
    "authenticates you",
  ]) {
    expect(view.toLowerCase()).not.toContain(banned);
  }
  expect(view).toContain("anyone or anything with your login's file access can use them");
});

test("repair states preservation and consequence plainly", () => {
  for (const line of [
    "Wyrd cannot use this identity file",
    "Create a new identity instead?",
    "your old file will be kept where it is",
    "Drives that trusted the old identity will not recognize this one",
    "This cannot be undone.",
  ]) {
    expect(repair).toContain(line);
  }
  // No silent overwrite path: the word never appears as an action.
  expect(repair).not.toContain("Overwrite");
});
