<!-- One drive in the overview. Extracted from LifecycleView because
  sibling interactive templates inline in a loop break happy-dom's
  re-registration across mounts. The card is clickable for convenience;
  the Open button remains the labelled, keyboard-accessible action. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import { Badge } from "$lib/components/ui/badge/index.js";
  import type { DriveView } from "$lib/ipc/results.js";
  import {
    driveFactLine,
    driveNeedsAttention,
    driveRunState,
    folderNameFromPath,
    sessionLabel,
    type OverviewDriveFacts,
  } from "./format.js";
  import ArrowRightIcon from "@lucide/svelte/icons/arrow-right";
  import FolderOpenIcon from "@lucide/svelte/icons/folder-open";

  interface Props {
    drive: DriveView;
    facts: OverviewDriveFacts | null;
    onOpen: () => void;
    onReveal: (path: string) => Promise<boolean>;
  }

  let { drive, facts, onOpen, onReveal }: Props = $props();
  let revealError = $state(false);

  async function reveal(): Promise<void> {
    const ok = await onReveal(drive.drive_dir);
    revealError = !ok;
  }
</script>

{#if revealError}
  <Alert.Root class="mb-3 border-primary/40 bg-primary/10">
    <p>Could not open the file manager. The drive folder is unchanged.</p>
  </Alert.Root>
{/if}

<Card.Root
  role="button"
  tabindex={0}
  class="cursor-pointer transition-colors hover:border-foreground/40 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
  onclick={onOpen}
  onkeydown={(event) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      onOpen();
    }
  }}
>
  <Card.Header>
    <div class="flex min-w-0 items-start justify-between gap-3">
      <div class="flex min-w-0 flex-1 flex-col gap-1">
        <Card.Title class="truncate">{drive.name}</Card.Title>
        <div class="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
          <span class="font-medium text-foreground">{folderNameFromPath(drive.drive_dir)}</span>
          <button
            type="button"
            class="inline-flex items-center gap-1 underline underline-offset-2 hover:text-foreground"
            onclick={(event) => {
              event.stopPropagation();
              void reveal();
            }}
            onkeydown={(event) => event.stopPropagation()}
          >
            <FolderOpenIcon class="size-3" />
            Show in Finder
          </button>
        </div>
      </div>
      <Badge
        variant="outline"
        class={driveNeedsAttention(facts)
          ? "border-primary/40 bg-primary/10"
          : "border-ok/30 bg-ok/10 text-ok"}
      >
        {driveRunState(facts)}
      </Badge>
    </div>
  </Card.Header>
  <Card.Content class="flex flex-col gap-2">
    <p class="text-xs leading-relaxed text-muted-foreground">
      {driveFactLine(facts, drive.relays.length)}
    </p>
    <div class="flex flex-wrap items-center gap-2">
      <span class="text-xs font-medium text-muted-foreground">
        {drive.relays.length === 1 ? "1 relay" : `${drive.relays.length} relays`}
      </span>
      {#if sessionLabel(drive.session) !== null}
        <Badge
          variant={drive.session?.state === "unlocked" ? "outline" : "secondary"}
          class={drive.session?.state === "unlocked"
            ? "border-ok/30 bg-ok/10 text-ok"
            : "border-primary/40 bg-primary/10"}
        >
          {sessionLabel(drive.session)}
        </Badge>
      {/if}
    </div>
  </Card.Content>
  <Card.Footer class="mt-auto justify-end">
    <Button
      size="sm"
      onclick={(event) => {
        event.stopPropagation();
        onOpen();
      }}
      onkeydown={(event) => event.stopPropagation()}
    >
      Open
      <ArrowRightIcon data-icon="inline-end" />
    </Button>
  </Card.Footer>
</Card.Root>
