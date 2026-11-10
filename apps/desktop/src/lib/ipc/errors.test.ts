// Fixture checks for the backend answer taxonomy mirror
// (`./types.ts`, `./errors.ts`). Fixture values are copied from
// the Rust test `error_taxonomy_serializes_typed` in
// `crates/wyrd-desktop/src/backend.rs` — they pin the JSON shape
// the frontend must keep reading, not a second implementation of
// it. Result-shape fixtures live in `results.test.ts`, copied from
// `result_shapes_serialize_verbatim`.
//
// Run: `bun test` from `apps/desktop/`.

import { describe, expect, test } from "bun:test";
import {
  asBackendError,
  describe as describeError,
  isWireErrorCode,
} from "./errors.js";

test("unavailable and unconfirmed stay distinct from the JSON alone", () => {
  const unavailable = asBackendError({
    kind: "unavailable",
    detail: "no owner",
  });
  const unconfirmed = asBackendError({
    kind: "unconfirmed",
    op: "lookup",
    detail: "torn frame",
  });
  expect(unavailable?.kind).toBe("unavailable");
  expect(unconfirmed?.kind).toBe("unconfirmed");
  expect(unconfirmed).toMatchObject({ op: "lookup" });
  expect(unavailable?.kind).not.toBe(unconfirmed?.kind);
});

test("a refusal keeps its exact code", () => {
  const refused = asBackendError({
    kind: "refused",
    code: "NotFound",
    message: "no such path",
  });
  expect(refused?.kind).toBe("refused");
  if (refused?.kind !== "refused") throw new Error("unreachable");
  expect(refused.code).toBe("NotFound");
  expect(describeError(refused)).toContain("NotFound");
});

test("a refusal with an unknown code stays a refusal, verbatim", () => {
  const refused = asBackendError({
    kind: "refused",
    code: "FutureCode",
    message: "a newer owner",
  });
  expect(refused?.kind).toBe("refused");
  if (refused?.kind !== "refused") throw new Error("unreachable");
  expect(refused.code).toBe("FutureCode");
  expect(isWireErrorCode(refused.code)).toBe(false);
  expect(describeError(refused)).toContain("FutureCode");
});

test("incompatible parses with its detail", () => {
  const incompatible = asBackendError({
    kind: "incompatible",
    detail: "wire version 2",
  });
  expect(incompatible?.kind).toBe("incompatible");
});

test("non-taxonomy payloads are never an owner answer", () => {
  for (const payload of [
    null,
    "owner unavailable: no owner",
    { kind: "Unavailable", detail: "wrong case" },
    { kind: "unavailable" },
    { kind: "refused", code: "NotFound" },
    { kind: "mystery", detail: "x" },
  ]) {
    expect(asBackendError(payload)).toBeNull();
  }
});
