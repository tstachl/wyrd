import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";

// Wyrd desktop frontend: Svelte 5 + Tailwind 4, bundled to `dist/`
// for the Tauri backend (`frontendDist` in
// `crates/wyrd-desktop/tauri.conf.json`). No dev-server URL is
// wired into Tauri: `bun run dev` watch-builds `dist/` and Tauri
// serves it. `bun run dev:ui` starts the Vite dev server for
// component iteration without the backend.
export default defineConfig({
  plugins: [svelte(), tailwindcss()],
  clearScreen: false,
  resolve: {
    alias: {
      $lib: fileURLToPath(new URL("./src/lib", import.meta.url)),
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "esnext",
  },
});
