<!-- Lifecycle, observational: the drives available on this device,
  each verified this session. Creation and joining live on the Add
  drive page; leaving lives in the selected drive's Settings. The card
  answers the user's order of questions: is it running, is it shared,
  then the relay count. Selection stays internal — every card has the
  same Open action. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import * as Empty from "$lib/components/ui/empty/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import { Skeleton } from "$lib/components/ui/skeleton/index.js";
  import PageHeader from "$lib/components/page-header.svelte";
  import InfoHint from "$lib/components/info-hint.svelte";
  import type { Stamped, StatusSnapshot } from "$lib/state/presentation.svelte.js";
  import type { DriveView, RegistryState } from "$lib/ipc/results.js";
  import ObservedNote from "./ObservedNote.svelte";
  import DriveCard from "./drive-card.svelte";
  import { sortDrivesByAttention, type OverviewDriveFacts } from "./format.js";
  import HardDriveIcon from "@lucide/svelte/icons/hard-drive";
  import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
  import PlusIcon from "@lucide/svelte/icons/plus";

  interface Props {
    snapshot: Stamped<RegistryState> | null;
    pending: boolean;
    transportNotice: string | null;
    /** Last Status answer per drive; absent means not read this session. */
    statusFor?: (driveDir: string) => Stamped<StatusSnapshot> | null;
    /** Best-effort reveal in the platform file manager. */
    onReveal?: (path: string) => Promise<boolean>;
    onRefresh: () => void;
    onAdd: () => void;
    onOpen: (driveDir: string) => void;
  }

  let {
    snapshot,
    pending,
    transportNotice,
    statusFor = () => null,
    onReveal = async () => false,
    onRefresh,
    onAdd,
    onOpen,
  }: Props = $props();

  const registry = $derived(snapshot?.data ?? null);
  const drives = $derived(registry?.drives ?? []);

  /** Facts come from the last Status answer, never from a guessed default. */
  function factsFor(drive: DriveView): OverviewDriveFacts | null {
    const status = statusFor(drive.drive_dir);
    if (status === null) return null;
    return {
      availability: drive.availability.state,
      paused: status.data.liveness.paused,
      connectedRelays: status.data.liveness.health.connected_relays,
      totalRelays: status.data.liveness.health.total_relays,
      peers: status.data.observe.peers.length,
      knownMembers: status.data.observe.known_members,
      stale: typeof status.data.freshness.state === "object",
    };
  }

  const orderedDrives = $derived(sortDrivesByAttention(drives, factsFor));

</script>

<PageHeader
  title="Drives"
  description="Drives available on this device sync independently. Open a drive to see its status and settings."
