<script lang="ts">
  import HeldMark from "./held-mark.svelte";

  interface Props {
    /** When present, the lockup becomes the way back to the drive list. */
    onHome?: () => void;
  }

  let { onHome }: Props = $props();
</script>

{#if onHome}
  <button
    type="button"
    onclick={onHome}
    aria-label="Wyrd — all drives"
    class="group flex items-center gap-2.5 rounded-lg py-1 pr-2 text-left text-bone outline-none transition-opacity focus-visible:ring-2 focus-visible:ring-amber/70 focus-visible:ring-offset-2 focus-visible:ring-offset-ink"
  >
    <HeldMark size={24} />
    <span class="text-lg font-semibold tracking-tight">Wyrd</span>
  </button>
{:else}
  <div class="flex items-center gap-2.5 py-1">
    <HeldMark size={24} />
    <span class="text-lg font-semibold tracking-tight">Wyrd</span>
  </div>
{/if}
