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
  credentialModeFor,
  describeProvisionResult,
  driveFactLine,
  driveNameValid,
  driveNeedsAttention,
  driveRunState,
  folderNameFromPath,
  formatBoundValue,
  formatByteSize,
  formatObservedAge,
  formatPublicationAge,
  formatRetentionLine,
  freshnessLabel,
  isFirstRun,
  membershipSummary,
  nextDriveDir,
  normalizeDriveName,
  observedRegistrySummary,
  parseBoundInput,
  shouldAdvanceAfterProvision,
  shouldAdvanceAfterRemoval,
  restartKindFor,
  sessionLabel,
  settingsSurviveLock,
  SETTING_BOUNDS,
  shortId,
  sortDrivesByAttention,
  statusAttention,
  statusRunState,
  statusSummaryLine,
  storeAccountFor,
  SUGGESTED_RELAYS,
  suggestedDriveDirFor,
  validateBoundInput,
  withDevCliHint,
} from "./format.js";
import type { OverviewDriveFacts, StatusFacts } from "./format.js";
import type {
  CredentialReference,
  DriveView,
  ProvisionReport,
  RegistryState,
} from "$lib/ipc/results.js";

test("credential mode follows observation and keeps the fallback only when unknown", () => {
  const store: CredentialReference = { backend: "os-store", account: "wyrd-drive-alpha" };
  const file: CredentialReference = {
    backend: "file",
    identityFile: "/keys/id",
    passphraseFile: "/keys/pass",
  };
  const unknown: CredentialReference = { backend: "unknown", reason: "no config" };

  expect(credentialModeFor(store, "file")).toBe("store");
  expect(credentialModeFor(file, "store")).toBe("file");
  expect(credentialModeFor(unknown, "store")).toBe("store");
  expect(credentialModeFor(unknown, "file")).toBe("file");
  expect(credentialModeFor(null, "store")).toBe("store");
});

test("observation ages are relative, local and never a sync time", () => {
  const now = Date.UTC(2026, 4, 1, 7, 8, 9);
  expect(formatObservedAge(now, now)).toBe("Checked just now");
  expect(formatObservedAge(now - 12_000, now)).toBe("Checked 12 seconds ago");
  expect(formatObservedAge(now - 90_000, now)).toBe("Checked 1 minute ago");
  expect(formatObservedAge(now - 3 * 60_000, now)).toBe("Checked 3 minutes ago");
  expect(formatObservedAge(now - 2 * 3_600_000, now)).toBe("Checked 2 hours ago");
  expect(formatObservedAge(now - 26 * 3_600_000, now)).toBe("Checked 1 day ago");
  // A future stamp is clamped, never rendered as a negative age.
  expect(formatObservedAge(now + 5_000, now)).toBe("Checked just now");
});

test("publication ages are relative, local and unknown at zero", () => {
  const now = Date.UTC(2026, 4, 1, 7, 8, 9);
  const at = now / 1000;
  expect(formatPublicationAge(undefined, now)).toBe("not yet updated on this device");
  expect(formatPublicationAge(0, now)).toBe("not yet updated on this device");
  expect(formatPublicationAge(Number.NaN, now)).toBe("not yet updated on this device");
  expect(formatPublicationAge(at, now)).toBe("Updated just now");
  expect(formatPublicationAge(at - 12, now)).toBe("Updated 12 seconds ago");
  expect(formatPublicationAge(at - 90, now)).toBe("Updated 1 minute ago");
  expect(formatPublicationAge(at - 3 * 60, now)).toBe("Updated 3 minutes ago");
  expect(formatPublicationAge(at - 2 * 3_600, now)).toBe("Updated 2 hours ago");
  expect(formatPublicationAge(at - 26 * 3_600, now)).toBe("Updated 1 day ago");
  // A future publication is clamped, never a negative age.
  expect(formatPublicationAge(at + 5, now)).toBe("Updated just now");
});

