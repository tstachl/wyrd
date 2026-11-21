// Wiring pin for the Add-drive form (`./ProvisioningView.svelte`):
// a store-mode create submits the derived account with the seeds,
// no file choosing — then editing, clearing, and restoring the
// suggested relays saves exactly what is listed — then a caller
// advance (null) renders no report and re-reads nothing. Exactly
// one mount in this file (and no other file mounts this view):
// sibling interactive templates inline in the form break the
// happy-dom re-registration across mounts in this unpinned
// environment, so payload shapes pin in
// `../ipc/provisioning-args.test.ts` and this file proves the view
// calls through, sequentially, in a single mount.
//
// Run: `bun run test` from `apps/desktop/`.

import { afterEach, beforeEach, expect, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { tick } from "svelte";
import ProvisioningView from "./ProvisioningView.svelte";
import { SUGGESTED_RELAYS } from "./format.js";
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
  let doneCalls = 0;
  // The fourth submit advances: the caller navigated off the form,
  // so no report renders and no re-read fires here.
  let advance = false;
  const { getAllByLabelText, getAllByRole, getByRole, queryAllByText, queryByText } = render(ProvisioningView, {
    props: {
      pending: false,
      initialIdentityFile: "/keys/identity",
      initialPassphraseFile: "/keys/passphrase",
      initialMode: "create",
      onCreate: async (args: ProvisionCreateArgs) => {
        seen.push(args);
        if (advance) return null;
        return report();
      },
      onJoin: async (_args: ProvisionJoinArgs) => report(),
      onDone: () => {
        doneCalls += 1;
      },
      onBack: () => {},
    },
  });
  const nameEntry = getAllByLabelText("Drive name")[0];
  const dirEntry = getAllByLabelText("Drive directory")[0];
  if (nameEntry === undefined || dirEntry === undefined) {
    throw new Error("the Add-drive form did not render its name and directory entries");
  }
  // The relay field starts with the suggestion — one badge per
  // suggested relay, editable and clearable before saving. Both
  // tabs render (the inactive tab stays mounted), so presence —
  // not count — pins here.
  for (const relay of SUGGESTED_RELAYS) {
    expect(queryAllByText(relay).length).toBeGreaterThan(0);
  }
  await fireEvent.input(nameEntry, { target: { value: "alpha" } });
  await fireEvent.input(dirEntry, {
    target: { value: "/drives/alpha" },
  });
  await tick();
  await fireEvent.click(getByRole("button", { name: "Create drive" }));
  await tick();
  await tick();
  const fullSuggestion = {
    name: "alpha",
    driveDir: "/drives/alpha",
    relays: [...SUGGESTED_RELAYS],
    identityFile: "/keys/identity",
    passphraseFile: "/keys/passphrase",
    credentialBackend: "os-store",
    credentialAccount: "wyrd-drive-alpha",
    mode: "vault",
    noService: false,
  };
  expect(seen).toEqual([fullSuggestion]);

  // Remove one suggestion (each remove button renders twice — once
  // per tab — against the same shared state) and submit the edited
  // list: the drive keeps the edited choice.
  const removeOffchain = getAllByRole("button", { name: "Remove wss://offchain.pub" });
  expect(removeOffchain.length).toBeGreaterThan(0);
  await fireEvent.click(removeOffchain[0]!);
  await tick();
  expect(queryAllByText("wss://offchain.pub").length).toBe(0);
  await fireEvent.click(getByRole("button", { name: "Create drive" }));
  await tick();
  await tick();
  expect(seen.length).toBe(2);
  expect(seen[1]!.relays).toEqual(
    SUGGESTED_RELAYS.filter((relay) => relay !== "wss://offchain.pub"),
  );

  // Clearing the rest surfaces the restore affordance.
  for (const relay of SUGGESTED_RELAYS.filter((entry) => entry !== "wss://offchain.pub")) {
    const remove = getAllByRole("button", { name: `Remove ${relay}` });
    await fireEvent.click(remove[0]!);
    await tick();
  }
  for (const relay of SUGGESTED_RELAYS) {
    expect(queryAllByText(relay).length).toBe(0);
  }
  const restore = getAllByRole("button", { name: "Restore suggested relays" });
  expect(restore.length).toBeGreaterThan(0);

  // Restoring brings the suggestion back; submitting saves it whole.
  await fireEvent.click(restore[0]!);
  await tick();
  for (const relay of SUGGESTED_RELAYS) {
    expect(queryAllByText(relay).length).toBeGreaterThan(0);
  }
  await fireEvent.click(getByRole("button", { name: "Create drive" }));
  await tick();
  await tick();
  expect(seen.length).toBe(3);
  expect(seen[2]).toEqual(fullSuggestion);
  expect(doneCalls).toBe(3);

  // The caller advancing (null) renders no report and re-reads
  // nothing here — the refresh already ran before navigation.
  advance = true;
  await fireEvent.click(getByRole("button", { name: "Create drive" }));
  await tick();
  await tick();
  expect(seen.length).toBe(4);
  expect(doneCalls).toBe(3);
  expect(queryByText("Provisioning report")).toBeNull();
});
