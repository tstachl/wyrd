<!-- App-wide local diagnostics. Opt-in, local-only, and never sent.
The control shows the level persisted in settings.json — never an
owner's in-memory override — and only reports success after the
owner reads the level back. A failed or unconfirmed update stays
unapplied in this view. The level governs verbosity only: default
error reporting keeps working with diagnostics off, and enabling
them never suppresses a default-visible error. -->
<script lang="ts">
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Field from "$lib/components/ui/field/index.js";
  import { Checkbox } from "$lib/components/ui/checkbox/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import InfoHint from "$lib/components/info-hint.svelte";
  import ObservedNote from "./ObservedNote.svelte";
  import { describe } from "$lib/ipc/errors.js";
  import type { IpcResult } from "$lib/ipc/types.js";
  import type {
    Diagnostics,
    DiagnosticsExport,
    FinderIntegration,
    WireDiagnosticLevel,
  } from "$lib/ipc/results.js";
  import type { Stamped } from "$lib/state/presentation.svelte.js";

  interface Props {
    /** The persisted level, stamped at the last read. Null before the first read. */
    snapshot: Stamped<Diagnostics> | null;
    /** This build's Finder capability. Null until first observed. */
    finderIntegration: FinderIntegration | null;
    /** The drive whose owner receives the live update. Null until one is selected. */
    driveDir: string | null;
    pending: boolean;
    /** Re-read the persisted level; null when the read failed. */
    onRefresh: () => Promise<Diagnostics | null>;
    /** Set the level on this drive's owner and read it back. */
    onSet: (level: WireDiagnosticLevel) => Promise<IpcResult<WireDiagnosticLevel>>;
    /** Explicitly collect a temporary local bundle. */
    onExport: () => Promise<DiagnosticsExport>;
  }

  let { snapshot, finderIntegration, driveDir, pending, onRefresh, onSet, onExport }: Props = $props();

  const persisted = $derived(snapshot?.data ?? null);
  const level = $derived<WireDiagnosticLevel | null>(
    persisted === null
      ? null
      : persisted.state === "configured"
        ? persisted.level
        : persisted.state === "nofile"
          ? "default"
          : null,
  );
  const explicit = $derived(persisted?.state === "configured" && persisted.explicit);
  const readable = $derived(level !== null);

  let working = $state(false);
  let outcome = $state<{ ok: boolean; detail: string } | null>(null);
  let exporting = $state(false);
  let exported = $state<DiagnosticsExport | null>(null);

  async function exportBundle(): Promise<void> {
    exporting = true;
    exported = null;
    try {
      exported = await onExport();
    } finally {
      exporting = false;
    }
  }
  // The checkbox is bound to local state so an unconfirmed click can
  // be rolled back. The persisted observation is the only authority
  // for what it shows; bits-ui otherwise keeps its own checked state.
  let controlChecked = $state(false);
  $effect(() => {
    controlChecked = level === "detailed";
  });
  // A toggle is only offered when the persisted level was read and a
  // drive is selected to carry the live owner update.
  const canToggle = $derived(readable && driveDir !== null && !pending && !working);

  /** The level saved in settings.json, or null when it was unreadable. */
  function savedLevel(observed: Diagnostics | null): WireDiagnosticLevel | null {
    if (observed === null || observed.state === "unreadable") return null;
    return observed.state === "configured" ? observed.level : "default";
  }

  async function changeLevel(next: WireDiagnosticLevel): Promise<void> {
    if (!canToggle) return;
    working = true;
    outcome = null;
    try {
      const result = await onSet(next);
      if (!result.ok) {
        // A refusal or an unconfirmed answer is not an applied
        // change. Roll the control back to the persisted level.
        controlChecked = level === "detailed";
        outcome = { ok: false, detail: describe(result.error) };
        return;
      }
      // The owner's read-back confirms delivery; the persisted
      // re-read is the only authority for what this view shows.
      if (result.value !== next) {
        controlChecked = level === "detailed";
        outcome = {
          ok: false,
          detail: `The owner read back "${result.value}" after being asked for "${next}" — the level was not applied as requested.`,
        };
        return;
      }
      const observed = await onRefresh();
      if (savedLevel(observed) !== next) {
        controlChecked = level === "detailed";
        outcome = {
          ok: false,
          detail: "The owner accepted the change, but the saved level does not show it — refresh to re-check.",
        };
        return;
      }
      // No optimistic assignment here: the bound control and the
      // snapshot now agree on the level the file actually holds.
      outcome = {
        ok: true,
        detail:
          "Saved. This drive's owner uses the new level for subsequent operations. Other drives use it when they next start.",
      };
    } catch (failure: unknown) {
      controlChecked = level === "detailed";
      outcome = {
        ok: false,
        detail:
          failure instanceof Error ? failure.message : `Unexpected diagnostics failure: ${String(failure)}`,
      };
    } finally {
      working = false;
    }
  }
