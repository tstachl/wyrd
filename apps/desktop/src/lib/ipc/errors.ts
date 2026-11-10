// Guards and rendering for the backend answer taxonomy
// (`./types.ts`). Load-bearing rules, from the interface contract:
//
// - Two outcomes are never collapsed: every variant stays distinct
//   from parsing through display.
// - `Unconfirmed` never becomes success or failure through mapping.
// - Refusal codes travel exact; `describe` always names the code.
// - Anything that is not the taxonomy is not an owner answer at
//   all (see `IpcTransportError` in `./client.ts`).

import type {
  BackendError,
  BackendIncompatible,
  BackendRefused,
  BackendUnavailable,
  BackendUnconfirmed,
  WireErrorCode,
} from "./types.js";
import { WIRE_ERROR_CODES } from "./types.js";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isWireErrorCode(value: unknown): value is WireErrorCode {
  return (
    typeof value === "string" &&
    (WIRE_ERROR_CODES as readonly string[]).includes(value)
  );
}

/**
 * Narrow an unknown invoke rejection to the backend taxonomy.
 * Returns null when the payload is not an owner answer — the
 * caller must treat that as a transport failure, never as one of
 * the four variants.
 */
export function asBackendError(value: unknown): BackendError | null {
  if (!isRecord(value) || typeof value["kind"] !== "string") return null;
  switch (value["kind"]) {
    case "unavailable":
      if (typeof value["detail"] === "string") {
        const error: BackendUnavailable = {
          kind: "unavailable",
          detail: value["detail"],
        };
        return error;
      }
      return null;
    case "unconfirmed":
      if (
        typeof value["op"] === "string" &&
        typeof value["detail"] === "string"
      ) {
        const error: BackendUnconfirmed = {
          kind: "unconfirmed",
          op: value["op"],
          detail: value["detail"],
        };
        return error;
      }
      return null;
    case "refused":
      if (
        isWireErrorCode(value["code"]) &&
        typeof value["message"] === "string"
      ) {
        const error: BackendRefused = {
          kind: "refused",
          code: value["code"],
          message: value["message"],
        };
        return error;
      }
      return null;
    case "incompatible":
      if (typeof value["detail"] === "string") {
        const error: BackendIncompatible = {
          kind: "incompatible",
          detail: value["detail"],
        };
        return error;
      }
      return null;
    default:
      return null;
  }
}

/** Exhaustiveness guard: adding a variant breaks callers loudly. */
export function assertNever(value: never): never {
  throw new Error(`unhandled backend error kind: ${String(value)}`);
}

/**
 * One-line human rendering. Every variant names itself; refusals
 * name their code. Never used to decide retries — decisions switch
 * on `kind`, not on this text.
 */
export function describe(error: BackendError): string {
  switch (error.kind) {
    case "unavailable":
      return `Owner unavailable: ${error.detail}`;
    case "unconfirmed":
      return `Unconfirmed ${error.op}: ${error.detail}`;
    case "refused":
      return `Owner refused (${error.code}): ${error.message}`;
    case "incompatible":
      return `Incompatible owner: ${error.detail}`;
    default:
      return assertNever(error);
  }
}
