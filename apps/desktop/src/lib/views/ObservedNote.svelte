<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import ClockIcon from "@lucide/svelte/icons/clock";
  import { formatObservedAge } from "$lib/views/format.js";

  interface Props {
    observedAtMs: number | null;
  }

  let { observedAtMs }: Props = $props();

  // The age is the shell's observation, not a sync time. Refresh it on
  // a timer so a relative phrase ages in place without a new read;
  // a real refresh still replaces the stamp.
  let nowMs = $state(Date.now());
  let timer: ReturnType<typeof setInterval> | undefined;

  onMount(() => {
    timer = setInterval(() => {
      nowMs = Date.now();
    }, 10_000);
  });
  onDestroy(() => {
    if (timer !== undefined) clearInterval(timer);
  });
</script>

<!-- Staleness qualification, shared by the views: every cached answer
     reads as historical until refreshed. -->
<p class="flex items-start gap-1.5 text-xs leading-relaxed text-muted-foreground">
  <ClockIcon class="mt-0.5 size-3.5 shrink-0 opacity-70" />
  <span>
    {#if observedAtMs === null}
      Not yet observed — refresh to read.
    {:else}
      {formatObservedAge(observedAtMs, nowMs)} — refresh to re-read. Cached data is not current state.
    {/if}
  </span>
</p>
