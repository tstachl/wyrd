<!-- Lifecycle, observational plus runtime provisioning: the
registered drives, each verified this session, and the explicitly
selected drive — plus create, join, and leave through the
constrained CLI adapter. The selection stays presentation state
only: it never authorizes backend access, and every command carries
the drive directory explicitly. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import { Badge } from "$lib/components/ui/badge/index.js";
  import * as Empty from "$lib/components/ui/empty/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import PageHeader from "$lib/components/page-header.svelte";
  import InfoHint from "$lib/components/info-hint.svelte";
  import AvailabilityBadge from "$lib/components/availability-badge.svelte";
  import CopyButton from "$lib/components/copy-button.svelte";
  import type { Stamped } from "$lib/state/presentation.svelte.js";
  import type { ProvisionReport, RegistryState } from "$lib/ipc/results.js";
  import type { ProvisionCreateArgs, ProvisionJoinArgs, ProvisionLeaveArgs } from "$lib/ipc/operations.js";
  import ObservedNote from "./ObservedNote.svelte";
  import ProvisioningView from "./ProvisioningView.svelte";
  import HardDriveIcon from "@lucide/svelte/icons/hard-drive";
  import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
  import ArrowRightIcon from "@lucide/svelte/icons/arrow-right";
  import PlusIcon from "@lucide/svelte/icons/plus";

  interface Props {
    snapshot: Stamped<RegistryState> | null;
    selectedDriveDir: string | null;
    pending: boolean;
    transportNotice: string | null;
    initialIdentityFile: string | null;
    initialPassphraseFile: string | null;
    initialMode: "create" | "join";
    onRefresh: () => void;
    onOpen: (driveDir: string) => void;
    onCreate: (args: ProvisionCreateArgs) => Promise<ProvisionReport>;
    onJoin: (args: ProvisionJoinArgs) => Promise<ProvisionReport>;
    onLeave: (args: ProvisionLeaveArgs) => Promise<ProvisionReport>;
  }

  let { snapshot, selectedDriveDir, pending, transportNotice, initialIdentityFile, initialPassphraseFile, initialMode, onRefresh, onOpen, onCreate, onJoin, onLeave }: Props = $props();

  const registry = $derived(snapshot?.data ?? null);
  const drives = $derived(registry?.drives ?? []);

  function scrollToProvisioning(): void {
    document.getElementById("provisioning")?.scrollIntoView({ behavior: "smooth", block: "start" });
  }
</script>

<PageHeader
  title="Drives"
  description="Registered on this device. Each drive syncs independently; open one to see its status and settings."
