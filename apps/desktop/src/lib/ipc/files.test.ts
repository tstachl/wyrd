// File-picker fallback checks (`./files.ts`). The native picker is
// a Tauri-only capability: outside the Tauri runtime (bun tests,
// browser dev) the helper resolves `null` instead of throwing, and
// the provisioning forms keep their manual path inputs working.

import { expect, test } from "bun:test";
import { pickExistingFile } from "./files.js";

test("no native picker outside Tauri resolves null without throwing", async () => {
  const picked = await pickExistingFile("Identity file");
  expect(picked).toBeNull();
});
