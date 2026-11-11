<script lang="ts">
  import HeldMark from "$lib/components/held-mark.svelte";
  import { describe } from "$lib/ipc/errors.js";
  import { IpcTransportError } from "$lib/ipc/client.js";
  import {
    driveSettings,
    freshness,
    listDrives,
    liveness,
    observe,
    reconnect,
  } from "$lib/ipc/operations.js";
  import type { IpcResult } from "$lib/ipc/types.js";
  import type { DriveView } from "$lib/ipc/results.js";
  import { presentation } from "$lib/state/presentation.svelte.js";
  import type { ShellView } from "$lib/state/presentation.svelte.js";
  import LifecycleView from "$lib/views/LifecycleView.svelte";
  import SettingsView from "$lib/views/SettingsView.svelte";
  import StatusView from "$lib/views/StatusView.svelte";
  import UnavailableView from "$lib/views/UnavailableView.svelte";
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

  const lastError = $derived(
    presentation.lastResult !== null && !presentation.lastResult.ok
      ? describe(presentation.lastResult.error)
      : null,
  );
  const unavailableDetail = $derived(
    presentation.lastResult !== null &&
      !presentation.lastResult.ok &&
      presentation.lastResult.error.kind === "unavailable"
      ? presentation.lastResult.error.detail
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
    await refreshCurrent();
  }

  onMount(() => {
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
    });
  });
</script>

<div class="flex min-h-screen bg-bone font-sans text-ink">
  <aside class="w-56 shrink-0 bg-ink p-4 text-bone">
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
    {#if drives.length > 1}
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
  </aside>

  <main class="max-w-2xl flex-1 p-8" aria-live="polite">
    {#if selectedDriveDir !== null && selectedAvailability === "unavailable" && (presentation.selectedView === "status" || presentation.selectedView === "settings")}
      <UnavailableView
        driveLabel={selectedDriveDir}
        detail={unavailableDetail}
        pending={busy}
        onRetry={() => void retryConnection(selectedDriveDir)}
      />
    {:else if presentation.selectedView === "status"}
      {#if selectedDriveDir === null}
        <p class="mt-4 text-sm text-smoke">
          No drive selected. Open Drives to register or select one — creation and joining run in the
          installation and owner flows, not in this shell.
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
        onRefresh={() => void refreshLifecycle()}
        onSelect={(driveDir) => selectDrive(driveDir)}
      />
    {:else if selectedDriveDir === null}
      <p class="mt-4 text-sm text-smoke">
        No drive selected. Open Drives to register or select one — creation and joining run in the
        installation and owner flows, not in this shell.
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
