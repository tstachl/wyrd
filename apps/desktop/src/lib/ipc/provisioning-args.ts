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

/** One join payload: as create, plus the sealed invitation file. */
export function buildJoinArgs(
  input: Parameters<typeof buildCreateArgs>[0] & { invitationFile: string },
): ProvisionJoinArgs {
  const { invitationFile, ...rest } = input;
  return { ...buildCreateArgs(rest), invitationFile };
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
