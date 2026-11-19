<script lang="ts">
  import Brand from "$lib/components/brand.svelte";
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import * as Tabs from "$lib/components/ui/tabs/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import AvailabilityBadge from "$lib/components/availability-badge.svelte";
  import CopyButton from "$lib/components/copy-button.svelte";
  import { describe } from "$lib/ipc/errors.js";
  import { IpcTransportError } from "$lib/ipc/client.js";
  import {
    applySettings,
    driveLock,
    driveSettings,
    driveUnlock,
    freshness,
    listDrives,
    liveness,
    observe,
    onboardingAdopt,
    onboardingCommit,
    onboardingDefaults,
    onboardingNewPassphrase,
    onboardingVerify,
    provisionCreate,
    provisionJoin,
    provisionLeave,
    provisionReconfigure,
    provisionServiceStart,
    reconnect,
    revealInFolder,
  } from "$lib/ipc/operations.js";
  import type {
    OnboardingAdoptArgs,
    OnboardingCommitArgs,
    OnboardingVerifyArgs,
    ProvisionCreateArgs,
    ProvisionJoinArgs,
    ProvisionLeaveArgs,
    ProvisionReconfigureArgs,
    ProvisionServiceStartArgs,
  } from "$lib/ipc/operations.js";
  import type { IpcResult } from "$lib/ipc/types.js";
  import type {
    DriveView,
    KeyDefaults,
    OnboardingReport,
    ProvisionReport,
    SettingsWriteReport,
  } from "$lib/ipc/results.js";
  import type { SparseUpdateArgs } from "$lib/ipc/results.js";
  import { settingsSurviveLock, withDevCliHint } from "$lib/views/format.js";
  import {
    autoOpenTarget,
    headerAffordance,
    soleDriveDir,
  } from "$lib/views/single-drive-select.js";
  import type { HeaderAffordance } from "$lib/views/single-drive-select.js";
  import { presentation } from "$lib/state/presentation.svelte.js";
  import type { ShellView } from "$lib/state/presentation.svelte.js";
  import LifecycleView from "$lib/views/LifecycleView.svelte";
  import OnboardingView from "$lib/views/OnboardingView.svelte";
  import ProvisioningView from "$lib/views/ProvisioningView.svelte";
  import RepairView from "$lib/views/RepairView.svelte";
  import SettingsView from "$lib/views/SettingsView.svelte";
  import StatusView from "$lib/views/StatusView.svelte";
  import UnavailableView from "$lib/views/UnavailableView.svelte";
  import UnlockView from "$lib/views/UnlockView.svelte";
  import LayoutGridIcon from "@lucide/svelte/icons/layout-grid";
  import PlusIcon from "@lucide/svelte/icons/plus";
  import LockIcon from "@lucide/svelte/icons/lock";
  import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
  import { onMount } from "svelte";

  // macOS keeps its native traffic lights over the ink header
  // (`titleBarStyle: "Overlay"` + `hiddenTitle`). Reserve the strip
  // they occupy so the lockup never sits under them; other platforms
  // keep native decorations and need no inset.
  const trafficLightInset =
    typeof navigator !== "undefined" && navigator.userAgent.includes("Mac");

  const drives = $derived<DriveView[]>(
    presentation.lifecycleSnapshot === null ? [] : registryDrives(),
  );

  function registryDrives(): DriveView[] {
    const snapshot = presentation.lifecycleSnapshot;
    if (snapshot === null) return [];
    return snapshot.data.status === "ready" || snapshot.data.status === "nofile"
      ? snapshot.data.drives
      : [];
  }

  const selectedDriveDir = $derived<string | null>(
    presentation.selectedDriveDir,
  );

  const selectedDrive = $derived<DriveView | null>(
    selectedDriveDir === null
      ? null
      : (drives.find((drive) => drive.drive_dir === selectedDriveDir) ?? null),
  );

  // The registry owns the drive's name. Where the registry cannot be
  // read (or the entry was skipped), the path below still identifies
  // the drive; the heading never invents a name.
  const selectedDriveName = $derived(selectedDrive?.name ?? "Drive");

  const selectedAvailability = $derived(
    selectedDriveDir === null
      ? "unknown"
      : presentation.availabilityFor(selectedDriveDir),
  );

  const selectedResult = $derived(
    selectedDriveDir === null
      ? null
      : presentation.lastResultFor(selectedDriveDir),
  );

  const lastError = $derived(
    selectedResult !== null && !selectedResult.ok
      ? describe(selectedResult.error)
      : null,
  );
  const unavailableDetail = $derived(
    selectedResult !== null &&
      !selectedResult.ok &&
      selectedResult.error.kind === "unavailable"
      ? selectedResult.error.detail
      : null,
  );
  const busy = $derived(presentation.pendingRequests > 0);

  /**
   * One owner read against one drive: settle the taxonomy, and
   * mark that drive available only on a fresh terminal answer. A
   * transport throw (the backend itself unreachable) is not an
   * owner answer — it settles the dispatch and surfaces as a
   * notice, never as taxonomy. One drive's outcome never changes
   * another drive's availability.
   */
  async function readOwner<T>(
    driveDir: string,
    run: () => Promise<IpcResult<T>>,
  ): Promise<T | null> {
    presentation.trackRequest();
    try {
      const result = await run();
      presentation.settleRequest(driveDir, result);
      if (!result.ok) return null;
      presentation.markAvailable(driveDir);
      return result.value;
    } catch (error: unknown) {
      presentation.finishRequest();
      presentation.noteTransportFailure(
        error instanceof IpcTransportError
          ? "The shell backend did not answer (transport failure, not an owner answer)."
          : `Unexpected owner-read failure: ${String(error)}`,
      );
      return null;
    }
  }

  async function refreshStatus(driveDir: string): Promise<void> {
    const [observed, live, fresh] = await Promise.all([
      readOwner(driveDir, () => observe(driveDir)),
      readOwner(driveDir, () => liveness(driveDir)),
      readOwner(driveDir, () => freshness(driveDir)),
    ]);
    // A partial status would mix generations: store only the whole
    // terminal answer, keep the stamped one otherwise.
    if (observed !== null && live !== null && fresh !== null) {
      presentation.storeStatus(driveDir, {
        observe: observed,
        liveness: live,
        freshness: fresh,
      });
    }
  }

  /** One local query: no taxonomy, no availability derivation — unknown states ride in the payload. */
  async function readLocal<T>(run: () => Promise<T>, store: (value: T) => void): Promise<void> {
    presentation.trackRequest();
    try {
      const value = await run();
      presentation.clearTransportNotice();
      store(value);
    } catch (error: unknown) {
      const detail =
        error instanceof IpcTransportError
          ? "The shell backend did not answer (transport failure, not an owner answer)."
          : `Unexpected local-query failure: ${String(error)}`;
      presentation.noteTransportFailure(detail);
    } finally {
      presentation.finishRequest();
    }
  }

  async function refreshLifecycle(): Promise<void> {
    await readLocal(
      () => listDrives(),
      (value) => presentation.storeLifecycle(value),
    );
    const listed = registryDrives();
    const beforeSelected = presentation.selectedDriveDir;
    const beforeView = presentation.selectedView;
    // First run: nothing selected yet. The sole drive opens
    // directly on Status (as if picked); two or more park on
    // the overview; an empty registry stays driveless. The add
    // page keeps the screen where it owns it — the selection
    // still repairs behind it, but the view never yanks off an
    // in-progress report.
    if (beforeSelected === null) {
      const target = autoOpenTarget(listed, null, beforeView);
      if (target !== null) {
        presentation.selectDrive(target.driveDir);
        if (target.advanceToStatus) {
          presentation.selectView("status");
          void refreshStatus(target.driveDir);
        }
        return;
      }
      if (listed.length > 0 && listed[0] !== undefined) {
        presentation.selectDrive(listed[0].drive_dir);
      }
      if (beforeView !== "provision") {
        presentation.selectView("lifecycle");
      }
      return;
    }
    // The registry is the device's list of drives. Where a readable
    // registry no longer names the selected drive (it was left), the
    // workspace has nothing to show: the remaining sole drive opens
    // directly, otherwise return to the device home. An unreadable
    // registry proves nothing, so the selection stands.
    const registryStatus = presentation.lifecycleSnapshot?.data.status;
    const readable = registryStatus === "ready" || registryStatus === "nofile";
    const inWorkspace =
      presentation.selectedView === "status" || presentation.selectedView === "settings";
    if (
      readable &&
      inWorkspace &&
      !listed.some((drive) => drive.drive_dir === presentation.selectedDriveDir)
    ) {
      const sole = soleDriveDir(listed);
      if (sole !== null) {
        presentation.selectDrive(sole);
        presentation.selectView("status");
        void refreshStatus(sole);
      } else {
        presentation.selectView("lifecycle");
      }
      return;
    }
    // Mid-session collapse: parked on the overview while the
    // registry names a single drive — advance to it. Any other
    // screen keeps its place; the header affordance already
    // follows the count without a reload.
    if (presentation.selectedView === "lifecycle") {
      const target = autoOpenTarget(listed, presentation.selectedDriveDir, "lifecycle");
      if (target !== null) {
        presentation.selectDrive(target.driveDir);
        presentation.selectView("status");
        void refreshStatus(target.driveDir);
      }
    }
  }

  async function refreshSettings(driveDir: string): Promise<void> {
    await readLocal(
      () => driveSettings(driveDir),
      (value) => presentation.storeSettings(driveDir, value),
    );
  }

  /**
   * One lifecycle operation through the CLI adapter: infallible
   * like the other local queries — the report carries the outcome
   * plus the post-run observation, and a transport throw (the
   * backend itself unreachable) surfaces as a notice, never as an
   * outcome. The registry re-read follows every operation.
   */
  async function runProvision(call: () => Promise<ProvisionReport>): Promise<ProvisionReport> {
    presentation.trackRequest();
    try {
      const report = await call();
      presentation.clearTransportNotice();
      return report;
    } catch (error: unknown) {
      const detail =
        error instanceof IpcTransportError
          ? "The shell backend did not answer (transport failure, not an owner answer)."
          : `Unexpected provisioning failure: ${String(error)}`;
      presentation.noteTransportFailure(detail);
      throw new Error(detail);
    } finally {
      presentation.finishRequest();
    }
  }

  /**
   * One settings edit through the CLI adapter: infallible like
   * provisioning — the report carries the outcome plus the
   * post-run observation, and the settings re-read follows every
   * operation so the display always shows observed state.
   */
  async function runSettingsWrite(
    driveDir: string,
    update: SparseUpdateArgs,
  ): Promise<SettingsWriteReport> {
    presentation.trackRequest();
    try {
      const report = await applySettings(driveDir, update);
      presentation.clearTransportNotice();
      await refreshSettings(driveDir);
      return report;
    } catch (error: unknown) {
      const detail =
        error instanceof IpcTransportError
          ? "The shell backend did not answer (transport failure, not an owner answer)."
          : `Unexpected settings failure: ${String(error)}`;
      presentation.noteTransportFailure(detail);
      throw new Error(detail);
    } finally {
      presentation.finishRequest();
    }
  }

  /**
   * One onboarding operation through the CLI adapter: infallible
   * like provisioning — the report carries the outcome plus fresh
   * existence, and a transport throw surfaces as a notice, never as
   * an outcome.
   */
  async function runOnboarding(call: () => Promise<OnboardingReport>): Promise<OnboardingReport> {
    presentation.trackRequest();
    try {
      const report = await call();
      presentation.clearTransportNotice();
      return report;
    } catch (error: unknown) {
      const detail =
        error instanceof IpcTransportError
          ? "The shell backend did not answer (transport failure, not an owner answer)."
          : `Unexpected onboarding failure: ${String(error)}`;
      presentation.noteTransportFailure(detail);
      throw new Error(detail);
    } finally {
      presentation.finishRequest();
    }
  }

  // First-run identity gate: no usable credentials -> onboarding
  // (welcome) or repair — never a silent invitation to generate, and
  // never past the gate on an unverified state. A verified state
  // auto-unlocks: the remembered passphrase means no typing. An
  // unreadable gate (transport failure, unusable paths) is its own
  // state with a retry — it never opens as done.
  type OnboardingGate = "loading" | "welcome" | "repair" | "unknown" | "done";
  let gate = $state<OnboardingGate>("loading");
  let keyDefaults = $state<KeyDefaults | null>(null);
  let repairDetail = $state("");
  // Repair has two causes with different fixes: unreadable
  // credentials (the identity-file repair states) versus a broken
  // app pairing (the desktop could not run its `wyrd` helper — never
  // evidence about the credential files). The kind selects the
  // headline; the pairing path renders before any credential state.
  let repairKind = $state<"credentials" | "versionmismatch">("credentials");
  let gateDetail = $state("");
  let onboardedIdentityFile = $state<string | null>(null);
  let onboardedPassphraseFile = $state<string | null>(null);
  let pendingProvisionMode = $state<"create" | "join">("create");
  // Fresh keys for the onboarding component (discard resets it).
  let onboardingEpoch = $state(0);
  // The last recovery attempt's refused outcome, keyed by drive so
  // a refusal on one drive never renders on another's screen. Shown
  // on the unavailable screen so a refused Start/Reinstall is never
  // silent. Cleared on every new attempt; a completed report clears
  // it through `refusedDetail` returning null.
  let recovery: { driveDir: string; detail: string } | null = $state(null);
  // The last unlock attempt's refused outcome, keyed by drive like
  // `recovery` so a refusal on one drive never renders on another's
  // screen. Shown on the unlock card; the passphrase that produced
  // it is already cleared by the unlock view.
  let unlockRefused: { driveDir: string; detail: string } | null = $state(null);

  const selectedServiceState = $derived(selectedDrive?.service_state ?? null);
  const selectedSession = $derived(selectedDrive?.session ?? null);
  const selectedSessionDetail = $derived(
    selectedSession !== null && selectedSession.state === "locked"
      ? selectedSession.detail
      : null,
  );
  const canUnlock = $derived(onboardedIdentityFile !== null);
  const canReinstall = $derived(
    onboardedIdentityFile !== null && onboardedPassphraseFile !== null,
  );

  /**
   * The refused half of a lifecycle report as display text, or null
   * where the operation completed (observed state decides the
   * display; a completed report carries no verdict of its own).
   */
  function refusedDetail(report: ProvisionReport): string | null {
    switch (report.result.result) {
      case "completed":
        return null;
      case "expectedfailure":
      case "helddrive":
      case "unknown":
        return report.result.detail;
      case "versionmismatch":
        return report.result.reason;
      case "busy":
        return "Another operation is already running on this drive. Wait for it to finish and try again.";
    }
  }

  async function checkIdentityGate(): Promise<void> {
    gate = "loading";
    try {
      const defaults = await onboardingDefaults();
      keyDefaults = defaults;
      if (!defaults.usable) {
        gateDetail = "Could not determine this device's credential locations: no home directory resolves. Fix the environment and retry — nothing was written.";
        gate = "unknown";
        return;
      }
      if (!defaults.identity_exists && !defaults.passphrase_exists) {
        gate = "welcome";
        return;
      }
      if (!defaults.identity_exists || !defaults.passphrase_exists) {
        repairDetail = "Missing credentials: one of the two files is missing — expected both the identity file and the passphrase file.";
        repairKind = "credentials";
        gate = "repair";
        return;
      }
      const report = await runOnboarding(() =>
        onboardingVerify({ identityFile: defaults.identity_file, passphraseFile: defaults.passphrase_file }),
      );
      if (report.result.result === "completed") {
        onboardedIdentityFile = defaults.identity_file;
        onboardedPassphraseFile = defaults.passphrase_file;
        gate = "done";
      } else {
        if (report.result.result === "versionmismatch") {
          // A broken pairing is never a credential verdict: the
          // version gate names the pair, so the repair screen must
          // not present the identity-file states.
          repairDetail = `Version mismatch (reinstall the matched pair): ${withDevCliHint(report.result.reason)}`;
          repairKind = "versionmismatch";
        } else {
          repairDetail = report.result.detail;
          repairKind = "credentials";
        }
        gate = "repair";
      }
    } catch (error: unknown) {
      // Transport failure: the backend did not answer, so nothing is
      // verified. Stay explicit — retry, never open as done.
      gateDetail = error instanceof Error ? error.message : String(error);
      gate = "unknown";
    }
  }

  function onboardingDone(
    identityFile: string,
    passphraseFile: string,
    mode: "create" | "join",
  ): void {
    // The pair is already adopted (the commit and restore paths
    // record it before reporting success); the gate resolves it on
    // the next check. This only carries the session forward.
    pendingProvisionMode = mode;
    onboardedIdentityFile = identityFile;
    onboardedPassphraseFile = passphraseFile;
    gate = "done";
    void refreshLifecycle().then(() => {
      // Ready's Create/Join choice seeds the dedicated add page; the
      // registry read above lands the drive list behind it.
      openProvisioning(mode);
    });
  }

  function repairCreateNew(): void {
    // Old files stay where they are (never overwritten, never
    // deleted): the fresh identity commits to `-new` siblings, shown
    // as the intended locations until Finish commits and adopts them.
    if (keyDefaults !== null) {
      keyDefaults = {
        identity_file: `${keyDefaults.identity_file}-new`,
        passphrase_file: `${keyDefaults.passphrase_file}-new`,
        identity_exists: false,
        passphrase_exists: false,
        usable: keyDefaults.usable,
      };
    }
    onboardingEpoch += 1;
    gate = "welcome";
  }

  function quitToStart(): void {
    // Discard confirmed in the view: reset to a fresh flow. The app
    // cannot function without an identity, so quitting mid-flow
    // returns to the start rather than to an unverified Drives view.
    onboardingEpoch += 1;
  }

  async function reveal(path: string): Promise<boolean> {
    try {
      return await revealInFolder(path);
    } catch {
      return false;
    }
  }

  function refreshCurrent(): Promise<void> {
    switch (presentation.selectedView) {
      case "status":
        return selectedDriveDir === null
          ? Promise.resolve()
          : refreshStatus(selectedDriveDir);
      case "provision":
        // The add page owns its own form and re-reads the registry
        // after each operation; selecting it fetches nothing.
        return Promise.resolve();
      case "lifecycle":
        return refreshLifecycle();
      case "settings":
        return selectedDriveDir === null
          ? Promise.resolve()
          : refreshSettings(selectedDriveDir);
    }
  }

  function selectView(view: ShellView): void {
    presentation.selectView(view);
    void refreshCurrent();
  }

  /**
   * Open one drive's workspace: select it and land on Status. The
   * selection is presentation state only — every drive-scoped
   * command still carries the directory explicitly.
   */
  function openDrive(driveDir: string): void {
    presentation.selectDrive(driveDir);
    presentation.selectView("status");
    void refreshStatus(driveDir);
  }

  /** Back to the device-level home: the registry of drives —
   * or the sole drive's workspace directly where only one drive
   * is configured (the overview earns no keep without a choice).
   */
  function allDrives(): void {
    const sole = soleDriveDir(registryDrives());
    if (sole !== null) {
      openDrive(sole);
      return;
    }
    selectView("lifecycle");
  }

  /**
   * Open the dedicated add page. The mode only seeds the tab; the
   * page still runs through the constrained CLI adapter.
   */
  function openProvisioning(mode: "create" | "join"): void {
    pendingProvisionMode = mode;
    presentation.selectView("provision");
  }

  /** Explicit retry for one drive: reconnect, then re-read. Recovery proves nothing by itself. */
  async function retryConnection(driveDir: string): Promise<void> {
    recovery = null;
    unlockRefused = null;
    presentation.trackRequest();
    try {
      const result = await reconnect(driveDir);
      presentation.settleRequest(driveDir, result);
      if (!result.ok) return;
    } catch (error: unknown) {
      presentation.finishRequest();
      presentation.noteTransportFailure(
        error instanceof IpcTransportError
          ? "The shell backend did not answer (transport failure, not an owner answer)."
          : `Unexpected reconnect failure: ${String(error)}`,
      );
      return;
    }
    // Re-derive availability from a live outcome regardless of the
    // selected view: a settings-only re-read never touches the
    // taxonomy, so without this probe the overlay would persist
    // after a successful reconnect on the Settings view. The probe
    // marks available on a terminal answer and unavailable where no
    // transport answers — it proves nothing about prior work.
    await readOwner(driveDir, () => liveness(driveDir));
    await refreshCurrent();
  }

  /**
   * Start one drive's installed service (the "installed but
   * stopped" / "wedged" recovery): enable and start the existing
   * definition through the constrained CLI adapter, then re-read
   * the registry and re-derive availability like `retryConnection`
   * does — a settings-scoped re-read never touches the taxonomy,
   * so without the explicit probe the overlay would persist after
   * a successful start on the Settings tab. Explicit, never
   * automatic — a stopped drive stays stopped until this runs. A
   * refused outcome surfaces on the screen; a transport throw
   * surfaces as the recovery detail instead of an unhandled
   * rejection. Observed state decides the display; the report's
   * outcome never does.
   */
  async function startService(driveDir: string): Promise<void> {
    recovery = null;
    unlockRefused = null;
    const args: ProvisionServiceStartArgs = { driveDir };
    try {
      const report = await runProvision(() => provisionServiceStart(args));
      const detail = refusedDetail(report);
      recovery = detail === null ? null : { driveDir, detail };
    } catch (error: unknown) {
      recovery = {
        driveDir,
        detail:
          error instanceof Error ? error.message : `Unexpected start failure: ${String(error)}`,
      };
      return;
    }
    await refreshLifecycle();
    await readOwner(driveDir, () => liveness(driveDir));
    await refreshCurrent();
  }

  /**
   * One in-place reconfigure with unchanged supervision: empty
   * relays keep the registered set, mode and mountpoint ride from
   * the registry observation, and the service is reinstalled —
   * which restarts the owner where one is managed. Shared by the
   * unavailable-overlay reinstall and the settings restart-to-apply
   * so the two can never drift into different argv. Null where the
   * gate-verified credential paths are absent; transport and
   * unexpected throws propagate to the caller, which reports them
   * in its own surface.
   */
  async function reconfigureSame(driveDir: string): Promise<ProvisionReport | null> {
    if (onboardedIdentityFile === null || onboardedPassphraseFile === null) return null;
    const mode = selectedDrive?.mode ?? "vault";
    const args: ProvisionReconfigureArgs = {
      driveDir,
      relays: [],
      identityFile: onboardedIdentityFile,
      passphraseFile: onboardedPassphraseFile,
      mode,
      mountpoint: selectedDrive?.mountpoint ?? undefined,
      noService: false,
    };
    return await runProvision(() => provisionReconfigure(args));
  }

  /**
   * Reinstall one drive's service definition (the "no definition
   * installed" recovery): stop and clear, rewrite the entry, and
   * reinstall/restart through the constrained CLI adapter with the
   * gate-verified credential paths, keeping the registered relays
   * and mode. Then re-read the registry and re-derive availability
   * like `retryConnection` does. The same no-silent-refusal rule
   * as `startService` applies.
   */
  async function reinstallService(driveDir: string): Promise<void> {
    if (onboardedIdentityFile === null || onboardedPassphraseFile === null) return;
    recovery = null;
    unlockRefused = null;
    try {
      const report = await reconfigureSame(driveDir);
      if (report === null) return;
      const detail = refusedDetail(report);
      recovery = detail === null ? null : { driveDir, detail };
    } catch (error: unknown) {
      recovery = {
        driveDir,
        detail:
          error instanceof Error
            ? error.message
            : `Unexpected reinstall failure: ${String(error)}`,
      };
      return;
    }
    await refreshLifecycle();
    await readOwner(driveDir, () => liveness(driveDir));
    await refreshCurrent();
  }

  /**
   * Restart one managed drive's service so saved bounds apply: the
   * same stop-and-reinstall reconfigure the recovery path uses,
   * with unchanged supervision (relays kept, mode and mountpoint
   * as registered). Returns the refusal detail, or null where the
   * restart completed *and* the post-restart liveness probe
   * answered — the verdict consults the observation, never restart
   * acceptance alone. A refused reconfigure, a silent owner after a
   * clean restart, and a backend transport throw all return as
   * details; the caller renders saved-but-restart-failed itself.
   */
  async function restartServiceForApply(driveDir: string): Promise<string | null> {
    if (onboardedIdentityFile === null || onboardedPassphraseFile === null) {
      return "This device's identity is not verified yet — finish the identity check first.";
    }
    try {
      const report = await reconfigureSame(driveDir);
      if (report === null) {
        return "This device's identity is not verified yet — finish the identity check first.";
      }
      const refused = refusedDetail(report);
      if (refused !== null) return refused;
    } catch (error: unknown) {
      return error instanceof Error ? error.message : `Unexpected restart failure: ${String(error)}`;
    }
    await refreshLifecycle();
    await readOwner(driveDir, () => liveness(driveDir));
    await refreshCurrent();
    return presentation.availabilityFor(driveDir) === "available"
      ? null
      : "The drive is not answering after the restart — the saved values wait for it to come back.";
  }

  /**
   * Lock one drive for the session and land on Status, where unlock
   * lives: the first half of a settings restart, and the header
   * Lock button. A session unlock needs the passphrase typed, so
   * locking never restarts anything by itself. Refusals surface on
   * the unlock card there, like every other lock.
   */
  async function lockAndShowStatus(driveDir: string): Promise<void> {
    await lockDrive(driveDir);
    selectView("status");
  }

  /**
   * Unlock one drive for the session: spawn the persistent owner
   * through the version-matched CLI with the passphrase on the
   * stdin pipe, verify the socket answers, then re-read the
   * registry and re-derive availability like `startService` does.
   * Explicit, never automatic — a locked drive stays locked until
   * this runs. A refused outcome surfaces on the unlock card;
   * the passphrase that produced it is already cleared by the
   * unlock view and never re-renders.
   */
  async function unlockDrive(driveDir: string, passphrase: string): Promise<void> {
    const identityFile = onboardedIdentityFile;
    if (identityFile === null) {
      unlockRefused = {
        driveDir,
        detail: "This device's identity is not verified yet — finish the identity check first.",
      };
      return;
    }
    unlockRefused = null;
    try {
      const report = await runProvision(() =>
        driveUnlock({ driveDir, identityFile, passphrase }),
      );
      const detail = refusedDetail(report);
      unlockRefused = detail === null ? null : { driveDir, detail };
    } catch (error: unknown) {
      unlockRefused = {
        driveDir,
        detail:
          error instanceof Error ? error.message : `Unexpected unlock failure: ${String(error)}`,
      };
      return;
    }
    await refreshLifecycle();
    await readOwner(driveDir, () => liveness(driveDir));
    await refreshCurrent();
  }

  /**
   * Lock one drive for the session: kill the app-held unlock child
   * and scrub the session secret, then re-read and re-derive
   * availability. Refuses where this app holds no unlock session —
   * a reattached owner the app did not start is never killed.
   */
  async function lockDrive(driveDir: string): Promise<void> {
    unlockRefused = null;
    try {
      const report = await runProvision(() => driveLock({ driveDir }));
      const detail = refusedDetail(report);
      unlockRefused = detail === null ? null : { driveDir, detail };
    } catch (error: unknown) {
      unlockRefused = {
        driveDir,
        detail:
          error instanceof Error ? error.message : `Unexpected lock failure: ${String(error)}`,
      };
      return;
    }
    await refreshLifecycle();
    await readOwner(driveDir, () => liveness(driveDir));
    await refreshCurrent();
  }

  // The header affordance off the overview follows the drive
  // count: the way back earns its keep at two or more drives,
  // while at or below one drive it becomes the Add drive route.
  // The add page always keeps its way back.
  const headerAction = $derived<HeaderAffordance>(
    gate === "done" ? headerAffordance(presentation.selectedView, drives.length) : "none",
  );

  onMount(() => {
    // Startup lands through the registry read — the sole drive
    // opens directly on Status, otherwise the drive list — never
    // an implied active drive past that. The in-window Add button
    // reaches the provision view through `openProvisioning` below;
    // no backend route exists (the tray's select-provision event is
    // gone with the tray).
    void refreshLifecycle().then(() => {
      void checkIdentityGate();
    });
  });
