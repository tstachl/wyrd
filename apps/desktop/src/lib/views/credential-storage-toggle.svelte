<!-- Two-option credential-storage toggle: store-backed (default) or
operator file-backed. Local display state only — the caller decides
what the selection means for its IPC args. Extracted (not inline)
because sibling interactive templates in the parent break the
happy-dom re-registration the mounted tests rely on. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";

  interface Props {
    useStore: boolean;
    onSelect: (useStore: boolean) => void;
    /** Store option label; the Add-drive form marks the default recommended. */
    storeLabel?: string;
  }

  let { useStore, onSelect, storeLabel = "OS credential store" }: Props = $props();
</script>

<div class="flex flex-col gap-2">
  <span class="text-sm font-medium">Credential storage</span>
  <div class="flex flex-wrap gap-2">
    <Button
      variant={useStore ? "default" : "outline"}
      size="sm"
      onclick={() => onSelect(true)}
    >
      {storeLabel}
    </Button>
    <Button
      variant={!useStore ? "default" : "outline"}
      size="sm"
      onclick={() => onSelect(false)}
    >
      Credential files (operator)
    </Button>
  </div>
</div>
