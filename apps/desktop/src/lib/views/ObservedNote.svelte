<!-- Staleness qualification, shared by the three views: every
cached answer reads as historical until refreshed. -->
<script lang="ts">
  import { formatObservedAt } from "./format.js";

  interface Props {
    observedAtMs: number | null;
  }

  let { observedAtMs }: Props = $props();
</script>

<p class="mt-1 text-xs text-smoke">
  {#if observedAtMs === null}
    Not yet observed — refresh to read from the owner.
  {:else}
    Last observed {formatObservedAt(observedAtMs)} — refresh to re-read. Cached data is not current state.
  {/if}
</p>
