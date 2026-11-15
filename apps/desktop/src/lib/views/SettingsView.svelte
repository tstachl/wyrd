<!-- Settings, read-only for configuration: this drive's configured
bounds, as configured. Absent keys leave the compiled defaults in
place, and the shell does not know those defaults — so absence renders
as unknown, never as a filled-in number. The shell never writes this
file; application of the values is the running owner's business.

The one action here is membership, not configuration: leaving the
drive reuses the identity pair the start-up gate verified (paths only),
stays behind an explicit acknowledgement and a confirmation dialog,
and never deletes local data as a side effect of the UI. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import * as Field from "$lib/components/ui/field/index.js";
  import * as AlertDialog from "$lib/components/ui/alert-dialog/index.js";
  import { Badge } from "$lib/components/ui/badge/index.js";
  import { Checkbox } from "$lib/components/ui/checkbox/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import DetailRow from "$lib/components/detail-row.svelte";
  import PageHeader from "$lib/components/page-header.svelte";
  import InfoHint from "$lib/components/info-hint.svelte";
  import type { Stamped } from "$lib/state/presentation.svelte.js";
  import type { DriveSettingsState, ProvisionReport } from "$lib/ipc/results.js";
  import type { ProvisionLeaveArgs } from "$lib/ipc/operations.js";
  import {
    boundLabel,
    describeProvisionResult,
    membershipSummary,
    observedRegistrySummary,
  } from "./format.js";
  import ObservedNote from "./ObservedNote.svelte";
  import FileQuestionIcon from "@lucide/svelte/icons/file-question";
  import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
  import LogOutIcon from "@lucide/svelte/icons/log-out";
  import CircleAlertIcon from "@lucide/svelte/icons/circle-alert";

  interface Props {
    driveDir: string;
    snapshot: Stamped<DriveSettingsState> | null;
    pending: boolean;
    transportNotice: string | null;
    /** The pair the identity gate verified; paths only, never contents. */
    identityFile: string | null;
    passphraseFile: string | null;
    onRefresh: () => void;
    onLeave: (args: ProvisionLeaveArgs) => Promise<ProvisionReport>;
    /** Re-read the registry and return to the drive list. */
    onRefreshDrives: () => void;
  }

  let { driveDir, snapshot, pending, transportNotice, identityFile, passphraseFile, onRefresh, onLeave, onRefreshDrives }: Props = $props();

  const settings = $derived(snapshot?.data ?? null);

  let acknowledged = $state(false);
  let confirmLeave = $state(false);
  let leaveWorking = $state(false);
  let leaveReport = $state<ProvisionReport | null>(null);
  let leaveError = $state<string | null>(null);

  const busy = $derived(pending || leaveWorking);
  const credentialsReady = $derived(identityFile !== null && passphraseFile !== null);

  async function leave(): Promise<void> {
    if (identityFile === null || passphraseFile === null) return;
    leaveWorking = true;
    leaveError = null;
    try {
      // The report carries the post-run observation; only that decides
      // what changed. A transport throw is not an outcome.
      leaveReport = await onLeave({ driveDir, identityFile, passphraseFile, acknowledged });
    } catch (failure: unknown) {
      leaveError = failure instanceof Error ? failure.message : String(failure);
    } finally {
      leaveWorking = false;
    }
  }
</script>