>
  {#snippet meta()}
    <InfoHint label="About this view">
      <p>
        This view verifies the drives set up on this device and whether each one answers.
        Creating and joining run through the installed CLI; leaving authors membership where
        the trust contract admits it.
      </p>
      <p>Observed state decides what you see here — never an operation's exit code alone.</p>
    </InfoHint>
  {/snippet}
  {#snippet actions()}
    <Button variant="outline" size="sm" disabled={pending} onclick={onRefresh}>
      {#if pending}
        <Spinner data-icon="inline-start" />
        Reading…
      {:else}
        <RefreshCwIcon data-icon="inline-start" />
        Refresh
      {/if}
    </Button>
    <Button size="sm" onclick={onAdd}>
      <PlusIcon data-icon="inline-start" />
      Add drive
    </Button>
  {/snippet}
</PageHeader>

{#if transportNotice !== null}
  <Alert.Root class="mt-4 border-primary/40 bg-primary/10">
    <p>{transportNotice}</p>
  </Alert.Root>
{/if}

{#if snapshot === null || registry === null}
  {#if pending}
    <!-- First read in flight: show the shape of the list, never a
         premature "no drives" claim. -->
    <section aria-label="Registered drives" aria-busy="true" class="mt-5">
      <div class="grid gap-4 lg:grid-cols-2">
        {#each [0, 1] as index (index)}
          <Card.Root aria-hidden="true">
            <Card.Header>
              <div class="flex items-center justify-between gap-3">
                <div class="flex min-w-0 flex-col gap-2">
                  <Skeleton class="h-5 w-32" />
                  <Skeleton class="h-3 w-56" />
                </div>
                <Skeleton class="h-5 w-24 rounded-full" />
              </div>
            </Card.Header>
            <Card.Content class="flex flex-col gap-3">
              <div class="flex flex-wrap gap-1.5">
                <Skeleton class="h-5 w-16 rounded-full" />
                <Skeleton class="h-5 w-36 rounded-full" />
              </div>
              <Skeleton class="h-3 w-16" />
              <Skeleton class="h-5 w-48 rounded-full" />
            </Card.Content>
            <Card.Footer class="mt-auto justify-between">
              <Skeleton class="h-3 w-48" />
              <Skeleton class="h-7 w-24 rounded-lg" />
            </Card.Footer>
          </Card.Root>
        {/each}
      </div>
    </section>
  {:else}
    <div class="mt-5">
      <Empty.Root class="border border-dashed border-border bg-card/50">
        <Empty.Header>
          <Empty.Media variant="icon"><HardDriveIcon /></Empty.Media>
          <Empty.Title>No drive list read yet</Empty.Title>
          <Empty.Description>Refresh to list the drives set up on this device.</Empty.Description>
        </Empty.Header>
      </Empty.Root>
    </div>
    <div class="mt-3">
      <ObservedNote observedAtMs={null} />
    </div>
  {/if}
{:else}
  {@const registry = snapshot.data}
  {#if registry.status === "unreadable"}
    <Alert.Root variant="destructive" class="mt-5">
      <p>
        Drive list unreadable: {registry.reason} — showing nothing rather than guessing.
      </p>
    </Alert.Root>
  {:else if registry.status === "nofile" || registry.drives.length === 0}
    <div class="mt-5">
      <Empty.Root class="border border-dashed border-border bg-card/50">
        <Empty.Header>
          <Empty.Media variant="icon"><HardDriveIcon /></Empty.Media>
          <Empty.Title>No drives set up yet</Empty.Title>
          <Empty.Description>
            Create one or join with an invitation — the drive appears here once it is ready.
            One window manages every drive on this device.
          </Empty.Description>
        </Empty.Header>
        <Empty.Content>
          <Button size="sm" onclick={onAdd}>
            <PlusIcon data-icon="inline-start" />
            Add drive
          </Button>
        </Empty.Content>
      </Empty.Root>
    </div>
  {:else}
    <section aria-label="Drives on this device" class="mt-5">
      <div class="grid gap-4 lg:grid-cols-2">
        {#each orderedDrives as drive (drive.drive_dir)}
          {@const facts = factsFor(drive)}
          <DriveCard
            {drive}
            {facts}
            onOpen={() => onOpen(drive.drive_dir)}
            onReveal={onReveal}
          />
        {/each}
      </div>
    </section>
  {/if}

  {#if registry.skipped.length > 0}
    <Card.Root class="mt-4">
      <Card.Header>
        <Card.Title class="text-sm">Some drives could not be loaded</Card.Title>
        <Card.Description>
          Entries this shell refused to use. They are listed, never guessed around.
        </Card.Description>
      </Card.Header>
      <Card.Content>
        <ul class="flex flex-col gap-1.5 text-xs text-muted-foreground">
          {#each registry.skipped as reason (reason)}
            <li class="flex gap-2">
              <span class="mt-1.5 size-1 shrink-0 rounded-full bg-current" aria-hidden="true"></span>
              <span class="min-w-0 break-words">{reason}</span>
            </li>
          {/each}
        </ul>
      </Card.Content>
    </Card.Root>
  {/if}

  <div class="mt-4">
    <ObservedNote observedAtMs={snapshot.observedAtMs} />
  </div>
{/if}
