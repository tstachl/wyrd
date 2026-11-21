<!-- Session unlock for definition-less drives: the drive needs its
passphrase to open for this session. The passphrase travels to the
backend once (stdin pipe, never argv), lives in this component's
state only for the entry, and is cleared after every attempt —
success or failure. Never persisted here, never logged. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import * as Field from "$lib/components/ui/field/index.js";
  import { Input } from "$lib/components/ui/input/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import { storeAccountFor } from "./format.js";
  import CredentialStorageToggle from "./credential-storage-toggle.svelte";
  import LockKeyholeIcon from "@lucide/svelte/icons/lock-keyhole";
  import CircleAlertIcon from "@lucide/svelte/icons/circle-alert";
  import KeyRoundIcon from "@lucide/svelte/icons/key-round";

  interface Props {
    driveLabel: string;
    /** Registry entry name; derives the store account label. Null where unknown. */
    driveName: string | null;
    /** The locked-gate detail naming both recoveries (session unlock and managed reinstall). */
    detail: string | null;
    pending: boolean;
    /** False where the start-up gate never verified credential paths — file unlock cannot run. Store unlock needs only the drive name. */
    identityReady: boolean;
    /** The last unlock attempt's refused outcome, or null where it completed. */
    unlockError: string | null;
    onUnlock: (passphrase: string, useStore: boolean) => Promise<void>;
  }

  let { driveLabel, driveName, detail, pending, identityReady, unlockError, onUnlock }: Props = $props();

  let passphrase = $state("");
  let showPassphrase = $state(false);
  let working = $state(false);
  let localError: string | null = $state(null);
  let useStore = $state(true);

  const busy = $derived(pending || working);
  const storeAccount = $derived(driveName === null ? null : storeAccountFor(driveName));
  const canSubmit = $derived(
    passphrase !== "" && !busy && (useStore ? storeAccount !== null : identityReady),
  );

  async function submit(): Promise<void> {
    localError = null;
    if (useStore && storeAccount === null) {
      localError = "This drive has no registry name to derive its store item from.";
      return;
    }
    if (!useStore && !identityReady) {
      localError = "This device's identity is not verified yet — finish the identity check first.";
      return;
    }
    if (passphrase === "") {
      localError = "Enter the drive passphrase to unlock it for this session.";
      return;
    }
    const attempt = passphrase;
    // Clear before the round trip so the secret lives in state for
    // the shortest possible window, whatever the outcome.
    passphrase = "";
    working = true;
    try {
      await onUnlock(attempt, useStore);
    } finally {
      working = false;
    }
  }
</script>

<Card.Root class="mt-6 border-primary/30">
  <Card.Header>
    <Card.Title class="flex items-center gap-2 text-base">
      <LockKeyholeIcon class="size-4" />
      Locked for this session
    </Card.Title>
    <Card.Description>
      <span class="font-mono text-xs">{driveLabel}</span> — this drive isn't running. Unlock it
      with its passphrase for this session, or use the recovery below.
    </Card.Description>
  </Card.Header>
  <Card.Content class="flex flex-col gap-4">
    {#if detail !== null}
      <Alert.Root class="border-primary/40 bg-primary/10">
        <KeyRoundIcon />
        <Alert.Description class="text-foreground/80">{detail}</Alert.Description>
      </Alert.Root>
    {/if}
    <!-- Toggle extracted to a child component: sibling interactive
      templates inline here break the happy-dom re-registration the
      mounted tests rely on (second mount crashes traversing the
      cloned template). -->
    <CredentialStorageToggle useStore={useStore} onSelect={(next) => (useStore = next)} />
    {#if useStore}
      <p class="text-xs leading-relaxed text-muted-foreground">
        Loads this drive's identity from its store item — one item per drive,
        never a secret. Only the passphrase below is typed.
      </p>
    {:else}
      <p class="text-xs leading-relaxed text-muted-foreground">
        Operator deployments only: loads the identity from this device's verified
        identity file. Only the passphrase below is typed.
      </p>
    {/if}
    <Field.Field>
      <Field.Label for="unlock-passphrase">Drive passphrase</Field.Label>
      <Input
        id="unlock-passphrase"
        type={showPassphrase ? "text" : "password"}
        bind:value={passphrase}
        disabled={busy}
        autocomplete="current-password"
        placeholder="Enter the drive passphrase"
        onkeydown={(event) => {
          if (event.key === "Enter") void submit();
        }}
      />
      <Field.Description>
        Typed only — never stored. Rebooting this device or locking the drive locks it
        again. Quitting the app leaves the drive running. <button
          type="button"
          class="underline underline-offset-2"
          disabled={busy}
          onclick={() => (showPassphrase = !showPassphrase)}
        >
          {showPassphrase ? "Hide" : "Show"}
        </button>
      </Field.Description>
    </Field.Field>
    {#if localError !== null}
      <Alert.Root class="border-primary/40 bg-primary/10">
        <CircleAlertIcon />
        <Alert.Description class="text-foreground/80">{localError}</Alert.Description>
      </Alert.Root>
    {/if}
    {#if unlockError !== null}
      <Alert.Root variant="destructive">
        <CircleAlertIcon />
        <Alert.Description>{unlockError}</Alert.Description>
      </Alert.Root>
    {/if}
    {#if useStore ? storeAccount === null : !identityReady}
      <p class="text-xs leading-relaxed text-muted-foreground">
        {#if useStore}
          Unlock needs this drive's registry name to derive its store item.
        {:else}
          Unlock needs this device's verified identity — finish the identity check first.
        {/if}
      </p>
    {/if}
  </Card.Content>
  <Card.Footer class="justify-end">
    <Button size="sm" disabled={!canSubmit} onclick={() => void submit()}>
      {#if working}
        <Spinner data-icon="inline-start" />
        Unlocking…
      {:else}
        <LockKeyholeIcon data-icon="inline-start" />
        Unlock for this session
      {/if}
    </Button>
  </Card.Footer>
</Card.Root>
