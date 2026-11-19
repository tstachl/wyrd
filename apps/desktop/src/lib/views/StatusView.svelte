<!-- Status/posture: sync activity, drive activity, and drive data
kept distinct. A live socket is not a durable fact; a paused loop
does not advance, but the drive still answers. Everything here is
the last terminal answer, stamped — never a claim about right now. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import * as Collapsible from "$lib/components/ui/collapsible/index.js";
  import { Badge } from "$lib/components/ui/badge/index.js";
  import { Separator } from "$lib/components/ui/separator/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import DetailRow from "$lib/components/detail-row.svelte";
  import PageHeader from "$lib/components/page-header.svelte";
  import AvailabilityBadge from "$lib/components/availability-badge.svelte";
  import InfoHint from "$lib/components/info-hint.svelte";
  import type { OwnerAvailability, Stamped, StatusSnapshot } from "$lib/state/presentation.svelte.js";
  import { freshnessLabel, shortId } from "./format.js";
  import ObservedNote from "./ObservedNote.svelte";
  import ActivityIcon from "@lucide/svelte/icons/activity";
  import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
  import PauseCircleIcon from "@lucide/svelte/icons/pause-circle";
  import CheckIcon from "@lucide/svelte/icons/check";
  import ChevronDownIcon from "@lucide/svelte/icons/chevron-down";
  import XIcon from "@lucide/svelte/icons/x";

  interface Props {
    snapshot: Stamped<StatusSnapshot> | null;
    availability: OwnerAvailability;
    pending: boolean;
    error: string | null;
    transportNotice: string | null;
    onRefresh: () => void;
  }

  let { snapshot, availability, pending, error, transportNotice, onRefresh }: Props = $props();

  /** Technical-detail disclosure (R-D6): epoch, head, queue and
   * residency rows live behind this; primary copy keeps to
   * consequences. Closed by default. */
  let showTechnical = $state(false);

  const status = $derived(snapshot?.data ?? null);
  const freshStale = $derived(
    status !== null && typeof status.freshness.state === "object" ? status.freshness.state : null,
  );
  const freshnessTone = $derived(
    status === null
      ? ""
      : freshStale !== null
        ? "border-primary/40 bg-primary/10 text-foreground"
        : "border-ok/30 bg-ok/10 text-ok",
  );
  const relayPercent = $derived(
    status === null || status.liveness.health.total_relays === 0
      ? 0
      : Math.round(
          (status.liveness.health.connected_relays / status.liveness.health.total_relays) * 100,
        ),
  );
</script>

