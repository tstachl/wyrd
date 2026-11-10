# Shared brand source (`apps/shared/brand/`)

Canonical Wyrd visual identity (the C · Held mark: Ink `#121614`,
Bone `#ECE7DA`, Amber `#E3A13B` + Smoke `#4A4F4B`, Space Grotesk):

- `held.svg` — the Held mark on an Ink tile (favicon shape).
- `fonts/` — self-hosted woff2 files, latin subsets.
- `tokens.json` — the hex values and display face in one place.

Consumers reference or vendor from here; nothing elsewhere in the
repo is a second source. `apps/desktop` imports these files
directly (Vite bundles them into `dist/`). `apps/marketing`
vendors plain copies when it lands (build-free static), with a
byte-equality check — see `nostr:nevent1qqstmkvak4hc3u4pnfd5qndn3gdk8g2u2s0x6c9z2re9rw5tjf43jmspz9mhxue69uhkwunpwdczuap49eehglrc7v3`.
