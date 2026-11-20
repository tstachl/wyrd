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
drive is a stable state until the person starts it. A refused
recovery reports its outcome on this screen instead of failing
silently. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import { Checkbox } from "$lib/components/ui/checkbox/index.js";
  import * as Field from "$lib/components/ui/field/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import * as Empty from "$lib/components/ui/empty/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import CloudOffIcon from "@lucide/svelte/icons/cloud-off";
  import PlayIcon from "@lucide/svelte/icons/play";
  import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
  import WrenchIcon from "@lucide/svelte/icons/wrench";
  import type { EnablementState, ServiceState } from "$lib/ipc/results.js";

  interface Props {
    driveLabel: string;
    detail: string | null;
    pending: boolean;
    onRetry: () => void;
    /** The service-manager fact beside the transport verdict, if the registry named it. */
    serviceState?: ServiceState | null;
    /** The login-start preference as the manager records it, if the registry named it. */
    enablement?: EnablementState | null;
    /** Whether the reinstall recovery can run (credential paths verified at the gate). */
    canReinstall?: boolean;
    /** The last recovery attempt's outcome, where it refused — never lifecycle state. */
    recoveryDetail?: string | null;
    onStart?: () => void;
    onReinstall?: () => void;
    /** Flip the login-start preference; the caller re-reads and the fresh probe decides the display. */
    onToggleEnablement?: (enable: boolean) => void;
  }

  let {
    driveLabel,
    detail,
    pending,
    onRetry,
    serviceState = null,
    enablement = null,
    canReinstall = false,
    recoveryDetail = null,
    onStart,
    onReinstall,
    onToggleEnablement,
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
  // The toggle rides beside Start/Reinstall, never on definition-less
  // or unanswered states: a session-posture drive has no unit to
  // enable, and an unanswered probe is not evidence either way.
  const showToggle = $derived(
    serviceState !== null &&
      (serviceState.state === "stopped" || serviceState.state === "wedged") &&
      onToggleEnablement !== undefined,
  );
  // Checked only where the manager positively records enabled;
  // indeterminate where it did not answer or keeps no record —
  // unchecked is a decision, never the display of an unknown.
  const toggleChecked = $derived(enablement !== null && enablement.enablement === "enabled");
  const toggleUnknown = $derived(enablement === null || enablement.enablement === "unknown");
  const enablementDetail = $derived(enablement !== null ? enablement.detail : null);
  // Dev-only honesty: no manager tool exists in this build, so no
  // managed-service verdict is possible. Shipped builds always
  // supervise the owner; their timeouts read `unknown` instead and
  // carry no manual-start wording. The state detail alert above
  // already carries the wording — no second paragraph.
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
      This drive was ready before and has stopped answering — something changed since
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
    {#if recoveryDetail !== null}
      <Alert.Root variant="destructive" class="text-left">
        <p>{recoveryDetail}</p>
      </Alert.Root>
    {/if}
    {#if showToggle}
      <Field.Group>
        <Field.Field orientation="horizontal">
          <Checkbox
            id="auto-start-toggle"
            checked={toggleChecked}
            indeterminate={toggleUnknown}
            disabled={pending || onToggleEnablement === undefined}
            onCheckedChange={(next) => {
              if (typeof next === "boolean" && onToggleEnablement !== undefined) {
                onToggleEnablement(next);
              }
            }}
          />
          <Field.Label for="auto-start-toggle" class="font-normal">
            Start drive service at login
          </Field.Label>
        </Field.Field>
        <Field.Description>
          The per-drive owner service the manager starts at login. Toggling never touches
          drive data and never implies the owner answered — a disabled drive stays stopped
          until started manually.
          {#if enablementDetail !== null}
            <span class="mt-1 block">{enablementDetail}</span>
          {/if}
        </Field.Description>
      </Field.Group>
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
      Retry reconnects and re-reads this drive. A stopped drive stays stopped until you
      start it — the shell never auto-starts a service. Anything that was in flight stays
      unconfirmed until reconciled — recovery never replays it and never calls it a success.
    </p>
  </Empty.Content>
</Empty.Root>
