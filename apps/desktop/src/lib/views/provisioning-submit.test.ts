// Wiring pin for the Add-drive form (`./ProvisioningView.svelte`):
// a store-mode create submits the derived account with the seeds,
// no file choosing. Exactly one mount per file: sibling interactive
// templates (the file-mode Browse buttons) break the happy-dom
// re-registration across mounts in this unpinned environment, so
// payload shapes pin in `../ipc/provisioning-args.test.ts` and this
// file proves the view calls through once.
//
// Run: `bun run test` from `apps/desktop/`.

import { afterEach, beforeEach, expect, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { tick } from "svelte";
import ProvisioningView from "./ProvisioningView.svelte";
import type { ProvisionCreateArgs, ProvisionJoinArgs } from "$lib/ipc/operations.js";
import type { ProvisionReport } from "$lib/ipc/results.js";

beforeEach(async () => {
  await GlobalRegistrator.register();
});

afterEach(async () => {
  cleanup();
  await GlobalRegistrator.unregister();
});

function report(): ProvisionReport {
  return {
    result: { result: "completed", detail: "ok" },
    observed: { status: "nofile", drives: [], skipped: [] },
    membership: null,
  };
}

test("store create submits the derived account with the seeds", async () => {
  const seen: ProvisionCreateArgs[] = [];
  const { getAllByLabelText, getByRole } = render(ProvisioningView, {
    props: {
      pending: false,
      initialIdentityFile: "/keys/identity",
      initialPassphraseFile: "/keys/passphrase",
      initialMode: "create",
      onCreate: async (args: ProvisionCreateArgs) => {
        seen.push(args);
        return report();
      },
      onJoin: async (_args: ProvisionJoinArgs) => report(),
      onDone: () => {},
      onBack: () => {},
    },
  });
  const nameEntry = getAllByLabelText("Drive name")[0];
  const dirEntry = getAllByLabelText("Drive directory")[0];
  if (nameEntry === undefined || dirEntry === undefined) {
    throw new Error("the Add-drive form did not render its name and directory entries");
  }
  await fireEvent.input(nameEntry, { target: { value: "alpha" } });
  await fireEvent.input(dirEntry, {
    target: { value: "/drives/alpha" },
  });
  await tick();
  await fireEvent.click(getByRole("button", { name: "Create drive" }));
  await tick();
  await tick();
  expect(seen).toEqual([
    {
      name: "alpha",
      driveDir: "/drives/alpha",
      relays: [],
      identityFile: "/keys/identity",
      passphraseFile: "/keys/passphrase",
      credentialBackend: "os-store",
      credentialAccount: "wyrd-drive-alpha",
      mode: "vault",
      noService: false,
    },
  ]);
});
