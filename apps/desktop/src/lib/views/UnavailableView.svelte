<!-- The unavailable state as a first-class view, not a toast:
the owner is down, the shell never opens the store itself, and
reconnecting resumes without restart. Recovery proves nothing
about operations that were in flight — those stay unconfirmed
until re-read through the owner. -->
<script lang="ts">
  import Button from "$lib/components/ui/button.svelte";

  interface Props {
    detail: string | null;
    pending: boolean;
    onRetry: () => void;
  }

  let { detail, pending, onRetry }: Props = $props();
</script>

<h1 class="text-2xl font-semibold tracking-tight">Owner unavailable</h1>
<p class="mt-2 text-sm text-smoke">
  The drive owner is not answering. This says nothing about the drive's contents — the shell does not
  open the store itself, so there is nothing stale or empty to show, only the absence of an answer.
</p>
{#if detail !== null}
  <p class="mt-2 text-sm">{detail}</p>
{/if}
<div class="mt-4">
  <Button disabled={pending} onclick={onRetry}>
    {pending ? "Retrying…" : "Retry connection"}
  </Button>
</div>
<p class="mt-2 text-xs text-smoke">
  Retry reconnects and re-reads from the owner. Anything that was in flight stays unconfirmed until
  reconciled — recovery never replays it and never calls it a success.
</p>
