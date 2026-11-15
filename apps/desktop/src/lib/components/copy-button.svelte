<script lang="ts">
  import CopyIcon from "@lucide/svelte/icons/copy";
  import CheckIcon from "@lucide/svelte/icons/check";
  import { onDestroy } from "svelte";

  interface Props {
    text: string;
    /** Human name for the accessible label, e.g. "drive path". */
    label?: string;
  }

  let { text, label = "text" }: Props = $props();

  let copied = $state(false);
  let timer: ReturnType<typeof setTimeout> | null = null;

  async function copy(): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      copied = true;
      if (timer !== null) clearTimeout(timer);
      timer = setTimeout(() => {
        copied = false;
        timer = null;
      }, 1400);
    } catch {
      // Clipboard permission is best-effort; the path is still readable.
    }
  }

  onDestroy(() => {
    if (timer !== null) clearTimeout(timer);
  });
</script>

<button
  type="button"
  class="inline-flex size-5 shrink-0 items-center justify-center rounded-md text-muted-foreground/70 transition-colors hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none"
  aria-label={copied ? `${label} copied` : `Copy ${label}`}
  title={copied ? "Copied" : `Copy ${label}`}
  onclick={() => void copy()}
>
  {#if copied}
    <CheckIcon class="size-3 text-ok" />
  {:else}
    <CopyIcon class="size-3" />
  {/if}
</button>
