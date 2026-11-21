// Wording pins for the frozen shell wording
// (`docs/design/product-terminology.md`, R-D1, R-D3, R-D4, R-D5,
// R-D8). Ordinary copy uses observable-state wording, the approved
// settings explanations, the device-removal explanation and the
// frozen navigation names — so the approved sentences pin here
// against the views that render them, and the retired strings pin
// absent.
//
// Source is comment-stripped and whitespace-normalized before
// pinning: comments are dev-facing, and sentences wrap across
// source lines. Pins cover rendered copy only.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

/** Rendered copy only: no comments, one space between words. */
function rendered(source: string): string {
  return source
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/\s+/g, " ");
}

const dir = `${import.meta.dir}`;
const app = rendered(await Bun.file(`${dir}/../../App.svelte`).text());
const lifecycle = rendered(
  await Bun.file(`${dir}/LifecycleView.svelte`).text(),
);
const provisioning = rendered(
  await Bun.file(`${dir}/ProvisioningView.svelte`).text(),
);
const credentialToggle = rendered(
  await Bun.file(`${dir}/credential-storage-toggle.svelte`).text(),
);
const onboarding = rendered(
  await Bun.file(`${dir}/OnboardingView.svelte`).text(),
);
const settings = rendered(
  await Bun.file(`${dir}/SettingsView.svelte`).text(),
);
const formatSrc = await Bun.file(`${dir}/format.ts`).text();
const unlock = rendered(await Bun.file(`${dir}/UnlockView.svelte`).text());
const unavailable = rendered(
  await Bun.file(`${dir}/UnavailableView.svelte`).text(),
);

test("background process uses observable-state wording", () => {
  for (const line of [
    "this drive isn't running",
    "The drive keeps its startup values until it restarts.",
    "Service restarted and the drive is answering",
    "The drive is not answering after the restart",
  ]) {
    expect(settings + app + unlock + unavailable).toContain(line);
  }
  for (const retired of [
    "running owner",
    "the owner is answering",
    "owner stops and starts",
    "owner will read them",
    "no owner serves",
  ]) {
    expect(settings + app + unlock).not.toContain(retired);
  }
});

test("settings header, tooltip and null bound use the approved explanations", () => {
  expect(settings).toContain(
    "These settings let you change the limits for this drive.",
  );
  expect(settings).toContain("About these limits");
  // The null-bound badge names the default; the helper in
  // `format.ts` renders it for values.
  expect(settings).toContain("Using Wyrd's default");
  expect(formatSrc).toContain("Using Wyrd's default");
  expect(settings).toContain(
    "The app cannot see that default value, so it does not show a number",
  );
  expect(settings).not.toContain("configured bounds, as configured");
  expect(settings).not.toContain("About editing bounds");
  expect(settings).not.toContain("unknown to the shell");
});

test("leave states the device-removal consequences before action", () => {
  for (const line of [
    "leaving removes this device from the drive going forward",
    "It does not erase copies this device already obtained",
    "hand over ownership before leaving if the drive should remain manageable",
  ]) {
    expect(settings).toContain(line);
  }
  expect(settings).not.toContain("freezes permanently");
  expect(settings).not.toContain("sole owner");
});

test("lock names the session consequence", () => {
  expect(app).toContain("unlocking needs the passphrase to open it again");
  expect(app).toContain("Quitting the app leaves the drive running");
});

test("navigation uses the frozen names", () => {
  expect(app).toContain("Back to drives");
  expect(app).not.toContain("All drives");
  expect(lifecycle).toContain(
    "Drives available on this device sync independently.",
  );
  expect(lifecycle).not.toContain("Registered on this device.");
  expect(provisioning).toContain('title="Add drive"');
  expect(provisioning).toContain("Back to drives");
  expect(provisioning).not.toContain("Add a drive");
  expect(provisioning).not.toContain("Sealed invitation");
});

test("in-window Add drive routes to the provision view", () => {
  // The header "Add drive" button fires `onAdd` (pinned behaviorally
  // in `lifecycle-add.test.ts`); the App wires that leg to the
  // provision view on the create tab. Both halves pin here so the
  // route survives without the removed backend event.
  expect(app).toContain('onAdd={() => openProvisioning("create")}');
  expect(app).toContain('function openProvisioning(mode: "create" | "join")');
  expect(app).toContain('presentation.selectView("provision")');
});

test("invitation carries the protection explanation", () => {
  expect(provisioning).toContain("Invitation file");
  expect(provisioning).toContain("Only the intended device can open it.");
});

test("new drives default to the OS credential store", () => {
  for (const line of [
    "OS credential store (recommended)",
    "Credential files (operator)",
    "no passphrase file is created",
  ]) {
    expect(provisioning + credentialToggle).toContain(line);
  }
  // The store item derives from the drive name; the label is a
  // reference, never a secret (the derivation itself pins in
  // `format.test.ts`).
  expect(provisioning).toContain("Store item");
});

test("onboarding follows the approved key explanation", () => {
  expect(onboarding).toContain(
    "A device is recognized by its public identity key.",
  );
  expect(onboarding).toContain("Identity key");
  expect(onboarding).not.toContain("establishes who this device is");
});
