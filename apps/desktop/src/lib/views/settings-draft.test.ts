// Pins for the pure settings-draft helpers
// (`./settings-draft.ts`). Draft rules are editor behavior, not
// component behavior — a regression here is a lost edit or a
// partial write, so the rules pin here.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";
import {
  buildDraftInputs,
  buildSparseDraft,
  isDraftDirty,
  keepsDraftAfterVerdict,
  toggleCleared,
} from "./settings-draft.js";
import { parseBoundInput, SETTING_BOUNDS } from "./format.js";

const KEYS = SETTING_BOUNDS.map((bound) => bound.key);

test("fresh drafts render configured values and leave absent keys empty", () => {
  const inputs = buildDraftInputs({ open_timeout_secs: 30 }, KEYS);
  expect(inputs["open_timeout_secs"]).toBe("30");
  expect(inputs["retained_bytes_quota"]).toBe("");
});

test("cleared keys toggle without touching the rest", () => {
  expect(toggleCleared([], "open_timeout_secs")).toEqual(["open_timeout_secs"]);
  expect(toggleCleared(["open_timeout_secs"], "open_timeout_secs")).toEqual([]);
});

test("an untouched draft is clean; edits and clears are dirty", () => {
  const configured = { open_timeout_secs: 30 };
  expect(isDraftDirty(buildDraftInputs(configured, KEYS), [], configured, KEYS)).toBe(false);
  expect(
    isDraftDirty({ ...buildDraftInputs(configured, KEYS), open_timeout_secs: "60" }, [], configured, KEYS),
  ).toBe(true);
  expect(isDraftDirty(buildDraftInputs(configured, KEYS), ["open_timeout_secs"], configured, KEYS)).toBe(true);
});

test("unchanged rows are skipped, not committed", () => {
  const configured = { open_timeout_secs: 30 };
  const draft = buildSparseDraft(
    buildDraftInputs(configured, KEYS),
    [],
    configured,
    KEYS,
    parseBoundInput,
  );
  expect(draft).toEqual({ set: {}, clear: [], errors: {} });
});

test("changed rows validate fully before writing — no partial saves", () => {
  const configured = { open_timeout_secs: 30, max_admit_per_pass: 64 };
  const inputs = { ...buildDraftInputs(configured, KEYS), open_timeout_secs: "60", max_admit_per_pass: "nope" };
  const draft = buildSparseDraft(inputs, [], configured, KEYS, parseBoundInput);
  expect(draft.set).toEqual({ open_timeout_secs: 60 });
  expect(draft.errors["max_admit_per_pass"]).toMatch("max_admit_per_pass");
});

test("cleared keys skip validation and ride the clear list", () => {
  const configured = { open_timeout_secs: 30 };
  const inputs = { ...buildDraftInputs(configured, KEYS), open_timeout_secs: "garbage" };
  const draft = buildSparseDraft(inputs, ["open_timeout_secs"], configured, KEYS, parseBoundInput);
  expect(draft).toEqual({ set: {}, clear: ["open_timeout_secs"], errors: {} });
});

test("storage reads and writes in GB, never in raw bytes", () => {
  const configured = { retained_bytes_quota: 1073741824 };
  const inputs = buildDraftInputs(configured, KEYS);
  // A stored 1 GiB shows as "1 GB"; displaying it is not an edit.
  expect(inputs["retained_bytes_quota"]).toBe("1");
  expect(isDraftDirty(inputs, [], configured, KEYS)).toBe(false);
  expect(buildSparseDraft(inputs, [], configured, KEYS, parseBoundInput)).toEqual({
    set: {},
    clear: [],
    errors: {},
  });
  // A typed GB value converts to the exact byte count the file takes.
  const edited = { ...inputs, retained_bytes_quota: "1.5" };
  expect(buildSparseDraft(edited, [], configured, KEYS, parseBoundInput).set).toEqual({
    retained_bytes_quota: 1610612736,
  });
});

test("only uncommitted verdicts keep the draft", () => {
  expect(keepsDraftAfterVerdict("applied")).toBe(false);
  expect(keepsDraftAfterVerdict("unverified")).toBe(false);
  expect(keepsDraftAfterVerdict("conflict")).toBe(true);
  expect(keepsDraftAfterVerdict("refused")).toBe(true);
  expect(keepsDraftAfterVerdict("failed")).toBe(true);
});
