<!-- Settings, read-only: this drive's configured bounds, as
configured. Absent keys leave the compiled defaults in place, and
the shell does not know those defaults — so absence renders as
unknown, never as a filled-in number. The shell never writes this
file; application of the values is the running owner's business. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import { Badge } from "$lib/components/ui/badge/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import DetailRow from "$lib/components/detail-row.svelte";
  import PageHeader from "$lib/components/page-header.svelte";
  import InfoHint from "$lib/components/info-hint.svelte";
  import type { Stamped } from "$lib/state/presentation.svelte.js";
  import type { DriveSettingsState } from "$lib/ipc/results.js";
  import { boundLabel } from "./format.js";
  import ObservedNote from "./ObservedNote.svelte";
  import FileQuestionIcon from "@lucide/svelte/icons/file-question";
  import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";

  interface Props {
    snapshot: Stamped<DriveSettingsState> | null;
    pending: boolean;
    transportNotice: string | null;
    onRefresh: () => void;
  }

  let { snapshot, pending, transportNotice, onRefresh }: Props = $props();

  const settings = $derived(snapshot?.data ?? null);
</script>

<PageHeader
  title="Settings"
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
