// TypeScript mirrors of the owner-interface result shapes.
//
// Source of truth: the `Wire*` types in
// `crates/wyrd-owner-client/src/wire.rs`. Serde defaults apply
// throughout: struct fields keep their snake_case names, enums are
// externally tagged (`{"Variant": ...}`, unit variants as bare
// `"Variant"`), tuples become JSON arrays, `Option` becomes `null`.
// Nothing here renames, defaults, or reinterprets — the single
// exception is `#[serde(default)]` tolerance fields on
// `WireLiveness`/`WireSyncReport`/`WireExecuteReport`, which stay
// required in these mirrors (a missing field is a shape break the
// frontend must notice, not absorb).
//
// Fixtures pinning these shapes live in `results.test.ts`, copied
// from the Rust test `result_shapes_serialize_verbatim` in
// `crates/wyrd-desktop/src/commands.rs`. Nothing here may add a
// shape the Rust side does not produce.

export type WireKind = "File" | "Dir" | "Symlink" | "Conflict";

export interface WireAttr {
  kind: WireKind;
  size: number;
  executable: boolean;
}

export type WireNode =
  | { File: { size: number; executable: boolean; chunks: string[] } }
  | { Dir: { subtree: string } }
  | { Symlink: { target: string } }
  | { MergedDir: { subtrees: [string, string][] } }
  | { Conflict: { versions: WireConflictVersion[] } };

export interface WireConflictVersion {
  snapshot: string;
  node: WireNode;
}

export interface WireDirEntry {
  name: string;
  node: WireNode;
}

export interface WireHeadSummary {
  id: string;
  epoch: number;
  author: string;
}

export interface WireHeadClasses {
  eligible: number;
  canonical_history: number;
  superseded: number;
  stranded: number;
  voided: number;
  pending: number;
  rejected: number;
}

export interface WireMailboxView {
  configured_relays: number;
}

export interface WirePeerHandle {
  handle: number;
  peer: string;
  pending: number;
}

export interface WireQueueDepth {
  outbox: number;
  carries: number;
  fetch: number;
}

export interface WirePendingObligations {
  announcements: [string, string][];
  transitions: [string, string][];
  capabilities: [number, string][];
}

export interface WireOutboxTotals {
  announcements_queued: number;
  announcements_delivered: number;
  transitions_queued: number;
  transitions_delivered: number;
  capabilities_queued: number;
  capabilities_delivered: number;
}

export interface WireConvergenceState {
  converged: boolean;
  pending_heads: number;
  unfetchable_heads: number;
}

export interface WireReconciliationCounters {
  statements_received: number;
  transitions_reconciled: number;
  capabilities_reconciled: number;
  announcements_reconciled: number;
}

export interface WireMaterializationSummary {
  pinned: number;
  cached: number;
  local_objects: number;
}

export type WireDurabilityLevel = "Working" | "Committed" | "Published";

export interface WireKnownState {
  epoch: number;
  transition_id: string;
  members_root: string;
  owners_root: string;
  readers_root: string;
}

export interface WireSyncStatus {
  tip: WireKnownState | null;
  held_epochs: number[];
  obligations: WirePendingObligations;
  totals: WireOutboxTotals;
  live_heads: WireHeadSummary[];
  head_classes: WireHeadClasses;
  mailbox: WireMailboxView;
  known_members: number;
  peers: WirePeerHandle[];
  queue: WireQueueDepth;
  reconciliation: WireReconciliationCounters;
  convergence: WireConvergenceState;
  materialization: WireMaterializationSummary;
  durability: WireDurabilityLevel;
}

export interface WireMailboxHealth {
  stream_alive: boolean;
  connected_relays: number;
  total_relays: number;
  saturation_recoveries: number;
  supervisor_ticks: number;
  relay_attached: boolean;
  stream_recovery_attempts: number;
  relay_recovery_attempts: number;
  closed_subscriptions: number;
}

export interface WireDrainReport {
  accepted: number;
  duplicates: number;
  deferred: number;
  deferred_unseen: number;
  deferred_status_blocked: number;
  deferred_shed: number;
  deferred_shed_unseen: number;
  deferred_shed_status_blocked: number;
  skipped: number;
  discarded: number;
  peers_observed: number;
}

export interface WireExecuteReport {
  manifests: number;
  snapshot_bodies: number;
  objects: number;
  unfulfilled: number;
  transport_errors: number;
  deadlines: number;
  missing: number;
  invalid: number;
  unavailable_keys: number;
  local_failures: number;
  refused: number;
}

export interface WireQuarantineReport {
  observed: number;
  claims_cleared: number;
  bytes_discarded: number;
  failures: number;
}

export interface WireScrubReport {
  observed: number;
  claims_cleared: number;
  bytes_subtracted: number;
  failures: number;
}

export interface WireSyncReport {
  drained: WireDrainReport;
  fetched: WireExecuteReport;
  quarantined: WireQuarantineReport;
  scrubbed: WireScrubReport;
  published: boolean;
  sent: number;
  generation: number;
  pending_heads: number;
  publish_clipped: boolean;
  publish_deferred: number;
  publish_elapsed_ms: number;
}

export interface WireLiveness {
  health: WireMailboxHealth;
  report: WireSyncReport | null;
  passes: number;
  paused: boolean;
}

export type WireFreshnessState =
  | "FreshEmpty"
  | "FreshNonEmpty"
  | { RetainedLastGood: { detail: string } };

export interface WireFreshness {
  state: WireFreshnessState;
  revision: number;
  generation: number;
}

export interface WireFileIdentity {
  size: number;
  executable: boolean;
  chunks: string[];
}

export interface WireParentToken {
  instance: number;
  counter: number;
}

export interface WireFoldMember {
  kind: WireMutationKind;
  forcer: boolean;
}

export type WireMutationKind =
  | { Mkdir: { path: string } }
  | { CreateFile: { path: string; parent: WireParentToken } }
  | {
      CommitFile: {
        path: string;
        base: WireFileIdentity;
        executable: boolean;
        content_hex: string;
      };
    }
  | { AppendFile: { path: string; content_hex: string } }
  | { Unlink: { path: string } }
  | { Rmdir: { path: string } }
  | { Rename: { from: string; to: string; no_replace: boolean } }
  | {
      SetAttrs: {
        path: string;
        size: number | null;
        executable: boolean | null;
        base: WireFileIdentity | null;
      };
    }
  | { Fold: { members: WireFoldMember[] } };

export interface WireMutationError {
  code: string;
  message: string;
}

export type WireFoldForcerOutcome =
  | { Applied: WireMutationOutcome }
  | { Failed: WireMutationError };

export type WireFoldDisposition =
  | { Committed: WireFileIdentity }
  | "Done"
  | { Created: WireFileIdentity }
  | { Failed: WireMutationError }
  | "Restored";

export type WireMutationOutcome =
  | "Done"
  | { Created: WireFileIdentity }
  | { Committed: WireFileIdentity }
  | { Fold: { forcer: WireFoldForcerOutcome; members: WireFoldDisposition[] } };

/** Served generation + durable revision accompany every read. */
export type Served<T> = [T, number, number];
