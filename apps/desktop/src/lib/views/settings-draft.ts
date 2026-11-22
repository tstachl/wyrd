// Pure settings-draft logic for the limits editor. No runes, no
// IPC, no owner facts — `SettingsView.svelte` calls these so the
// draft rules pin in `settings-draft.test.ts` instead of drifting
// per component.
//
// The draft is one pending sparse update: `inputs` holds each field's
// human-unit text (empty means no change), `cleared` names keys the
// operator returned to Wyrd's default. The text is converted to the
// file's raw units on build, so a storage field showing "1 GB" writes
// the byte count the CLI takes. Validation runs over the whole draft
// on save — no partial writes — and cancel discards it untouched.

import {
  boundFor,
  formatBoundValue,
  parseBoundInput,
  type ParsedBound,
} from "./format.js";

/** One pending sparse update plus its validation errors. */
export interface SparseDraft {
  /** Raw bound values to set (converted from the input units). */
  set: Record<string, number>;
  /** Bound names to clear back to Wyrd's default. */
  clear: string[];
  /** Key-named validation problems; empty means committable. */
  errors: Record<string, string>;
}

/**
 * Fresh input text from the latest observation: configured values
 * render in the bound's human unit, absent keys render empty (a
 * default, never a filled-in number).
 */
export function buildDraftInputs(
  configured: Record<string, number | null>,
  keys: string[],
): Record<string, string> {
  const next: Record<string, string> = {};
  for (const key of keys) {
    const bound = boundFor(key);
    next[key] = bound === null ? "" : formatBoundValue(bound, configured[key]);
  }
  return next;
}

/** Toggle one key's cleared state, returning the new list. */
export function toggleCleared(cleared: string[], key: string): string[] {
  if (cleared.includes(key)) {
    return cleared.filter((candidate) => candidate !== key);
  }
  return [...cleared, key];
}

/**
 * Whether the draft differs from the observation. A clear always
 * counts. A non-empty row counts when it cannot be parsed (so the
 * error is reachable) or when its parsed raw value differs from the
 * configured one; empty rows are untouched.
 */
export function isDraftDirty(
  inputs: Record<string, string>,
  cleared: string[],
  configured: Record<string, number | null>,
  keys: string[],
): boolean {
  if (cleared.length > 0) return true;
  return keys.some((key) => {
    const text = (inputs[key] ?? "").trim();
    if (text === "") return false;
    const parsed = parseBoundInput(key, text);
    if (!parsed.ok) return true;
    return parsed.raw !== configured[key];
  });
}

/**
 * Whether the editor keeps its draft after a save verdict. Only
 * uncommitted outcomes keep it: applied and unverified both
 * committed (the latter's bytes landed, only the read-back
 * failed), so both discard the draft and show the fresh
 * observation; conflict, refusal, and failure keep the draft for
 * retry.
 */
export type SaveVerdict = "applied" | "unverified" | "conflict" | "refused" | "failed";

export function keepsDraftAfterVerdict(verdict: SaveVerdict): boolean {
  return verdict !== "applied" && verdict !== "unverified";
}

/**
 * Build one committable sparse update from the draft: unchanged
 * rows are skipped, cleared keys skip validation (they carry no
 * value), and every other changed row parses — errors keyed by
 * name, no partial writes. `parse` is injected (`parseBoundInput`
 * in the view) so the rule pins without importing the view.
 */
export function buildSparseDraft(
  inputs: Record<string, string>,
  cleared: string[],
  configured: Record<string, number | null>,
  keys: string[],
  parse: (key: string, text: string) => ParsedBound,
): SparseDraft {
  const set: Record<string, number> = {};
  const clear: string[] = [...cleared];
  const errors: Record<string, string> = {};
  for (const key of keys) {
    if (cleared.includes(key)) continue;
    const text = (inputs[key] ?? "").trim();
    if (text === "") continue;
    const parsed = parse(key, text);
    if (!parsed.ok) {
      errors[key] = parsed.error;
      continue;
    }
    if (parsed.raw === configured[key]) continue;
    set[key] = parsed.raw;
  }
  return { set, clear, errors };
}
