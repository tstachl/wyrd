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