>
  {#snippet meta()}
    <InfoHint label="About this view">
      <p>
        This view verifies what the provisioning and membership flows registered and whether each
        drive's owner answers. Creation and joining run through the installed CLI; leaving authors
        membership where the trust contract admits it.
      </p>
      <p>Observed state decides what you see here — never the operation's exit code alone.</p>
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
  {/snippet}
</PageHeader>

{#if transportNotice !== null}
  <Alert.Root class="mt-4 border-primary/40 bg-primary/10">
    <p>{transportNotice}</p>
  </Alert.Root>
{/if}

{#if snapshot === null || registry === null}
  <div class="mt-5">
    <Empty.Root class="border border-dashed border-border bg-card/50">
      <Empty.Header>
        <Empty.Media variant="icon"><HardDriveIcon /></Empty.Media>
        <Empty.Title>No registry read yet</Empty.Title>
        <Empty.Description>Refresh to list the drives registered on this device.</Empty.Description>
      </Empty.Header>
    </Empty.Root>
  </div>
  <div class="mt-3">
    <ObservedNote observedAtMs={null} />
  </div>
{:else}
  {@const registry = snapshot.data}
  {#if registry.status === "unreadable"}
    <Alert.Root variant="destructive" class="mt-5">
      <p>
        Registry unreadable: {registry.reason} — showing nothing rather than guessing.
      </p>
    </Alert.Root>
  {:else if registry.status === "nofile" || registry.drives.length === 0}
    <div class="mt-5">
      <Empty.Root class="border border-dashed border-border bg-card/50">
        <Empty.Header>
          <Empty.Media variant="icon"><HardDriveIcon /></Empty.Media>
          <Empty.Title>No drives registered yet</Empty.Title>
          <Empty.Description>
            Create one below or join from a sealed invitation — the drive appears here once the
            registry names it. One window manages every registered drive.
          </Empty.Description>
        </Empty.Header>
        <Empty.Content>
          <Button size="sm" onclick={scrollToProvisioning}>
            <PlusIcon data-icon="inline-start" />
            Create a drive
          </Button>
        </Empty.Content>
      </Empty.Root>
    </div>
  {:else}
    <section aria-label="Registered drives" class="mt-5">
      <div class="grid gap-4 lg:grid-cols-2">
        {#each drives as drive (drive.drive_dir)}
          {@const selected = selectedDriveDir === drive.drive_dir}
          <Card.Root class={selected ? "ring-1 ring-primary/50" : ""}>
            <Card.Header>
              <div class="flex min-w-0 items-start justify-between gap-3">
                <div class="flex min-w-0 flex-1 flex-col gap-1">
                  <Card.Title class="flex flex-wrap items-center gap-2">
                    <span class="truncate">{drive.name}</span>
                    {#if selected}
                      <Badge variant="outline" class="border-primary/40 bg-primary/10">Open</Badge>
                    {/if}
                  </Card.Title>
                  <Card.Description class="flex min-w-0 items-center gap-1">
                    <span class="truncate font-mono text-[0.7rem]">{drive.drive_dir}</span>
                    <CopyButton text={drive.drive_dir} label="drive path" />
                  </Card.Description>
                </div>
                <AvailabilityBadge
                  state={drive.availability.state}
                  detail={drive.availability.state === "available" ? null : drive.availability.detail}
                />
              </div>
            </Card.Header>
            <Card.Content class="flex flex-col gap-3">
              <div class="flex flex-wrap gap-1.5">
                <Badge variant="secondary">{drive.mode}</Badge>
                <Badge variant="outline" class="font-mono text-[0.7rem]">service {drive.service}</Badge>
                {#if drive.mountpoint !== null}
                  <Badge variant="outline" class="font-mono text-[0.7rem]">{drive.mountpoint}</Badge>
                {/if}
              </div>
              <div class="flex flex-col gap-1.5">
                <p class="text-xs font-medium tracking-wide text-muted-foreground uppercase">Relays</p>
                {#if drive.relays.length === 0}
                  <p class="text-xs text-muted-foreground">None configured</p>
                {:else}
                  <div class="flex flex-wrap gap-1.5">
                    {#each drive.relays as relay (relay)}
                      <Badge variant="outline" class="max-w-full border-border bg-background font-mono text-[0.7rem] font-normal">
                        <span class="truncate" title={relay}>{relay}</span>
                      </Badge>
                    {/each}
                  </div>
                {/if}
              </div>
            </Card.Content>
            <Card.Footer class="justify-between">
              <span class="text-xs text-muted-foreground">
                {selected ? "Selected for Status and Settings." : "Open to inspect this drive."}
              </span>
              <Button size="sm" variant={selected ? "outline" : "default"} onclick={() => onOpen(drive.drive_dir)}>
                {selected ? "Open" : "Select & open"}
                <ArrowRightIcon data-icon="inline-end" />
              </Button>
            </Card.Footer>
          </Card.Root>
        {/each}
      </div>
    </section>
  {/if}

  {#if registry.skipped.length > 0}
    <Card.Root class="mt-4">
      <Card.Header>
        <Card.Title class="text-sm">Skipped registry entries</Card.Title>
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

  <div class="mt-10">
    <ProvisioningView
      pending={pending}
      initialIdentityFile={initialIdentityFile}
      initialPassphraseFile={initialPassphraseFile}
      initialMode={initialMode}
      onCreate={onCreate}
      onJoin={onJoin}
      onLeave={onLeave}
      onDone={onRefresh}
    />
  </div>

  <div class="mt-4">
    <ObservedNote observedAtMs={snapshot.observedAtMs} />
  </div>
{/if}
