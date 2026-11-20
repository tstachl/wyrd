// Behavioral pin for the in-window Add-drive navigation
// (`./LifecycleView.svelte`). The tray's select-provision route is
// gone, so the header "Add drive" button calling `onAdd` is the only
// provisioning route — the App wires it to the provision view on
// the create tab. Mounts the real component under happy-dom and
// clicks the button: the callback must fire exactly once.
//
// Run: `bun run test` from `apps/desktop/`.

import { afterEach, beforeEach, expect, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { tick } from "svelte";
import LifecycleView from "./LifecycleView.svelte";
import type { RegistryState } from "$lib/ipc/results.js";
import type { Stamped } from "$lib/state/presentation.svelte.js";

beforeEach(async () => {
  await GlobalRegistrator.register();
});

afterEach(async () => {
  cleanup();
  await GlobalRegistrator.unregister();
});

function readySnapshot(): Stamped<RegistryState> {
  return {
    data: {
      status: "ready",
      drives: [
        {
          name: "alpha",
          drive_dir: "/drives/alpha",
          relays: [],
          service: "none",
          mode: "vault",
          mountpoint: null,
          availability: { state: "available" },
          service_state: { state: "unknown", detail: "probe" },
          enablement: { enablement: "unknown", detail: "probe" },
          session: null,
        },
      ],
      skipped: [],
    },
    observedAtMs: Date.now(),
  };
}

async function settle(): Promise<void> {
  await tick();
  await tick();
  await tick();
}

test("Add drive calls onAdd once, the only provisioning route", async () => {
  let calls = 0;
  const { getAllByRole } = render(LifecycleView, {
    props: {
      snapshot: readySnapshot(),
      selectedDriveDir: null,
      pending: false,
      transportNotice: null,
      onRefresh: () => {},
      onAdd: () => {
        calls += 1;
      },
      onOpen: (_driveDir: string) => {},
    },
  });
  await settle();
  const addButtons = getAllByRole("button", { name: "Add drive" });
  expect(addButtons.length).toBeGreaterThan(0);
  const addButton = addButtons[0];
  if (addButton === undefined) throw new Error("Add drive button missing");
  await fireEvent.click(addButton);
  await settle();
  expect(calls).toBe(1);
});
