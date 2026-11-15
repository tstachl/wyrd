<script lang="ts">
  import { Badge } from "$lib/components/ui/badge/index.js";
  import * as InputGroup from "$lib/components/ui/input-group/index.js";
  import PlusIcon from "@lucide/svelte/icons/plus";
  import XIcon from "@lucide/svelte/icons/x";

  interface Props {
    relays?: string[];
    disabled?: boolean;
    id?: string;
  }

  let { relays = $bindable([]), disabled = false, id = "relays" }: Props = $props();

  let draft = $state("");
  let error = $state<string | null>(null);

  function add(): void {
    const value = draft.trim();
    if (value === "") return;
    if (/\s/.test(value)) {
      error = "A relay address cannot contain spaces.";
      return;
    }
    if (relays.includes(value)) {
      error = "That relay is already listed.";
      return;
    }
    relays = [...relays, value];
    draft = "";
    error = null;
  }

  function remove(relay: string): void {
    relays = relays.filter((entry) => entry !== relay);
    error = null;
  }

  function onkeydown(event: KeyboardEvent): void {
    if (event.key === "Enter") {
      event.preventDefault();
      add();
    }
  }
</script>

<!-- Relays read as a set, not a comma-separated string: each entry is
     a removable badge and the field below adds one more. Enter adds;
     whitespace is refused (the store refuses it too) and duplicates
     are skipped. -->
<div class="flex flex-wrap items-center gap-1.5" aria-live="polite">
  {#if relays.length === 0}
    <p class="py-0.5 text-xs text-muted-foreground">No relays yet.</p>
  {:else}
    {#each relays as relay (relay)}
      <Badge
        variant="outline"
        class="h-auto gap-1 overflow-visible border-border bg-background py-0.5 pr-1 pl-2 font-mono text-[0.7rem] font-normal"
      >
        <span class="max-w-64 truncate" title={relay}>{relay}</span>
        <button
          type="button"
          class="rounded-full p-0.5 text-muted-foreground transition-colors hover:bg-destructive/15 hover:text-destructive focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none disabled:pointer-events-none disabled:opacity-50"
          aria-label={`Remove ${relay}`}
          onclick={() => remove(relay)}
          {disabled}
        >
          <XIcon class="size-3" />
        </button>
      </Badge>
    {/each}
  {/if}
</div>

<InputGroup.Root class="mt-2">
  <InputGroup.Input
    {id}
    bind:value={draft}
    {disabled}
    placeholder="wss://relay.example"
    autocomplete="off"
    spellcheck={false}
    onkeydown={onkeydown}
    aria-invalid={error !== null}
  />
  <InputGroup.Addon align="inline-end">
    <InputGroup.Button
      variant="secondary"
      size="xs"
      disabled={disabled || draft.trim() === ""}
      onclick={add}
    >
      <PlusIcon data-icon="inline-start" />
      Add
    </InputGroup.Button>
  </InputGroup.Addon>
</InputGroup.Root>

{#if error !== null}
  <p class="text-xs text-destructive" role="alert">{error}</p>
{/if}
