<!-- Lifecycle, observational: the registered drives, each
verified this session, and the drive this shell serves. Creation,
joining, and leaving live outside this shell — in the provisioning
and membership flows the installer owns — so this view observes
and guides instead of executing. -->
<script lang="ts">
  import Button from "$lib/components/ui/button.svelte";
  import type { Stamped } from "$lib/state/presentation.svelte.js";
  import type { DriveView, RegistryState } from "$lib/ipc/results.js";
  import { availabilityLabel } from "./format.js";
  import ObservedNote from "./ObservedNote.svelte";

  interface Props {
    snapshot: Stamped<RegistryState> | null;
    pending: boolean;
    transportNotice: string | null;
    onRefresh: () => void;
  }

  let { snapshot, pending, transportNotice, onRefresh }: Props = $props();

  function currentDrive(drives: DriveView[]): DriveView | null {
    return drives.find((drive) => drive.current) ?? null;
  }
</script>

<div class="flex items-center justify-between">
  <h1 class="text-2xl font-semibold tracking-tight">Drives</h1>
  <Button variant="secondary" size="sm" disabled={pending} onclick={onRefresh}>
    {pending ? "Reading…" : "Refresh"}
  </Button>
</div>

{#if transportNotice !== null}
  <p class="mt-2 text-sm" role="alert">{transportNotice}</p>
{/if}

{#if snapshot === null}
  <p class="mt-4 text-sm text-smoke">No registry read yet. Refresh to list the registered drives.</p>
  <ObservedNote observedAtMs={null} />
{:else}
  {@const registry = snapshot.data}
  {@const current = currentDrive(registry.drives)}
  {#if registry.status === "unreadable"}
    <p class="mt-4 rounded-md bg-ink px-3 py-2 text-sm text-bone" role="alert">
      Registry unreadable: {registry.reason} — showing nothing rather than guessing.
    </p>
  {:else if registry.status === "nofile"}
    <p class="mt-4 text-sm text-smoke">
      No drives registered yet. Creation and joining happen in the installation and provisioning flow —
      once a drive is registered, it appears here.
    </p>
  {:else}
    {#if current !== null}
      <section aria-label="This drive" class="mt-4 rounded-md bg-ink px-4 py-3 text-bone">
        <h2 class="text-sm font-semibold tracking-wide uppercase">This drive</h2>
        <p class="mt-1 text-lg font-semibold">{current.name}</p>
        <p class="mt-1 text-sm text-bone/80">{availabilityLabel(current.availability)}</p>
        <p class="mt-1 text-xs text-bone/70">{current.drive_dir} · {current.mode} · service {current.service}</p>
        {#if current.relays.length > 0}
          <p class="mt-1 text-xs text-bone/70">{current.relays.length} relay{current.relays.length === 1 ? "" : "s"} configured</p>
        {/if}
      </section>
    {:else}
      <p class="mt-4 text-sm text-smoke">
        This drive is not in the registry — it was launched directly, not through registration.
      </p>
    {/if}

    {#if registry.drives.length > 0}
      <section aria-label="Registered drives" class="mt-6">
        <h2 class="text-sm font-semibold tracking-wide text-smoke uppercase">Registered drives</h2>
        <ul class="mt-2 space-y-2">
          {#each registry.drives as drive (drive.name)}
            <li class="rounded-md border border-ink/15 px-3 py-2 text-sm">
              <p class="font-medium">{drive.name}{drive.current ? " · this drive" : ""}</p>
              <p class="mt-0.5 text-smoke">{availabilityLabel(drive.availability)}</p>
              <p class="mt-0.5 text-xs text-smoke">{drive.drive_dir} · {drive.mode} · service {drive.service}</p>
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if registry.skipped.length > 0}
      <section aria-label="Skipped entries" class="mt-6">
        <h2 class="text-sm font-semibold tracking-wide text-smoke uppercase">Skipped entries</h2>
        <ul class="mt-2 space-y-1 text-xs text-smoke">
          {#each registry.skipped as reason (reason)}
            <li>{reason}</li>
          {/each}
        </ul>
      </section>
    {/if}
  {/if}

  <section aria-label="Lifecycle guidance" class="mt-6 rounded-md border border-ink/15 px-3 py-2 text-sm text-smoke">
    <p>
      Drive creation, joining, and leaving are provisioning and membership operations — they run in the
      installation and owner flows, not in this shell. This view verifies what those flows registered and
      whether each drive's owner answers; it never provisions or authors membership itself.
    </p>
  </section>

  <ObservedNote observedAtMs={snapshot.observedAtMs} />
{/if}
