// `invokeCommand` behavior through the mocked Tauri bridge
// (`@tauri-apps/api/mocks`). The mock only stands in for the
// transport — payloads keep the exact backend shapes, and no
// command argument shapes are asserted here (they do not exist
// until the Tauri command layer lands).
//
// Run: `bun test` from `apps/desktop/`.

import { beforeEach, describe, expect, test } from "bun:test";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { invokeCommand, IpcTransportError } from "./client.js";

beforeEach(() => {
  clearMocks();
});

test("success resolves the value", async () => {
  mockIPC((cmd) => {
    if (cmd === "liveness") return { alive: true };
    throw new Error(`unexpected command ${cmd}`);
  });
  const result = await invokeCommand<{ alive: boolean }>("liveness");
  expect(result).toEqual({ ok: true, value: { alive: true } });
});

test("an owner answer rejects into the taxonomy, intact", async () => {
  // A real backend `Err(BackendError)` rejects `invoke` with the
  // serialized object, so the mock throws the payload verbatim.
  mockIPC(() => {
    throw { kind: "unavailable", detail: "no owner" };
  });
  const result = await invokeCommand("liveness");
  expect(result).toEqual({
    ok: false,
    error: { kind: "unavailable", detail: "no owner" },
  });
});

test("a non-taxonomy rejection throws a transport error", async () => {
  mockIPC(() => {
    throw new Error("no bridge");
  });
  const seen = await invokeCommand("liveness").then(
    () => null,
    (error: unknown) => error,
  );
  expect(seen).toBeInstanceOf(IpcTransportError);
});
