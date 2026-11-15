<script lang="ts">
  import { Badge } from "$lib/components/ui/badge/index.js";
  import InfoHint from "./info-hint.svelte";

  type State = "available" | "unavailable" | "unknown";

  interface Props {
    state: State;
    /** The owner's verbatim detail, where one exists. Shown behind the hint. */
    detail?: string | null;
  }

  let { state, detail = null }: Props = $props();

  const tone = $derived(
    state === "available"
      ? "border-ok/30 bg-ok/10 text-ok"
      : state === "unavailable"
        ? "border-bad/30 bg-bad/10 text-bad"
        : "border-border bg-muted text-muted-foreground",
  );
</script>

<span class="inline-flex items-center gap-1">
  <Badge variant="outline" class={tone}>
    <span class="size-1.5 rounded-full bg-current" aria-hidden="true"></span>
    {state === "available" ? "Available" : state === "unavailable" ? "Unavailable" : "Unknown"}
  </Badge>
  {#if detail}
    <InfoHint label={`${state} — owner detail`}>
      <p class="font-medium capitalize text-foreground">{state}</p>
      <p class="text-muted-foreground">{detail}</p>
    </InfoHint>
  {/if}
</span>
