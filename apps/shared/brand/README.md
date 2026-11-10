# Shared brand source (`apps/shared/brand/`)

Canonical Wyrd visual identity (the C · Held mark: Ink `#121614`,
Bone `#ECE7DA`, Amber `#E3A13B` + Smoke `#4A4F4B`, Space Grotesk
+ IBM Plex Mono):

- `held.svg` — the Held mark on an Ink tile (favicon shape).
- `fonts/` — self-hosted woff2 files, latin subsets.
- `tokens.json` — the hex values and display face in one place.

Consumers reference or vendor from here; nothing elsewhere in the
repo is a second source. `apps/desktop` imports these files
directly (Vite bundles them into `dist/`). `apps/marketing`
vendors plain copies when it lands (build-free static), with a
byte-equality check (`bun run apps/shared/check.ts`).
