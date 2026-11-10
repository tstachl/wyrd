// apps/shared/check.ts — pin the shared brand source to its consumers.
//
// `apps/shared/brand/` is the one brand source of truth (the C · Held
// mark: Ink #121614, Bone #ECE7DA, Amber #E3A13B + Smoke #4A4F4B,
// Space Grotesk + IBM Plex Mono). Consumers hold copies or direct
// imports; this checker keeps them from drifting silently:
//
// - Marketing stays build-free static, so it vendors plain copies of
//   the brand fonts. Each copy must stay byte-identical to the brand
//   file. (The marketing favicon is a deliberate Bone-on-Ink variant of
//   the Amber-on-Ink Held mark, so SVGs pin shared geometry, not
//   bytes: the mark path data must appear in `held.svg`,
//   `favicon.svg`, and the desktop's inlined `held-mark.svelte`.)
// - Desktop imports the brand font directly (Vite bundles it into
//   `dist/`), so the check pins the import path instead of a copy.
// - Both consumers' CSS must contain every token hex, so a token edit
//   that misses a consumer fails here instead of shipping two palettes.
//
// Verify:  bun run apps/shared/check.ts

const root = decodeURIComponent(new URL("../../", import.meta.url).pathname);

const failures: string[] = [];
const fail = (message: string): void => {
  failures.push(message);
};

const bytes = async (rel: string): Promise<Uint8Array> =>
  new Uint8Array(await Bun.file(root + rel).arrayBuffer());

const text = (rel: string): Promise<string> =>
  Bun.file(root + rel).text();

// Marketing's vendored copies must stay byte-identical to the brand
// source. Desktop needs no copies: it imports the brand files
// directly (pinned below).
const binaries = [
  "fonts/space-grotesk.woff2",
  "fonts/ibm-plex-mono-400.woff2",
  "fonts/ibm-plex-mono-500.woff2",
];
for (const file of binaries) {
  const brand = await bytes(`apps/shared/brand/${file}`);
  const copy = await bytes(`apps/marketing/${file}`);
  if (brand.length !== copy.length || !brand.every((b, i) => b === copy[i])) {
    fail(`apps/marketing/${file} drifts from apps/shared/brand/${file}`);
  }
}

// The Held geometry is one path shared three ways: the canonical
// `held.svg`, the marketing favicon variant, and the desktop's
// inlined component. The path data must read identically in all
// three; only paint (Amber vs Bone) may differ.
const mark = "M48 62 L88 211.3 L128 62 L168 211.3 L208 62";
for (const rel of [
  "apps/shared/brand/held.svg",
  "apps/marketing/favicon.svg",
  "apps/desktop/src/lib/components/held-mark.svelte",
]) {
  if (!(await text(rel)).includes(mark)) {
    fail(`${rel} no longer carries the Held mark geometry`);
  }
}

// Desktop imports the brand font directly instead of vendoring a
// copy; pin the import so a move to a second source fails here.
const desktopCss = await text("apps/desktop/src/app.css");
if (!desktopCss.includes("shared/brand/fonts/space-grotesk.woff2")) {
  fail("apps/desktop/src/app.css no longer imports the brand font directly");
}

// Every token hex must appear in both consumers' stylesheets
// (case-insensitive: the desktop theme uses lowercase hex). A token
// edit that misses a consumer ships two palettes; this fails first.
const tokens = ["#121614", "#ECE7DA", "#E3A13B", "#4A4F4B"];
const stylesheets = ["apps/desktop/src/app.css", "apps/marketing/index.html"];
for (const rel of stylesheets) {
  const lower = (await text(rel)).toLowerCase();
  for (const token of tokens) {
    if (!lower.includes(token.toLowerCase())) {
      fail(`${rel} misses the ${token} token`);
    }
  }
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.error(`shared-check: ${failure}`);
  }
  process.exit(1);
}
console.log(
  `shared-check: ${binaries.length} binaries pinned, mark geometry in 3 files, ${tokens.length} tokens in ${stylesheets.length} stylesheets`
);
