// The single way the frontend talks to the backend: Tauri `invoke`
// only. No socket, no wire bytes, no drive directory, no protocol
// networking of its own.
//
// Per-operation wrappers live in `./operations.ts`, one per
// registered command (`crates/wyrd-desktop/src/commands.rs`), same
// names and argument shapes. This module stays the single `invoke`
// path underneath them and invents nothing.
//
// Answer discipline: success resolves `{ ok: true, value }`; an
// owner answer rejects into `{ ok: false, error }` with the
// taxonomy intact. A rejection that is not the taxonomy is a
// transport failure — it throws `IpcTransportError`, which is
// deliberately not one of the four variants: failing to reach the
// backend says nothing about any owner or any request, and must
// never be mapped into `Unavailable` or `Unconfirmed`.

import { invoke } from "@tauri-apps/api/core";
import { asBackendError } from "./errors.js";
import type { IpcResult } from "./types.js";

/** Reaching the backend itself failed. Not an owner answer. */
export class IpcTransportError extends Error {
  readonly raw: unknown;

  constructor(raw: unknown) {
    super("IPC transport failed before any owner answer");
    this.name = "IpcTransportError";
    this.raw = raw;
  }
}

export async function invokeCommand<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<IpcResult<T>> {
  let value: T;
  try {
    value = await invoke<T>(command, args);
  } catch (raw: unknown) {
    const error = asBackendError(raw);
    if (error === null) throw new IpcTransportError(raw);
    return { ok: false, error };
  }
  return { ok: true, value };
}

/**
 * Invoke an infallible local query (`list_drives`,
 * `drive_settings`). These commands answer state structs, never the
 * owner taxonomy, so any rejection is a transport failure or a shell
 * bug — never an owner answer — and throws `IpcTransportError`.
 * Unknown states (missing files, version skew, version-skewed or
 * malformed content) ride inside the payload, never as rejections.
 */
export async function invokeValue<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (raw: unknown) {
    throw new IpcTransportError(raw);
  }
}
