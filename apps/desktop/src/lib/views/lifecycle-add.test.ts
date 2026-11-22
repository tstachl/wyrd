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

test("every drive card shows the same Open action and a bottom-aligned footer", async () => {
  const snapshot = readySnapshot();
  const alpha = snapshot.data.drives[0];
  if (alpha === undefined) throw new Error("fixture drive missing");
  snapshot.data.drives.push({
    ...alpha,
    name: "beta",
    drive_dir: "/drives/beta",
    relays: [
      "wss://relay.example/a",
      "wss://relay.example/b",
      "wss://relay.example/c",
      "wss://relay.example/d",
      "wss://relay.example/e",
    ],
  });

  const { getAllByRole, getAllByText, queryByText } = render(LifecycleView, {
    props: {
      snapshot,
      pending: false,
      transportNotice: null,
      onRefresh: () => {},
      onAdd: () => {},
      onOpen: (_driveDir: string) => {},
    },
  });
  await settle();

  // The overview does not present selection: no card gets a different
  // label, badge or footer because a drive was selected elsewhere.
  expect(getAllByRole("button", { name: "Open" }).length).toBe(2);
  expect(queryByText("Select & open")).toBeNull();
  expect(queryByText("Selected for Status and Settings.")).toBeNull();
  expect(queryByText("Open to inspect this drive.")).toBeNull();
  // Folder names replace truncated paths; the whole card is clickable,
  // but the Open button stays the single labelled action per card.
  expect(getAllByText("alpha").length).toBeGreaterThan(0);
  expect(getAllByText("beta").length).toBeGreaterThan(0);
  expect(queryByText("/drives/alpha")).toBeNull();

  // Cards share a grid row, so the footer needs an auto top margin to
  // stay at the bottom when one drive lists more relays than another.
  const footers = document.querySelectorAll('[data-slot="card-footer"]');
  expect(footers.length).toBe(2);
  for (const footer of footers) {
    expect(footer.className).toContain("mt-auto");
  }
});

test("the whole card opens the drive and Show in Finder reveals its folder", async () => {
  const seen: { opened: string | null; revealed: string | null } = {
    opened: null,
    revealed: null,
  };
  const { getAllByRole } = render(LifecycleView, {
    props: {
      snapshot: readySnapshot(),
      pending: false,
      transportNotice: null,
      statusFor: () => null,
      onReveal: async (path: string) => {
        seen.revealed = path;
        return true;
      },
      onRefresh: () => {},
      onAdd: () => {},
      onOpen: (driveDir: string) => {
        seen.opened = driveDir;
      },
    },
  });
  await settle();
  const finder = getAllByRole("button", { name: "Show in Finder" })[0];
  if (finder === undefined) throw new Error("Show in Finder missing");
  await fireEvent.click(finder);
  await settle();
  expect(seen.revealed).toBe("/drives/alpha");
  // Revealing a folder must not also open the drive.
  expect(seen.opened).toBeNull();

  // A click on the card body uses the same Open route as the button.
  const card = document.querySelector('[data-slot="card"]');
  if (card === null) throw new Error("drive card missing");
  await fireEvent.click(card);
  await settle();
  expect(seen.opened).toBe("/drives/alpha");
});

test("relay chips collapse to a count", async () => {
  const snapshot = readySnapshot();
  const alpha = snapshot.data.drives[0];
  if (alpha === undefined) throw new Error("fixture drive missing");
  snapshot.data.drives.push({
    ...alpha,
    name: "beta",
    drive_dir: "/drives/beta",
    relays: ["wss://relay.example/a", "wss://relay.example/b"],
  });
  const { getByText, queryByText } = render(LifecycleView, {
    props: {
      snapshot,
      pending: false,
      transportNotice: null,
      onRefresh: () => {},
      onAdd: () => {},
      onOpen: () => {},
    },
  });
  await settle();
  expect(getByText("2 relays")).toBeDefined();
  expect(queryByText("wss://relay.example/a")).toBeNull();
});
