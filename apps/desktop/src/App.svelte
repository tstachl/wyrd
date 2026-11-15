<script lang="ts">
  import HeldMark from "$lib/components/held-mark.svelte";
  import Button from "$lib/components/ui/button.svelte";
  import { describe } from "$lib/ipc/errors.js";
  import { IpcTransportError } from "$lib/ipc/client.js";
  import {
    driveSettings,
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
  } from "$lib/ipc/operations.js";
  import type { IpcResult } from "$lib/ipc/types.js";
  import type { DriveView, KeyDefaults, OnboardingReport, ProvisionReport } from "$lib/ipc/results.js";
  import { presentation } from "$lib/state/presentation.svelte.js";
  import type { ShellView } from "$lib/state/presentation.svelte.js";
  import LifecycleView from "$lib/views/LifecycleView.svelte";
  import OnboardingView from "$lib/views/OnboardingView.svelte";
  import RepairView from "$lib/views/RepairView.svelte";
  import SettingsView from "$lib/views/SettingsView.svelte";
  import StatusView from "$lib/views/StatusView.svelte";
  import UnavailableView from "$lib/views/UnavailableView.svelte";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";

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

  const views: Array<{ id: ShellView; label: string }> = [
    { id: "status", label: "Status" },
    { id: "lifecycle", label: "Drives" },
    { id: "settings", label: "Settings" },
  ];

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
    // First-run selection: where nothing is selected yet and the
    // registry names drives, select the first. The selection stays
    // presentation state — later registry changes never silently
    // move it, and an empty registry leaves it null (driveless
    // first-run, not an implicit drive).
    if (presentation.selectedDriveDir === null) {
      const listed = registryDrives();
      if (listed.length > 0 && listed[0] !== undefined) {
        presentation.selectDrive(listed[0].drive_dir);
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
  let gateDetail = $state("");
  let onboardedIdentityFile = $state<string | null>(null);
  let onboardedPassphraseFile = $state<string | null>(null);
  let pendingProvisionMode = $state<"create" | "join">("create");
  // Fresh keys for the onboarding component (discard resets it).
  let onboardingEpoch = $state(0);

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
        repairDetail = "Incomplete credentials: one of the two files is missing — expected both the identity file and the passphrase file.";
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
        repairDetail = report.result.result === "expectedfailure"
          ? report.result.detail
          : report.result.result === "versionmismatch"
            ? `Version mismatch: ${report.result.reason}`
            : report.result.detail;
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
      presentation.selectView("lifecycle");
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

  function selectDrive(driveDir: string): void {
    presentation.selectDrive(driveDir);
    void refreshCurrent();
  }

  /** Explicit retry for one drive: reconnect, then re-read. Recovery proves nothing by itself. */
  async function retryConnection(driveDir: string): Promise<void> {
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

  onMount(() => {
    // Tray route: "Set up / connect drives" shows the window from
    // the backend and asks the frontend to render the lifecycle
    // view. Best-effort: a missing Tauri runtime (browser preview)
    // never fails startup.
    void listen("wyrd:select-lifecycle", () => {
      presentation.selectView("lifecycle");
      void refreshLifecycle();
    }).catch(() => {});
    void refreshLifecycle().then(() => {
      if (presentation.selectedDriveDir === null) {
        // Driveless first-run: land on the empty lifecycle view
        // with its registration guidance, not a status page with
        // nothing to show. The lifecycle snapshot just stored is
        // already current, so no re-read.
        presentation.selectView("lifecycle");
      } else {
        void refreshCurrent();
      }
      void checkIdentityGate();
    });
  });
</script>

<div class="flex min-h-screen bg-bone font-sans text-ink dark:bg-ink dark:text-bone">
  <aside class="w-56 shrink-0 bg-ink p-4 text-bone dark:border-r dark:border-bone/10 dark:bg-black dark:text-bone">
    <p class="flex items-center gap-2 text-lg font-semibold tracking-tight">
      <HeldMark size={24} />
      Wyrd
    </p>
    {#if selectedDriveDir !== null}
      <p class="mt-2 text-xs text-bone/70">
        Drive: {selectedDriveDir}
      </p>
      <p class="mt-1 text-xs text-bone/70">
        Owner: {selectedAvailability}
        {#if busy}
          · {presentation.pendingRequests} request(s) in flight
        {/if}
      </p>
    {:else if busy}
      <p class="mt-2 text-xs text-bone/70">
        {presentation.pendingRequests} request(s) in flight
      </p>
    {/if}
    {#if drives.length > 1 && gate === "done"}
      <nav aria-label="Drives" class="mt-4 space-y-1">
        {#each drives as drive (drive.drive_dir)}
          <button
            type="button"
            aria-current={presentation.selectedDriveDir === drive.drive_dir ? "true" : undefined}
            onclick={() => selectDrive(drive.drive_dir)}
            class="w-full rounded-md px-3 py-2 text-left text-sm font-medium transition-colors {presentation.selectedDriveDir ===
            drive.drive_dir
              ? 'bg-amber text-ink'
              : 'text-bone hover:bg-bone/10'}"
          >
            {drive.name}
          </button>
        {/each}
      </nav>
    {/if}
    {#if gate === "done"}
    <nav aria-label="Views" class="mt-4 space-y-1">
      {#each views as view (view.id)}
        <button
          type="button"
          aria-current={presentation.selectedView === view.id ? "page" : undefined}
          onclick={() => selectView(view.id)}
          class="w-full rounded-md px-3 py-2 text-left text-sm font-medium transition-colors {presentation.selectedView ===
          view.id
            ? 'bg-amber text-ink'
            : 'text-bone hover:bg-bone/10'}"
        >
          {view.label}
        </button>
      {/each}
    </nav>
    {:else if gate === "unknown"}
    <p class="mt-4 text-xs text-bone/70">This device's identity could not be checked — retry in the main view to unlock Status, Drives, and Settings.</p>
    {:else}
    <p class="mt-4 text-xs text-bone/70">Set up this device's identity to unlock Status, Drives, and Settings.</p>
    {/if}
  </aside>

  <main class="max-w-2xl flex-1 p-8" aria-live="polite">
    {#if gate === "loading"}
      <p class="mt-4 text-sm text-smoke">Checking this device's identity…</p>
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
        detail={repairDetail}
        pending={busy}
        onVerify={(args: OnboardingVerifyArgs) => runOnboarding(() => onboardingVerify(args))}
        onAdopt={(args: OnboardingAdoptArgs) => runOnboarding(() => onboardingAdopt(args))}
        onAdopted={(identityFile, passphraseFile) => onboardingDone(identityFile, passphraseFile, "create")}
        onCreateNew={repairCreateNew}
      />
    {:else if gate === "unknown"}
      <p class="mt-4 text-sm" role="alert">This device's identity could not be checked: {gateDetail}</p>
      <div class="mt-2">
        <Button variant="secondary" size="sm" disabled={busy} onclick={() => void checkIdentityGate()}>Retry</Button>
      </div>
    {:else if selectedDriveDir !== null && selectedAvailability === "unavailable" && (presentation.selectedView === "status" || presentation.selectedView === "settings")}
      <UnavailableView
        driveLabel={selectedDriveDir}
        detail={unavailableDetail}
        pending={busy}
        onRetry={() => void retryConnection(selectedDriveDir)}
      />
    {:else if presentation.selectedView === "status"}
      {#if selectedDriveDir === null}
        <p class="mt-4 text-sm text-smoke">
          No drive selected. Open Drives to create, join, or select one.
        </p>
      {:else}
        {@const snapshot = presentation.statusSnapshotFor(selectedDriveDir)}
        <StatusView
          driveLabel={selectedDriveDir}
          snapshot={snapshot}
          pending={busy}
          error={lastError}
          transportNotice={presentation.transportNotice}
          onRefresh={() => void refreshStatus(selectedDriveDir)}
        />
      {/if}
    {:else if presentation.selectedView === "lifecycle"}
      <LifecycleView
        snapshot={presentation.lifecycleSnapshot}
        selectedDriveDir={presentation.selectedDriveDir}
        pending={busy}
        transportNotice={presentation.transportNotice}
        initialIdentityFile={onboardedIdentityFile}
        initialPassphraseFile={onboardedPassphraseFile}
        initialMode={pendingProvisionMode}
        onRefresh={() => void refreshLifecycle()}
        onSelect={(driveDir) => selectDrive(driveDir)}
        onCreate={(args: ProvisionCreateArgs) => runProvision(() => provisionCreate(args))}
        onJoin={(args: ProvisionJoinArgs) => runProvision(() => provisionJoin(args))}
        onLeave={(args: ProvisionLeaveArgs) => runProvision(() => provisionLeave(args))}
      />
    {:else if selectedDriveDir === null}
      <p class="mt-4 text-sm text-smoke">
        No drive selected. Open Drives to create, join, or select one.
      </p>
    {:else}
      {@const snapshot = presentation.settingsSnapshotFor(selectedDriveDir)}
      <SettingsView
        driveLabel={selectedDriveDir}
        snapshot={snapshot}
        pending={busy}
        transportNotice={presentation.transportNotice}
        onRefresh={() => void refreshSettings(selectedDriveDir)}
      />
    {/if}
  </main>
</div>
