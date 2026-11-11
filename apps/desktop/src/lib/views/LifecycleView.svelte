<!-- Lifecycle, observational: the registered drives, each
verified this session, and the explicitly selected drive.
Creation, joining, and leaving live outside this shell — in the
provisioning and membership flows the installer owns — so this
view observes and guides instead of executing. The selection is
presentation state only: it never authorizes backend access, and
every command carries the drive directory explicitly. -->
<script lang="ts">
  import Button from "$lib/components/ui/button.svelte";
  import type { Stamped } from "$lib/state/presentation.svelte.js";
  import type { RegistryState } from "$lib/ipc/results.js";
  import { availabilityLabel } from "./format.js";
  import ObservedNote from "./ObservedNote.svelte";

  interface Props {
    snapshot: Stamped<RegistryState> | null;
    selectedDriveDir: string | null;
    pending: boolean;
    transportNotice: string | null;
    onRefresh: () => void;
    onSelect: (driveDir: string) => void;
  }

  let { snapshot, selectedDriveDir, pending, transportNotice, onRefresh, onSelect }: Props = $props();
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
  {#if registry.status === "unreadable"}
    <p class="mt-4 rounded-md bg-ink px-3 py-2 text-sm text-bone" role="alert">
      Registry unreadable: {registry.reason} — showing nothing rather than guessing.
    </p>
  {:else if registry.status === "nofile" || registry.drives.length === 0}
    <p class="mt-4 text-sm text-smoke">
      No drives registered yet. This shell observes drives that the installation and owner flows
      register — it never provisions or authors membership itself.
    </p>
    <section aria-label="First drive guidance" class="mt-4 rounded-md border border-ink/15 px-3 py-2 text-sm text-smoke">
      <p>
        Until runtime provisioning lands in this shell, create or join a drive from the terminal:
        run <code>wyrd bootstrap</code> to register a drive, then return here — the drive appears
        in this view once the registry names it. Only one window is needed: every registered drive
        is managed from here.
      </p>
    </section>
  {:else}
    <section aria-label="Registered drives" class="mt-6">
      <h2 class="text-sm font-semibold tracking-wide text-smoke uppercase">Registered drives</h2>
      <ul class="mt-2 space-y-2">
        {#each registry.drives as drive (drive.drive_dir)}
          <li class="rounded-md border border-ink/15 px-3 py-2 text-sm">
            <div class="flex items-center justify-between gap-2">
              <p class="font-medium">{drive.name}{selectedDriveDir === drive.drive_dir ? " · selected" : ""}</p>
              {#if selectedDriveDir !== drive.drive_dir}
                <Button variant="secondary" size="sm" disabled={pending} onclick={() => onSelect(drive.drive_dir)}>
                  Select
                </Button>
              {/if}
            </div>
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

  <section aria-label="Lifecycle guidance" class="mt-6 rounded-md border border-ink/15 px-3 py-2 text-sm text-smoke">
    <p>
      Drive creation, joining, and leaving are provisioning and membership operations — they run in the
      installation and owner flows, not in this shell. Until runtime provisioning lands, use
      <code>wyrd bootstrap</code> to register a drive. This view verifies what those flows registered and
      whether each drive's owner answers; it never provisions or authors membership itself.
    </p>
  </section>

  <ObservedNote observedAtMs={snapshot.observedAtMs} />
{/if}