</script>

<Card.Root class="mt-6">
  <Card.Header>
    <Card.Title class="flex items-center gap-1.5">
      Diagnostics on this app
      <InfoHint label="About diagnostics">
        <p>
          This setting is opt-in and local-only. Wyrd does not send diagnostic records anywhere and
          does not upload them. Detailed records may be sensitive, so export needs a deliberate
          action by you.
        </p>
      </InfoHint>
    </Card.Title>
    <Card.Description>
      Detailed records help explain a failed operation. They apply to subsequent operations, not to
      work already in flight. Default error reporting keeps working when this is off.
    </Card.Description>
  </Card.Header>
  <Card.Content class="flex flex-col gap-3">
    {#if finderIntegration === "unprovisioned"}
      <Alert.Root>
        <p class="font-medium">Finder integration needs an App Group container</p>
        <p class="text-xs leading-relaxed">
          This build has no appex provisioning profile. An ad-hoc extension cannot resolve the
          group container, so Finder can show a server or network error for a drive whose owner
          is running; the drive still works in Wyrd. The packaging README lists the signed-build
          options, with and without a profile. Reinstalling also restarts the drive agents for
          you.
        </p>
      </Alert.Root>
    {/if}
    {#if persisted === null}
      <p class="text-sm text-muted-foreground">No persisted level read yet — refresh to read it.</p>
    {:else if persisted.state === "unreadable"}
      <Alert.Root variant="destructive">
        <p>Diagnostics level unreadable: {persisted.reason} — showing nothing rather than a guessed level.</p>
      </Alert.Root>
    {:else}
      <Field.Group>
        <Field.Field orientation="horizontal">
          <Checkbox
            id="app-diagnostics-detailed"
            bind:checked={controlChecked}
            disabled={!canToggle}
            onCheckedChange={(next) => {
              if (typeof next === "boolean") void changeLevel(next ? "detailed" : "default");
            }}
          />
          <Field.Label for="app-diagnostics-detailed" class="font-normal">
            Detailed diagnostics for this app
          </Field.Label>
        </Field.Field>
      </Field.Group>
      <p class="text-xs leading-relaxed text-muted-foreground">
        {#if level === "detailed"}
          Detailed is on. Only the fields the diagnostics contract allows are recorded.
        {:else if explicit}
          Default is on. This level was saved explicitly; no detailed records are kept.
        {:else}
          Default is on. Default is the genuine absence state — no detailed records are kept.
        {/if}
      </p>
      {#if driveDir === null}
        <p class="text-xs text-muted-foreground">
          Select a drive whose owner is answering to change this setting.
        </p>
      {/if}
    {/if}

    {#if outcome !== null}
      {#if outcome.ok}
        <Alert.Root class="border-primary/40 bg-primary/10">
          <p>{outcome.detail}</p>
        </Alert.Root>
      {:else}
        <Alert.Root variant="destructive">
          <p>{outcome.detail}</p>
        </Alert.Root>
      {/if}
    {/if}
    {#if working}
      <p class="flex items-center gap-1.5 text-xs text-muted-foreground">
        <Spinner class="size-3" />
        Saving…
      </p>
    {/if}

  </Card.Content>
</Card.Root>

<Card.Root class="mt-4">
  <Card.Header>
    <Card.Title class="text-sm">Export a local bundle</Card.Title>
    <Card.Description>
      Collect the current local diagnostic records into one temporary file. Wyrd does not send it
      anywhere. Sharing the file is a separate action you take yourself.
    </Card.Description>
  </Card.Header>
  <Card.Content>
    {#if exported !== null}
      {#if exported.state === "written"}
        <Alert.Root class="border-primary/40 bg-primary/10">
          <p>Temporary bundle written with {exported.records} records.</p>
          <p class="break-all font-mono text-xs">{exported.path}</p>
        </Alert.Root>
      {:else}
        <Alert.Root variant="destructive">
          <p>{exported.reason}</p>
        </Alert.Root>
      {/if}
    {/if}
  </Card.Content>
  <Card.Footer class="justify-end">
    <Button variant="outline" size="sm" disabled={exporting} onclick={() => void exportBundle()}>
      {#if exporting}
        <Spinner data-icon="inline-start" />
        Collecting…
      {:else}
        Export bundle
      {/if}
    </Button>
  </Card.Footer>
</Card.Root>
