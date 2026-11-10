<script lang="ts">
  import Button from "$lib/components/ui/button.svelte";
  import { describe } from "$lib/ipc/errors.js";
  import { presentation } from "$lib/state/presentation.svelte.js";

  const lastError =
    presentation.lastResult !== null && !presentation.lastResult.ok
      ? describe(presentation.lastResult.error)
      : null;
</script>

<div class="flex min-h-screen bg-neutral-100 text-neutral-900">
  <aside class="w-56 shrink-0 border-r border-neutral-200 bg-neutral-50 p-4">
    <p class="text-lg font-semibold tracking-tight">Wyrd</p>
    <nav aria-label="Views" class="mt-4">
      <p class="rounded-md bg-neutral-900 px-3 py-2 text-sm font-medium text-neutral-50">
        Status
      </p>
    </nav>
  </aside>

  <main class="max-w-2xl flex-1 p-8" aria-live="polite">
    <h1 class="text-2xl font-semibold tracking-tight">Status</h1>
    <p class="mt-2 text-sm text-neutral-600">
      Desktop shell: the owner connection is not wired yet.
    </p>
    <p class="mt-1 text-sm text-neutral-600">
      Owner availability: {presentation.availability}.
      {#if presentation.pendingRequests > 0}
        {presentation.pendingRequests} request(s) in flight.
      {/if}
    </p>
    {#if lastError !== null}
      <p class="mt-2 text-sm text-neutral-800">{lastError}</p>
    {/if}
    <div class="mt-4">
      <Button
        disabled
        title="Per-operation IPC calls land with the Tauri command layer (first view)."
      >
        Retry when wired
      </Button>
    </div>
  </main>
</div>
