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
  import { presentation } from "$lib/state/presentation.svelte.js";
  import type { ShellView } from "$lib/state/presentation.svelte.js";
  import LifecycleView from "$lib/views/LifecycleView.svelte";
  import SettingsView from "$lib/views/SettingsView.svelte";
  import StatusView from "$lib/views/StatusView.svelte";
  import UnavailableView from "$lib/views/UnavailableView.svelte";
  import { onMount } from "svelte";

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
   * One owner read: settle the taxonomy, and mark available only on
   * a fresh terminal answer. A transport throw (the backend itself
   * unreachable) is not an owner answer — it settles the dispatch
   * and surfaces as a notice, never as taxonomy.
   */
  async function readOwner<T>(run: () => Promise<IpcResult<T>>): Promise<T | null> {
    presentation.trackRequest();
    try {
      const result = await run();
      presentation.settleRequest(result);
      if (!result.ok) return null;
      presentation.markAvailable();
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

  async function refreshStatus(): Promise<void> {
    const [observed, live, fresh] = await Promise.all([
      readOwner(() => observe()),
      readOwner(() => liveness()),
      readOwner(() => freshness()),
    ]);
    // A partial status would mix generations: store only the whole
    // terminal answer, keep the stamped one otherwise.
    if (observed !== null && live !== null && fresh !== null) {
      presentation.storeStatus({ observe: observed, liveness: live, freshness: fresh });
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
  }

  async function refreshSettings(): Promise<void> {
    await readLocal(
      () => driveSettings(),
      (value) => presentation.storeSettings(value),
    );
  }

  function refreshCurrent(): Promise<void> {
    switch (presentation.selectedView) {
      case "status":
        return refreshStatus();
      case "lifecycle":
        return refreshLifecycle();
      case "settings":
        return refreshSettings();
    }
  }

  function selectView(view: ShellView): void {
    presentation.selectView(view);
    void refreshCurrent();
  }

  /** Explicit retry from the unavailable view: reconnect, then re-read. Recovery proves nothing by itself. */
  async function retryConnection(): Promise<void> {
    presentation.trackRequest();
    try {
      const result = await reconnect();
      presentation.settleRequest(result);
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
    void refreshStatus();
  });
</script>

<div class="flex min-h-screen bg-bone font-sans text-ink">
  <aside class="w-56 shrink-0 bg-ink p-4 text-bone">
    <p class="flex items-center gap-2 text-lg font-semibold tracking-tight">
      <HeldMark size={24} />
      Wyrd
    </p>
    <p class="mt-2 text-xs text-bone/70">
      Owner: {presentation.availability}
      {#if busy}
        · {presentation.pendingRequests} request(s) in flight
      {/if}
    </p>
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
    {#if presentation.availability === "unavailable"}
      <UnavailableView detail={unavailableDetail} pending={busy} onRetry={() => void retryConnection()} />
    {:else if presentation.selectedView === "status"}
      <StatusView
        snapshot={presentation.statusSnapshot}
        pending={busy}
        error={lastError}
        transportNotice={presentation.transportNotice}
        onRefresh={() => void refreshStatus()}
      />
    {:else if presentation.selectedView === "lifecycle"}
      <LifecycleView
        snapshot={presentation.lifecycleSnapshot}
        pending={busy}
        transportNotice={presentation.transportNotice}
        onRefresh={() => void refreshLifecycle()}
      />
    {:else}
      <SettingsView
        snapshot={presentation.settingsSnapshot}
        pending={busy}
        transportNotice={presentation.transportNotice}
        onRefresh={() => void refreshSettings()}
      />
    {/if}
  </main>
</div>
