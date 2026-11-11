// Fixture checks for the result-shape mirrors (`./results.ts`).
// Fixture values are copied from the Rust test
// `result_shapes_serialize_verbatim` in
// `crates/wyrd-desktop/src/commands.rs` — they pin the JSON the
// backend produces, not a second implementation of it. Tuples
// arrive as arrays, enums stay externally tagged, `Option` arrives
// as `null`.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";
import type {
  ProvisionReport,
  ProvisionResult,
  Served,
  WireAttr,
  WireDirEntry,
  WireFreshness,
  WireMutationError,
  WireMutationKind,
  WireMutationOutcome,
  WireNode,
  WireParentToken,
} from "./results.js";

function singleKey(value: object): string {
  const keys = Object.keys(value);
  if (keys.length !== 1) throw new Error("expected a singly-tagged enum");
  return keys[0] as string;
}

test("served node: tuple array with an externally tagged node", () => {
  const served: Served<WireNode> = [
    { File: { size: 11, executable: false, chunks: ["9be2"] } },
    3, 7,
  ];
  expect(Array.isArray(served)).toBe(true);
  expect(served).toHaveLength(3);
  expect(singleKey(served[0])).toBe("File");
  expect(served[1]).toBe(3);
  expect(served[2]).toBe(7);
});

test("served attr keeps snake_case fields", () => {
  const served: Served<WireAttr> = [
    { kind: "File", size: 11, executable: false },
    3, 7,
  ];
  expect(served[0].kind).toBe("File");
});

test("directory entries nest tagged nodes", () => {
  const served: Served<WireDirEntry[]> = [
    [
      {
        name: "notes.txt",
        node: { File: { size: 11, executable: false, chunks: ["9be2"] } },
      },
    ],
    3, 7,
  ];
  expect(served[0][0]?.name).toBe("notes.txt");
  expect(singleKey(served[0][0]?.node ?? {})).toBe("File");
});

test("file bytes arrive as a number array", () => {
  const served: Served<number[]> = [[104, 105], 3, 7];
  expect(served[0]).toEqual([104, 105]);
});

test("freshness state stays externally tagged", () => {
  const freshness: WireFreshness = {
    state: { RetainedLastGood: { detail: "relay quiet" } },
    revision: 9,
    generation: 4,
  };
  expect(singleKey(freshness.state as object)).toBe("RetainedLastGood");
});

test("parent token is plain data", () => {
  const token: WireParentToken = { instance: 2, counter: 5 };
  expect(token).toEqual({ instance: 2, counter: 5 });
});

test("setattrs keeps absent fields as null", () => {
  const kind: WireMutationKind = {
    SetAttrs: { path: "notes.txt", size: null, executable: true, base: null },
  };
  const inner = (kind as { SetAttrs: Record<string, unknown> })["SetAttrs"];
  expect(inner["size"]).toBeNull();
  expect(inner["executable"]).toBe(true);
  expect(inner["base"]).toBeNull();
});

test("outcomes stay tagged; Done is bare", () => {
  const created: WireMutationOutcome = {
    Created: { size: 11, executable: false, chunks: ["9be2"] },
  };
  expect(singleKey(created as object)).toBe("Created");
  const done: WireMutationOutcome = "Done";
  expect(done).toBe("Done");
});

test("reconcile absence is null, never a refusal shape", () => {
  const absent: Served<WireNode> | null = null;
  expect(absent).toBeNull();
});

test("mutation errors keep code and message verbatim", () => {
  const error: WireMutationError = { code: "NotFound", message: "no such path" };
  expect(error).toEqual({ code: "NotFound", message: "no such path" });
});

// Fixtures for the local-query mirrors. Copied from the Rust test
// `local_query_shapes_serialize_verbatim` in
// `crates/wyrd-desktop/src/commands.rs` — they pin the JSON the
// backend produces, not a second implementation of it. Internally
// tagged enums carry their tag alongside flattened content.

import type {
  DriveAvailability,
  DriveSettingsState,
  DriveView,
  RegistryState,
} from "./results.js";