test("retention lines name drive data, a limit or no limit, never a disk figure", () => {
  expect(formatRetentionLine(undefined)).toBe("Not yet observed");
  expect(formatRetentionLine(null)).toBe("Not yet observed");
  expect(
    formatRetentionLine({ retained_bytes: 2 * 1024 ** 3, ceiling: 5 * 1024 ** 3 }),
  ).toBe("Using 2 GB of 5 GB");
  expect(formatRetentionLine({ retained_bytes: 1024 ** 3, ceiling: null })).toBe(
    "Using 1 GB · no limit set",
  );
  expect(formatByteSize(1536)).toBe("1.5 KB");
  expect(formatByteSize(2 * 1024 ** 3)).toBe("2 GB");
  expect(formatByteSize(Number.NaN)).toBe("unknown");
});

const overviewFacts: OverviewDriveFacts = {
  availability: "available",
  paused: false,
  connectedRelays: 5,
  totalRelays: 5,
  peers: 0,
  knownMembers: 1,
  stale: false,
};

test("drive cards name the folder, not a truncated path", () => {
  expect(folderNameFromPath("/drives/alpha")).toBe("alpha");
  expect(folderNameFromPath("/Users/t/drives/project-x/")).toBe("project-x");
  expect(folderNameFromPath("C:\\drives\\beta")).toBe("beta");
});

test("drive cards state observable run state and separate facts", () => {
  expect(driveRunState(overviewFacts)).toBe("Running");
  expect(driveRunState({ ...overviewFacts, paused: true })).toBe("Paused");
  expect(driveRunState({ ...overviewFacts, availability: "unavailable" })).toBe("Not running");
  expect(driveRunState(null)).toBe("Not yet observed");
  expect(driveFactLine(overviewFacts, 5)).toBe(
    "Connected to 5 of 5 relays · No other devices yet",
  );
  expect(driveFactLine(null, 3)).toBe("3 relays configured · Status not read this session");
  expect(driveFactLine({ ...overviewFacts, totalRelays: 0, connectedRelays: 0 }, 0)).toContain(
    "No relays connected at the last pass",
  );
});

test("only observed problems move a drive into the attention group", () => {
  expect(driveNeedsAttention(null)).toBe(false);
  expect(driveNeedsAttention(overviewFacts)).toBe(false);
  expect(driveNeedsAttention({ ...overviewFacts, paused: true })).toBe(true);
  expect(driveNeedsAttention({ ...overviewFacts, connectedRelays: 0 })).toBe(true);
  expect(driveNeedsAttention({ ...overviewFacts, stale: true })).toBe(true);
  const quiet = { name: "quiet", facts: overviewFacts };
  const busy = { name: "busy", facts: { ...overviewFacts, paused: true } };
  const middle = { name: "middle", facts: overviewFacts };
  expect(
    sortDrivesByAttention([quiet, busy, middle], (drive) => drive.facts).map((d) => d.name),
  ).toEqual(["busy", "quiet", "middle"]);
});

const baseFacts: StatusFacts = {
  availability: "available",
  paused: false,
  connectedRelays: 5,
  totalRelays: 5,
  knownMembers: 1,
  peers: 0,
  staleDetail: null,
  freshEmpty: true,
  localObjects: 0,
  retention: undefined,
};

test("the status summary is separate facts, never a green verdict", () => {
  expect(statusSummaryLine(baseFacts)).toBe(
    "Running · 5 of 5 relays connected · not shared with another device yet",
  );
  expect(statusRunState({ ...baseFacts, paused: true })).toBe("Paused");
  expect(statusRunState({ ...baseFacts, availability: "unavailable" })).toBe("Not running");
  expect(statusRunState({ ...baseFacts, availability: "unknown" })).toBe("State not yet observed");
  expect(
    statusSummaryLine({ ...baseFacts, knownMembers: 3, peers: 2 }),
  ).toBe("Running · 5 of 5 relays connected · 2 other devices connected at the last pass");
  expect(
    statusSummaryLine({ ...baseFacts, totalRelays: 0, connectedRelays: 0 }),
  ).toContain("no relays connected at the last pass");
});

