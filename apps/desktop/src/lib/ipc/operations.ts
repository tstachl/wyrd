// Per-operation wrappers over the Tauri command surface
// (`crates/wyrd-desktop/src/commands.rs`). One wrapper per
// command, same name, same arguments — this module invents no
// command names and no argument shapes. Owner-operation wrappers
// forward through the single `invokeCommand` path and return the
// typed `IpcResult`; the local queries (`listDrives`,
// `driveSettings`) forward through `invokeValue` and return their
// state structs directly — unknown states ride inside the payload,
// and any rejection throws. Result shapes are mirrored in
// `./results.ts`.

import { invokeCommand, invokeValue } from "./client.js";
import type { IpcResult } from "./types.js";
import type {
  DriveSettingsState,
  RegistryState,
  Served,
  WireAttr,
  WireDirEntry,
  WireFreshness,
  WireLiveness,
  WireMutationKind,
  WireMutationOutcome,
  WireNode,
  WireParentToken,
  WireSyncStatus,
} from "./results.js";

export function lookup(path: string): Promise<IpcResult<Served<WireNode>>> {
  return invokeCommand<Served<WireNode>>("lookup", { path });
}

export function stat(path: string): Promise<IpcResult<Served<WireAttr>>> {
  return invokeCommand<Served<WireAttr>>("stat", { path });
}

export function readdir(path: string): Promise<IpcResult<Served<WireDirEntry[]>>> {
  return invokeCommand<Served<WireDirEntry[]>>("readdir", { path });
}

export function read(path: string): Promise<IpcResult<Served<number[]>>> {
  return invokeCommand<Served<number[]>>("read", { path });
}

export function observe(): Promise<IpcResult<WireSyncStatus>> {
  return invokeCommand<WireSyncStatus>("observe");
}

export function liveness(): Promise<IpcResult<WireLiveness>> {
  return invokeCommand<WireLiveness>("liveness");
}

export function freshness(): Promise<IpcResult<WireFreshness>> {
  return invokeCommand<WireFreshness>("freshness");
}

export function captureParent(path: string): Promise<IpcResult<WireParentToken>> {
  return invokeCommand<WireParentToken>("capture_parent", { path });
}

export function submit(kind: WireMutationKind): Promise<IpcResult<WireMutationOutcome>> {
  return invokeCommand<WireMutationOutcome>("submit", { kind });
}

export function reconcileLookup(
  path: string,
): Promise<IpcResult<Served<WireNode> | null>> {
  return invokeCommand<Served<WireNode> | null>("reconcile_lookup", { path });
}

/**
 * Drop the current connection and mint a new instance. Called
 * explicitly (the unavailable view's retry) after an unconfirmed
 * answer — never automatically, and recovery proves nothing about
 * the prior operation.
 */
export function reconnect(): Promise<IpcResult<null>> {
  return invokeCommand<null>("reconnect");
}

/**
 * Every registered drive plus per-drive reachability, verified this
 * session. Infallible: unknown states ride inside the payload.
 */
export function listDrives(): Promise<RegistryState> {
  return invokeValue<RegistryState>("list_drives");
}

/**
 * This drive's configured bounds, as configured — never defaults,
 * never effective posture. Infallible: unknown states ride inside
 * the payload.
 */
export function driveSettings(): Promise<DriveSettingsState> {
  return invokeValue<DriveSettingsState>("drive_settings");
}