{#snippet queueHint()}
  <p>
    Wyrd still has work to do for this drive. This can include
    updates to share with other devices and content waiting to be
    transferred.
  </p>
  <p>
    In order: outbox control work, locally staged work and unstarted
    fetches — not files or items.
  </p>
{/snippet}

{#snippet materializedHint()}
  <p>Kept on this device — promised retention; pinning never fetches.</p>
  <p>Stored on this device — bytes recorded locally, reclaimable unless kept.</p>
  <p>Not stored on this device — a residency fact only, never read as available elsewhere.</p>
{/snippet}

<PageHeader
  title="Status"
  size="section"
  description="The last reported posture for this drive — sync activity, drive activity, and drive data kept separate."
>
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

{#if error !== null}
  <Alert.Root variant="destructive" class="mt-4">
    <p>{error}</p>
  </Alert.Root>
{/if}
{#if transportNotice !== null}
  <Alert.Root class="mt-4 border-primary/40 bg-primary/10">
    <p>{transportNotice}</p>
  </Alert.Root>
{/if}

{#if snapshot === null || status === null}
  <div class="mt-6 flex flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border bg-card/50 px-6 py-14 text-center">
    <div class="flex size-10 items-center justify-center rounded-full bg-muted text-muted-foreground">
      <ActivityIcon class="size-5" />
    </div>
    <p class="text-sm font-medium">No observation yet</p>
    <p class="max-w-sm text-sm text-muted-foreground">
      Refresh to read this drive's latest reported posture. Nothing is filled in from memory.
    </p>
  </div>
  <div class="mt-3">
    <ObservedNote observedAtMs={null} />
  </div>
{:else}
  <div class="mt-5 flex flex-wrap items-center gap-x-6 gap-y-2 rounded-xl border border-border bg-card px-4 py-3 text-sm">
    <span class="flex items-center gap-1.5">
      <span class="text-muted-foreground">Connection</span>
      <AvailabilityBadge state={availability} />
    </span>
    <Separator orientation="vertical" class="h-4" />
    <span class="flex items-center gap-1.5">
      <span class="text-muted-foreground">Drive data</span>
      <Badge variant="outline" class={freshnessTone}>
        {freshStale === null ? freshnessLabel(status.freshness.state) : "Stale"}
      </Badge>
    </span>
    <Separator orientation="vertical" class="h-4" />
    <span class="flex items-center gap-1.5">
      <span class="text-muted-foreground">Converged</span>
      {#if status.observe.convergence.converged}
        <Badge variant="outline" class="border-ok/30 bg-ok/10 text-ok">
          <CheckIcon />
          Yes
        </Badge>
      {:else}
        <Badge variant="outline" class="border-bad/30 bg-bad/10 text-bad">
          <XIcon />
          No
        </Badge>
      {/if}
    </span>
    <span class="ml-auto font-mono text-xs tabular-nums text-muted-foreground">
      Pass {status.liveness.passes}
    </span>
  </div>

  {#if status.liveness.paused}
    <Alert.Root class="mt-4 border-primary/40 bg-primary/10">
      <PauseCircleIcon />
      <Alert.Title>Synchronization is paused.</Alert.Title>
      <Alert.Description class="text-foreground/80">
        The drive is still running and can answer requests.
      </Alert.Description>
    </Alert.Root>
  {/if}

  <div class="mt-4 flex flex-col gap-4">
    <Card.Root>
      <Card.Header>
        <Card.Title class="flex items-center gap-1.5">
          Sync activity
          <InfoHint label="About sync activity">
            <p>What Wyrd has recorded or completed — never liveness.</p>
          </InfoHint>
        </Card.Title>
      </Card.Header>
      <Card.Content>
        <dl class="grid gap-x-10 md:grid-cols-2">
          <DetailRow label="Durability">
            <Badge variant="outline" class="font-mono text-[0.7rem]">{status.observe.durability}</Badge>
          </DetailRow>
          <DetailRow label="Converged">
            {#if status.observe.convergence.converged}
              <span class="text-ok">Yes</span>
            {:else}
              <span class="text-bad">
                No — {status.observe.convergence.pending_heads} pending, {status.observe.convergence.unfetchable_heads} unfetchable
              </span>
            {/if}
          </DetailRow>
          <DetailRow label="Known members">
            <span class="font-mono tabular-nums">{status.observe.known_members}</span>
          </DetailRow>
          <DetailRow label="Peers">
            <span class="font-mono tabular-nums">{status.observe.peers.length}</span>
          </DetailRow>
        </dl>
        <Collapsible.Root bind:open={showTechnical}>
          <Collapsible.Trigger>
            {#snippet child({ props })}
              <Button
                {...props}
                variant="ghost"
                size="sm"
                class="-ml-2 mt-2 px-2 text-muted-foreground"
              >
                {showTechnical ? "Hide technical detail" : "Show technical detail"}
                <ChevronDownIcon
                  data-icon="inline-end"
                  class={showTechnical ? "rotate-180 transition-transform" : "transition-transform"}
                />
              </Button>
            {/snippet}
          </Collapsible.Trigger>
          <Collapsible.Content>
            <dl class="grid gap-x-10 md:grid-cols-2">
              <DetailRow label="Tip epoch">
                {#if status.observe.tip === null}
                  <span class="font-normal text-muted-foreground">None observed</span>
                {:else}
                  <span class="font-mono tabular-nums">{status.observe.tip.epoch}</span>
                {/if}
              </DetailRow>
              <DetailRow label="Tip transition">
                <span class="font-mono text-xs">
                  {status.observe.tip === null ? "—" : shortId(status.observe.tip.transition_id)}
                </span>
              </DetailRow>
              <DetailRow label="Held epochs">
                {#if status.observe.held_epochs.length === 0}
                  <span class="font-normal text-muted-foreground">None</span>
                {:else}
                  <span class="flex flex-wrap justify-end gap-1">
                    {#each status.observe.held_epochs as epoch (epoch)}
                      <Badge variant="outline" class="font-mono text-[0.7rem] tabular-nums">{epoch}</Badge>
                    {/each}
                  </span>
                {/if}
              </DetailRow>
              <DetailRow label="Pending work" hint={queueHint} hintLabel="About pending work">
                <span class="font-mono tabular-nums">
                  {status.observe.queue.outbox} / {status.observe.queue.carries} / {status.observe.queue.fetch}
                </span>
              </DetailRow>
              <DetailRow label="Local files" hint={materializedHint} hintLabel="About local files">
                <span class="font-mono tabular-nums">
                  {status.observe.materialization.pinned} / {status.observe.materialization.cached}
                </span>
              </DetailRow>
            </dl>
          </Collapsible.Content>
        </Collapsible.Root>
      </Card.Content>
    </Card.Root>

    <div class="grid gap-4 lg:grid-cols-2">
      <Card.Root>
        <Card.Header>
          <Card.Title class="flex items-center gap-1.5">
            Drive activity
            <InfoHint label="About drive activity">
              <p>
                The owner's last reported loop state — completed passes,
                whether it is paused, whether the relay stream was alive
                and how many relays were connected, and whether that pass
                published. Never "recent activity": pass counts include
                idle iterations.
              </p>
            </InfoHint>
          </Card.Title>
        </Card.Header>
        <Card.Content>
          <dl>
            <DetailRow label="Paused">
              {status.liveness.paused ? "Yes — the loop is not advancing" : "No"}
            </DetailRow>
            <DetailRow label="Passes">
              <span class="font-mono tabular-nums">{status.liveness.passes}</span>
            </DetailRow>
            <DetailRow label="Relay stream">
              {#if status.liveness.health.stream_alive}
                <Badge variant="outline" class="border-ok/30 bg-ok/10 text-ok">
                  <CheckIcon />
                  Alive
                </Badge>
              {:else}
                <Badge variant="outline" class="border-bad/30 bg-bad/10 text-bad">
                  <XIcon />
                  Not alive
                </Badge>
              {/if}
            </DetailRow>
            <DetailRow label="Relays (connected / total)">
              <span class="flex items-center gap-2">
                <span class="h-1.5 w-16 overflow-hidden rounded-full bg-muted">
                  <span
                    class="block h-full rounded-full bg-primary transition-[width]"
                    style:width={`${relayPercent}%`}
                  ></span>
                </span>
                <span class="font-mono text-xs tabular-nums">
                  {status.liveness.health.connected_relays} / {status.liveness.health.total_relays}
                </span>
              </span>
            </DetailRow>
            <DetailRow label="Published on last pass">
              {#if status.liveness.report === null}
                <span class="font-normal text-muted-foreground">No pass reported</span>
              {:else}
                {status.liveness.report.published ? "Published" : "Not published"}
              {/if}
            </DetailRow>
          </dl>
        </Card.Content>
      </Card.Root>

      <Card.Root>
        <Card.Header>
          <Card.Title class="flex items-center gap-1.5">
            Drive data
            <InfoHint label="About drive data">
              <p>
                Whether the served data is current for this device.
                Current does not mean converged with peers: pending
                heads can coexist with current data.
              </p>
            </InfoHint>
          </Card.Title>
        </Card.Header>
        <Card.Content>
          <dl>
            <DetailRow label="State">
              <Badge variant="outline" class={freshnessTone}>{freshnessLabel(status.freshness.state)}</Badge>
            </DetailRow>
            <DetailRow label="Revision / generation">
              <span class="font-mono tabular-nums">
                {status.freshness.revision} / {status.freshness.generation}
              </span>
            </DetailRow>
          </dl>
        </Card.Content>
      </Card.Root>
    </div>
  </div>

  <div class="mt-4">
    <ObservedNote observedAtMs={snapshot.observedAtMs} />
  </div>
{/if}
