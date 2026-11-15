<!-- The unavailable state as a first-class view, not a toast:
the owner is down, the shell never opens the store itself, and
reconnecting resumes without restart. Recovery proves nothing
about operations that were in flight — those stay unconfirmed
until re-read through the owner.

This screen always means "something changed since ready": the
first-run ordering guarantees a completed create ends at a verified
socket, so an unavailable drive was verified before and its owner
stopped answering — never that the drive itself is broken. The
service fact beside the transport verdict names which cause holds
(definition missing, installed but stopped, running but
socket-absent), and the recovery buttons offer only the matching
move — start the installed agent, or reinstall the definition —
through the constrained CLI adapter. Never auto-start: a stopped
drive is a stable state until the person starts it. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import * as Empty from "$lib/components/ui/empty/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import CloudOffIcon from "@lucide/svelte/icons/cloud-off";
  import PlayIcon from "@lucide/svelte/icons/play";
  import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
  import WrenchIcon from "@lucide/svelte/icons/wrench";
  import type { ServiceState } from "$lib/ipc/results.js";

  interface Props {
    driveLabel: string;
    detail: string | null;
    pending: boolean;
    onRetry: () => void;
    /** The service-manager fact beside the transport verdict, if the registry named it. */
    serviceState?: ServiceState | null;
    /** Whether the reinstall recovery can run (credential paths verified at the gate). */
    canReinstall?: boolean;
    onStart?: () => void;
    onReinstall?: () => void;
  }

  let {
    driveLabel,
    detail,
    pending,
    onRetry,
    serviceState = null,
    canReinstall = false,
    onStart,
    onReinstall,
  }: Props = $props();

  const serviceDetail = $derived(
    serviceState !== null && serviceState.state !== "running" ? serviceState.detail : null,
  );
  const showStart = $derived(
    serviceState !== null &&
      (serviceState.state === "stopped" || serviceState.state === "wedged") &&
      onStart !== undefined,
  );
  const showReinstall = $derived(
    serviceState !== null && serviceState.state === "notinstalled" && onReinstall !== undefined,
  );
  const showDevHonesty = $derived(serviceState !== null && serviceState.state === "unknown");
</script>

<Empty.Root class="mt-6 border border-dashed border-border bg-card/50 py-12">
  <Empty.Header class="max-w-xl">
    <Empty.Media variant="icon" class="size-11">
      <CloudOffIcon class="size-5" />
    </Empty.Media>
    <Empty.Title class="text-base">Owner unavailable</Empty.Title>
    <Empty.Description class="leading-relaxed">
      <span class="font-mono text-xs">{driveLabel}</span> — other drives are unaffected.
    </Empty.Description>
    <Empty.Description class="leading-relaxed">
      This drive was ready before and its owner stopped answering — something changed since
      then, not something broken about the drive itself. The shell does not open the store
      itself, so there is nothing stale or empty to show, only the absence of an answer.
    </Empty.Description>
  </Empty.Header>
  <Empty.Content class="max-w-xl">
    {#if detail !== null}
      <Alert.Root variant="destructive" class="text-left">
        <p>{detail}</p>
      </Alert.Root>
    {/if}
    {#if serviceDetail !== null}
      <Alert.Root class="text-left">
        <p>{serviceDetail}</p>
      </Alert.Root>
    {/if}
    {#if showDevHonesty}
      <p class="text-xs leading-relaxed text-muted-foreground">
        No managed service verdict in this build — start the owner manually. Shipped builds
        supervise the owner through the OS service manager instead.
      </p>
    {/if}
    <div class="flex flex-wrap gap-2">
      <Button disabled={pending} onclick={onRetry}>
        {#if pending}
          <Spinner data-icon="inline-start" />
          Retrying…
        {:else}
          <RefreshCwIcon data-icon="inline-start" />
          Retry connection
        {/if}
      </Button>
      {#if showStart}
        <Button variant="outline" disabled={pending} onclick={onStart}>
          <PlayIcon data-icon="inline-start" />
          Start service
        </Button>
      {/if}
      {#if showReinstall}
        <Button variant="outline" disabled={pending || !canReinstall} onclick={onReinstall}>
          <WrenchIcon data-icon="inline-start" />
          Reinstall service
        </Button>
      {/if}
    </div>
    {#if showReinstall && !canReinstall}
      <p class="text-xs leading-relaxed text-muted-foreground">
        Reinstall needs this device's credential paths — finish the identity check first.
      </p>
    {/if}
    <p class="text-xs leading-relaxed text-muted-foreground">
      Retry reconnects and re-reads from the owner. A stopped drive stays stopped until you
      start it — the shell never auto-starts a service. Anything that was in flight stays
      unconfirmed until reconciled — recovery never replays it and never calls it a success.
    </p>
  </Empty.Content>
</Empty.Root>
