// Per-operation wrapper checks (`./operations.ts`) through the
// mocked Tauri bridge. The mock stands in for the transport only:
// it asserts the command name and arguments each wrapper sends
// and returns backend-shaped payloads verbatim. No argument shape
// here is invented — names and shapes come from
// `crates/wyrd-desktop/src/commands.rs`.
//
// Run: `bun run test` from `apps/desktop/`.

import { beforeEach, expect, test } from "bun:test";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import {
  captureParent,
  lookup,
  observe,
  reconnect,
  submit,
} from "./operations.js";

beforeEach(() => {
  clearMocks();
});

test("lookup sends the command name and path", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    return [
      { File: { size: 11, executable: false, chunks: ["9be2"] } },
      3, 7,
    ];
  });
  const result = await lookup("notes.txt");
  expect(seen).toEqual([{ cmd: "lookup", args: { path: "notes.txt" } }]);
  expect(result.ok).toBe(true);
});

test("observe sends no arguments", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    throw { kind: "unavailable", detail: "no owner" };
  });
  const result = await observe();
  expect(seen).toEqual([{ cmd: "observe", args: {} }]);
  expect(result).toEqual({
    ok: false,
    error: { kind: "unavailable", detail: "no owner" },
  });
});

test("submit forwards the mutation kind verbatim", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    return "Done";
  });
  const result = await submit({ Unlink: { path: "notes.txt" } });
  expect(seen).toEqual([
    { cmd: "submit", args: { kind: { Unlink: { path: "notes.txt" } } } },
  ]);
  expect(result).toEqual({ ok: true, value: "Done" });
});

test("captureParent names the parent, reconnect sends nothing", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    if (cmd === "capture_parent") return { instance: 2, counter: 5 };
    return null;
  });
  expect(await captureParent("")).toEqual({
    ok: true,
    value: { instance: 2, counter: 5 },
  });
  expect(await reconnect()).toEqual({ ok: true, value: null });
  expect(seen).toEqual([
    { cmd: "capture_parent", args: { path: "" } },
    { cmd: "reconnect", args: {} },
  ]);
});
