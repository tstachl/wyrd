// Copy and structure pins for the Add drive screen
// (`ProvisioningView.svelte`). The decision comes first, the default
// path is name plus relays, and every operator setting sits behind
// one Advanced disclosure. Reserved product terminology stays out
// of ordinary copy; the relay line follows the delivery contract
// (a relay accepting a message is not recipient receipt).
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const view = await Bun.file(`${import.meta.dir}/ProvisioningView.svelte`).text();

test("the decision is two large cards before any setting", () => {
  expect(view).toContain("Create a new drive");
  expect(view).toContain("Join with an invitation");
  expect(view).toContain("<ToggleGroup.Root");
  expect(view).toContain('value="create"');
  expect(view).toContain('value="join"');
  // A second click must restore the active card, not clear both.
  expect(view).toContain("bind:value={selectedMode}");
  expect(view).toContain("onValueChange={onModeChange}");
  // The credential selector is no longer the first surface.
  const decision = view.indexOf("Create a new drive");
  const selector = view.indexOf("<CredentialStorageToggle");
  expect(decision).toBeGreaterThan(-1);
  expect(selector).toBeGreaterThan(decision);
});

test("the default path is a name and relays", () => {
  expect(view).toContain('oninput={onNameInput}');
  expect(view).not.toContain("1–58 characters: lowercase letters, digits, and dashes");
  // Both tabs render the relay editor with the shared suggestion.
  expect(view).toContain('<RelayEditor id="create-relays"');
  expect(view).toContain('<RelayEditor id="join-relays"');
});

test("operator settings live behind one Advanced disclosure", () => {
  expect(view).toContain("<summary class=\"cursor-pointer text-sm font-medium\">Advanced</summary>");
  const advanced = view.slice(view.indexOf("<details"));
  for (const marker of [
    "Drive directory",
    "Choose folder…",
    "CredentialStorageToggle",
    "Credential entry",
  ]) {
    expect(advanced).toContain(marker);
  }
  // The directory input appears once, after the disclosure opens.
  expect(view).toContain('id="advanced-dir"');
});

test("the relay explanation names transport without promising receipt", () => {
  expect(view).toContain(
    "a relay accepting a message does not mean another device received it",
  );
  expect(view).not.toContain("Vault service mode");
  expect(view).not.toContain("managed drive");
});

test("retired internal wording is gone from rendered copy", () => {
  for (const retired of [
    "The keystore lives here",
    "Seeded from this device's verified identity",
    "cannot be seeded yet",
    "Vault service mode (platform default)",
    "Mount mode travels with the installer follow-up",
    "The drive appears in the list once the registry names it",
  ]) {
    expect(view).not.toContain(retired);
  }
});

test("the primary action explains why it is unavailable", () => {
  expect(view).toContain("To create a drive: {createBlockers.join");
  expect(view).toContain("To join a drive: {joinBlockers.join");
  expect(view).toContain("disabled={busy || createBlockers.length > 0}");
  expect(view).toContain("disabled={busy || joinBlockers.length > 0}");
});

test("there is one Back to drives in the page header", () => {
  expect(view.match(/Back to drives/g)?.length).toBe(1);
});