test("attention appears only for observed problems", () => {
  expect(statusAttention(baseFacts)).toEqual([]);
  expect(statusAttention({ ...baseFacts, paused: true })).toContain(
    "Synchronization is paused. The drive is still running and can answer requests.",
  );
  expect(statusAttention({ ...baseFacts, connectedRelays: 0 })).toContain(
    "No relays connected at the last pass. Nothing can be exchanged with other devices until a relay is reachable.",
  );
  expect(statusAttention({ ...baseFacts, staleDetail: "relay quiet" })).toContain(
    "The served data is stale: relay quiet.",
  );
  expect(statusAttention({ ...baseFacts, availability: "unavailable" })).toContain(
    "The drive is not answering. Its last reported state is still shown below.",
  );
  // Near-limit is observed: a set ceiling and a count that reaches it.
  const near = "This drive is close to its storage limit. Wyrd will refuse new drive data when the limit is reached.";
  expect(
    statusAttention({
      ...baseFacts,
      retention: { retained_bytes: 950, ceiling: 1000 },
    }),
  ).toContain(near);
  // No limit, no count, or an old writer with no accountant adds nothing.
  expect(
    statusAttention({ ...baseFacts, retention: { retained_bytes: 950, ceiling: null } }),
  ).toEqual([]);
  expect(statusAttention({ ...baseFacts, retention: null })).toEqual([]);
  expect(
    statusAttention({
      ...baseFacts,
      retention: { retained_bytes: 100, ceiling: 1000 },
    }),
  ).toEqual([]);
});

