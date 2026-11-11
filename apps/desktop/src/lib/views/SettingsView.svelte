<!-- Settings, read-only: this drive's configured bounds, as
configured. Absent keys leave the compiled defaults in place, and
the shell does not know those defaults — so absence renders as
unknown, never as a filled-in number. The shell never writes this
file; application of the values is the running owner's business. -->
<script lang="ts">
  import Button from "$lib/components/ui/button.svelte";
  import type { Stamped } from "$lib/state/presentation.svelte.js";
  import type { DriveSettingsState } from "$lib/ipc/results.js";
  import { boundLabel } from "./format.js";
  import ObservedNote from "./ObservedNote.svelte";

  interface Props {
    driveLabel: string;
    snapshot: Stamped<DriveSettingsState> | null;
    pending: boolean;
    transportNotice: string | null;
    onRefresh: () => void;
  }

  let { driveLabel, snapshot, pending, transportNotice, onRefresh }: Props = $props();
</script>

<div class="flex items-center justify-between">
  <h1 class="text-2xl font-semibold tracking-tight">Settings</h1>
  <Button variant="secondary" size="sm" disabled={pending} onclick={onRefresh}>
    {pending ? "Reading…" : "Refresh"}
  </Button>
</div>
<p class="mt-1 text-xs text-smoke">{driveLabel}</p>

{#if transportNotice !== null}
  <p class="mt-2 text-sm" role="alert">{transportNotice}</p>
{/if}

{#if snapshot === null}
  <p class="mt-4 text-sm text-smoke">No settings read yet. Refresh to read this drive's configured bounds.</p>
  <ObservedNote observedAtMs={null} />
{:else}
  {@const settings = snapshot.data}
  {#if settings.state === "configured"}
    <dl class="mt-4 grid grid-cols-2 gap-x-6 gap-y-1 text-sm">
      <dt class="text-smoke">Retained bytes quota</dt>
      <dd>{boundLabel(settings.retained_bytes_quota, "bytes")}</dd>
      <dt class="text-smoke">Wants admitted per sync pass</dt>
      <dd>{boundLabel(settings.max_admit_per_pass, "wants")}</dd>
      <dt class="text-smoke">Fetch phase budget</dt>
      <dd>{boundLabel(settings.fetch_pass_budget_secs, "seconds")}</dd>
      <dt class="text-smoke">Held mutation deadline</dt>
      <dd>{boundLabel(settings.max_mutation_wait_secs, "seconds")}</dd>
      <dt class="text-smoke">Open demand wait</dt>
      <dd>{boundLabel(settings.open_timeout_secs, "seconds")}</dd>
    </dl>
    <p class="mt-4 rounded-md border border-ink/15 px-3 py-2 text-xs text-smoke">
      As configured in the per-user settings file — not as applied. The running owner applies these values
      at startup, command-line flags win over the file, and this shell never writes the file.
    </p>
  {:else if settings.state === "nofile"}
    <p class="mt-4 text-sm text-smoke">
      No settings file yet — nothing is configured, so the owner runs its compiled defaults.
      Defaults are unknown to this shell, so nothing is filled in here.
    </p>
  {:else if settings.state === "nosection"}
    <p class="mt-4 text-sm text-smoke">
      No section for this drive — the file configures other drives. The owner runs its compiled defaults
      for this drive, which are unknown to this shell.
    </p>
  {:else}
    <p class="mt-4 rounded-md bg-ink px-3 py-2 text-sm text-bone" role="alert">
      Settings unreadable: {settings.reason} — showing nothing rather than defaults that were never configured.
    </p>
  {/if}
  <ObservedNote observedAtMs={snapshot.observedAtMs} />
{/if}
