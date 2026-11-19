// Wording and placement pins for the header Lock button
// (`App.svelte`). Lock is an action beside the Status | Settings
// tabs — never a third tab — and it shows only for an unlocked
// session, landing on Status where unlock lives.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";

const app = await Bun.file(`${import.meta.dir}/../../App.svelte`).text();

test("lock sits beside the tabs, gated on an unlocked session", () => {
  // Slice the header: between the tab list and the tab content.
  // The pins below only hold inside this slice, so moving the
  // button out of the header or dropping its guard fails them.
  const headerStart = app.indexOf('<Tabs.Trigger value="settings">Settings</Tabs.Trigger>');
  const headerEnd = app.indexOf('<Tabs.Content value="status">');
  expect(headerStart).toBeGreaterThan(-1);
  expect(headerEnd).toBeGreaterThan(headerStart);
  const header = app.slice(headerStart, headerEnd);
  expect(header).toContain("lockAndShowStatus(selectedDriveDir)");
  expect(header).toContain('selectedSession.state === "unlocked"');
  expect(header).toContain("LockIcon");
  expect(header).toMatch(/<Button[\s\S]*?LockIcon[\s\S]*?Lock[\s\S]*?<\/Button>/);
});

test("exactly one lock call site: definition plus the header button", () => {
  // A second lock button anywhere would add a third occurrence.
  expect(app.match(/lockAndShowStatus/g)?.length).toBe(2);
});

test("locking never claims a restart and points at typed unlock", () => {
  expect(app).toContain("unlocking needs the passphrase");
  expect(app).toContain("Lock from the header to end the session");
  expect(app).not.toContain("Lock for this session");
});
