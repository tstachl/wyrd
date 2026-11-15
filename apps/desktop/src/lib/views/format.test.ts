// Wording pins for the view helpers (`./format.ts`). Display
// text is a contract with the acceptance criterion — a person
// reads these states — so the labels pin here, not in components.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";
import {
  availabilityLabel,
  baseDirFromIdentity,
  boundLabel,
  formatObservedAt,
  freshnessLabel,
  shortId,
  suggestedDriveDirFor,
  withDevCliHint,
} from "./format.js";

test("observation stamps render as UTC clock times", () => {
  expect(formatObservedAt(Date.UTC(2026, 4, 1, 7, 8, 9))).toBe("07:08:09 UTC");
});

test("short ids keep short values whole", () => {
  expect(shortId("9be2")).toBe("9be2");
  expect(shortId("9be2c4d6e8f0a2b4c6d8")).toBe("9be2c4d6…");
});

test("freshness names staleness instead of implying health", () => {
  expect(freshnessLabel("FreshEmpty")).toBe("Fresh (empty)");
  expect(freshnessLabel("FreshNonEmpty")).toBe("Fresh");
  expect(
    freshnessLabel({ RetainedLastGood: { detail: "relay quiet" } }),
  ).toBe("Last good, stale: relay quiet");
});

test("reachability never claims what transport alone cannot prove", () => {
  expect(availabilityLabel({ state: "available" })).toBe("Available");
  expect(availabilityLabel({ state: "unavailable", detail: "no owner" })).toBe(
    "Unavailable — no owner",
  );
  expect(availabilityLabel({ state: "unknown", detail: "no terminal answer" })).toBe(
    "Unknown — no terminal answer",
  );
});

test("absent bounds stay unknown, never defaulted", () => {
  expect(boundLabel(null, "bytes")).toBe(
    "Not configured — compiled default applies (unknown to the shell)",
  );
  expect(boundLabel(undefined, "bytes")).toBe(
    "Not configured — compiled default applies (unknown to the shell)",
  );
  expect(boundLabel(0, "bytes")).toBe("0 bytes");
});

test("drive suggestions derive from the credential base, never guesses", () => {
  expect(baseDirFromIdentity("")).toBeNull();
  expect(baseDirFromIdentity("/keys/identity")).toBeNull();
  expect(baseDirFromIdentity("/a/keys/identity")).toBe("/a");
  expect(
    baseDirFromIdentity("/Users/t/Library/Application Support/Wyrd/keys/identity"),
  ).toBe("/Users/t/Library/Application Support/Wyrd");
  expect(
    baseDirFromIdentity("/home/t/.local/state/wyrd/keys/passphrase"),
  ).toBe("/home/t/.local/state/wyrd");
  expect(baseDirFromIdentity("/a/other/identity")).toBeNull();
  expect(baseDirFromIdentity("/a/keys/other")).toBeNull();
  expect(
    suggestedDriveDirFor("", "/a/keys/identity"),
  ).toBe("/a/drives/alpha");
  expect(
    suggestedDriveDirFor("beta", "/a/keys/identity"),
  ).toBe("/a/drives/beta");
  expect(suggestedDriveDirFor("beta", "")).toBeNull();
});

test("missing dev CLI names the build that provides it", () => {
  const missing =
    "`__build_info` failed (no exit code): cannot execute /tmp/wyrd: No such file or directory (os error 2)";
  expect(withDevCliHint(missing)).toContain("cargo build -p wyrd-cli");
  expect(withDevCliHint("CLI build 9.9.9 does not match")).toBe(
    "CLI build 9.9.9 does not match",
  );
});
