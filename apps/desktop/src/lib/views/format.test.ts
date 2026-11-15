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
  describeProvisionResult,
  formatObservedAt,
  freshnessLabel,
  membershipSummary,
  nextDriveDir,
  observedRegistrySummary,
  shortId,
  suggestedDriveDirFor,
  withDevCliHint,
} from "./format.js";
import type { ProvisionReport, RegistryState } from "$lib/ipc/results.js";

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

test("the suggestion follows the name while untouched", () => {
  // Untouched create follows the suggestion, including renames.
  expect(nextDriveDir(false, "", "/b/drives/alpha")).toBe("/b/drives/alpha");
  expect(nextDriveDir(false, "/b/drives/alpha", "/b/drives/project-x")).toBe(
    "/b/drives/project-x",
  );
  // Touched fields keep the operator's value, including clears.
  expect(nextDriveDir(true, "/custom/dir", "/b/drives/alpha")).toBe("/custom/dir");
  expect(nextDriveDir(true, "", "/b/drives/alpha")).toBe("");
  // No suggestion keeps whatever is there.
  expect(nextDriveDir(false, "/operator/kept", null)).toBe("/operator/kept");
});

const emptyRegistry: RegistryState = { status: "ready", drives: [], skipped: [] };

function report(result: ProvisionReport["result"], membership: ProvisionReport["membership"] = null): ProvisionReport {
  return { result, observed: emptyRegistry, membership };
}

test("provisioning outcomes name the exit code without claiming state", () => {
  expect(describeProvisionResult(report({ result: "completed", detail: "wrote" }))).toContain(
    "Only the observation below says what happened",
  );
  expect(describeProvisionResult(report({ result: "expectedfailure", detail: "refused" }))).toContain(
    "Exit 2",
  );
  expect(describeProvisionResult(report({ result: "helddrive", detail: "held" }))).toContain(
    "not retried",
  );
  expect(describeProvisionResult(report({ result: "busy", drive_dir: "/d/alpha" }))).toContain(
    "not queued",
  );
  expect(describeProvisionResult(report({ result: "unknown", detail: "torn" }))).toContain(
    "observation below decides",
  );
});

test("post-run observation names the registry and the membership tip", () => {
  expect(observedRegistrySummary(report({ result: "completed", detail: "" }))).toBe(
    "Registry ready: 0 drive(s).",
  );
  expect(membershipSummary(report({ result: "completed", detail: "" }))).toBe(
    "Membership: unknown (the owner did not answer).",
  );
  expect(
    membershipSummary(report({ result: "completed", detail: "" }, { epoch: 3, known_members: 2 })),
  ).toBe("Membership: 2 member(s), epoch 3.");
});

test("missing dev CLI names the build that provides it", () => {
  const missing =
    "`__build_info` failed (no exit code): cannot execute /tmp/wyrd: No such file or directory (os error 2)";
  expect(withDevCliHint(missing)).toContain("cargo build -p wyrd-cli");
  expect(withDevCliHint("CLI build 9.9.9 does not match")).toBe(
    "CLI build 9.9.9 does not match",
  );
});