test("a drive with one member, no peers and no data is a first run", () => {
  expect(isFirstRun(baseFacts)).toBe(true);
  expect(isFirstRun({ ...baseFacts, knownMembers: 2 })).toBe(false);
  expect(isFirstRun({ ...baseFacts, peers: 1 })).toBe(false);
  expect(isFirstRun({ ...baseFacts, freshEmpty: false })).toBe(false);
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
  expect(boundLabel(null, "bytes")).toBe("Using Wyrd's default");
  expect(boundLabel(undefined, "bytes")).toBe("Using Wyrd's default");
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

test("drive names format themselves into the registry token shape", () => {
  expect(normalizeDriveName("My Drive!")).toBe("my-drive");
  expect(normalizeDriveName("  Alpha--Beta  ")).toBe("alpha-beta");
  expect(normalizeDriveName("Éclair")).toBe("clair");
  expect(driveNameValid(normalizeDriveName("My Drive!"))).toBe(true);
  expect(driveNameValid("")).toBe(false);
  expect(driveNameValid("has space")).toBe(false);
  expect(driveNameValid("has_underscore")).toBe(false);
  // The registry reserves six characters for the service prefix.
  expect(normalizeDriveName("a".repeat(70)).length).toBe(58);
  expect(driveNameValid(normalizeDriveName("a".repeat(70)))).toBe(true);
});

test("store accounts derive one label per drive name", () => {
  expect(storeAccountFor("alpha")).toBe("wyrd-drive-alpha");
  expect(storeAccountFor("  beta-2  ")).toBe("wyrd-drive-beta-2");
  expect(storeAccountFor("")).toBe("wyrd-drive-alpha");
  expect(storeAccountFor("   ")).toBe("wyrd-drive-alpha");
});

test("the relay suggestion names the probed default set", () => {
  // The 2026-11-19 live policy probes picked these five (no auth on
  // gift-wrap reads, no payment, unrestricted writes, write accepted
  // and read back, 3/3 repeat consistency); the form pre-fills from
  // this constant, so the set pins here. A copy keeps RelayEditor
  // from mutating the shared list.
  expect(SUGGESTED_RELAYS).toEqual([
    "wss://relay.primal.net",
    "wss://relay.ditto.pub",
    "wss://purplerelay.com",
    "wss://relay.snort.social",
    "wss://offchain.pub",
  ]);
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

test("the advance decision needs a clean exit and a naming observation", () => {
  const betaDrive = { name: "beta", drive_dir: "/drives/beta" } as DriveView;
  const named: ProvisionReport = {
    result: { result: "completed", detail: "wrote" },
    observed: { status: "ready", drives: [betaDrive], skipped: [] },
    membership: null,
  };
  // Clean exit plus ready observation naming the drive: advance.
  expect(shouldAdvanceAfterProvision(named, "/drives/beta")).toBe(true);
  // Any other registry state keeps the form.
  expect(shouldAdvanceAfterProvision(named, "/drives/gamma")).toBe(false);
  expect(
    shouldAdvanceAfterProvision(
      { ...named, observed: { status: "nofile", drives: [], skipped: [] } },
      "/drives/beta",
    ),
  ).toBe(false);
  expect(
    shouldAdvanceAfterProvision(
      { ...named, observed: { status: "unreadable", reason: "denied", drives: [], skipped: [] } },
      "/drives/beta",
    ),
  ).toBe(false);
  // A refused or uncertain outcome never advances, even where the
  // observation still names the drive — its reason stays on the form.
  for (const result of [
    { result: "expectedfailure", detail: "refused" },
    { result: "helddrive", detail: "held" },
    { result: "versionmismatch", detail: "mismatch" },
    { result: "busy", drive_dir: "/drives/beta" },
    { result: "unknown", detail: "torn" },
  ] as ProvisionReport["result"][]) {
    expect(shouldAdvanceAfterProvision({ ...named, result }, "/drives/beta")).toBe(false);
  }
});

const emptyRegistry: RegistryState = { status: "ready", drives: [], skipped: [] };

function report(result: ProvisionReport["result"], membership: ProvisionReport["membership"] = null): ProvisionReport {
  return { result, observed: emptyRegistry, membership };
}

test("the removal decision is the observation, never the exit code alone", () => {
  const drive = { name: "beta", drive_dir: "/drives/beta" } as DriveView;
  const listed: RegistryState = { status: "ready", drives: [drive], skipped: [] };
  const tip = { epoch: 4, known_members: 2 };
  const completed: ProvisionReport["result"] = { result: "completed", detail: "removed" };

  // Full delete: the ready registry must no longer name the drive dir.
  expect(
    shouldAdvanceAfterRemoval({ result: completed, observed: emptyRegistry, membership: null }, "/drives/beta", false),
  ).toBe(true);
  expect(
    shouldAdvanceAfterRemoval({ result: completed, observed: listed, membership: null }, "/drives/beta", false),
  ).toBe(false);
  // A still-listed drive with no tip is not a confirmed leave.
  expect(
    shouldAdvanceAfterRemoval({ result: completed, observed: listed, membership: null }, "/drives/beta", true),
  ).toBe(false);
  // Keep-local leave: local data stays registered; the owner's tip
  // answering alongside the still-listed drive is the confirmation.
  expect(
    shouldAdvanceAfterRemoval({ result: completed, observed: listed, membership: tip }, "/drives/beta", true),
  ).toBe(true);
  // A leave that removed the registry entry is not this path.
  expect(
    shouldAdvanceAfterRemoval({ result: completed, observed: emptyRegistry, membership: tip }, "/drives/beta", true),
  ).toBe(false);

  // An unreadable registry proves nothing either way.
  const unreadable: RegistryState = { status: "unreadable", reason: "denied", drives: [], skipped: [] };
  expect(shouldAdvanceAfterRemoval({ result: completed, observed: unreadable, membership: tip }, "/drives/beta", false)).toBe(false);
  expect(shouldAdvanceAfterRemoval({ result: completed, observed: unreadable, membership: tip }, "/drives/beta", true)).toBe(false);

  // A refusal, held drive, or uncertain outcome never advances even
  // where the observation would otherwise allow it. The report stays
  // visible with a way back; a no-op is never a finished removal.
  for (const result of [
    { result: "expectedfailure", detail: "refused" },
    { result: "helddrive", detail: "held" },
    { result: "versionmismatch", detail: "mismatch" },
    { result: "busy", drive_dir: "/drives/beta" },
    { result: "unknown", detail: "torn" },
  ] as ProvisionReport["result"][]) {
    expect(
      shouldAdvanceAfterRemoval({ result, observed: emptyRegistry, membership: tip }, "/drives/beta", false),
    ).toBe(false);
    expect(
      shouldAdvanceAfterRemoval({ result, observed: listed, membership: tip }, "/drives/beta", true),
    ).toBe(false);
  }
});

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
    "Drives on this device: 0.",
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
    ["max_admit_per_pass", "Sync work limit", "items"],
    ["fetch_pass_budget_secs", "Download time per sync pass", "seconds"],
    ["max_mutation_wait_secs", "File change wait limit", "seconds"],
    ["open_timeout_secs", "File open wait limit", "seconds"],
  ]);
  // Descriptions say what the implementation enforces, beneath
  // each label — pinned here alongside the labels.
  expect(SETTING_BOUNDS.map((bound) => bound.description)).toEqual([
    "The most drive data Wyrd will keep on this device. When that limit is reached, Wyrd refuses new data before writing anything. This is not a limit on your disk's free space.",
    "How many items of sync work Wyrd starts in each sync pass. Anything beyond this waits for the next pass; nothing is dropped.",
    "How long Wyrd spends downloading in one sync pass. When that time runs out, unfinished downloads wait for the next pass.",
    "How long a change you make can wait for the content it needs from other devices. If that content does not arrive in time, the change fails and you can retry it; nothing is saved.",
    "How long Wyrd waits for a file's content to become available on this device when you open or read it. If it takes longer, the open or read fails and you can try again while the download continues.",
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

test("storage is entered and shown in GB, never as a raw byte count", () => {
  const storage = SETTING_BOUNDS[0]!;
  expect([storage.inputUnit, storage.inputScale]).toEqual(["GB", 1073741824]);
  expect(formatBoundValue(storage, null)).toBe("");
  expect(formatBoundValue(storage, 1073741824)).toBe("1");
  expect(formatBoundValue(storage, 1610612736)).toBe("1.5");
  // Whole-number bounds stay 1:1 with the file value.
  const work = SETTING_BOUNDS[1]!;
  expect([work.inputUnit, work.inputScale]).toEqual(["items", 1]);
  expect(formatBoundValue(work, 64)).toBe("64");

  expect(parseBoundInput("retained_bytes_quota", "1")).toEqual({ ok: true, raw: 1073741824 });
  expect(parseBoundInput("retained_bytes_quota", " 1.5 ")).toEqual({ ok: true, raw: 1610612736 });
  // A fraction that would not land on a whole number of bytes is refused,
  // and every refusal names the key rather than a remote component.
  expect(parseBoundInput("retained_bytes_quota", "0.0000000001").ok).toBe(false);
  expect(parseBoundInput("retained_bytes_quota", "abc")).toHaveProperty("error");
});

test("the limits form mirrors the bounds table, never its own wording", async () => {
  // One form now serves every observation state and iterates the
  // table, so labels and descriptions cannot drift per row. This
  // pins the iteration and that the table's descriptions render
  // verbatim in `SettingsView.svelte`.
  const view = await Bun.file(`${import.meta.dir}/SettingsView.svelte`).text();
  const source = await Bun.file(`${import.meta.dir}/format.ts`).text();
  expect(view).toContain("{#each SETTING_BOUNDS as bound (bound.key)}");
  expect(view).toContain("<DetailRow label={bound.label} description={bound.description}>");
  // The form renders the table's fields, so the approved descriptions
  // now live once in `format.ts` rather than duplicated in the view.
  for (const bound of SETTING_BOUNDS) {
    expect(source).toContain(bound.description);
  }
});
