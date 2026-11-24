// Behavioral pins for the app-wide diagnostics card
// (`./DiagnosticsCard.svelte`). The card shows the persisted level
// from settings.json, not an optimistic answer: a refusal, an
// unconfirmed outcome, a transport throw, or a re-read that fails to
// show the new level never display as applied. Only a matching
// owner read-back plus a matching persisted re-read earns success.
// Mounts the real component under happy-dom through the test-setup
// Svelte loader.
//
// Run: `bun run test` from `apps/desktop/`.

import { afterEach, beforeEach, expect, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { tick } from "svelte";
import DiagnosticsCard from "./DiagnosticsCard.svelte";
import type {
  Diagnostics,
  DiagnosticsExport,
  FinderIntegration,
  WireDiagnosticLevel,
} from "$lib/ipc/results.js";
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

function observed(level: WireDiagnosticLevel): Diagnostics {
  return { state: "configured", level, explicit: true, path: "/x/settings.json" };
}

function props(overrides: Record<string, unknown> = {}) {
  return {
    snapshot: snapshot("default"),
    finderIntegration: "provisioned" as FinderIntegration,
    driveDir: "/drives/alpha",
    pending: false,
    onRefresh: async () => observed("detailed"),
    onSet: async (_level: WireDiagnosticLevel): Promise<IpcResult<WireDiagnosticLevel>> => ({
      ok: true,
      value: "detailed",
    }),
    onExport: async (): Promise<DiagnosticsExport> => ({
      state: "written",
      path: "/tmp/wyrd-diagnostics-ab.json",
      records: 3,
      generated_at: 1_700_000_000,
    }),
    ...overrides,
  };
}

const LABEL = "Detailed diagnostics for this app";

test("the checkbox shows the persisted level, not an in-memory override", () => {
  const rendered = render(DiagnosticsCard, {
    props: props({ snapshot: snapshot("detailed", true) }),
  });
  // bits-ui renders a button with `role="checkbox"`, not a native
  // input: the checked state rides `aria-checked`.
  expect(
    (rendered.getByLabelText(LABEL) as HTMLElement).getAttribute("aria-checked"),
  ).toBe("true");
});

test("a refused set keeps the old level and never re-reads as applied", async () => {
  let refreshes = 0;
  const rendered = render(DiagnosticsCard, {
    props: props({
      onRefresh: async () => {
        refreshes += 1;
        return observed("detailed");
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
  const checkbox = rendered.getByLabelText(LABEL) as HTMLElement;
  expect(checkbox.getAttribute("aria-checked")).toBe("false");
  await fireEvent.click(checkbox);
  await tick();
  expect(refreshes).toBe(0);
  expect(checkbox.getAttribute("aria-checked")).toBe("false");
  expect(rendered.getByText("Owner refused (DiagnosticsMirror): no app-group container")).toBeTruthy();
});

test("an unconfirmed answer rolls back and is never shown as applied", async () => {
  let refreshes = 0;
  const rendered = render(DiagnosticsCard, {
    props: props({
      onRefresh: async () => {
        refreshes += 1;
        return observed("detailed");
      },
      onSet: async () => ({
        ok: false,
        error: {
          kind: "unconfirmed",
          op: "diagnostics_set",
          detail: "torn frame",
        },
      }),
    }),
  });
  const checkbox = rendered.getByLabelText(LABEL) as HTMLElement;
  await fireEvent.click(checkbox);
  await tick();
  expect(refreshes).toBe(0);
  expect(checkbox.getAttribute("aria-checked")).toBe("false");
  expect(rendered.getByText("Unconfirmed diagnostics_set: torn frame")).toBeTruthy();
});

test("a transport throw rolls back without a re-read", async () => {
  let refreshes = 0;
  const rendered = render(DiagnosticsCard, {
    props: props({
      onRefresh: async () => {
        refreshes += 1;
        return observed("detailed");
      },
      onSet: async () => {
        throw new Error("backend unreachable");
      },
    }),
  });
  const checkbox = rendered.getByLabelText(LABEL) as HTMLElement;
  await fireEvent.click(checkbox);
  await tick();
  expect(refreshes).toBe(0);
  expect(checkbox.getAttribute("aria-checked")).toBe("false");
  expect(rendered.getByText("backend unreachable")).toBeTruthy();
});

test("a confirmed set re-reads the persisted level before reporting success", async () => {
  const order: string[] = [];
  const rendered = render(DiagnosticsCard, {
    props: props({
      onSet: async () => {
        order.push("set");
        return { ok: true, value: "detailed" };
      },
      onRefresh: async () => {
        order.push("refresh");
        return observed("detailed");
      },
    }),
  });
  const checkbox = rendered.getByLabelText(LABEL) as HTMLElement;
  await fireEvent.click(checkbox);
  await tick();
  await tick();
  expect(order).toEqual(["set", "refresh"]);
  expect(checkbox.getAttribute("aria-checked")).toBe("true");
  expect(rendered.getByText(/Saved\. This drive's owner uses the new level/)).toBeTruthy();
});

test("a re-read that does not show the level is not success", async () => {
  for (const reRead of [() => Promise.resolve(null), () => Promise.resolve(observed("default"))]) {
    const rendered = render(DiagnosticsCard, {
      props: props({ onRefresh: reRead }),
    });
    const checkbox = rendered.getByLabelText(LABEL) as HTMLElement;
    await fireEvent.click(checkbox);
    await tick();
    await tick();
    expect(checkbox.getAttribute("aria-checked")).toBe("false");
    expect(rendered.queryByText(/Saved\. This drive's owner uses the new level/)).toBeNull();
    cleanup();
  }
});

test("a read-back that disagrees with the request is not success", async () => {
  const rendered = render(DiagnosticsCard, {
    props: props({
      onSet: async () => ({ ok: true, value: "default" }),
    }),
  });
  const checkbox = rendered.getByLabelText(LABEL) as HTMLElement;
  await fireEvent.click(checkbox);
  await tick();
  expect(checkbox.getAttribute("aria-checked")).toBe("false");
  expect(rendered.getByText(/read back "default"/)).toBeTruthy();
});

test("export is explicit and reports the temporary file without sharing it", async () => {
  let calls = 0;
  const rendered = render(DiagnosticsCard, {
    props: props({
      onExport: async () => {
        calls += 1;
        return {
          state: "written",
          path: "/tmp/wyrd-diagnostics-ab.json",
          records: 3,
          generated_at: 1_700_000_000,
        };
      },
    }),
  });
  expect(calls).toBe(0);
  await fireEvent.click(rendered.getByRole("button", { name: "Export bundle" }));
  await tick();
  expect(calls).toBe(1);
  expect(rendered.getByText(/Temporary bundle written with 3 records/)).toBeTruthy();
  expect(rendered.getByText("/tmp/wyrd-diagnostics-ab.json")).toBeTruthy();
});

test("an unprovisioned build states the signing cause and the recovery", async () => {
  const rendered = render(DiagnosticsCard, {
    props: props({ finderIntegration: "unprovisioned" as FinderIntegration }),
  });
  expect(rendered.getByText("Finder integration requires a signed build")).toBeTruthy();
  expect(rendered.getByText(/File Provider extension ad-hoc/)).toBeTruthy();
  expect(rendered.getByText(/re-run install-dev.sh with/)).toBeTruthy();
});

test("a provisioned build shows no signing warning", async () => {
  const rendered = render(DiagnosticsCard, {
    props: props({ finderIntegration: "provisioned" as FinderIntegration }),
  });
  expect(rendered.queryByText("Finder integration requires a signed build")).toBeNull();
});

test("no drive selected disables the toggle with an explicit hint", async () => {
  const rendered = render(DiagnosticsCard, {
    props: props({ driveDir: null }),
  });
  const checkbox = rendered.getByLabelText(LABEL) as HTMLButtonElement;
  expect(checkbox.disabled).toBe(true);
  expect(
    rendered.getByText("Select a drive whose owner is answering to change this setting."),
  ).toBeTruthy();
});
