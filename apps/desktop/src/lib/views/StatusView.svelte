<!-- Status in three layers: a plain summary sentence to glance at,
  cards that answer the user's questions, and a technical disclosure
  to diagnose. Reachability, relay connectivity, membership, freshness
  and convergence stay separate facts — never one green verdict. Every
  value is the last terminal answer plus the shell's observation stamp;
  the stamp is never presented as a synchronization time. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import * as Collapsible from "$lib/components/ui/collapsible/index.js";
  import { Badge } from "$lib/components/ui/badge/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import DetailRow from "$lib/components/detail-row.svelte";
  import PageHeader from "$lib/components/page-header.svelte";
  import InfoHint from "$lib/components/info-hint.svelte";
  import type { OwnerAvailability, Stamped, StatusSnapshot } from "$lib/state/presentation.svelte.js";
  import {
    freshnessLabel,
    isFirstRun,
    shortId,
    statusAttention,
    statusSummaryLine,
    type StatusFacts,
  } from "./format.js";
  import ObservedNote from "./ObservedNote.svelte";
  import ActivityIcon from "@lucide/svelte/icons/activity";
  import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
  import CircleAlertIcon from "@lucide/svelte/icons/circle-alert";
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

  /** Technical detail (R-D6): protocol terms live behind this. Closed by default. */
  let showTechnical = $state(false);

  const status = $derived(snapshot?.data ?? null);
  const freshStale = $derived(
    status !== null && typeof status.freshness.state === "object" ? status.freshness.state : null,
  );
  const facts = $derived<StatusFacts>({
    availability,
    paused: status?.liveness.paused ?? false,
    connectedRelays: status?.liveness.health.connected_relays ?? 0,
    totalRelays: status?.liveness.health.total_relays ?? 0,
    knownMembers: status?.observe.known_members ?? 0,
    peers: status?.observe.peers.length ?? 0,
    staleDetail: freshStale?.RetainedLastGood.detail ?? null,
    freshEmpty: status?.freshness.state === "FreshEmpty",
    localObjects: status?.observe.materialization.local_objects ?? 0,
  });
  const summary = $derived(statusSummaryLine(facts));
  const attention = $derived(statusAttention(facts));
  const firstRun = $derived(isFirstRun(facts));
  const freshnessTone = $derived(
    status === null
      ? ""
      : freshStale !== null
        ? "border-primary/40 bg-primary/10 text-foreground"
        : "border-ok/30 bg-ok/10 text-ok",
  );
  const relayPercent = $derived(
    facts.totalRelays === 0 ? 0 : Math.round((facts.connectedRelays / facts.totalRelays) * 100),
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
  description="What this drive is doing, whether other devices are sharing it, and when something needs attention."
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
      Refresh to read this drive's latest reported state. Nothing is filled in from memory.
    </p>
  </div>
  <div class="mt-3">
    <ObservedNote observedAtMs={null} />
  </div>
{:else}
  <!-- Glance: one sentence built from separate facts, not a verdict badge. -->
  <div class="mt-5 rounded-xl border border-border bg-card px-4 py-3">
    <p class="text-sm font-medium leading-relaxed">{summary}</p>
  </div>

  <!-- Attention appears only where the shell observed a problem. -->
  {#if attention.length > 0}
    <div class="mt-4 flex flex-col gap-2">
      {#each attention as item (item)}
        <Alert.Root class="border-primary/40 bg-primary/10">
          <CircleAlertIcon />
          <p class="text-sm leading-relaxed">{item}</p>
        </Alert.Root>
      {/each}
    </div>
  {/if}

  {#if firstRun}
    <Card.Root class="mt-4">
      <Card.Header>
        <Card.Title>This is a new drive</Card.Title>
        <Card.Description>Only this device is a member and no other devices have joined yet.</Card.Description>
      </Card.Header>
      <Card.Content class="flex flex-col gap-2 text-sm leading-relaxed">
        <p>
          Nothing has been shared with another device yet. When another device joins, its changes
          appear here as they sync.
        </p>
        <p>
          To share it, create an invitation on this device and use it to join from the other device.
        </p>
      </Card.Content>
    </Card.Root>
  {:else}
    <!-- Understand: the three dimensions that answer the user's questions. -->
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
            <DetailRow label="Known members">
              <span class="font-mono tabular-nums">{status.observe.known_members}</span>
            </DetailRow>
            <DetailRow label="Peers">
              <span class="font-mono tabular-nums">{status.observe.peers.length}</span>
            </DetailRow>
          </dl>
        </Card.Content>
      </Card.Root>

      <div class="grid gap-4 lg:grid-cols-2">
        <Card.Root>
          <Card.Header>
            <Card.Title class="flex items-center gap-1.5">
              Drive activity
              <InfoHint label="About drive activity">
                <p>
                  The last reported loop state — whether it was paused and how many relays were
                  connected. Never "recent activity": pass counts include idle iterations.
                </p>
              </InfoHint>
            </Card.Title>
          </Card.Header>
          <Card.Content>
            <dl>
              <DetailRow label="Paused">
                {status.liveness.paused ? "Yes — the loop is not advancing" : "No"}
              </DetailRow>
              <DetailRow label="Relays">
                <span class="flex items-center gap-2">
                  <span class="h-1.5 w-16 overflow-hidden rounded-full bg-muted">
                    <span
                      class="block h-full rounded-full bg-primary transition-[width]"
                      style:width={`${relayPercent}%`}
                    ></span>
                  </span>
                  <span class="font-mono text-xs tabular-nums">
                    {facts.connectedRelays} / {facts.totalRelays}
                  </span>
                </span>
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
            </dl>
          </Card.Content>
        </Card.Root>
      </div>
    </div>
  {/if}

  <!-- Diagnose: protocol vocabulary and counters stay behind this. -->
  <Collapsible.Root bind:open={showTechnical} class="mt-4">
    <Collapsible.Trigger>
      {#snippet child({ props })}
        <Button
          {...props}
          variant="ghost"
          size="sm"
          class="-ml-2 px-2 text-muted-foreground"
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
      <Card.Root class="mt-2">
        <Card.Header>
          <Card.Title>Technical detail</Card.Title>
          <Card.Description>Protocol vocabulary and the counters behind the sentence above.</Card.Description>
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
            <DetailRow label="Published on last pass">
              {#if status.liveness.report === null}
                <span class="font-normal text-muted-foreground">No pass reported</span>
              {:else}
                {status.liveness.report.published ? "Published" : "Not published"}
              {/if}
            </DetailRow>
            <DetailRow label="Revision / generation">
              <span class="font-mono tabular-nums">
                {status.freshness.revision} / {status.freshness.generation}
              </span>
            </DetailRow>
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
        </Card.Content>
      </Card.Root>
    </Collapsible.Content>
  </Collapsible.Root>

  <div class="mt-4">
    <ObservedNote observedAtMs={snapshot.observedAtMs} />
  </div>
{/if}
