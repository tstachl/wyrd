// Pure settings-draft logic for the bounds editor. No runes, no
// IPC, no owner facts — `SettingsView.svelte` calls these so the
// draft rules pin in `settings-draft.test.ts` instead of drifting
// per component.
//
// The draft is one pending sparse update: `inputs` holds the row
// text (empty means no change), `cleared` names keys the operator
// returned to the compiled default. Validation runs over the whole
// draft on save — no partial writes — and cancel discards it
// untouched.

/** One pending sparse update plus its validation errors. */
export interface SparseDraft {
  /** Bound values to set (validated whole numbers). */
  set: Record<string, number>;
  /** Bound names to clear back to the compiled default. */
  clear: string[];
  /** Key-named validation problems; empty means committable. */
  errors: Record<string, string>;
}

/**
 * Fresh row text from the latest observation: configured values
 * render as numbers, absent keys render empty (unknown, never a
 * filled-in default).
 */
export function buildDraftInputs(
  configured: Record<string, number | null>,
  keys: string[],
): Record<string, string> {
  const next: Record<string, string> = {};
  for (const key of keys) {
    const current = configured[key] ?? null;
    next[key] = current === null ? "" : String(current);
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
 * Whether the draft differs from the observation: a clear always
 * counts, otherwise a non-empty row counts when its text differs
 * from the configured value.
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
    return text !== String(configured[key] ?? "");
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
 * value), and every other changed row validates — errors keyed by
 * name, no partial writes. `validate` is the key-named validator
 * (`validateBoundInput` in the view).
 */
export function buildSparseDraft(
  inputs: Record<string, string>,
  cleared: string[],
  configured: Record<string, number | null>,
  keys: string[],
  validate: (key: string, text: string) => string | null,
): SparseDraft {
  const set: Record<string, number> = {};
  const clear: string[] = [...cleared];
  const errors: Record<string, string> = {};
  for (const key of keys) {
    if (cleared.includes(key)) continue;
    const text = (inputs[key] ?? "").trim();
    if (text === "") continue;
    if (text === String(configured[key] ?? "")) continue;
    const problem = validate(key, text);
    if (problem !== null) {
      errors[key] = problem;
      continue;
    }
    set[key] = Number(text);
  }
  return { set, clear, errors };
}
