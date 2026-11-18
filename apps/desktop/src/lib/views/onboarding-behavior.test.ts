// Behavioral pins for the wizard write-down proof
// (`./OnboardingView.svelte`, create branch). Beyond the copy pins
// in `onboarding-copy.test.ts`: Finish refuses before `onCommit`
// where the re-typed copy differs from the effective passphrase,
// and commits the pair where it matches. Mounts the real component
// under happy-dom through the test-setup Svelte loader.
//
// Run: `bun run test` from `apps/desktop/`.

import { afterEach, beforeEach, expect, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { tick } from "svelte";
import OnboardingView from "./OnboardingView.svelte";
import type { OnboardingReport } from "$lib/ipc/results.js";

beforeEach(async () => {
  await GlobalRegistrator.register();
});

afterEach(async () => {
  cleanup();
  await GlobalRegistrator.unregister();
});

const GENERATED = "correct horse battery staple wire";
const DEFAULTS = {
  identity_file: "/k/identity",
  passphrase_file: "/k/passphrase",
  identity_exists: false,
  passphrase_exists: false,
  usable: true,
};

function completed(passphrase: string | null = null): OnboardingReport {
  return {
    result: { result: "completed", detail: "ok", passphrase },
    defaults: { ...DEFAULTS, identity_exists: true, passphrase_exists: true },
  };
}

async function settle(): Promise<void> {
  await tick();
  await tick();
  await tick();
}

test("Finish refuses on a mismatched copy and commits on a match", async () => {
  const commits: Array<{ identityFile: string; passphraseFile: string; passphrase: string }> = [];
  const { getByRole, getByLabelText, getByText, queryByText } = render(OnboardingView, {
    props: {
      defaults: DEFAULTS,
      pending: false,
      onNewPassphrase: async () => completed(GENERATED),
      onCommit: async (args: {
        identityFile: string;
        passphraseFile: string;
        passphrase: string;
      }) => {
        commits.push(args);
        return completed(null);
      },
      onVerify: async () => completed(null),
      onAdopt: async () => completed(null),
      onReveal: async (_path: string) => true,
      onDone: (_identityFile: string, _passphraseFile: string, _mode: "create" | "join") => {},
      onQuit: () => {},
    },
  });

  // Identity -> Passphrase -> Recovery: the create branch generates
  // on the way through.
  await fireEvent.click(getByRole("button", { name: "Continue" }));
  await settle();
  await fireEvent.click(getByRole("button", { name: "Continue" }));
  await settle();
  getByText("Plan for losing this device");

  // Acknowledge without proving the copy: Finish refuses before any
  // commit, and names the stored copy as the source of truth.
  await fireEvent.click(getByRole("checkbox"));
  await settle();
  await fireEvent.click(getByRole("button", { name: "Finish" }));
  await settle();
  expect(commits).toEqual([]);
  getByText("That does not match the passphrase — check your stored copy and re-type it.");

  // A wrong copy refuses the same way.
  const confirm = getByLabelText("Confirm your passphrase") as HTMLInputElement;
  await fireEvent.input(confirm, { target: { value: "some other words entirely" } });
  await settle();
  await fireEvent.click(getByRole("button", { name: "Finish" }));
  await settle();
  expect(commits).toEqual([]);

  // The stored copy re-typed: Finish commits the pair.
  await fireEvent.input(confirm, { target: { value: GENERATED } });
  await settle();
  await fireEvent.click(getByRole("button", { name: "Finish" }));
  await settle();
  expect(commits).toEqual([
    {
      identityFile: "/k/identity",
      passphraseFile: "/k/passphrase",
      passphrase: GENERATED,
    },
  ]);
  // The commit lands the ready stage; the secrets leave the form.
  getByText("This device is ready");
  expect(queryByText("That does not match the passphrase — check your stored copy and re-type it.")).toBeNull();
});