test("drive availability states stay tagged and distinct", () => {
  const available: DriveAvailability = { state: "available" };
  expect(available).toEqual({ state: "available" });
  const unavailable: DriveAvailability = {
    state: "unavailable",
    detail: "no owner",
  };
  expect(unavailable).toEqual({ state: "unavailable", detail: "no owner" });
  const unknown: DriveAvailability = {
    state: "unknown",
    detail: "no terminal answer",
  };
  expect(unknown).toEqual({ state: "unknown", detail: "no terminal answer" });
});

test("drive view carries facts plus the live verdict", () => {
  const drive: DriveView = {
    name: "alpha",
    drive_dir: "/drives/alpha",
    relays: ["wss://relay.example"],
    service: "drive-alpha",
    mode: "vault",
    availability: { state: "available" },
  };
  expect(drive).toEqual({
    name: "alpha",
    drive_dir: "/drives/alpha",
    relays: ["wss://relay.example"],
    service: "drive-alpha",
    mode: "vault",
    availability: { state: "available" },
  });
});

test("registry states flatten the status without stutter", () => {
  const ready: RegistryState = {
    status: "ready",
    drives: [
      {
        name: "alpha",
        drive_dir: "/drives/alpha",
        relays: ["wss://relay.example"],
        service: "drive-alpha",
        mode: "vault",
        availability: { state: "available" },
      },
    ],
    skipped: ['entry "bad": key "bad" does not match entry name "other"'],
  };
  expect(ready.status).toBe("ready");
  expect(ready.drives).toHaveLength(1);
  const nofile: RegistryState = { status: "nofile", drives: [], skipped: [] };
  expect(nofile).toEqual({ status: "nofile", drives: [], skipped: [] });
  const unreadable: RegistryState = {
    status: "unreadable",
    reason: "registry /x/drives.json is version 2, this shell reads 1",
    drives: [],
    skipped: [],
  };
  expect(unreadable.reason).toContain("version 2");
});

test("drive settings distinguish configured from unknown", () => {
  const configured: DriveSettingsState = {
    state: "configured",
    retained_bytes_quota: 0,
    max_admit_per_pass: 50,
    fetch_pass_budget_secs: null,
    max_mutation_wait_secs: null,
    open_timeout_secs: 45,
  };
  expect(configured).toEqual({
    state: "configured",
    retained_bytes_quota: 0,
    max_admit_per_pass: 50,
    fetch_pass_budget_secs: null,
    max_mutation_wait_secs: null,
    open_timeout_secs: 45,
  });
  const nosection: DriveSettingsState = { state: "nosection" };
  expect(nosection).toEqual({ state: "nosection" });
  const nofile: DriveSettingsState = { state: "nofile" };
  expect(nofile).toEqual({ state: "nofile" });
  const unreadable: DriveSettingsState = {
    state: "unreadable",
    reason: "settings /x/settings.json is version 2, this shell reads 1",
  };
  expect(unreadable.reason).toContain("version 2");
});

test("provisioning report pairs the outcome with the observation", () => {
  // Copied from the Rust test `provision_report_serializes_verbatim`
  // in `crates/wyrd-desktop/src/commands.rs` — pins the JSON the
  // backend produces, not a second implementation of it.
  const ready: ProvisionReport = {
    result: {
      result: "verifiedavailable",
      detail: "drive alpha verified available (socket /drives/alpha/owner.sock)",
    },
    observed: { status: "nofile", drives: [], skipped: [] },
  };
  expect(ready.result.result).toBe("verifiedavailable");
  expect(ready.observed.status).toBe("nofile");
  const busy: ProvisionReport = {
    result: { result: "busy", drive_dir: "/drives/alpha" },
    observed: { status: "nofile", drives: [], skipped: [] },
  };
  expect(busy.result).toEqual({ result: "busy", drive_dir: "/drives/alpha" });
  const confirm: ProvisionResult = {
    result: "incomplete",
    stage: "confirm",
    reason: "sole-owner leave needs explicit acknowledgement",
  };
  expect(confirm.stage).toBe("confirm");
});
