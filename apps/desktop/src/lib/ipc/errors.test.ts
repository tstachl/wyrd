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

test("error wording follows the failure source (R-D7)", () => {
  // Local path refusal: no request was sent, so no remote
  // component is named and no version is claimed.
  expect(
    describeError({
      kind: "incompatible",
      detail: "drive_dir drives/alpha must be absolute",
    }),
  ).toBe(
    "Wyrd refused this path: drive_dir drives/alpha must be absolute",
  );
  // Wire-version skew in a response may say so.
  expect(
    describeError({
      kind: "incompatible",
      detail: "owner answered wire version 3, want 2",
    }),
  ).toBe(
    "The response used a different wire version: owner answered wire version 3, want 2",
  );
  // Wrong echoed id and unexpected result shape are malformed
  // answers, never version mismatches.
  expect(
    describeError({
      kind: "incompatible",
      detail: "owner echoed id 5, want 3 for lookup",
    }),
  ).toBe(
    "The answer did not match the request: owner echoed id 5, want 3 for lookup",
  );
  expect(
    describeError({
      kind: "incompatible",
      detail: "owner answered lookup with an unrelated result: Stat { .. }",
    }),
  ).toBe(
    "The answer did not match the request: owner answered lookup with an unrelated result: Stat { .. }",
  );
  // Non-hex bytes are unreadable, never a version mismatch.
  expect(
    describeError({
      kind: "incompatible",
      detail: "non-hex read bytes: invalid character",
    }),
  ).toBe("The answer could not be read: non-hex read bytes: invalid character");
});

test("incompatible wording never claims a service version mismatch", () => {
  for (const detail of [
    "drive_dir drives/alpha must be absolute",
    "owner answered wire version 3, want 2",
    "owner echoed id 5, want 3 for lookup",
    "owner answered lookup with an unrelated result: Stat",
    "non-hex read bytes: invalid character",
    "something a newer owner invented",
  ]) {
    const rendered = describeError({ kind: "incompatible", detail });
    expect(rendered).not.toContain("Incompatible");
    expect(rendered).not.toContain("service version");
    expect(rendered).not.toContain("do not match");
  }
});

test("unavailable, unconfirmed and refused stay distinct in prose", () => {
  const unavailable = describeError({ kind: "unavailable", detail: "no owner" });
  const unconfirmed = describeError({
    kind: "unconfirmed",
    op: "lookup",
    detail: "torn frame",
  });
  const refused = describeError({
    kind: "refused",
    code: "NotFound",
    message: "no such path",
  });
  expect(unavailable).toContain("unavailable");
  expect(unconfirmed).toContain("Unconfirmed lookup");
  expect(refused).toContain("(NotFound)");
  expect(new Set([unavailable, unconfirmed, refused]).size).toBe(3);
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
