// Pure argument builders for the constrained CLI adapter surface
// (`./operations.ts`, backend `crates/wyrd-desktop/src/commands.rs`).
// Views call these so the store-reference shapes pin here instead of
// only in copy: store mode names `credentialBackend: "os-store"` plus
// the derived account (seeds ride beside it for a fresh bootstrap,
// never for leave), file mode carries the picked paths and no backend
// keys. References only, never secrets.
//
// Run: `bun run test` from `apps/desktop/`.

import type {
  ProvisionCreateArgs,
  ProvisionJoinArgs,
  ProvisionLeaveArgs,
  ProvisionReconfigureArgs,
} from "./operations.js";

export type CredentialMode = "store" | "file";

/**
 * One create payload: store mode names the derived account with the
 * seeds beside it (the CLI's one-time bootstrap inputs); file mode
 * carries the picked paths and no backend keys.
 */
export function buildCreateArgs(input: {
  name: string;
  driveDir: string;
  relays: string[];
  credentialMode: CredentialMode;
  storeAccount: string;
  seedIdentityFile: string;
  seedPassphraseFile: string;
  fileIdentityFile: string;
  filePassphraseFile: string;
}): ProvisionCreateArgs {
  if (input.credentialMode === "store") {
    return {
      name: input.name,
      driveDir: input.driveDir,
      relays: input.relays,
      identityFile: input.seedIdentityFile,
      passphraseFile: input.seedPassphraseFile,
      credentialBackend: "os-store",
      credentialAccount: input.storeAccount,
      mode: "vault",
      noService: false,
    };
  }
  return {
    name: input.name,
    driveDir: input.driveDir,
    relays: input.relays,
    identityFile: input.fileIdentityFile,
    passphraseFile: input.filePassphraseFile,
    mode: "vault",
    noService: false,
  };
}

/** One join payload: as create, plus the sealed invitation file and
 * the optional out-of-band expectation pins (absent or blank pins
 * are not sent, so the backend leaves those fields unchecked). */
export function buildJoinArgs(
  input: Parameters<typeof buildCreateArgs>[0] & {
    invitationFile: string;
    expectedOwner?: string;
    expectedDrive?: string;
    expectedGenesis?: string;
  },
): ProvisionJoinArgs {
  const { invitationFile, expectedOwner, expectedDrive, expectedGenesis, ...rest } = input;
  const trimmed = {
    expectedOwner: expectedOwner?.trim() || undefined,
    expectedDrive: expectedDrive?.trim() || undefined,
    expectedGenesis: expectedGenesis?.trim() || undefined,
  };
  return {
    ...buildCreateArgs(rest),
    invitationFile,
    ...(trimmed.expectedOwner ? { expectedOwner: trimmed.expectedOwner } : {}),
    ...(trimmed.expectedDrive ? { expectedDrive: trimmed.expectedDrive } : {}),
    ...(trimmed.expectedGenesis ? { expectedGenesis: trimmed.expectedGenesis } : {}),
  };
}

/**
 * One reconfigure payload: empty relays keep the registered set,
 * mode and mountpoint ride from the registry observation. Store mode
 * names the derived reference (reads the provisioned item, never
 * seeds); file mode carries the verified pair. The file branch
 * refuses before any spawn where the pair is absent — mirrored by
 * the caller's gate.
 */
export function buildReconfigureArgs(input: {
  driveDir: string;
  relays: string[];
  credentialMode: CredentialMode;
  storeAccount: string;
  identityFile: string | null;
  passphraseFile: string | null;
  mode: string;
  mountpoint: string | undefined;
  noService: boolean;
}): ProvisionReconfigureArgs | null {
  if (input.credentialMode === "store") {
    return {
      driveDir: input.driveDir,
      relays: input.relays,
      credentialBackend: "os-store",
      credentialAccount: input.storeAccount,
      mode: input.mode,
      mountpoint: input.mountpoint,
      noService: input.noService,
    };
  }
  if (input.identityFile === null || input.passphraseFile === null) return null;
  return {
    driveDir: input.driveDir,
    relays: input.relays,
    identityFile: input.identityFile,
    passphraseFile: input.passphraseFile,
    mode: input.mode,
    mountpoint: input.mountpoint,
    noService: input.noService,
  };
}

/**
 * One leave payload: store mode names only the derived account (the
 * provisioned item, never seeds); file mode carries the verified
 * pair.
 */
export function buildLeaveArgs(input: {
  driveDir: string;
  credentialMode: CredentialMode;
  storeAccount: string | null;
  identityFile: string | null;
  passphraseFile: string | null;
  acknowledged: boolean;
}): ProvisionLeaveArgs {
  if (input.credentialMode === "store" && input.storeAccount !== null) {
    return {
      driveDir: input.driveDir,
      credentialBackend: "os-store",
      credentialAccount: input.storeAccount,
      acknowledged: input.acknowledged,
    };
  }
  return {
    driveDir: input.driveDir,
    identityFile: input.identityFile ?? undefined,
    passphraseFile: input.passphraseFile ?? undefined,
    acknowledged: input.acknowledged,
  };
}
