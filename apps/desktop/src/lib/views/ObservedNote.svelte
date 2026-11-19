<script lang="ts">
  import ClockIcon from "@lucide/svelte/icons/clock";
  import { formatObservedAt } from "$lib/views/format.js";

  interface Props {
    observedAtMs: number | null;
  }

  let { observedAtMs }: Props = $props();
</script>

<!-- Staleness qualification, shared by the three views: every
     cached answer reads as historical until refreshed. -->
<p class="flex items-start gap-1.5 text-xs leading-relaxed text-muted-foreground">
  <ClockIcon class="mt-0.5 size-3.5 shrink-0 opacity-70" />
  <span>
    {#if observedAtMs === null}
      Not yet observed — refresh to read.
    {:else}
      Last observed {formatObservedAt(observedAtMs)} — refresh to re-read. Cached data is not current state.
    {/if}
  </span>
</p>
