// Per-operation wrapper checks (`./operations.ts`) through the
// mocked Tauri bridge. The mock stands in for the transport only:
// it asserts the command name and arguments each wrapper sends
// and returns backend-shaped payloads verbatim. No argument shape
// here is invented — names and shapes come from
// `crates/wyrd-desktop/src/commands.rs`.
//
// Run: `bun run test` from `apps/desktop/`.

import { beforeEach, expect, test } from "bun:test";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import {
  applySettings,
  captureParent,
  driveLock,
  driveSettings,
  driveUnlock,
  listDrives,
  lookup,
  observe,
  onboardingAdopt,
  onboardingCommit,
  onboardingDefaults,
  onboardingNewPassphrase,
  onboardingVerify,
  provisionCreate,
  provisionJoin,
  provisionLeave,
  provisionReconfigure,
  reconnect,
  revealInFolder,
  submit,
} from "./operations.js";
import { IpcTransportError } from "./client.js";

beforeEach(() => {
  clearMocks();
});

test("lookup sends the command name and path", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    return [
      { File: { size: 11, executable: false, chunks: ["9be2"] } },
      3, 7,
    ];
  });
  const result = await lookup("/drives/alpha", "notes.txt");
  expect(seen).toEqual([
    { cmd: "lookup", args: { driveDir: "/drives/alpha", path: "notes.txt" } },
  ]);
  expect(result.ok).toBe(true);
});

test("observe sends no arguments", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    throw { kind: "unavailable", detail: "no owner" };
  });
  const result = await observe("/drives/alpha");
  expect(seen).toEqual([{ cmd: "observe", args: { driveDir: "/drives/alpha" } }]);
  expect(result).toEqual({
    ok: false,
    error: { kind: "unavailable", detail: "no owner" },
  });
});

test("submit forwards the mutation kind verbatim", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    return "Done";
  });
  const result = await submit("/drives/alpha", { Unlink: { path: "notes.txt" } });
  expect(seen).toEqual([
    {
      cmd: "submit",
      args: { driveDir: "/drives/alpha", kind: { Unlink: { path: "notes.txt" } } },
    },
  ]);
  expect(result).toEqual({ ok: true, value: "Done" });
});

test("captureParent names the parent, reconnect names the drive", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    if (cmd === "capture_parent") return { instance: 2, counter: 5 };
    return null;
  });
  expect(await captureParent("/drives/alpha", "")).toEqual({
    ok: true,
    value: { instance: 2, counter: 5 },
  });
  expect(await reconnect("/drives/alpha")).toEqual({ ok: true, value: null });
  expect(seen).toEqual([
    { cmd: "capture_parent", args: { driveDir: "/drives/alpha", path: "" } },
    { cmd: "reconnect", args: { driveDir: "/drives/alpha" } },
  ]);
});

test("listDrives sends no arguments and returns the state struct", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    return { status: "nofile", drives: [], skipped: [] };
  });
  expect(await listDrives()).toEqual({ status: "nofile", drives: [], skipped: [] });
  expect(seen).toEqual([{ cmd: "list_drives", args: {} }]);
});

test("driveSettings returns configured bounds verbatim", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    return {
      state: "configured",
      retained_bytes_quota: 0,
      max_admit_per_pass: 50,
      fetch_pass_budget_secs: null,
      max_mutation_wait_secs: null,
      open_timeout_secs: 45,
    };
  });
  expect(await driveSettings("/drives/alpha")).toEqual({
    state: "configured",
    retained_bytes_quota: 0,
    max_admit_per_pass: 50,
    fetch_pass_budget_secs: null,
    max_mutation_wait_secs: null,
    open_timeout_secs: 45,
  });
  expect(seen).toEqual([
    { cmd: "drive_settings", args: { driveDir: "/drives/alpha" } },
  ]);
});

test("local-query rejections throw a transport error, never taxonomy", async () => {
  mockIPC(() => {
    throw { kind: "unavailable", detail: "no owner" };
  });
  // A rejection here is a shell bug, not an owner answer: even a
  // taxonomy-shaped payload throws instead of masquerading as one.
  // The mock stands in for the transport; the backend never
  // rejects these commands with the taxonomy.
  const seen = await listDrives().then(
    () => null,
    (error: unknown) => error,
  );
  expect(seen).toBeInstanceOf(IpcTransportError);
});

