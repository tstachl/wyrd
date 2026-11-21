// Behavior pins for the provisioning argument builders
// (`./provisioning-args.ts`). Copy pins cover the toggle wording;
// these pin the actual IPC payloads the flip submits: store mode
// names `credentialBackend: "os-store"` plus the derived account
// (seeds beside a fresh bootstrap, never beside leave), file mode
// carries the picked paths and no backend keys.
//
// Run: `bun run test` from `apps/desktop/`.

import { expect, test } from "bun:test";
import { buildCreateArgs, buildJoinArgs, buildLeaveArgs } from "./provisioning-args.js";

test("store create seeds the item and names the reference", () => {
  expect(
    buildCreateArgs({
      name: "alpha",
      driveDir: "/drives/alpha",
      relays: ["wss://relay.example"],
      credentialMode: "store",
      storeAccount: "wyrd-drive-alpha",
      seedIdentityFile: "/keys/identity",
      seedPassphraseFile: "/keys/passphrase",
      fileIdentityFile: "/operator/identity",
      filePassphraseFile: "/operator/passphrase",
    }),
  ).toEqual({
    name: "alpha",
    driveDir: "/drives/alpha",
    relays: ["wss://relay.example"],
    identityFile: "/keys/identity",
    passphraseFile: "/keys/passphrase",
    credentialBackend: "os-store",
    credentialAccount: "wyrd-drive-alpha",
    mode: "vault",
    noService: false,
  });
});

test("file create carries the picked paths and no backend keys", () => {
  expect(
    buildCreateArgs({
      name: "alpha",
      driveDir: "/drives/alpha",
      relays: ["wss://relay.example"],
      credentialMode: "file",
      storeAccount: "wyrd-drive-alpha",
      seedIdentityFile: "/keys/identity",
      seedPassphraseFile: "/keys/passphrase",
      fileIdentityFile: "/operator/identity",
      filePassphraseFile: "/operator/passphrase",
    }),
  ).toEqual({
    name: "alpha",
    driveDir: "/drives/alpha",
    relays: ["wss://relay.example"],
    identityFile: "/operator/identity",
    passphraseFile: "/operator/passphrase",
    mode: "vault",
    noService: false,
  });
});

test("store join adds the invitation beside the seeds and reference", () => {
  expect(
    buildJoinArgs({
      name: "beta",
      driveDir: "/drives/beta",
      relays: [],
      credentialMode: "store",
      storeAccount: "wyrd-drive-beta",
      seedIdentityFile: "/keys/identity",
      seedPassphraseFile: "/keys/passphrase",
      fileIdentityFile: "/operator/identity",
      filePassphraseFile: "/operator/passphrase",
      invitationFile: "/tmp/invite.sealed",
    }),
  ).toEqual({
    name: "beta",
    driveDir: "/drives/beta",
    relays: [],
    identityFile: "/keys/identity",
    passphraseFile: "/keys/passphrase",
    credentialBackend: "os-store",
    credentialAccount: "wyrd-drive-beta",
    invitationFile: "/tmp/invite.sealed",
    mode: "vault",
    noService: false,
  });
});

test("store leave names only the item, never seeds", () => {
  expect(
    buildLeaveArgs({
      driveDir: "/drives/alpha",
      credentialMode: "store",
      storeAccount: "wyrd-drive-alpha",
      identityFile: "/keys/identity",
      passphraseFile: "/keys/passphrase",
      acknowledged: true,
    }),
  ).toEqual({
    driveDir: "/drives/alpha",
    credentialBackend: "os-store",
    credentialAccount: "wyrd-drive-alpha",
    acknowledged: true,
  });
});

test("file leave carries the verified pair and no backend keys", () => {
  expect(
    buildLeaveArgs({
      driveDir: "/drives/alpha",
      credentialMode: "file",
      storeAccount: "wyrd-drive-alpha",
      identityFile: "/keys/identity",
      passphraseFile: "/keys/passphrase",
      acknowledged: true,
    }),
  ).toEqual({
    driveDir: "/drives/alpha",
    identityFile: "/keys/identity",
    passphraseFile: "/keys/passphrase",
    acknowledged: true,
  });
});
