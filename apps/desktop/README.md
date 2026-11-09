# Desktop frontend (`apps/desktop/`)

Toolchain-pure web directory (bun, matching the `languages.javascript.bun`
toolchain in `devenv.nix`): no Rust code, no Cargo membership, invisible
to the dependency-DAG checker by construction. No deno anywhere: this
workspace's JavaScript toolchain is bun.

- The three views live here and talk to the backend through Tauri IPC
  only — never to the socket, never to the wire bytes.
- `src/main.ts` is the scaffold entry point; views land here per
  `docs/design/desktop-shell.md` stage 2.
- Build with `bun run build` (emits `dist/`), develop with
  `bun run dev` (rebuilds on change). Both are wired from the backend
  crate's `tauri.conf.json` (`frontendDist` points at `dist/`).
- No framework without warranted complexity: three views do not
  warrant one. No mocks that invent interface semantics in tests.
