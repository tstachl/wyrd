<!-- Status/posture: durable observation, liveness, and freshness
kept distinct. A live socket is not a durable fact; a paused owner
is reachable and quiet, never down. Everything here is the last
terminal answer, stamped — never a claim about right now. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
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
  <p>Outbox, carries, and fetch entries, in that order.</p>
{/snippet}

{#snippet materializedHint()}
  <p>Pinned and cached entries, in that order.</p>
{/snippet}

<PageHeader
  title="Status"
  size="section"
  description="The owner's last reported posture for this drive — observation, liveness, and freshness kept separate."
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
      Refresh to read this drive's posture from the owner. Nothing is filled in from memory.
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
      <span class="text-muted-foreground">Freshness</span>
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
      <Alert.Title>Paused — reachable and quiet, not down.</Alert.Title>
      <Alert.Description class="text-foreground/80">
        Pass {status.liveness.passes} holds; the loop answers, it does not advance.
      </Alert.Description>
    </Alert.Root>
  {/if}

  <div class="mt-4 flex flex-col gap-4">
    <Card.Root>
      <Card.Header>
        <Card.Title class="flex items-center gap-1.5">
          Durable observation
          <InfoHint label="About durable observation">
            <p>What a member may observe — never liveness.</p>
          </InfoHint>
        </Card.Title>
      </Card.Header>
      <Card.Content>
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
          <DetailRow label="Queue" hint={queueHint} hintLabel="About the queue">
            <span class="font-mono tabular-nums">
              {status.observe.queue.outbox} / {status.observe.queue.carries} / {status.observe.queue.fetch}
            </span>
          </DetailRow>
          <DetailRow label="Materialized" hint={materializedHint} hintLabel="About materialization">
            <span class="font-mono tabular-nums">
              {status.observe.materialization.pinned} / {status.observe.materialization.cached}
            </span>
          </DetailRow>
        </dl>
      </Card.Content>
    </Card.Root>

    <div class="grid gap-4 lg:grid-cols-2">
      <Card.Root>
        <Card.Header>
          <Card.Title class="flex items-center gap-1.5">
            Liveness
            <InfoHint label="About liveness">
              <p>Transport health and the last pass — a separate object from the observation.</p>
            </InfoHint>
          </Card.Title>
        </Card.Header>
        <Card.Content>
          <dl>
            <DetailRow label="Paused">
              {status.liveness.paused ? "Yes — quiet, not down" : "No"}
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
            <DetailRow label="Last publish">
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
            Freshness
            <InfoHint label="About freshness">
              <p>The projection judgment: fresh, or the last good retained with its reason.</p>
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
