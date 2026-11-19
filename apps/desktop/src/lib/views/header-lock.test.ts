// Wording and placement pins for the header Lock button
// (`App.svelte`). Lock is an action beside the Status | Settings
// tabs — never a third tab — and it shows only for an unlocked
// session, landing on Status where unlock lives.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const app = await Bun.file(`${import.meta.dir}/../../App.svelte`).text();

test("lock sits beside the tabs, gated on an unlocked session", () => {
  expect(app).toContain('<Tabs.Trigger value="settings">Settings</Tabs.Trigger>');
  expect(app).toContain("LockIcon");
  expect(app).toContain('selectedSession.state === "unlocked"');
  expect(app).toContain("lockAndShowStatus(selectedDriveDir)");
});

test("locking never claims a restart and points at typed unlock", () => {
  expect(app).toContain("unlocking needs the passphrase");
  expect(app).toContain("Lock from the header to end the session");
  expect(app).not.toContain("Lock for this session");
});