test("provisioning wrappers send one args object per command", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    return {
      result: { result: "completed", detail: "ready" },
      observed: { status: "nofile", drives: [], skipped: [] },
    };
  });
  const created = await provisionCreate({
    name: "alpha",
    driveDir: "/drives/alpha",
    relays: ["wss://relay.example"],
    identityFile: "/keys/identity",
    passphraseFile: "/keys/passphrase",
    mode: "vault",
    noService: false,
  });
  expect(created.observed.status).toBe("nofile");
  const joined = await provisionJoin({
    name: "beta",
    driveDir: "/drives/beta",
    relays: [],
    identityFile: "/keys/identity",
    passphraseFile: "/keys/passphrase",
    invitationFile: "/tmp/invite.sealed",
    mode: "vault",
    noService: true,
  });
  expect(joined.result.result).toBe("completed");
  const left = await provisionLeave({
    driveDir: "/drives/alpha",
    identityFile: "/keys/identity",
    passphraseFile: "/keys/passphrase",
    acknowledged: true,
  });
  expect(left.result.result).toBe("completed");
  expect(seen).toEqual([
    {
      cmd: "provision_create",
      args: {
        args: {
          name: "alpha",
          driveDir: "/drives/alpha",
          relays: ["wss://relay.example"],
          identityFile: "/keys/identity",
          passphraseFile: "/keys/passphrase",
          mode: "vault",
          noService: false,
        },
      },
    },
    {
      cmd: "provision_join",
      args: {
        args: {
          name: "beta",
          driveDir: "/drives/beta",
          relays: [],
          identityFile: "/keys/identity",
          passphraseFile: "/keys/passphrase",
          invitationFile: "/tmp/invite.sealed",
          mode: "vault",
          noService: true,
        },
      },
    },
    {
      cmd: "provision_leave",
      args: {
        args: {
          driveDir: "/drives/alpha",
          identityFile: "/keys/identity",
          passphraseFile: "/keys/passphrase",
          acknowledged: true,
        },
      },
    },
  ]);
});

test("provisioning wrappers send the store reference for os-store drives", async () => {
  // Wire-agreement pin for the store backend: the camelCase keys
  // and the file-or-reference shapes below must match
  // `crates/wyrd-desktop/src/commands.rs` verbatim — types alone
  // do not prove the boundary agrees.
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    return {
      result: { result: "completed", detail: "ok" },
      observed: { status: "nofile", drives: [], skipped: [] },
      membership: null,
    };
  });
  // A fresh store-backed create seeds from the input files.
  await provisionCreate({
    name: "alpha",
    driveDir: "/drives/alpha",
    relays: [],
    identityFile: "/keys/identity",
    passphraseFile: "/keys/passphrase",
    credentialBackend: "os-store",
    credentialAccount: "wyrd-drive-alpha",
    mode: "vault",
    noService: false,
  });
  // A converged store-backed join names only the item.
  await provisionJoin({
    name: "beta",
    driveDir: "/drives/beta",
    relays: [],
    credentialBackend: "os-store",
    credentialAccount: "wyrd-drive-beta",
    invitationFile: "/tmp/invite.sealed",
    mode: "vault",
    noService: true,
  });
  await provisionLeave({
    driveDir: "/drives/alpha",
    credentialBackend: "os-store",
    credentialAccount: "wyrd-drive-alpha",
    acknowledged: true,
  });
  await provisionReconfigure({
    driveDir: "/drives/alpha",
    relays: [],
    credentialBackend: "os-store",
    credentialAccount: "wyrd-drive-alpha",
    mode: "vault",
    noService: true,
  });
  await driveUnlock({
    driveDir: "/drives/alpha",
    credentialBackend: "os-store",
    credentialAccount: "wyrd-drive-alpha",
    passphrase: "hunter2-hunter2",
  });
  expect(seen).toEqual([
    {
      cmd: "provision_create",
      args: {
        args: {
          name: "alpha",
          driveDir: "/drives/alpha",
          relays: [],
          identityFile: "/keys/identity",
          passphraseFile: "/keys/passphrase",
          credentialBackend: "os-store",
          credentialAccount: "wyrd-drive-alpha",
          mode: "vault",
          noService: false,
        },
      },
    },
    {
      cmd: "provision_join",
      args: {
        args: {
          name: "beta",
          driveDir: "/drives/beta",
          relays: [],
          credentialBackend: "os-store",
          credentialAccount: "wyrd-drive-beta",
          invitationFile: "/tmp/invite.sealed",
          mode: "vault",
          noService: true,
        },
      },
    },
    {
      cmd: "provision_leave",
      args: {
        args: {
          driveDir: "/drives/alpha",
          credentialBackend: "os-store",
          credentialAccount: "wyrd-drive-alpha",
          acknowledged: true,
        },
      },
    },
    {
      cmd: "provision_reconfigure",
      args: {
        args: {
          driveDir: "/drives/alpha",
          relays: [],
          credentialBackend: "os-store",
          credentialAccount: "wyrd-drive-alpha",
          mode: "vault",
          noService: true,
        },
      },
    },
    {
      cmd: "drive_unlock",
      args: {
        args: {
          driveDir: "/drives/alpha",
          credentialBackend: "os-store",
          credentialAccount: "wyrd-drive-alpha",
          passphrase: "hunter2-hunter2",
        },
      },
    },
  ]);
});

