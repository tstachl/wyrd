<!-- Status/posture: durable observation, liveness, and freshness
kept distinct. A live socket is not a durable fact; a paused owner
is reachable and quiet, never down. Everything here is the last
terminal answer, stamped — never a claim about right now. -->
<script lang="ts">
  import Button from "$lib/components/ui/button.svelte";
  import type { Stamped, StatusSnapshot } from "$lib/state/presentation.svelte.js";
  import { freshnessLabel, shortId } from "./format.js";
  import ObservedNote from "./ObservedNote.svelte";

  interface Props {
    driveLabel: string;
    snapshot: Stamped<StatusSnapshot> | null;
    pending: boolean;
    error: string | null;
    transportNotice: string | null;
    onRefresh: () => void;
  }

  let { driveLabel, snapshot, pending, error, transportNotice, onRefresh }: Props = $props();
</script>

<div class="flex items-center justify-between">
  <h1 class="text-2xl font-semibold tracking-tight">Status</h1>
  <Button variant="secondary" size="sm" disabled={pending} onclick={onRefresh}>
    {pending ? "Reading…" : "Refresh"}
  </Button>
</div>
<p class="mt-1 text-xs text-smoke">{driveLabel}</p>

{#if error !== null}
  <p class="mt-2 text-sm" role="alert">{error}</p>
{/if}
{#if transportNotice !== null}
  <p class="mt-2 text-sm" role="alert">{transportNotice}</p>
{/if}

{#if snapshot === null}
  <p class="mt-4 text-sm text-smoke">No observation yet. Refresh to read from the owner.</p>
  <ObservedNote observedAtMs={null} />
{:else}
  {@const status = snapshot.data}
  {#if status.liveness.paused}
    <p class="mt-4 rounded-md bg-amber px-3 py-2 text-sm font-medium text-ink" role="status">
      Paused — reachable and quiet, not down. Pass {status.liveness.passes} holds; the loop answers, it does not advance.
    </p>
  {/if}

  <section aria-label="Durable observation" class="mt-6">
    <h2 class="text-sm font-semibold tracking-wide text-smoke uppercase">Durable observation</h2>
    <p class="mt-1 text-xs text-smoke">What a member may observe — never liveness.</p>
    <dl class="mt-2 grid grid-cols-2 gap-x-6 gap-y-1 text-sm">
      <dt class="text-smoke">Tip epoch</dt>
      <dd>{status.observe.tip === null ? "none observed" : status.observe.tip.epoch}</dd>
      <dt class="text-smoke">Tip transition</dt>
      <dd>{status.observe.tip === null ? "—" : shortId(status.observe.tip.transition_id)}</dd>
      <dt class="text-smoke">Durability</dt>
      <dd>{status.observe.durability}</dd>
      <dt class="text-smoke">Converged</dt>
      <dd>{status.observe.convergence.converged ? "yes" : `no — ${status.observe.convergence.pending_heads} pending, ${status.observe.convergence.unfetchable_heads} unfetchable`}</dd>
      <dt class="text-smoke">Known members</dt>
      <dd>{status.observe.known_members}</dd>
      <dt class="text-smoke">Peers</dt>
      <dd>{status.observe.peers.length}</dd>
      <dt class="text-smoke">Held epochs</dt>
      <dd>{status.observe.held_epochs.length === 0 ? "none" : status.observe.held_epochs.join(", ")}</dd>
      <dt class="text-smoke">Queue (outbox / carries / fetch)</dt>
      <dd>{status.observe.queue.outbox} / {status.observe.queue.carries} / {status.observe.queue.fetch}</dd>
      <dt class="text-smoke">Materialized (pinned / cached)</dt>
      <dd>{status.observe.materialization.pinned} / {status.observe.materialization.cached}</dd>
    </dl>
  </section>

  <section aria-label="Liveness" class="mt-6">
    <h2 class="text-sm font-semibold tracking-wide text-smoke uppercase">Liveness</h2>
    <p class="mt-1 text-xs text-smoke">Transport health and the last pass — a separate object from the observation above.</p>
    <dl class="mt-2 grid grid-cols-2 gap-x-6 gap-y-1 text-sm">
      <dt class="text-smoke">Paused</dt>
      <dd>{status.liveness.paused ? "yes — quiet, not down" : "no"}</dd>
      <dt class="text-smoke">Passes</dt>
      <dd>{status.liveness.passes}</dd>
      <dt class="text-smoke">Relay stream</dt>
      <dd>{status.liveness.health.stream_alive ? "alive" : "not alive"}</dd>
      <dt class="text-smoke">Relays (connected / total)</dt>
      <dd>{status.liveness.health.connected_relays} / {status.liveness.health.total_relays}</dd>
      <dt class="text-smoke">Last publish</dt>
      <dd>{status.liveness.report === null ? "no pass reported" : status.liveness.report.published ? "published" : "not published"}</dd>
    </dl>
  </section>

  <section aria-label="Freshness" class="mt-6">
    <h2 class="text-sm font-semibold tracking-wide text-smoke uppercase">Freshness</h2>
    <p class="mt-1 text-xs text-smoke">The projection judgment: fresh, or the last good retained with its reason.</p>
    <dl class="mt-2 grid grid-cols-2 gap-x-6 gap-y-1 text-sm">
      <dt class="text-smoke">State</dt>
      <dd>{freshnessLabel(status.freshness.state)}</dd>
      <dt class="text-smoke">Revision / generation</dt>
      <dd>{status.freshness.revision} / {status.freshness.generation}</dd>
    </dl>
  </section>

  <ObservedNote observedAtMs={snapshot.observedAtMs} />
{/if}