</script>

{#snippet unavailableOverlay(driveDir: string)}
  <UnavailableView
    driveLabel={driveDir}
    detail={unavailableDetail}
    pending={busy}
    onRetry={() => void retryConnection(driveDir)}
    serviceState={selectedServiceState}
    canReinstall={canReinstall}
    recoveryDetail={recovery !== null && recovery.driveDir === driveDir ? recovery.detail : null}
    onStart={() => void startService(driveDir)}
    onReinstall={() => void reinstallService(driveDir)}
  />
{/snippet}

<div class="flex min-h-screen flex-col bg-background font-sans text-foreground">
  <header
    data-tauri-drag-region="deep"
    class="sticky top-0 z-40 flex h-12 shrink-0 items-center gap-3 border-b border-bone/10 bg-ink px-4 text-bone {trafficLightInset
      ? 'pl-20'
      : ''}"
  >
    <Brand
      onHome={headerAction === "back" ? allDrives : undefined}
    />
    <div class="ml-auto flex items-center gap-2">
      {#if busy}
        <span class="hidden items-center gap-1.5 text-xs text-bone/60 sm:flex">
          <Spinner class="size-3" />
          {presentation.pendingRequests} in flight
        </span>
      {/if}
      {#if headerAction === "back"}
        <Button
          variant="ghost"
          size="sm"
          class="text-bone hover:bg-bone/10 hover:text-bone"
          onclick={allDrives}
        >
          <LayoutGridIcon data-icon="inline-start" />
          Back to drives
        </Button>
      {:else if headerAction === "add"}
        <Button
          variant="ghost"
          size="sm"
          class="text-bone hover:bg-bone/10 hover:text-bone"
          onclick={() => openProvisioning("create")}
        >
          <PlusIcon data-icon="inline-start" />
          Add drive
        </Button>
      {/if}
    </div>
  </header>

  <main class="mx-auto w-full max-w-5xl flex-1 px-6 pt-8 pb-16" aria-live="polite">
    {#if gate === "loading"}
      <div class="flex min-h-[45vh] flex-col items-center justify-center gap-3 text-center">
        <Spinner class="size-5 text-muted-foreground" />
        <p class="text-sm text-muted-foreground">Checking this device's identity…</p>
      </div>
    {:else if gate === "welcome" && keyDefaults !== null}
      {#key onboardingEpoch}
        <OnboardingView
          defaults={keyDefaults}
          pending={busy}
          onNewPassphrase={() => runOnboarding(() => onboardingNewPassphrase())}
          onCommit={(args: OnboardingCommitArgs) => runOnboarding(() => onboardingCommit(args))}
          onVerify={(args: OnboardingVerifyArgs) => runOnboarding(() => onboardingVerify(args))}
          onAdopt={(args: OnboardingAdoptArgs) => runOnboarding(() => onboardingAdopt(args))}
          onReveal={reveal}
          onDone={(identityFile, passphraseFile, mode) => onboardingDone(identityFile, passphraseFile, mode)}
          onQuit={quitToStart}
        />
      {/key}
    {:else if gate === "repair" && keyDefaults !== null}
      <RepairView
        defaults={keyDefaults}
        kind={repairKind}
        detail={repairDetail}
        pending={busy}
        onVerify={(args: OnboardingVerifyArgs) => runOnboarding(() => onboardingVerify(args))}
        onAdopt={(args: OnboardingAdoptArgs) => runOnboarding(() => onboardingAdopt(args))}
        onAdopted={(identityFile, passphraseFile) => onboardingDone(identityFile, passphraseFile, "create")}
        onCreateNew={repairCreateNew}
      />
    {:else if gate === "unknown"}
      <Card.Root class="mx-auto mt-10 max-w-xl">
        <Card.Header>
          <Card.Title>This device's identity could not be checked</Card.Title>
          <Card.Description>
            Wyrd cannot read this device's identity state, so it cannot open any drive yet.
          </Card.Description>
        </Card.Header>
        <Card.Content>
          <Alert.Root variant="destructive">
            <p>{gateDetail}</p>
          </Alert.Root>
        </Card.Content>
        <Card.Footer class="justify-end">
          <Button variant="outline" size="sm" disabled={busy} onclick={() => void checkIdentityGate()}>
            <RefreshCwIcon data-icon="inline-start" />
            Retry
          </Button>
        </Card.Footer>
      </Card.Root>
    {:else if presentation.selectedView === "provision"}
      <ProvisioningView
        pending={busy}
        initialIdentityFile={onboardedIdentityFile}
        initialPassphraseFile={onboardedPassphraseFile}
        initialMode={pendingProvisionMode}
        onCreate={(args: ProvisionCreateArgs) => runProvision(() => provisionCreate(args))}
        onJoin={(args: ProvisionJoinArgs) => runProvision(() => provisionJoin(args))}
        onDone={() => void refreshLifecycle()}
        onBack={allDrives}
      />
    {:else if presentation.selectedView === "lifecycle" || selectedDriveDir === null}
      <LifecycleView
        snapshot={presentation.lifecycleSnapshot}
        selectedDriveDir={presentation.selectedDriveDir}
        pending={busy}
        transportNotice={presentation.transportNotice}
        onRefresh={() => void refreshLifecycle()}
        onAdd={() => openProvisioning("create")}
        onOpen={(driveDir) => openDrive(driveDir)}
      />
    {:else}
      <Tabs.Root
        value={presentation.selectedView}
        onValueChange={(value) => selectView(value as ShellView)}
        class="gap-6"
      >
        <div class="flex flex-wrap items-start justify-between gap-x-6 gap-y-4">
          <div class="min-w-0">
            <div class="flex flex-wrap items-center gap-2.5">
              <h1 class="truncate text-2xl font-semibold tracking-tight">{selectedDriveName}</h1>
              <AvailabilityBadge state={selectedAvailability} />
              {#if selectedSession !== null}
                <span
                  class="inline-flex items-center rounded-full border px-2.5 py-0.5 text-xs font-medium {selectedSession.state === 'unlocked'
                    ? 'border-ok/30 bg-ok/10 text-ok'
                    : 'border-primary/40 bg-primary/10 text-foreground'}"
                >
                  {selectedSession.state === "unlocked" ? "Unlocked for this session" : "Locked — needs passphrase"}
                </span>
              {/if}
            </div>
            <div class="mt-1 flex min-w-0 items-center gap-1">
              <span class="truncate font-mono text-xs text-muted-foreground">{selectedDriveDir}</span>
              <CopyButton text={selectedDriveDir} label="drive path" />
            </div>
          </div>
          <div class="flex flex-wrap items-center gap-2.5">
            <Tabs.List>
              <Tabs.Trigger value="status">Status</Tabs.Trigger>
              <Tabs.Trigger value="settings">Settings</Tabs.Trigger>
            </Tabs.List>
            {#if selectedSession !== null && selectedSession.state === "unlocked"}
              <Button
                variant="outline"
                size="sm"
                disabled={busy}
                onclick={() => void lockAndShowStatus(selectedDriveDir)}
              >
                <LockIcon data-icon="inline-start" />
                Lock
              </Button>
            {/if}
          </div>
        </div>

        <Tabs.Content value="status">
          {#if selectedSession !== null && selectedSession.state === "locked"}
            <UnlockView
              driveLabel={selectedDriveDir}
              detail={selectedSessionDetail}
              pending={busy}
              identityReady={canUnlock}
              unlockError={unlockRefused !== null && unlockRefused.driveDir === selectedDriveDir
                ? unlockRefused.detail
                : null}
              onUnlock={(passphrase) => unlockDrive(selectedDriveDir, passphrase)}
            />
            {@render unavailableOverlay(selectedDriveDir)}
          {:else if selectedAvailability === "unavailable"}
            {@render unavailableOverlay(selectedDriveDir)}
          {:else}
            {#if selectedSession !== null && selectedSession.state === "unlocked"}
              <div class="mb-4 flex flex-wrap items-center justify-between gap-3 rounded-xl border border-ok/30 bg-ok/10 px-4 py-3 text-sm">
                <span class="text-foreground">Unlocked for this session — the drive is answering. Lock from the header to end the session; unlocking needs the passphrase to open it again, and rebooting this device or locking the drive also ends the session. Quitting the app leaves the drive running.</span>
              </div>
            {/if}
            <StatusView
              snapshot={presentation.statusSnapshotFor(selectedDriveDir)}
              availability={selectedAvailability}
              pending={busy}
              error={lastError}
              transportNotice={presentation.transportNotice}
              onRefresh={() => void refreshStatus(selectedDriveDir)}
            />
            {#if unlockRefused !== null && unlockRefused.driveDir === selectedDriveDir}
              <Alert.Root variant="destructive" class="mt-4">
                <p>{unlockRefused.detail}</p>
              </Alert.Root>
            {/if}
          {/if}
        </Tabs.Content>

        <Tabs.Content value="settings">
          {#if selectedAvailability === "unavailable" && !settingsSurviveLock(selectedSession)}
            {@render unavailableOverlay(selectedDriveDir)}
          {:else}
            <!-- Locked drives keep their local surfaces here:
                 configured bounds read from the settings file and
                 the identity-pair Leave card need no owner. Status
                 stays gated on unlock; managed-down drives keep the
                 overlay with its Start/Reinstall recovery. -->
            <SettingsView
              driveDir={selectedDriveDir}
              snapshot={presentation.settingsSnapshotFor(selectedDriveDir)}
              pending={busy}
              transportNotice={presentation.transportNotice}
              identityFile={onboardedIdentityFile}
              passphraseFile={onboardedPassphraseFile}
              session={selectedSession}
              onRefresh={() => void refreshSettings(selectedDriveDir)}
              onLeave={(args: ProvisionLeaveArgs) => runProvision(() => provisionLeave(args))}
              onApply={(update: SparseUpdateArgs) =>
                runSettingsWrite(selectedDriveDir, update)
              }
              onRestartService={() => restartServiceForApply(selectedDriveDir)}
              onRefreshDrives={allDrives}
            />
          {/if}
        </Tabs.Content>
      </Tabs.Root>
    {/if}
  </main>
</div>
