<script lang="ts">
  import InfoHint from "./info-hint.svelte";
  import type { Snippet } from "svelte";

  interface Props {
    label: string;
    /** Optional one-liner rendered beneath the label. */
    description?: string;
    /** Optional "why" text shown behind a question mark. */
    hint?: Snippet;
    /** Accessible name for the hint trigger. */
    hintLabel?: string;
    children: Snippet;
  }

  let { label, description, hint, hintLabel, children }: Props = $props();
</script>

<!-- One row of a definition list. The wrapper div is valid inside
     <dl> (HTML5 permits a single div around each dt/dd group). -->
<div class="flex items-start justify-between gap-6 border-b border-border/60 py-2.5 last:border-0">
  <dt class="min-w-0 text-sm text-muted-foreground">
    <span class="flex items-center gap-1.5">
      <span>{label}</span>
      {#if hint}
        <InfoHint label={hintLabel ?? `About ${label.toLowerCase()}`}>
          {@render hint()}
        </InfoHint>
      {/if}
    </span>
    {#if description}
      <span class="mt-0.5 block text-xs text-muted-foreground/80">{description}</span>
    {/if}
  </dt>
  <dd class="min-w-0 max-w-[65%] text-right text-sm font-medium">{@render children()}</dd>
</div>
