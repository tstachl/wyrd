<script lang="ts">
  import type { Snippet } from "svelte";

  interface Props {
    title: string;
    description?: string;
    /** `page` is the device-level home; `section` sits under a page header. */
    size?: "page" | "section";
    /** Rendered beside the title, for status badges. */
    meta?: Snippet;
    /** Right-aligned actions (refresh and the like). */
    actions?: Snippet;
  }

  let { title, description, size = "page", meta, actions }: Props = $props();
</script>

<header class="flex flex-wrap items-start justify-between gap-x-6 gap-y-3">
  <div class="flex min-w-0 flex-col gap-1.5">
    <div class="flex flex-wrap items-center gap-2.5">
      <h1 class={size === "page" ? "text-2xl font-semibold tracking-tight" : "text-lg font-semibold tracking-tight"}>
        {title}
      </h1>
      {#if meta}{@render meta()}{/if}
    </div>
    {#if description}
      <p class="max-w-2xl text-sm leading-relaxed text-muted-foreground">{description}</p>
    {/if}
  </div>
  {#if actions}
    <div class="flex shrink-0 items-center gap-2">{@render actions()}</div>
  {/if}
</header>
