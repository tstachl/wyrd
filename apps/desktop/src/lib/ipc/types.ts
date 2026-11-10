// TypeScript mirror of the backend answer taxonomy.
//
// Source of truth: `BackendError` in
// `crates/wyrd-desktop/src/backend.rs` (`#[serde(tag = "kind",
// rename_all = "lowercase")]`, `Refused` flattening `WireError`).
// Refusal codes are the exact `WireErrorCode` variant names in
// `crates/wyrd-owner-client/src/wire.rs`.
//
// This mirror is checked against fixtures copied from the Rust
// test `error_taxonomy_serializes_typed` (see `errors.test.ts`).
// Full Rust-to-TS wire-agreement proofs land with the Tauri
// command layer (first view); until then this file must not gain
// per-operation shapes — command argument names do not exist yet.

export const WIRE_ERROR_CODES = [
  "UnknownVersion",
  "Malformed",
  "UnsupportedOperation",
  "NotFound",
  "NotADirectory",
  "NotAFile",
  "Conflicted",
  "NotMaterialized",
  "Unavailable",
  "Corrupt",
  "RejectedRepresentation",
  "LostRepresentation",
  "Store",
  "TooLarge",
  "Timeout",
  "Saturated",
  "IsDirectory",
  "AlreadyExists",
  "DirectoryNotEmpty",
  "InvalidRename",
  "Stale",
  "StaleParent",
  "TooMany",
  "NeedContent",
  "Shutdown",
  "Engine",
  "Unobserved",
  "Poisoned",
  "Io",
  "PinnedPresent",
  "TooDeep",
] as const;

export type WireErrorCode = (typeof WIRE_ERROR_CODES)[number];

export interface BackendUnavailable {
  kind: "unavailable";
  detail: string;
}

export interface BackendUnconfirmed {
  kind: "unconfirmed";
  op: string;
  detail: string;
}

export interface BackendRefused {
  kind: "refused";
  // Verbatim owner code. Known codes are the `WireErrorCode` union;
  // an unknown code is still a terminal refusal — never a transport
  // failure, never remapped. See `isWireErrorCode`.
  code: string;
  message: string;
}

export interface BackendIncompatible {
  kind: "incompatible";
  detail: string;
}

/** The backend-side answer to one IPC request, never collapsed. */
export type BackendError =
  | BackendUnavailable
  | BackendUnconfirmed
  | BackendRefused
  | BackendIncompatible;

/** Success carries the value; failure carries the typed taxonomy. */
export type IpcResult<T> =
  | { ok: true; value: T }
  | { ok: false; error: BackendError };
