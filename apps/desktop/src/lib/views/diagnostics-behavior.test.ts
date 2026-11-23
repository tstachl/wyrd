// Behavioral pins for the app-wide diagnostics card
// (`./DiagnosticsCard.svelte`). The card shows the persisted level
// from settings.json, not an optimistic answer: a refusal or an
// unconfirmed outcome never re-reads as applied, and a confirmed
// change re-reads the persisted level before reporting success.
// Mounts the real component under happy-dom through the test-setup
// Svelte loader.
//
// Run: `bun run test` from `apps/desktop/`.

import { afterEach, beforeEach, expect, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { tick } from "svelte";
import DiagnosticsCard from "./DiagnosticsCard.svelte";
import type { Diagnostics, WireDiagnosticLevel } from "$lib/ipc/results.js";
import type { IpcResult } from "$lib/ipc/types.js";
import type { Stamped } from "$lib/state/presentation.svelte.js";

beforeEach(async () => {
  await GlobalRegistrator.register();
});

afterEach(async () => {
  cleanup();
  await GlobalRegistrator.unregister();
});

function snapshot(level: WireDiagnosticLevel, explicit = false): Stamped<Diagnostics> {
  const data: Diagnostics = {
    state: "configured",
    level,
    explicit,
    path: "/x/settings.json",
  };
  return { data, observedAtMs: Date.now() };
}

function props(overrides: Record<string, unknown> = {}) {
  return {
    snapshot: snapshot("default"),
    driveDir: "/drives/alpha",
    pending: false,
    onRefresh: async () => {},
    onSet: async (_level: WireDiagnosticLevel): Promise<IpcResult<WireDiagnosticLevel>> => ({
      ok: true,
      value: "detailed",
    }),
    ...overrides,
  };
}

test("the checkbox shows the persisted level, not an in-memory override", async () => {
  const { getByLabelText } = render(DiagnosticsCard, {
    props: props({ snapshot: snapshot("detailed", true) }),
  });
  const checkbox = getByLabelText("Detailed diagnostics for this app") as HTMLElement;
  // bits-ui renders a button with `role="checkbox"`, not a native
  // input: the checked state rides `aria-checked`.
  expect(checkbox.getAttribute("aria-checked")).toBe("true");
});

test("a refused set keeps the old level and never re-reads as applied", async () => {
  let refreshes = 0;
  const { getByLabelText, getByText } = render(DiagnosticsCard, {
    props: props({
      onRefresh: async () => {
        refreshes += 1;
      },
      onSet: async () => ({
        ok: false,
        error: {
          kind: "refused",
          code: "DiagnosticsMirror",
          message: "no app-group container",
        },
      }),
    }),
  });
  const checkbox = getByLabelText("Detailed diagnostics for this app") as HTMLElement;
  expect(checkbox.getAttribute("aria-checked")).toBe("false");
  await fireEvent.click(checkbox);
  await tick();
  expect(refreshes).toBe(0);
  expect(checkbox.getAttribute("aria-checked")).toBe("false");
  expect(getByText("Owner refused (DiagnosticsMirror): no app-group container")).toBeTruthy();
});

test("a confirmed set re-reads the persisted level before reporting success", async () => {
  const order: string[] = [];
  const { getByLabelText, getByText } = render(DiagnosticsCard, {
    props: props({
      onSet: async () => {
        order.push("set");
        return { ok: true, value: "detailed" };
      },
      onRefresh: async () => {
        order.push("refresh");
      },
    }),
  });
  const checkbox = getByLabelText("Detailed diagnostics for this app") as HTMLInputElement;
  await fireEvent.click(checkbox);
  await tick();
  await tick();
  expect(order).toEqual(["set", "refresh"]);
  expect(getByText(/Saved\. This drive's owner uses the new level/)).toBeTruthy();
});

test("no drive selected disables the toggle with an explicit hint", async () => {
  const { getByLabelText, getByText } = render(DiagnosticsCard, {
    props: props({ driveDir: null }),
  });
  const checkbox = getByLabelText("Detailed diagnostics for this app") as HTMLButtonElement;
  expect(checkbox.disabled).toBe(true);
  expect(getByText("Select a drive whose owner is answering to change this setting.")).toBeTruthy();
});
