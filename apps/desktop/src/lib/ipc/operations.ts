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
  CredentialReference,
  Diagnostics,
  DiagnosticsExport,
  DriveSettingsState,
  KeyDefaults,
  OnboardingReport,
  ProvisionReport,
  RegistryState,
  Served,
  SettingsWriteReport,
  SparseUpdateArgs,
  WireAttr,
  WireDiagnosticLevel,
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

/**
 * One drive's credential backend, observed from its deployment
 * config through the installed CLI. A reference only — never secret
 * contents. Infallible: the unknown state rides inside the payload.
 */
export function driveCredentialReference(driveDir: string): Promise<CredentialReference> {
  return invokeValue<CredentialReference>("drive_credential_reference", { driveDir });
}

/**
 * Apply one sparse update to one drive's settings section through
 * the installed CLI, then re-read. One drive plus one sparse update
 * (set-list with values, clear-list) — never a whole-document
 * replacement. Infallible: the outcome plus the post-run
 * observation ride inside the payload.
 */
export function applySettings(
  driveDir: string,
  update: SparseUpdateArgs,
): Promise<SettingsWriteReport> {
  return invokeValue<SettingsWriteReport>("drive_settings_write", { driveDir, update });
}

/**
 * The persisted application-wide diagnostics level. A local query:
 * no owner connection, no drive identity — the level is read from
 * `settings.json`, so it is visible even while every drive is
 * locked. Infallible: unreadable states ride inside the payload.
 */
export function diagnosticsLevel(): Promise<Diagnostics> {
  return invokeValue<Diagnostics>("diagnostics_level");
}

/**
 * Set the application-wide diagnostics level on one drive's running
 * owner and read it back over the same exchange. The owner persists
 * the setting and publishes the File Provider mirror; this answer
 * only confirms delivery. The shell re-reads `diagnosticsLevel` to
 * decide what the UI shows — never this optimistic answer.
 */
export function diagnosticsSet(
  driveDir: string,
  level: WireDiagnosticLevel,
): Promise<IpcResult<WireDiagnosticLevel>> {
  return invokeCommand<WireDiagnosticLevel>("diagnostics_set", { driveDir, level });
}

/**
 * Explicitly build a temporary local diagnostics bundle. No drive,
 * no owner, no network: the backend writes one private file and
 * returns its path. The user takes any further step themselves.
 */
export function diagnosticsExport(): Promise<DiagnosticsExport> {
  return invokeValue<DiagnosticsExport>("diagnostics_export");
}

// Provisioning operations (constrained CLI adapter surface). One
// wrapper per command, same names and argument shapes — this module
// invents nothing. Secret paths travel, never contents; every call
// returns the operation outcome plus the post-run observation, and
// observed state decides the display. Infallible: outcomes ride
// inside the payload, rejections throw `IpcTransportError`.

export interface ProvisionCreateArgs {
  name: string;
  driveDir: string;
  relays: string[];
  /** Credential path, never contents. Required with the file backend; the seed identity beside a store bootstrap. */
  identityFile?: string;
  /** Credential path, never contents. Required with the file backend; the seed passphrase beside a store bootstrap. */
  passphraseFile?: string;
  /** Credential backend selector: absent or `"file"` reads files; `"os-store"` resolves the drive through the OS credential store. */
  credentialBackend?: string;
  /** OS credential-store account naming the drive's item. Required exactly where the backend is `"os-store"`. */
  credentialAccount?: string;
  mode: string;
  mountpoint?: string;
  noService: boolean;
}

export interface ProvisionJoinArgs extends ProvisionCreateArgs {
  invitationFile: string;
  /** Expected owner pin (64 hex): the inviter the invitee meant to trust. */
  expectedOwner?: string;
  /** Expected drive pin (64 hex): the drive being joined. */
  expectedDrive?: string;
  /** Expected genesis pin (64 hex): the genesis transition the invitation must carry. */
  expectedGenesis?: string;
}

