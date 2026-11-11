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
  driveSettings,
  listDrives,
  lookup,
  observe,
  reconnect,
  submit,
} from "./operations.js";
import { IpcTransportError } from "./client.js";

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
  const result = await lookup("/drives/alpha", "notes.txt");
  expect(seen).toEqual([
    { cmd: "lookup", args: { driveDir: "/drives/alpha", path: "notes.txt" } },
  ]);
  expect(result.ok).toBe(true);
});

test("observe sends no arguments", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    throw { kind: "unavailable", detail: "no owner" };
  });
  const result = await observe("/drives/alpha");
  expect(seen).toEqual([{ cmd: "observe", args: { driveDir: "/drives/alpha" } }]);
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
  const result = await submit("/drives/alpha", { Unlink: { path: "notes.txt" } });
  expect(seen).toEqual([
    {
      cmd: "submit",
      args: { driveDir: "/drives/alpha", kind: { Unlink: { path: "notes.txt" } } },
    },
  ]);
  expect(result).toEqual({ ok: true, value: "Done" });
});

test("captureParent names the parent, reconnect names the drive", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    if (cmd === "capture_parent") return { instance: 2, counter: 5 };
    return null;
  });
  expect(await captureParent("/drives/alpha", "")).toEqual({
    ok: true,
    value: { instance: 2, counter: 5 },
  });
  expect(await reconnect("/drives/alpha")).toEqual({ ok: true, value: null });
  expect(seen).toEqual([
    { cmd: "capture_parent", args: { driveDir: "/drives/alpha", path: "" } },
    { cmd: "reconnect", args: { driveDir: "/drives/alpha" } },
  ]);
});

test("listDrives sends no arguments and returns the state struct", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    return { status: "nofile", drives: [], skipped: [] };
  });
  expect(await listDrives()).toEqual({ status: "nofile", drives: [], skipped: [] });
  expect(seen).toEqual([{ cmd: "list_drives", args: {} }]);
});

test("driveSettings returns configured bounds verbatim", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    return {
      state: "configured",
      retained_bytes_quota: 0,
      max_admit_per_pass: 50,
      fetch_pass_budget_secs: null,
      max_mutation_wait_secs: null,
      open_timeout_secs: 45,
    };
  });
  expect(await driveSettings("/drives/alpha")).toEqual({
    state: "configured",
    retained_bytes_quota: 0,
    max_admit_per_pass: 50,
    fetch_pass_budget_secs: null,
    max_mutation_wait_secs: null,
    open_timeout_secs: 45,
  });
  expect(seen).toEqual([
    { cmd: "drive_settings", args: { driveDir: "/drives/alpha" } },
  ]);
});

test("local-query rejections throw a transport error, never taxonomy", async () => {
  mockIPC(() => {
    throw { kind: "unavailable", detail: "no owner" };
  });
  // A rejection here is a shell bug, not an owner answer: even a
  // taxonomy-shaped payload throws instead of masquerading as one.
  // The mock stands in for the transport; the backend never
  // rejects these commands with the taxonomy.
  const seen = await listDrives().then(
    () => null,
    (error: unknown) => error,
  );
  expect(seen).toBeInstanceOf(IpcTransportError);
});
