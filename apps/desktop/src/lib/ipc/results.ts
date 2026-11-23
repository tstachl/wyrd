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
  /** The entry's stable token as lowercase hex, or null for
   * pre-identity entries and synthetic states. The Rust side
   * carries `#[serde(default)]` tolerance; the mirror keeps the
   * key required (possibly null) per the header policy — a missing
   * key is a shape break the frontend must notice, not absorb. */
  identity: string | null;
}

export type WireNode =
  | { File: { size: number; executable: boolean; chunks: string[]; identity: string | null } }
  | { Dir: { subtree: string; identity: string | null } }
  | { Symlink: { target: string; identity: string | null } }
  | { MergedDir: { subtrees: [string, string][]; identity: string | null } }
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

// Advisory owner standing. "Unknown" is the default when an older
// owner writes an observation without this field.
export type WireSoleOwnerState = "Unknown" | "Sole" | "NotSole";

export interface WireKnownState {
  epoch: number;
  transition_id: string;
  members_root: string;
  owners_root: string;
  readers_root: string;
}

// Drive data kept here and the ceiling in force. The count is the
// byte accountant's figure, not a disk or sync figure. `ceiling: null`
// means no limit is set; an absent field means an older owner or an
// unheld read, never zero bytes kept.
export interface WireRetention {
  retained_bytes: number;
  ceiling: number | null;
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
  // Absent from an older owner. Treat missing as "Unknown", never as
  // a fact inferred from known_members.
  sole_owner?: WireSoleOwnerState;
  peers: WirePeerHandle[];
  queue: WireQueueDepth;
  reconciliation: WireReconciliationCounters;
  convergence: WireConvergenceState;
  materialization: WireMaterializationSummary;
  durability: WireDurabilityLevel;
  retention?: WireRetention | null;
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
  // Absent from an older owner. Zero or missing means unknown; never
  // render it as a 1970 date.
  published_unix_secs?: number;
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

// Local-query mirrors (read-only registry and settings views).
//
// Source of truth: `DriveAvailability`, `DriveView`,
// `RegistryStatus`, `RegistryState` in
// `crates/wyrd-desktop/src/registry.rs`, and `DriveSettings`,
// `DriveSettingsView` in
// `crates/wyrd-desktop/src/settings_view.rs` — the TypeScript side
// names the flattened payloads (`RegistryState`,
// `DriveSettingsState`) while the tag vocabularies match
// verbatim. Internally tagged enums carry their tag (`state` /
// `status`) alongside flattened content. Fixtures pinning these
// shapes live in `results.test.ts`,
// copied from the Rust test `local_query_shapes_serialize_verbatim`
// in `crates/wyrd-desktop/src/commands.rs`. Nothing here may add a
// shape the Rust side does not produce.

/** One drive's reachability, verified this session. Transport-level only. */
export type DriveAvailability =
  | { state: "available" }
  | { state: "unavailable"; detail: string }
  | { state: "unknown"; detail: string };

/** One drive's service-manager fact beside the transport verdict: not-installed vs stopped vs wedged vs running vs unknown vs unsupported. Observation only — never supervision. */
export type ServiceState =
  | { state: "notinstalled"; detail: string }
  | { state: "stopped"; detail: string }
  | { state: "wedged"; detail: string }
  | { state: "running"; detail: string }
  | { state: "unknown"; detail: string }
  | { state: "unsupported"; detail: string };

/** One drive's login-start preference as the service manager records it: enabled vs disabled vs unknown. Independent of `ServiceState` — a manually started service may run while disabled at login. No `Stopped*` variants: the two observations pair instead of merging. */
export type EnablementState =
  | { enablement: "enabled"; detail: string }
  | { enablement: "disabled"; detail: string }
  | { enablement: "unknown"; detail: string };

/**
 * One drive's session gate for the per-session posture: whether the
 * drive is open in this session or needs an explicit unlock. `null`
 * where a service definition exists — managed recovery owns that
 * drive. Source of truth: `SessionState` in
 * `crates/wyrd-desktop/src/registry.rs`.
 */
export type SessionState =
  | { state: "unlocked"; detail: string }
  | { state: "locked"; detail: string };

/** One registered drive for the lifecycle view: facts plus the live verdict. No implicit active drive — the frontend's selection is presentation state, never an authorization boundary. `mountpoint` carries the FUSE mountpoint in `mount` mode (`null` in `vault` mode). */
export interface DriveView {
  name: string;
  drive_dir: string;
  relays: string[];
  service: string;
  mode: string;
  mountpoint: string | null;
  availability: DriveAvailability;
  service_state: ServiceState;
  enablement: EnablementState;
  session: SessionState | null;
}

/** How the registry file itself read. Loud about unreadability, never guessing. */
export type RegistryState =
  | { status: "ready"; drives: DriveView[]; skipped: string[] }
  | { status: "nofile"; drives: DriveView[]; skipped: string[] }
  | {
      status: "unreadable";
      reason: string;
      drives: DriveView[];
      skipped: string[];
    };

/** One drive's configured bounds, as configured — never defaults, never effective posture. */
export interface DriveSettingsView {
  retained_bytes_quota: number | null;
  max_admit_per_pass: number | null;
  fetch_pass_budget_secs: number | null;
  max_mutation_wait_secs: number | null;
  open_timeout_secs: number | null;
}

/** What the settings file said about this drive. Missing and malformed are explicit unknown states. */
export type DriveSettingsState =
  | ({ state: "configured" } & DriveSettingsView)
  | { state: "nofile"; path: string }
  | { state: "nosection"; path: string }
  | { state: "unreadable"; reason: string };

/**
 * One drive's credential backend as observed from its deployment
 * config by the installed CLI. References only — the store account
 * label or the configured file paths, never secret contents. An
 * unknown answer is explicit and carries its reason, so the shell
 * keeps the operator choice instead of guessing "store".
 */
export type CredentialReference =
  | { backend: "os-store"; account: string }
  | { backend: "file"; identityFile: string; passphraseFile: string }
  | { backend: "unknown"; reason: string };

// Provisioning mirrors (constrained CLI adapter surface).
//
// Source of truth: `ProvisionResult` in
// `crates/wyrd-desktop/src/provision.rs` and `ProvisionReport` in
// `crates/wyrd-desktop/src/commands.rs` — only the exit code is
// ever mapped, CLI text rides as opaque detail, and the observed
// registry nests under `observed` with the membership tip under
// `membership`. Nothing here may add a shape the Rust side does
// not produce.

/** The adapter's verdict on the operation — never lifecycle state. Only the exit code decides; CLI text rides along as opaque display detail. */
export type ProvisionResult =
  | { result: "completed"; detail: string }
  | { result: "expectedfailure"; detail: string }
  | { result: "helddrive"; detail: string }
  | { result: "versionmismatch"; reason: string }
  | { result: "busy"; drive_dir: string }
  | { result: "unknown"; detail: string };

/** The operated drive's membership tip as re-observed: terminal owner answer only, never guessed. */
export interface MembershipObservation {
  epoch: number | null;
  known_members: number;
}

/** One completed lifecycle operation plus the post-run observation. Infallible: unknown states ride inside. */
export interface ProvisionReport {
  result: ProvisionResult;
  observed: RegistryState;
  membership: MembershipObservation | null;
}

// Onboarding mirrors (first-run identity surface).
//
// Source of truth: `OnboardingResult` / `OnboardingReport` /
// `KeyDefaults` in `crates/wyrd-desktop/src/onboarding.rs`. Only the
// exit code is ever mapped; CLI text rides as opaque detail, and
// fresh existence nests under `defaults`. The generated passphrase
// travels only in `new-passphrase` answers for write-down — every
// other success carries `passphrase: null`.

/** Platform key paths plus existence (no secret reads). `usable` is false only where no home directory resolves — paths are then empty and nothing may commit. */
export interface KeyDefaults {
  identity_file: string;
  passphrase_file: string;
  identity_exists: boolean;
  passphrase_exists: boolean;
  usable: boolean;
}

/** One onboarding operation outcome. `passphrase` is set only for `new-passphrase` success. */
export type OnboardingResult =
  | { result: "completed"; detail: string; passphrase: string | null }
  | { result: "expectedfailure"; detail: string }
  | { result: "versionmismatch"; reason: string }
  | { result: "unknown"; detail: string };

/** One completed onboarding call plus fresh existence (the read-back signal). Infallible. */
export interface OnboardingReport {
  result: OnboardingResult;
  defaults: KeyDefaults;
}

// Settings-write mirrors (constrained CLI adapter surface).
//
// Source of truth: `SettingsApplyResult` in
// `crates/wyrd-desktop/src/settings_apply.rs` and
// `SettingsWriteReport` in `crates/wyrd-desktop/src/commands.rs` —
// only the exit code is ever mapped (0 applied, 2 refused, 3
// conflict, 4 committed-but-unverified), CLI text rides as opaque
// detail, and the observed settings re-read nests under
// `observed`. Nothing here may add a shape the Rust side does not
// produce.

/** The adapter's verdict on one sparse edit — never configured state. Only the exit code decides. */
export type SettingsWriteState =
  | { result: "applied"; detail: string }
  | { result: "refused"; reason: string }
  | { result: "conflict"; reason: string }
  | { result: "unverified"; reason: string }
  | { result: "failed"; reason: string };

/** One completed settings edit plus the post-run observation. Infallible: unknown states ride inside. */
export interface SettingsWriteReport {
  result: SettingsWriteState;
  observed: DriveSettingsState;
}

/** One sparse update: bound values to set plus bound names to clear — never a whole document. */
export interface SparseUpdateArgs {
  set: Record<string, number>;
  clear: string[];
}

// App-wide diagnostics mirrors (T20).
//
// Source of truth: `WireDiagnosticLevel` in
// `crates/wyrd-owner-client/src/wire.rs` and `Diagnostics` in
// `crates/wyrd-desktop/src/diagnostics_view.rs`. The level is a
// closed pair of bare lowercase strings; the persisted view is
// internally tagged like the other local-query shapes. The shell
// shows the persisted level from `settings.json`, never an owner's
// in-memory override. Fixtures live in `results.test.ts`.

/** The closed application-wide diagnostics level. */
export type WireDiagnosticLevel = "default" | "detailed";

/** The level persisted in `settings.json`, as the shell can observe it. */
export type Diagnostics =
  | {
      state: "configured";
      level: WireDiagnosticLevel;
      /** False when the file carries no level — the genuine default state. */
      explicit: boolean;
      path: string;
    }
  | { state: "nofile"; path: string }
  | { state: "unreadable"; reason: string };
