# Desktop frontend (`apps/desktop/`)

Toolchain-pure web directory (bun, matching the `languages.javascript.bun`
toolchain in `devenv.nix`): no Rust code, no Cargo membership, invisible
to the dependency-DAG checker by construction. No deno anywhere: this
workspace's JavaScript toolchain is bun.

Framework decision (v0.4): Svelte 5 + shadcn-svelte, no SvelteKit.
The file/member/conflict/vault/status surface warranted component
composition, reactive presentation state, and accessible primitives;
a local desktop app needs none of SSR, routing infrastructure, or
server endpoints, so SvelteKit stays out. `components.json` records
the shadcn-svelte paths so further primitives arrive via its CLI.

- The views live here and talk to the backend through Tauri IPC
  only — never to the socket, never to the wire bytes.
  `src/lib/ipc/client.ts` is the single invoke path; per-operation
  wrappers land with the Tauri command layer (first view), whose
  argument names do not exist yet. The backend answer taxonomy is
  mirrored in `src/lib/ipc/types.ts` exactly (`kind`-tagged,
  refusal codes verbatim); transport failure is `IpcTransportError`
  and is never mapped into the taxonomy.
- `src/App.svelte` is the shell entry (`src/main.ts` mounts it);
  frontend state is a presentation cache
  (`src/lib/state/presentation.svelte.ts`), never a second drive
  model. The unavailable state stays a first-class view.
- Build with `bun run build` (emits `dist/`), develop with
  `bun run dev` (watch-builds `dist/`). Both are wired from the backend
  crate's `tauri.conf.json` (`frontendDist` points at `dist/`): Tauri
  serves `dist/` itself, so no dev-server URL is configured until a
  view defines a dev server. `bun run dev:ui` starts the Vite dev
  server for component iteration without the backend.
- Gate on `bun run typecheck` (`svelte-check`): `vite build`
  transpiles only and does not typecheck. No `any` at IPC
  boundaries; the four variants are handled exhaustively.
- No mocks that invent interface semantics in tests. Fixture
  values in `src/lib/ipc/errors.test.ts` (`bun test`) are copied
  from the Rust taxonomy test; full Rust-to-TS wire agreement
  lands with the command layer.
- Visual identity follows the C · Held mark, canonical in
  `apps/shared/brand/` (`held.svg`, `fonts/`, `tokens.json`: Ink
  `#121614`, Bone `#ECE7DA`, Amber `#E3A13B` + Smoke `#4A4F4B`).
  Amber is the sole accent. Type is Space Grotesk, self-hosted and
  bundled by Vite; IBM Plex Mono for machine text arrives with the
  first view that renders hashes. The Held mark component
  (`src/lib/components/held-mark.svelte`) reuses the logo geometry
  verbatim.
- Production frontend is static, locally bundled assets only: no
  remote frontend, CDN, external app server, or remote fonts.
