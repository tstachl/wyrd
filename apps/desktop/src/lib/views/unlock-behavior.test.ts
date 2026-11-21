// Behavioral pins for the session unlock card
// (`./UnlockView.svelte`). Beyond the copy pins in
// `unlock-copy.test.ts`: the passphrase reaches the backend once
// and clears from component state after every attempt, the form
// cannot run before the identity gate resolves, and a refused
// unlock renders its outcome. Mounts the real component under
// happy-dom through the test-setup Svelte loader.
//
// Run: `bun run test` from `apps/desktop/`.

import { afterEach, beforeEach, expect, test } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { tick } from "svelte";
import UnlockView from "./UnlockView.svelte";

beforeEach(async () => {
  await GlobalRegistrator.register();
});

afterEach(async () => {
  cleanup();
  await GlobalRegistrator.unregister();
});

function props(overrides: Record<string, unknown> = {}) {
  return {
    driveLabel: "/drives/alpha",
    driveName: "alpha",
    detail: "no owner serves and no service definition exists",
    pending: false,
    identityReady: true,
    unlockError: null,
    onUnlock: async (_passphrase: string, _useStore: boolean) => {},
    ...overrides,
  };
}

test("typing and unlocking passes the passphrase once, then clears the entry", async () => {
  const seen: Array<{ passphrase: string; useStore: boolean }> = [];
  const { getByLabelText, getByRole } = render(UnlockView, {
    props: props({
      onUnlock: async (passphrase: string, useStore: boolean) =>
        void seen.push({ passphrase, useStore }),
    }),
  });
  const entry = getByLabelText("Drive passphrase") as HTMLInputElement;
  const button = getByRole("button", { name: "Unlock for this session" });
  // Empty entry cannot submit: the secret travels only when typed.
  expect((button as HTMLButtonElement).disabled).toBe(true);
  await fireEvent.input(entry, { target: { value: "hunter2-hunter2" } });
  await tick();
  expect((button as HTMLButtonElement).disabled).toBe(false);
  await fireEvent.click(button);
  await tick();
  await tick();
  // Store-backed by default: the identity loads from the drive's item.
  expect(seen).toEqual([{ passphrase: "hunter2-hunter2", useStore: true }]);
  // Cleared after the round trip, whatever the outcome — the
  // component never retains the secret.
  expect(entry.value).toBe("");
});

test("the file fallback stays disabled where the identity gate never resolved", async () => {
  const seen: string[] = [];
  const { getByLabelText, getByRole, getByText } = render(UnlockView, {
    props: props({
      identityReady: false,
      onUnlock: async (passphrase: string) => void seen.push(passphrase),
    }),
  });
  // Store mode needs no verified identity: switch to the operator
  // file fallback, where the gate still blocks the form.
  await fireEvent.click(getByRole("button", { name: "Credential files (operator)" }));
  await tick();
  const entry = getByLabelText("Drive passphrase") as HTMLInputElement;
  await fireEvent.input(entry, { target: { value: "hunter2-hunter2" } });
  await tick();
  const button = getByRole("button", { name: "Unlock for this session" });
  expect((button as HTMLButtonElement).disabled).toBe(true);
  expect(
    getByText("Unlock needs this device's verified identity — finish the identity check first."),
  ).toBeDefined();
  expect(seen).toEqual([]);
});

test("the store default needs only the drive name, never the identity gate", async () => {
  const seen: Array<{ passphrase: string; useStore: boolean }> = [];
  const { getByLabelText, getByRole } = render(UnlockView, {
    props: props({
      identityReady: false,
      onUnlock: async (passphrase: string, useStore: boolean) =>
        void seen.push({ passphrase, useStore }),
    }),
  });
  const entry = getByLabelText("Drive passphrase") as HTMLInputElement;
  await fireEvent.input(entry, { target: { value: "hunter2-hunter2" } });
  await tick();
  const button = getByRole("button", { name: "Unlock for this session" });
  expect((button as HTMLButtonElement).disabled).toBe(false);
  await fireEvent.click(button);
  await tick();
  await tick();
  expect(seen).toEqual([{ passphrase: "hunter2-hunter2", useStore: true }]);
});

test("a refused unlock renders its outcome on the card", async () => {
  const { getByText } = render(UnlockView, {
    props: props({ unlockError: "wrong passphrase (exit 2)" }),
  });
  expect(getByText("wrong passphrase (exit 2)")).toBeDefined();
});
