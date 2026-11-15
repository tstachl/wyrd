<script lang="ts">
  import * as HoverCard from "$lib/components/ui/hover-card/index.js";
  import CircleHelpIcon from "@lucide/svelte/icons/circle-help";
  import type { Snippet } from "svelte";

  interface Props {
    /** Accessible name for the trigger, e.g. "About freshness". */
    label: string;
    children: Snippet;
  }

  let { label, children }: Props = $props();
</script>

<!-- Secondary explanation only. Normative disclosure copy (the
     onboarding security boundary, recovery rules) stays visible in
     the page; this affordance keeps "why does this exist" text out
     of the reading path. It opens on hover and on keyboard focus. -->
<HoverCard.Root openDelay={120} closeDelay={60}>
  <HoverCard.Trigger>
    {#snippet child({ props })}
      <button
        {...props}
        type="button"
        aria-label={label}
        class="inline-flex size-5 shrink-0 items-center justify-center rounded-full text-muted-foreground/70 outline-none transition-colors hover:text-foreground focus-visible:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50"
      >
        <CircleHelpIcon class="size-3.5" />
      </button>
    {/snippet}
  </HoverCard.Trigger>
  <HoverCard.Content class="w-72 space-y-1.5 leading-relaxed">
    {@render children()}
  </HoverCard.Content>
</HoverCard.Root>
