<!-- The unavailable state as a first-class view, not a toast:
the owner is down, the shell never opens the store itself, and
reconnecting resumes without restart. Recovery proves nothing
about operations that were in flight — those stay unconfirmed
until re-read through the owner. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import * as Empty from "$lib/components/ui/empty/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import CloudOffIcon from "@lucide/svelte/icons/cloud-off";
  import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";

  interface Props {
    driveLabel: string;
    detail: string | null;
    pending: boolean;
    onRetry: () => void;
  }

  let { driveLabel, detail, pending, onRetry }: Props = $props();
</script>

<Empty.Root class="mt-6 border border-dashed border-border bg-card/50 py-12">
  <Empty.Header class="max-w-xl">
    <Empty.Media variant="icon" class="size-11">
      <CloudOffIcon class="size-5" />
    </Empty.Media>
    <Empty.Title class="text-base">Owner unavailable</Empty.Title>
    <Empty.Description class="leading-relaxed">
      <span class="font-mono text-xs">{driveLabel}</span> — other drives are unaffected.
    </Empty.Description>
    <Empty.Description class="leading-relaxed">
      The drive owner is not answering. This says nothing about the drive's contents — the shell
      does not open the store itself, so there is nothing stale or empty to show, only the absence
      of an answer.
    </Empty.Description>
  </Empty.Header>
  <Empty.Content class="max-w-xl">
    {#if detail !== null}
      <Alert.Root variant="destructive" class="text-left">
        <p>{detail}</p>
      </Alert.Root>
    {/if}
    <Button disabled={pending} onclick={onRetry}>
      {#if pending}
        <Spinner data-icon="inline-start" />
        Retrying…
      {:else}
        <RefreshCwIcon data-icon="inline-start" />
        Retry connection
      {/if}
    </Button>
    <p class="text-xs leading-relaxed text-muted-foreground">
      Retry reconnects and re-reads from the owner. Anything that was in flight stays unconfirmed
      until reconciled — recovery never replays it and never calls it a success.
    </p>
  </Empty.Content>
</Empty.Root>
