// Wording pins for the frozen status terminology
// (`docs/design/product-terminology.md`, R-D2 and R-D6) and for the
// redesigned three-layer Status screen. Plain fact labels live in the
// view; the summary, attention and first-run sentences are pure
// helpers in `format.ts`, so both sources pin here. Retired names and
// the UTC clock wording pin absent.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const view = await Bun.file(`${import.meta.dir}/StatusView.svelte`).text();
const formatSrc = await Bun.file(`${import.meta.dir}/format.ts`).text();
const observed = await Bun.file(`${import.meta.dir}/ObservedNote.svelte`).text();
// Whitespace-normalized once for the retired-string pins, so a
// reintroduction with different line breaks cannot evade them.
const flat = view.replace(/\s+/g, " ");

test("status cards carry the frozen dimension names", () => {
  for (const title of ["Sync activity", "Drive activity", "Drive data"]) {
    expect(view).toContain(title);
  }
  expect(view).not.toContain("Durable observation");
  expect(flat).not.toMatch(/>\s*Liveness\s*</);
  expect(flat).not.toMatch(/>\s*Freshness\s*</);
});

test("status rows carry the frozen row names", () => {
  for (const label of [
    'label="Pending work"',
    'label="Local files"',
    'label="Published on last pass"',
  ]) {
    expect(view).toContain(label);
  }
  expect(flat).not.toMatch(/label\s*=\s*"Queue"/);
  expect(flat).not.toMatch(/label\s*=\s*"Materialized"/);
  expect(flat).not.toMatch(/label\s*=\s*"Last publish"/);
});

test("paused names the state without the quiet/down gloss", () => {
  expect(formatSrc).toContain("Synchronization is paused.");
  expect(formatSrc).toContain("The drive is still running and can answer requests.");
  expect(view + formatSrc).not.toContain("reachable and quiet");
  expect(view + formatSrc).not.toContain("quiet, not down");
});

test("load-bearing clauses pin verbatim, not just labels", () => {
  // The no-recency definition, the no-convergence note and the
  // no-fetchability caveat are the clauses a regression would drop
  // while keeping every label green.
  expect(view).toContain('Never "recent activity"');
  expect(view).toContain("does not mean converged with peers");
  expect(view).toContain("never read as available elsewhere");
  expect(view).toContain("not files or items");
});

test("technical detail lives behind a disclosure", () => {
  expect(view).toContain("Show technical detail");
  expect(view).toContain("Hide technical detail");
  // The mechanism counters moved out of the primary cards.
  for (const label of [
    'label="Durability"',
    'label="Converged"',
    'label="Passes"',
    'label="Revision / generation"',
    'label="Relay stream"',
  ]) {
    const at = view.indexOf(label);
    expect(at).toBeGreaterThan(view.indexOf("<Collapsible.Content>"));
  }
});

test("the summary is a sentence of separate facts", () => {
  expect(view).toContain("{summary}");
  expect(formatSrc).toContain("not shared with another device yet");
  expect(formatSrc).toContain("relays connected");
  // No verdict badge in the glance layer.
  expect(view).not.toContain("Converged</span>");
  expect(view).not.toContain("AvailabilityBadge");
});

test("attention renders only for observed problems", () => {
  expect(view).toContain("{#if attention.length > 0}");
  expect(view).toContain("{#each attention as item (item)}");
  expect(formatSrc).toContain("No relays connected at the last pass.");
  expect(formatSrc).toContain("The served data is stale:");
});

test("a new drive gets a first-run state, not a grid of zeros", () => {
  expect(view).toContain("{#if firstRun}");
  expect(view).toContain("This is a new drive");
  expect(formatSrc).toContain("facts.knownMembers === 1 && facts.peers === 0 && facts.freshEmpty");
});

test("observation ages are local, relative and auto-refreshing", () => {
  expect(observed).toContain("formatObservedAge");
  expect(observed).toContain("setInterval");
  expect(observed).toContain("clearInterval");
  expect(view + observed).not.toContain("UTC");
  expect(formatSrc).not.toContain("toISOString");
});

test("the Drive data publication row is local, relative and unknown at zero", () => {
  expect(view).toContain('label="Drive data last updated on this device"');
  expect(view).toContain("formatPublicationAge(status.freshness.published_unix_secs, nowMs)");
  expect(view).toContain("setInterval");
  expect(view).toContain("clearInterval");
  expect(view).not.toContain("Last synced");
  expect(view).not.toContain("UTC");
  expect(formatSrc).toContain("not yet updated on this device");
  expect(formatSrc).not.toContain("toISOString");
});

test("the Drive data retention row names drive data, never a disk figure", () => {
  expect(view).toContain('label="Drive data kept on this device"');
  expect(view).toContain("formatRetentionLine(status.observe.retention)");
  expect(view).toContain("Finder may show");
  expect(formatSrc).toContain("Using ${used} · no limit set");
  expect(formatSrc).toContain("Using ${used} of ${formatByteSize(retention.ceiling)}");
  expect(formatSrc).toContain('return "Not yet observed"');
  expect(view + formatSrc).not.toContain("disk usage");
  expect(view + formatSrc).not.toContain("disk space");
  expect(view).not.toContain("Last synced");
});

test("the empty state keeps the original visual rhythm", () => {
  expect(view).toContain("<Empty.Root");
  expect(view).toContain("bg-card/50 px-6 py-14");
  expect(view).toContain('<Empty.Media variant="icon" class="size-10">');
});