export interface ProvisionLeaveArgs {
  driveDir: string;
  /** Credential path, never contents. Required with the file backend; refused beside a store reference. */
  identityFile?: string;
  /** Credential path, never contents. Required with the file backend; refused beside a store reference. */
  passphraseFile?: string;
  /** Credential backend selector: absent or `"file"` reads files; `"os-store"` resolves the drive through the OS credential store. */
  credentialBackend?: string;
  /** OS credential-store account naming the drive's item. Required exactly where the backend is `"os-store"`. */
  credentialAccount?: string;
  acknowledged: boolean;
}

/** Remove one drive's local data through the installed CLI. */
export interface ProvisionDeleteArgs {
  driveDir: string;
  /** Credential path, never contents. Required with the file backend; refused beside a store reference. */
  identityFile?: string;
  /** Credential path, never contents. Required with the file backend; refused beside a store reference. */
  passphraseFile?: string;
  /** Credential backend selector: absent or `"file"` reads files; `"os-store"` resolves the drive through the OS credential store. */
  credentialBackend?: string;
  /** OS credential-store account naming the drive's item. Required exactly where the backend is `"os-store"`. */
  credentialAccount?: string;
  /** Terminal confirmation carried to the CLI's `--yes`. */
  acknowledged: boolean;
}

export interface ProvisionReconfigureArgs {
  driveDir: string;
  relays: string[];
  /** Credential path, never contents. Required with the file backend; refused beside a store reference. */
  identityFile?: string;
  /** Credential path, never contents. Required with the file backend; refused beside a store reference. */
  passphraseFile?: string;
  /** Credential backend selector: absent or `"file"` reads files; `"os-store"` resolves the drive through the OS credential store. */
  credentialBackend?: string;
  /** OS credential-store account naming the drive's item. Required exactly where the backend is `"os-store"`. */
  credentialAccount?: string;
  mode: string;
  mountpoint?: string;
  noService: boolean;
}

export interface ProvisionServiceStartArgs {
  driveDir: string;
}

export interface ProvisionServiceEnableArgs {
  driveDir: string;
}

export interface ProvisionServiceDisableArgs {
  driveDir: string;
}

/**
 * Create one drive through the installed CLI, then re-read and
 * probe. `mode` is `vault` or `mount` (`mount` needs `mountpoint`).
 */
export function provisionCreate(args: ProvisionCreateArgs): Promise<ProvisionReport> {
  return invokeValue<ProvisionReport>("provision_create", { args });
}

/**
 * Join one drive through the installed CLI with the sealed
 * invitation file, then re-read and probe.
 */
export function provisionJoin(args: ProvisionJoinArgs): Promise<ProvisionReport> {
  return invokeValue<ProvisionReport>("provision_join", { args });
}

/**
 * Leave the drive through the installed CLI, then re-observe.
 * `acknowledged` carries the sole-owner terminal confirmation.
 */
export function provisionLeave(args: ProvisionLeaveArgs): Promise<ProvisionReport> {
  return invokeValue<ProvisionReport>("provision_leave", { args });
}

/**
 * Remove one drive's local data through the installed CLI, then
 * re-read and re-observe. `acknowledged` carries the terminal
 * confirmation to `--yes`.
 */
export function provisionDelete(args: ProvisionDeleteArgs): Promise<ProvisionReport> {
  return invokeValue<ProvisionReport>("provision_delete", { args });
}

/**
 * Reconfigure the drive's supervision through the installed CLI
 * (stop, clear, rewrite, reinstall, restart, verify), then
 * re-read and probe. The reinstall recovery for "no service
 * definition installed" — empty `relays` keeps the registered set.
 */
export function provisionReconfigure(args: ProvisionReconfigureArgs): Promise<ProvisionReport> {
  return invokeValue<ProvisionReport>("provision_reconfigure", { args });
}

/**
 * Start the drive's installed service through the installed CLI
 * (start the existing definition now without touching its
 * login-start preference, verify the socket), then re-read and
 * probe. The recovery for "installed but stopped" — explicit,
 * never automatic; no credentials travel.
 */
export function provisionServiceStart(args: ProvisionServiceStartArgs): Promise<ProvisionReport> {
  return invokeValue<ProvisionReport>("provision_service_start", { args });
}

