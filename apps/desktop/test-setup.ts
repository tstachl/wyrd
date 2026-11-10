// Minimal browser shim so `@tauri-apps/api/mocks` runs under
// `bun test`. Only what the mock bridge touches: `window` itself
// and `window.crypto.getRandomValues`. No DOM, no test framework.

Object.defineProperty(globalThis, "window", {
  value: { crypto: globalThis.crypto },
  writable: true,
  configurable: true,
});
