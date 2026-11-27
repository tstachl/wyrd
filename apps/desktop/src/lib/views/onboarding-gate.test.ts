// Gate-decision pins for drives set up outside the GUI.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";
import { classifyFirstRun, describeAdoptedDrive } from "./onboarding-gate.js";
import type { ExternalDrivesState, KeyDefaults } from "$lib/ipc/results.js";

const DEFAULTS: KeyDefaults = {
  identity_file: "/k/identity",
  passphrase_file: "/k/passphrase",
  identity_exists: false,
  passphrase_exists: false,
  usable: true,
};

const EMPTY: ExternalDrivesState = { status: "ready", drives: [] };

function discovered(status: "ready" | "unreadable" = "ready", reachable = true) {
  const state: ExternalDrivesState =
    status === "unreadable"
      ? { status, reason: "read /x/drives.json: denied", drives: [] }
      : {
          status,
          drives: [
            {
              name: "outside",
              source: "alias",
              drive_dir: null,
              drive_id: "ab".repeat(32),
              reachable,
            },
          ],
        };
  return state;
}

test("a fresh device with no drives keeps the welcome guide", () => {
  expect(classifyFirstRun(DEFAULTS, EMPTY)).toEqual({ state: "welcome" });
});

test("a discovered live owner replaces welcome with adoption", () => {
  expect(classifyFirstRun(DEFAULTS, discovered())).toEqual({
    state: "adopt",
    drive: {
      name: "outside",
      source: "alias",
      drive_dir: null,
      drive_id: "ab".repeat(32),
      reachable: true,
    },
  });
});

test("a registered but stopped drive still replaces welcome", () => {
  expect(classifyFirstRun(DEFAULTS, discovered("ready", false))).toEqual({
    state: "adopt",
    drive: {
      name: "outside",
      source: "alias",
      drive_dir: null,
      drive_id: "ab".repeat(32),
      reachable: false,
    },
  });
});

test("an unreadable discovery fails closed on a credential-free device", () => {
  const result = classifyFirstRun(DEFAULTS, discovered("unreadable"));
  expect(result.state).toBe("unknown");
  if (result.state !== "unknown") throw new Error("must fail closed");
  expect(result.detail).toContain("read /x/drives.json: denied");
  expect(result.detail).toContain("second identity");
});

test("an unreadable discovery does not block the verify path", () => {
  const both = { ...DEFAULTS, identity_exists: true, passphrase_exists: true };
  expect(classifyFirstRun(both, discovered("unreadable"))).toEqual({ state: "verify" });
});

test("an incomplete credential pair stays repair, not adoption", () => {
  const one = { ...DEFAULTS, identity_exists: true };
  expect(classifyFirstRun(one, discovered())).toEqual({ state: "repair" });
});

test("an unusable home is unknown before any discovery reading", () => {
  const unusable = { ...DEFAULTS, usable: false };
  expect(classifyFirstRun(unusable, discovered()).state).toBe("unknown");
});

test("the adoption banner names provenance and forbids a new identity", () => {
  const text = describeAdoptedDrive({
    name: "outside",
    source: "alias",
    drive_dir: null,
    drive_id: "ab".repeat(32),
    reachable: true,
  });
  expect(text).toContain('"outside"');
  expect(text).toContain("live File Provider alias");
  expect(text).toContain("no new identity is generated");
  const registered = describeAdoptedDrive({
    name: "outside",
    source: "registry",
    drive_dir: "/drives/outside",
    drive_id: null,
    reachable: false,
  });
  expect(registered).toContain("drive registry");
  expect(registered).toContain("/drives/outside");
});