test("onboarding wrappers send paths-only args plus the stdin-pipe passphrase", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    if (cmd === "onboarding_defaults") {
      return {
        identity_file: "/k/identity",
        passphrase_file: "/k/passphrase",
        identity_exists: false,
        passphrase_exists: false,
      };
    }
    return {
      result: { result: "completed", detail: "ok", passphrase: null },
      defaults: {
        identity_file: "/k/identity",
        passphrase_file: "/k/passphrase",
        identity_exists: true,
        passphrase_exists: true,
      },
    };
  });
  const defaults = await onboardingDefaults();
  expect(defaults.identity_exists).toBe(false);
  // new-passphrase sends nothing but the command: writes nothing.
  await onboardingNewPassphrase();
  // commit sends paths plus the passphrase text (stdin pipe, never argv).
  await onboardingCommit({
    identityFile: "/k/identity",
    passphraseFile: "/k/passphrase",
    passphrase: "abcd6e-789abc-d6e789-abcd6e",
  });
  // verify sends paths only, never contents; adopt records the
  // verified pair so the gate resolves it after the session.
  await onboardingVerify({ identityFile: "/k/identity", passphraseFile: "/k/passphrase" });
  await onboardingAdopt({ identityFile: "/k/identity", passphraseFile: "/k/passphrase" });
  await revealInFolder("/k/identity");
  expect(seen).toEqual([
    { cmd: "onboarding_defaults", args: {} },
    { cmd: "onboarding_new_passphrase", args: {} },
    {
      cmd: "onboarding_commit",
      args: {
        args: {
          identityFile: "/k/identity",
          passphraseFile: "/k/passphrase",
          passphrase: "abcd6e-789abc-d6e789-abcd6e",
        },
      },
    },
    {
      cmd: "onboarding_verify",
      args: { args: { identityFile: "/k/identity", passphraseFile: "/k/passphrase" } },
    },
    {
      cmd: "onboarding_adopt",
      args: { args: { identityFile: "/k/identity", passphraseFile: "/k/passphrase" } },
    },
    { cmd: "reveal_in_folder", args: { args: { path: "/k/identity" } } },
  ]);
});

test("applySettings sends one drive plus one sparse update", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    return {
      result: { result: "applied", detail: "settings applied" },
      observed: {
        state: "configured",
        retained_bytes_quota: null,
        max_admit_per_pass: 64,
        fetch_pass_budget_secs: null,
        max_mutation_wait_secs: null,
        open_timeout_secs: 30,
      },
    };
  });
  const report = await applySettings("/drives/alpha", {
    set: { max_admit_per_pass: 64 },
    clear: ["open_timeout_secs"],
  });
  expect(report.result).toEqual({ result: "applied", detail: "settings applied" });
  expect(seen).toEqual([
    {
      cmd: "drive_settings_write",
      args: {
        driveDir: "/drives/alpha",
        update: { set: { max_admit_per_pass: 64 }, clear: ["open_timeout_secs"] },
      },
    },
  ]);
});

test("driveUnlock sends the passphrase in args, driveLock names the drive", async () => {
  const seen: Array<{ cmd: string; args: unknown }> = [];
  mockIPC((cmd, args) => {
    seen.push({ cmd, args });
    return {
      result: { result: "completed", detail: "owner answering" },
      observed: { status: "nofile", drives: [], skipped: [] },
      membership: null,
    };
  });
  const unlocked = await driveUnlock({
    driveDir: "/drives/alpha",
    identityFile: "/keys/identity",
    passphrase: "hunter2-hunter2",
  });
  expect(unlocked.result.result).toBe("completed");
  const locked = await driveLock({ driveDir: "/drives/alpha" });
  expect(locked.result.result).toBe("completed");
  expect(seen).toEqual([
    {
      cmd: "drive_unlock",
      args: {
        args: {
          driveDir: "/drives/alpha",
          identityFile: "/keys/identity",
          passphrase: "hunter2-hunter2",
        },
      },
    },
    {
      cmd: "drive_lock",
      args: { args: { driveDir: "/drives/alpha" } },
    },
  ]);
});