{#snippet credentialsHint()}
  <p>
    The pair this device's identity gate verified. Only the paths are passed to the CLI; the
    files' contents never enter the shell.
  </p>
{/snippet}

<PageHeader
  title="Settings"
  size="section"
  description="This drive's configured bounds, read-only. Absent values stay unknown — the shell never fills in a default it cannot see."
>
  {#snippet actions()}
    <Button variant="outline" size="sm" disabled={pending} onclick={onRefresh}>
      {#if pending}
        <Spinner data-icon="inline-start" />
        Reading…
      {:else}
        <RefreshCwIcon data-icon="inline-start" />
        Refresh
      {/if}
    </Button>
  {/snippet}
</PageHeader>

{#if transportNotice !== null}
  <Alert.Root class="mt-4 border-primary/40 bg-primary/10">
    <p>{transportNotice}</p>
  </Alert.Root>
{/if}

{#if snapshot === null || settings === null}
  <div class="mt-6 flex flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border bg-card/50 px-6 py-14 text-center">
    <div class="flex size-10 items-center justify-center rounded-full bg-muted text-muted-foreground">
      <FileQuestionIcon class="size-5" />
    </div>
    <p class="text-sm font-medium">No settings read yet</p>
    <p class="max-w-sm text-sm text-muted-foreground">
      Refresh to read this drive's configured bounds. Nothing is filled in from memory.
    </p>
  </div>
  <div class="mt-3">
    <ObservedNote observedAtMs={null} />
  </div>
{:else if settings.state === "configured"}
  <Card.Root class="mt-6">
    <Card.Header>
      <Card.Title class="flex items-center gap-1.5">
        Configured bounds
        <InfoHint label="About configured bounds">
          <p>
            As configured in the per-user settings file — not as applied. The running owner applies
            these values at startup, command-line flags win over the file, and this shell never
            writes the file.
          </p>
        </InfoHint>
      </Card.Title>
      <Card.Description>One drive's saved limits, as the owner will read them.</Card.Description>
    </Card.Header>
    <Card.Content>
      <dl>
        <DetailRow label="Retained bytes quota">
          {#if settings.retained_bytes_quota === null}
            <span class="inline-flex items-center gap-1">
              <Badge variant="outline" class="text-muted-foreground">Compiled default</Badge>
              <InfoHint label="About the retained bytes quota">
                <p>{boundLabel(settings.retained_bytes_quota, "bytes")}</p>
              </InfoHint>
            </span>
          {:else}
            <span class="font-mono tabular-nums">{boundLabel(settings.retained_bytes_quota, "bytes")}</span>
          {/if}
        </DetailRow>
        <DetailRow label="Wants admitted per sync pass">
          {#if settings.max_admit_per_pass === null}
            <span class="inline-flex items-center gap-1">
              <Badge variant="outline" class="text-muted-foreground">Compiled default</Badge>
              <InfoHint label="About admitted wants">
                <p>{boundLabel(settings.max_admit_per_pass, "wants")}</p>
              </InfoHint>
            </span>
          {:else}
            <span class="font-mono tabular-nums">{boundLabel(settings.max_admit_per_pass, "wants")}</span>
          {/if}
        </DetailRow>
        <DetailRow label="Fetch phase budget">
          {#if settings.fetch_pass_budget_secs === null}
            <span class="inline-flex items-center gap-1">
              <Badge variant="outline" class="text-muted-foreground">Compiled default</Badge>
              <InfoHint label="About the fetch phase budget">
                <p>{boundLabel(settings.fetch_pass_budget_secs, "seconds")}</p>
              </InfoHint>
            </span>
          {:else}
            <span class="font-mono tabular-nums">{boundLabel(settings.fetch_pass_budget_secs, "seconds")}</span>
          {/if}
        </DetailRow>
        <DetailRow label="Held mutation deadline">
          {#if settings.max_mutation_wait_secs === null}
            <span class="inline-flex items-center gap-1">
              <Badge variant="outline" class="text-muted-foreground">Compiled default</Badge>
              <InfoHint label="About the held mutation deadline">
                <p>{boundLabel(settings.max_mutation_wait_secs, "seconds")}</p>
              </InfoHint>
            </span>
          {:else}
            <span class="font-mono tabular-nums">{boundLabel(settings.max_mutation_wait_secs, "seconds")}</span>
          {/if}
        </DetailRow>
        <DetailRow label="Open demand wait">
          {#if settings.open_timeout_secs === null}
            <span class="inline-flex items-center gap-1">
              <Badge variant="outline" class="text-muted-foreground">Compiled default</Badge>
              <InfoHint label="About the open demand wait">
                <p>{boundLabel(settings.open_timeout_secs, "seconds")}</p>
              </InfoHint>
            </span>
          {:else}
            <span class="font-mono tabular-nums">{boundLabel(settings.open_timeout_secs, "seconds")}</span>
          {/if}
        </DetailRow>
      </dl>
    </Card.Content>
  </Card.Root>
{:else if settings.state === "nofile"}
  <div class="mt-6 flex flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border bg-card/50 px-6 py-14 text-center">
    <div class="flex size-10 items-center justify-center rounded-full bg-muted text-muted-foreground">
      <FileQuestionIcon class="size-5" />
    </div>
    <p class="text-sm font-medium">No settings file yet</p>
    <p class="max-w-md text-sm text-muted-foreground">
      Nothing is configured, so the owner runs its compiled defaults. Defaults are unknown to this
      shell, so nothing is filled in here.
    </p>
  </div>
{:else if settings.state === "nosection"}
  <div class="mt-6 flex flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border bg-card/50 px-6 py-14 text-center">
    <div class="flex size-10 items-center justify-center rounded-full bg-muted text-muted-foreground">
      <FileQuestionIcon class="size-5" />
    </div>
    <p class="text-sm font-medium">No section for this drive</p>
    <p class="max-w-md text-sm text-muted-foreground">
      The file configures other drives. The owner runs its compiled defaults for this drive, which
      are unknown to this shell.
    </p>
  </div>
{:else}
  <Alert.Root variant="destructive" class="mt-6">
    <p>
      Settings unreadable: {settings.reason} — showing nothing rather than defaults that were never
      configured.
    </p>
  </Alert.Root>
{/if}

<div class="mt-4">
  <ObservedNote observedAtMs={snapshot?.observedAtMs ?? null} />
</div>

<Card.Root class="mt-6 ring-destructive/25">
  <Card.Header>
    <Card.Title class="flex items-center gap-2">
      <LogOutIcon class="size-4 text-destructive" />
      Leave this drive
    </Card.Title>
    <Card.Description>This removes this device from the drive's membership.</Card.Description>
  </Card.Header>
  <Card.Content class="flex flex-col gap-4">
    <dl>
      <DetailRow label="Identity file" hint={credentialsHint} hintLabel="About the credential paths">
        <span class="inline-flex min-w-0 items-center gap-1.5">
          {#if identityFile === null}
            <span class="text-muted-foreground">Unknown</span>
          {:else}
            <span class="truncate font-mono text-xs">{identityFile}</span>
          {/if}
        </span>
      </DetailRow>
      <DetailRow label="Passphrase file" hint={credentialsHint} hintLabel="About the credential paths">
        <span class="inline-flex min-w-0 items-center gap-1.5">
          {#if passphraseFile === null}
            <span class="text-muted-foreground">Unknown</span>
          {:else}
            <span class="truncate font-mono text-xs">{passphraseFile}</span>
          {/if}
        </span>
      </DetailRow>
    </dl>

    {#if leaveReport !== null}
      <Alert.Root class="border-primary/30 bg-primary/5">
        <CircleAlertIcon />
        <Alert.Title>Leave report</Alert.Title>
        <Alert.Description class="space-y-1 text-foreground/80 [&_p:not(:last-child)]:mb-1">
          <p>{describeProvisionResult(leaveReport)}</p>
          <p>{observedRegistrySummary(leaveReport)}</p>
          <p>{membershipSummary(leaveReport)}</p>
        </Alert.Description>
      </Alert.Root>
    {/if}
    {#if leaveError !== null}
      <Alert.Root variant="destructive">
        <CircleAlertIcon />
        <Alert.Title>Leave failed</Alert.Title>
        <Alert.Description>{leaveError}</Alert.Description>
      </Alert.Root>
    {/if}

    {#if leaveReport === null}
      {#if !credentialsReady}
        <p class="text-sm text-destructive">
          This device's verified identity pair is unavailable, so leaving is disabled. Fix the
          identity gate first.
        </p>
      {/if}
      <Field.Field orientation="horizontal" class="rounded-xl border border-destructive/30 bg-destructive/5 p-3">
        <Checkbox id="leave-acknowledged" bind:checked={acknowledged} disabled={busy} />
        <Field.Content>
          <Field.Label for="leave-acknowledged" class="leading-relaxed font-normal">
            I understand: leaving as the sole owner is terminal — the drive freezes permanently and no future transition can ever be authorized.
          </Field.Label>
        </Field.Content>
      </Field.Field>
    {/if}
  </Card.Content>
  <Card.Footer class="justify-end">
    {#if leaveReport !== null}
      <Button variant="outline" size="sm" disabled={busy} onclick={onRefreshDrives}>
        <RefreshCwIcon data-icon="inline-start" />
        Refresh drives
      </Button>
    {:else}
      <Button
        variant="destructive"
        size="sm"
        disabled={busy || !acknowledged || !credentialsReady}
        onclick={() => (confirmLeave = true)}
      >
        {#if leaveWorking}
          <Spinner data-icon="inline-start" />
          Leaving…
        {:else}
          <LogOutIcon data-icon="inline-start" />
          Leave this drive…
        {/if}
      </Button>
    {/if}
  </Card.Footer>
</Card.Root>

<AlertDialog.Root bind:open={confirmLeave}>
  <AlertDialog.Content>
    <AlertDialog.Header>
      <AlertDialog.Title>Leave this drive?</AlertDialog.Title>
      <AlertDialog.Description>
        This removes this device from the drive's membership. Leaving as the sole owner is terminal — the drive freezes permanently and no future transition can ever be authorized.
      </AlertDialog.Description>
    </AlertDialog.Header>
    <AlertDialog.Footer>
      <AlertDialog.Cancel>Keep this drive</AlertDialog.Cancel>
      <AlertDialog.Action variant="destructive" disabled={busy} onclick={() => void leave()}>
        Leave drive
      </AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>