/**
 * Enable the drive's login start through the installed CLI
 * (record the preference with the service manager without
 * starting anything, verify the record), then re-read and probe.
 * No credentials travel.
 */
export function provisionServiceEnable(args: ProvisionServiceEnableArgs): Promise<ProvisionReport> {
  return invokeValue<ProvisionReport>("provision_service_enable", { args });
}

/**
 * Disable the drive's login start and stop it now through the
 * installed CLI (record the preference, verify the record and the
 * stopped state, keep the definition file), then re-read and
 * probe. No credentials travel.
 */
export function provisionServiceDisable(
  args: ProvisionServiceDisableArgs,
): Promise<ProvisionReport> {
  return invokeValue<ProvisionReport>("provision_service_disable", { args });
}

/**
 * Unlock one drive for the session: spawn the persistent owner
 * through the version-matched CLI with the passphrase on the
 * stdin pipe (never argv), verify the socket answers, then hold
 * the child under the session monitor. The passphrase text travels
 * here like the onboarding commit pipe — never persisted in the
 * frontend, never logged.
 */
export interface DriveUnlockArgs {
  driveDir: string;
  /** Credential path, never contents. Absent beside a store reference, which names the identity instead. */
  identityFile?: string;
  /** Credential backend selector: absent or `"file"` reads the identity file; `"os-store"` loads the identity from the drive's store item. */
  credentialBackend?: string;
  /** OS credential-store account naming the drive's item. Required exactly where the backend is `"os-store"`. */
  credentialAccount?: string;
  passphrase: string;
}

export function driveUnlock(args: DriveUnlockArgs): Promise<ProvisionReport> {
  return invokeValue<ProvisionReport>("drive_unlock", { args });
}

/**
 * Lock one drive for the session: kill the app-held unlock child
 * and scrub the session secret, then re-read and probe. Refuses
 * where this app holds no unlock session — a reattached owner the
 * app did not start is never killed.
 */
export interface DriveLockArgs {
  driveDir: string;
}

export function driveLock(args: DriveLockArgs): Promise<ProvisionReport> {
  return invokeValue<ProvisionReport>("drive_lock", { args });
}

// Onboarding operations (first-run identity surface). Paths travel,
// never secret contents — except the two documented passphrase
// transits: `new-passphrase` returns the generated text for
// write-down, and `commit` sends the write-down text back on the
// stdin pipe (never argv). The identity secret never leaves the CLI.

export interface OnboardingCommitArgs {
  identityFile: string;
  passphraseFile: string;
  passphrase: string;
}

export interface OnboardingVerifyArgs {
  identityFile: string;
  passphraseFile: string;
}

export interface OnboardingAdoptArgs {
  identityFile: string;
  passphraseFile: string;
}

export interface RevealArgs {
  path: string;
}

/** Platform key defaults plus existence (no secret reads). */
export function onboardingDefaults(): Promise<KeyDefaults> {
  return invokeValue<KeyDefaults>("onboarding_defaults");
}

/** One generated passphrase for write-down: writes nothing. */
export function onboardingNewPassphrase(): Promise<OnboardingReport> {
  return invokeValue<OnboardingReport>("onboarding_new_passphrase");
}

/** Commit both credential files (passphrase via stdin pipe, never argv). */
export function onboardingCommit(args: OnboardingCommitArgs): Promise<OnboardingReport> {
  return invokeValue<OnboardingReport>("onboarding_commit", { args });
}

/** Verify both files through the hardened reader (exit codes only). */
export function onboardingVerify(args: OnboardingVerifyArgs): Promise<OnboardingReport> {
  return invokeValue<OnboardingReport>("onboarding_verify", { args });
}

/** Verify the pair, then record it as the adopted credential pair so the gate and later launches resolve it. */
export function onboardingAdopt(args: OnboardingAdoptArgs): Promise<OnboardingReport> {
  return invokeValue<OnboardingReport>("onboarding_adopt", { args });
}

/** Best-effort reveal of one path in the platform file manager. */
export function revealInFolder(path: string): Promise<boolean> {
  return invokeValue<boolean>("reveal_in_folder", { args: { path } });
}
