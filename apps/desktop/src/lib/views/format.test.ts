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
  restartKindFor,
  sessionLabel,
  settingsSurviveLock,
  SETTING_BOUNDS,
  shortId,
  suggestedDriveDirFor,
  validateBoundInput,
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

test("freshness names currency, never convergence or health", () => {
  expect(freshnessLabel("FreshEmpty")).toBe("Current (empty)");
  expect(freshnessLabel("FreshNonEmpty")).toBe("Current");
  expect(
    freshnessLabel({ RetainedLastGood: { detail: "relay quiet" } }),
  ).toBe("Last good copy — stale: relay quiet");
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

test("session gate names the per-session posture, managed drives render nothing", () => {
  expect(sessionLabel(null)).toBeNull();
  expect(sessionLabel({ state: "unlocked", detail: "open" })).toBe(
    "Unlocked for this session",
  );
  expect(sessionLabel({ state: "locked", detail: "needs passphrase" })).toBe(
    "Locked — needs passphrase",
  );
});

test("locked drives keep their local settings surfaces, everything else gates on the owner", () => {
  // Locked: configured bounds read from the settings file and the
  // identity-pair Leave card need no owner, so the settings tab
  // renders them instead of the unavailable overlay.
  expect(settingsSurviveLock({ state: "locked", detail: "needs passphrase" })).toBe(true);
  // Unlocked drives render settings through the available path.
  expect(settingsSurviveLock({ state: "unlocked", detail: "open" })).toBe(false);
  // Managed-down drives (no session gate) keep the overlay with
  // its Start/Reinstall recovery — file facts alone would bury it.
  expect(settingsSurviveLock(null)).toBe(false);
});

test("restart routing follows registry posture, never the view", () => {
  // Managed (no session gate): the shell restarts the service.
  expect(restartKindFor(null)).toBe("service");
  // Unlocked session: lock, then unlock with the typed passphrase —
  // one click can never restart without holding the secret.
  expect(restartKindFor({ state: "unlocked", detail: "open" })).toBe("session");
  // Locked or stopped: no owner runs, so saving is the whole
  // operation — values apply at the next start.
  expect(restartKindFor({ state: "locked", detail: "needs passphrase" })).toBe(null);
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
    "already running on",
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

test("bound inputs validate per key, naming the key", () => {
  expect(SETTING_BOUNDS.map((bound) => bound.key)).toEqual([
    "retained_bytes_quota",
    "max_admit_per_pass",
    "fetch_pass_budget_secs",
    "max_mutation_wait_secs",
    "open_timeout_secs",
  ]);
  // Labels stay plain-worded and units stay honest: the suite
  // pins them so wording cannot drift per view.
  expect(SETTING_BOUNDS.map((bound) => [bound.key, bound.label, bound.unit])).toEqual([
    ["retained_bytes_quota", "Storage limit", "bytes"],
    ["max_admit_per_pass", "Items processed per sync", "items"],
    ["fetch_pass_budget_secs", "Download time limit", "seconds"],
    ["max_mutation_wait_secs", "File change wait limit", "seconds"],
    ["open_timeout_secs", "File open wait limit", "seconds"],
  ]);
  // Descriptions say what the implementation enforces, beneath
  // each label — pinned here alongside the labels.
  expect(SETTING_BOUNDS.map((bound) => bound.description)).toEqual([
    "Maximum amount of data Wyrd retains for this drive.",
    "Maximum number of items admitted during one sync pass.",
    "Maximum time spent fetching content during one sync pass.",
    "Maximum time a submitted file change waits before timing out.",
    "Maximum time to wait for requested file content to become available.",
  ]);
  expect(validateBoundInput("max_admit_per_pass", "64")).toBeNull();
  expect(validateBoundInput("retained_bytes_quota", "0")).toBeNull();
  expect(validateBoundInput("open_timeout_secs", " 30 ")).toBeNull();
  expect(validateBoundInput("max_admit_per_pass", "0")).toContain("max_admit_per_pass");
  expect(validateBoundInput("max_admit_per_pass", "1000001")).toContain("max_admit_per_pass");
  expect(validateBoundInput("open_timeout_secs", "0")).toContain("open_timeout_secs");
  expect(validateBoundInput("open_timeout_secs", "86401")).toContain("open_timeout_secs");
  expect(validateBoundInput("open_timeout_secs", "1.5")).toContain("open_timeout_secs");
  expect(validateBoundInput("open_timeout_secs", "-3")).toContain("open_timeout_secs");
  expect(validateBoundInput("open_timeout_secs", "")).toContain("open_timeout_secs");
  expect(validateBoundInput("admit_all", "1")).toContain("admit_all");
});

test("display card mirrors the bounds table, never its own wording", async () => {
  // The configured-values card hardcodes its rows instead of
  // iterating the table, so this pins the lockstep: every label
  // and description renders verbatim in `SettingsView.svelte`.
  const view = await Bun.file(`${import.meta.dir}/SettingsView.svelte`).text();
  for (const bound of SETTING_BOUNDS) {
    expect(view).toContain(`label="${bound.label}"`);
    expect(view).toContain(bound.description);
  }
});
