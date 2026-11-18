// Minimal browser shim so `@tauri-apps/api/mocks` runs under
// `bun test`. Only what the mock bridge touches: `window` itself
// and `window.crypto.getRandomValues`. No DOM, no test framework.

Object.defineProperty(globalThis, "window", {
  value: { crypto: globalThis.crypto },
  writable: true,
  configurable: true,
});

// Svelte component loader for behavioral tests (see
// `src/lib/views/unlock-behavior.test.ts`). Compiles `.svelte`
// sources to DOM-target JS on import: TypeScript script blocks are
// stripped with esbuild first (the Svelte compiler takes JS), then
// compiled with `generate: "dom"`. Test files pair this with
// `@happy-dom/global-registrator` per file, so the DOM exists only
// where a component mounts — pure-logic tests never see it.
import { plugin } from "bun";
import { preprocess, compile, compileModule } from "svelte/compiler";
import { transformSync } from "esbuild";
import { readFileSync } from "fs";

plugin({
  name: "wyrd svelte test loader",
  setup(builder) {
    // `bun test` resolves the bare `svelte` specifier to the
    // server build (`default` export condition), whose `mount` and
    // runes throw. Mounted components need the client build —
    // serve it for the server entry. (`onResolve` does not fire
    // for resolved packages at runtime here; `onLoad` does.)
    // Subpath imports (`svelte/compiler`, `svelte/internal/*`)
    // keep their own resolution.
    builder.onLoad({ filter: /svelte\/src\/index-server\.js$/ }, ({ path }) => {
      const client = path.replace("index-server.js", "index-client.js");
      return { contents: readFileSync(client, "utf-8"), loader: "js" };
    });
    builder.onLoad({ filter: /\.svelte(\.[jt]s)?(\?[^]*)?$/ }, async ({ path }) => {
      const filename = path.includes("?")
        ? path.slice(0, path.indexOf("?"))
        : path;
      try {
        let source = readFileSync(filename, "utf-8");
        // Rune modules (`.svelte.js/ts`) and TS script blocks need
        // JS first: the Svelte compiler takes JS. Components go
        // through preprocess + DOM compile; rune modules through
        // the module compiler.
        if (filename.endsWith(".svelte.ts")) {
          // `verbatimModuleSyntax`: preserve value imports (template
          // usage is invisible to esbuild, so the default elision
          // would drop components the markup names) while still
          // erasing types and interfaces.
          source = transformSync(source, {
            loader: "ts",
            tsconfigRaw: JSON.stringify({ compilerOptions: { verbatimModuleSyntax: true } }),
          }).code;
        }
        if (filename.endsWith(".svelte.js") || filename.endsWith(".svelte.ts")) {
          const result = compileModule(source, { filename, dev: false });
          return { contents: result.js.code, loader: "js" };
        }
        const preprocessed = await preprocess(
          source,
          {
            script: ({ content, attributes }) => {
              if (attributes.lang === "ts") {
                // `verbatimModuleSyntax`: preserve value imports
                // (template usage is invisible to esbuild, so the
                // default elision would drop components the markup
                // names) while still erasing types and interfaces.
                return {
                  code: transformSync(content, {
                    loader: "ts",
                    tsconfigRaw: JSON.stringify({
                      compilerOptions: { verbatimModuleSyntax: true },
                    }),
                  }).code,
                };
              }
              return { code: content };
            },
          },
          { filename },
        );
        const result = compile(preprocessed.code, {
          filename,
          generate: "client",
          dev: false,
        });
        return { contents: result.js.code, loader: "js" };
      } catch (error: unknown) {
        throw new Error(
          `Failed to compile Svelte component ${filename}: ${error instanceof Error ? error.message : String(error)}`,
        );
      }
    });
  },
});
