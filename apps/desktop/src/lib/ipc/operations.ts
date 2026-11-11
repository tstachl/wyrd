// Per-operation wrappers over the Tauri command surface
// (`crates/wyrd-desktop/src/commands.rs`). One wrapper per
// command, same name, same arguments — this module invents no
// command names and no argument shapes. Owner-operation wrappers
// forward through the single `invokeCommand` path and return the
// typed `IpcResult`; the local query (`listDrives`) forwards
// through `invokeValue` and returns its state struct directly —
// unknown states ride inside the payload, and any rejection
// throws. Result shapes are mirrored in `./results.ts`.
//
// Every drive-scoped wrapper takes `driveDir` first: the absolute
// drive directory as an exact string, matching the backend's
// `drive_dir` command parameter (Tauri maps it to `driveDir` on
// the wire). The selected drive is presentation state, never an
// authorization boundary — the wrapper passes the caller's string
// through verbatim and resolves nothing implicitly.

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

export function lookup(driveDir: string, path: string): Promise<IpcResult<Served<WireNode>>> {
  return invokeCommand<Served<WireNode>>("lookup", { driveDir, path });
}

export function stat(driveDir: string, path: string): Promise<IpcResult<Served<WireAttr>>> {
  return invokeCommand<Served<WireAttr>>("stat", { driveDir, path });
}

export function readdir(
  driveDir: string,
  path: string,
): Promise<IpcResult<Served<WireDirEntry[]>>> {
  return invokeCommand<Served<WireDirEntry[]>>("readdir", { driveDir, path });
}

export function read(driveDir: string, path: string): Promise<IpcResult<Served<number[]>>> {
  return invokeCommand<Served<number[]>>("read", { driveDir, path });
}

export function observe(driveDir: string): Promise<IpcResult<WireSyncStatus>> {
  return invokeCommand<WireSyncStatus>("observe", { driveDir });
}

export function liveness(driveDir: string): Promise<IpcResult<WireLiveness>> {
  return invokeCommand<WireLiveness>("liveness", { driveDir });
}

export function freshness(driveDir: string): Promise<IpcResult<WireFreshness>> {
  return invokeCommand<WireFreshness>("freshness", { driveDir });
}

export function captureParent(
  driveDir: string,
  path: string,
): Promise<IpcResult<WireParentToken>> {
  return invokeCommand<WireParentToken>("capture_parent", { driveDir, path });
}

export function submit(
  driveDir: string,
  kind: WireMutationKind,
): Promise<IpcResult<WireMutationOutcome>> {
  return invokeCommand<WireMutationOutcome>("submit", { driveDir, kind });
}

export function reconcileLookup(
  driveDir: string,
  path: string,
): Promise<IpcResult<Served<WireNode> | null>> {
  return invokeCommand<Served<WireNode> | null>("reconcile_lookup", { driveDir, path });
}

/**
 * Drop the connection for one drive and mint a new instance
 * against the same drive. Called explicitly (the unavailable
 * view's retry) after an unconfirmed answer — never
 * automatically, and recovery proves nothing about the prior
 * operation. Where no connection exists yet, this establishes
 * one.
 */
export function reconnect(driveDir: string): Promise<IpcResult<null>> {
  return invokeCommand<null>("reconnect", { driveDir });
}

/**
 * Every registered drive plus per-drive reachability, verified this
 * session. Infallible: unknown states ride inside the payload.
 */
export function listDrives(): Promise<RegistryState> {
  return invokeValue<RegistryState>("list_drives");
}

/**
 * One drive's configured bounds, as configured — never defaults,
 * never effective posture. `driveDir` selects the section without
 * touching any owner connection. Infallible: unknown states ride
 * inside the payload.
 */
export function driveSettings(driveDir: string): Promise<DriveSettingsState> {
  return invokeValue<DriveSettingsState>("drive_settings", { driveDir });
}
