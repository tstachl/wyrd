// Behavioral pin for the external-drive adoption entry
// (`./OnboardingView.svelte`). A drive found outside the shell must
// not invite a second identity: the create choice and the add-drive
// actions are absent, the adoption branch is preselected, and the
// chosen credential paths go through the existing verify/adopt
// boundary rather than a new identity commit.
//
// Run: `bun run test` from `apps/desktop/`.

import { afterEach, beforeEach, expect, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { tick } from "svelte";
import OnboardingView from "./OnboardingView.svelte";
import type { ExternalDriveView, OnboardingReport } from "$lib/ipc/results.js";

beforeEach(async () => {
  await GlobalRegistrator.register();
});

afterEach(async () => {
  cleanup();
  await GlobalRegistrator.unregister();
});

const DEFAULTS = {
  identity_file: "/k/identity",
  passphrase_file: "/k/passphrase",
  identity_exists: false,
  passphrase_exists: false,
  usable: true,
};

const EXTERNAL: ExternalDriveView = {
  name: "outside",
  source: "alias",
  drive_dir: null,
  drive_id: "ab".repeat(32),
  reachable: true,
};

function completed(): OnboardingReport {
  return {
    result: { result: "completed", detail: "ok", passphrase: null },
    defaults: { ...DEFAULTS, identity_exists: true, passphrase_exists: true },
  };
}

async function settle(): Promise<void> {
  await tick();
  await tick();
  await tick();
}

test("an external drive preselects adoption and never offers a new identity", async () => {
  const verified: Array<{ identityFile: string; passphraseFile: string }> = [];
  const committed: unknown[] = [];
  const { getByRole, getByLabelText, getByText, queryByText } = render(OnboardingView, {
    props: {
      defaults: DEFAULTS,
      pending: false,
      externalDrive: EXTERNAL,
      onNewPassphrase: async () => completed(),
      onCommit: async (args) => {
        committed.push(args);
        return completed();
      },
      onVerify: async (args) => {
        verified.push(args);
        return completed();
      },
      onAdopt: async () => completed(),
      onReveal: async (_path: string) => true,
      onDone: () => {},
      onQuit: () => {},
    },
  });
  await settle();

  getByText("Adopt a drive on this device");
  getByText(/Wyrd found/);
  getByText((text) => text.includes("live File Provider alias"));
  getByText((text) => text.includes("no new identity is generated"));
  expect(queryByText("Create a new identity")).toBeNull();
  expect(queryByText("Create a drive")).toBeNull();
  expect(queryByText("Join with an invitation")).toBeNull();
  const restore = getByRole("radio", { name: /Restore from backup/ });
  expect(restore.getAttribute("aria-checked")).toBe("true");

  await fireEvent.input(getByLabelText("Identity file"), {
    target: { value: "/outside/identity.bin" },
  });
  await fireEvent.input(getByLabelText("Passphrase file"), {
    target: { value: "/outside/passphrase.txt" },
  });
  await settle();
  await fireEvent.click(getByRole("button", { name: "Continue" }));
  await settle();

  // The existing verify/adopt boundary receives the chosen pair;
  // nothing commits a fresh identity.
  expect(verified).toEqual([
    { identityFile: "/outside/identity.bin", passphraseFile: "/outside/passphrase.txt" },
  ]);
  expect(committed).toEqual([]);
});
