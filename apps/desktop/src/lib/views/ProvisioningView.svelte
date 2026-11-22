<!-- Dedicated Add drive page: create on this device or join with an
invitation through the constrained CLI adapter. Every operation
returns the outcome plus the post-run observation; observed state
decides the display, the outcome only indicates what the operation
reported. A provision that both reported no error and is confirmed
by the observation (the registry names the new drive) resolves
null — the caller navigates off this form, so no success report
renders here. Anything else resolves the report, which renders
below with the entered values kept for fix-and-retry. Leaving a drive lives in that drive's Settings — a
destructive membership action does not belong in an add flow. New
drives provision into the OS credential store by default (the
account derives from the drive name, seeds come from this device's
verified identity — no file choosing); credential files stay for
operator-provisioned deployments. Invitation inputs are paths only,
picked natively where the dialog capability answers and entered
by hand elsewhere. Secret contents never travel. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Field from "$lib/components/ui/field/index.js";
  import * as Tabs from "$lib/components/ui/tabs/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import * as InputGroup from "$lib/components/ui/input-group/index.js";
  import { Input } from "$lib/components/ui/input/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import PageHeader from "$lib/components/page-header.svelte";
  import RelayEditor from "$lib/components/relay-editor.svelte";
  import InfoHint from "$lib/components/info-hint.svelte";
  import CredentialStorageToggle from "./credential-storage-toggle.svelte";
  import type { ProvisionReport } from "$lib/ipc/results.js";
  import { pickExistingFile } from "$lib/ipc/files.js";
  import type { ProvisionCreateArgs, ProvisionJoinArgs } from "$lib/ipc/operations.js";
  import {
    buildCreateArgs,
    buildJoinArgs,
    type CredentialMode,
  } from "$lib/ipc/provisioning-args.js";
  import {
    describeProvisionResult,
    membershipSummary,
    nextDriveDir,
    observedRegistrySummary,
    storeAccountFor,
    SUGGESTED_RELAYS,
    suggestedDriveDirFor,
  } from "./format.js";
  import ArrowLeftIcon from "@lucide/svelte/icons/arrow-left";
  import PlusIcon from "@lucide/svelte/icons/plus";
  import DownloadIcon from "@lucide/svelte/icons/download";
  import FolderOpenIcon from "@lucide/svelte/icons/folder-open";
  import SparklesIcon from "@lucide/svelte/icons/sparkles";
  import TerminalIcon from "@lucide/svelte/icons/terminal";
  import CircleAlertIcon from "@lucide/svelte/icons/circle-alert";

  interface Props {
    pending: boolean;
    initialIdentityFile: string | null;
    initialPassphraseFile: string | null;
    initialMode: "create" | "join";
    /**
     * Null where the caller navigated off the form (clean exit plus
     * an observation confirming the new drive) — no report renders
     * and `onDone` stays silent because the caller already re-read.
     * A report otherwise, rendered below with values kept.
     */
    onCreate: (args: ProvisionCreateArgs) => Promise<ProvisionReport | null>;
    onJoin: (args: ProvisionJoinArgs) => Promise<ProvisionReport | null>;
    /** Registry re-read after every operation; observed state decides. */
    onDone: () => void;
    /** Back to the drive list. */
    onBack: () => void;
  }

  let { pending, initialIdentityFile, initialPassphraseFile, initialMode, onCreate, onJoin, onDone, onBack }: Props = $props();

  type Mode = "create" | "join";

  let mode = $state<Mode>("create");
  let credentialMode = $state<CredentialMode>("store");
  let name = $state("");
  let driveDir = $state("");
  let driveDirTouched = $state(false);
  // Suggested relays pre-filled (a copy — RelayEditor mutates its
  // own list): keep, edit, or clear before saving; the drive keeps
  // exactly what is listed. Shared across the create and join tabs.
  let relays = $state<string[]>([...SUGGESTED_RELAYS]);
  // True where the list no longer matches the suggestion (including a
  // full clear) — the restore affordance below shows itself then.
  const relaysDiffer = $derived(
    relays.length !== SUGGESTED_RELAYS.length ||
      relays.some((relay, index) => relay !== SUGGESTED_RELAYS[index]),
  );
  let identityFile = $state("");
  let passphraseFile = $state("");
  let invitationFile = $state("");
  // Out-of-band expectation pins (optional 64-hex): the owner meant
  // to trust, the drive, the genesis. Blank means unchecked — the
  // backend enforces every supplied pin before touching disk.
  let expectedOwner = $state("");
  let expectedDrive = $state("");
  let expectedGenesis = $state("");
  // Pre-fill from onboarding (the first create or join goes through the
  // unchanged constrained CLI adapter with the committed credentials).
  // Only fills empty fields, never clobbers user edits; the mode
  // follows Ready's Create/Join choice until the user picks a tab.
  let modeTouched = $state(false);
  $effect(() => {
    if (identityFile === "" && initialIdentityFile !== null) identityFile = initialIdentityFile;
  });
  $effect(() => {
    if (passphraseFile === "" && initialPassphraseFile !== null) passphraseFile = initialPassphraseFile;
  });
  $effect(() => {
    if (!modeTouched && mode !== initialMode) mode = initialMode;
  });
  let working = $state(false);
  let report: ProvisionReport | null = $state(null);
  let error: string | null = $state(null);

  const busy = $derived(pending || working);
  // Expectation pins are blank (unchecked) or exactly 64 hex
  // characters; anything else blocks the join before any submit.
  // The backend re-validates, so this gate is typo-catching only.
  const pinOk = (pin: string): boolean => pin.trim() === "" || /^[0-9a-fA-F]{64}$/.test(pin.trim());
  const expectationsValid = $derived(
    pinOk(expectedOwner) && pinOk(expectedDrive) && pinOk(expectedGenesis),
  );
  // Store mode seeds the item from this device's verified identity
  // (no file choosing); file mode reads the picked credential paths.
  const seedsReady = $derived(
    initialIdentityFile !== null &&
      initialIdentityFile.trim() !== "" &&
      initialPassphraseFile !== null &&
      initialPassphraseFile.trim() !== "",
  );
  const storeAccount = $derived(storeAccountFor(name));
  const credentialsReady = $derived(
    credentialMode === "store"
      ? seedsReady
      : identityFile.trim() !== "" && passphraseFile.trim() !== "",
  );

  // Drives live beside the registry: the credential files sit at
  // `<base>/keys/<leaf>`, so the suggestion is `<base>/drives/<name>`
  // (pure helper in `./format.js`, pinned by `format.test.ts`).
  // Parents are the store's business (it creates the chain,
  // hardening only the leaf), so a nested suggestion is safe.
  const identityForSuggestion = $derived(
    identityFile.trim() !== "" ? identityFile : (initialIdentityFile ?? ""),
  );
  const suggestedDriveDir = $derived(suggestedDriveDirFor(name, identityForSuggestion));

  // While untouched, follow the suggestion; otherwise keep the
  // operator's value (including an explicit clear, which marks the
  // field touched).
  $effect(() => {
    driveDir = nextDriveDir(driveDirTouched, driveDir, suggestedDriveDir);
  });

  // Empty relays are refused before anything is created: this form
  // always runs managed (`noService: false`), so the CLI's
  // managed-service refusal is the honest outcome and the detail
  // below reports it verbatim. There is no local-only mode in this
  // form. The field starts with the suggested relays (see
  // `SUGGESTED_RELAYS` in `./format.js`) — an editable starting
  // point, not a requirement: the drive saves exactly what is
  // listed, including an explicit clear.
  const relaysEmpty = $derived(relays.length === 0);

  async function run(call: () => Promise<ProvisionReport | null>): Promise<void> {
    working = true;
    error = null;
    try {
      // Null means the caller advanced off this form (refresh
      // already re-read there); only a report re-reads here.
      report = await call();
      if (report !== null) onDone();
    } catch (failure: unknown) {
      error = failure instanceof Error ? failure.message : String(failure);
    } finally {
      working = false;
    }
  }

  async function browse(set: (path: string) => void, title: string): Promise<void> {
    const picked = await pickExistingFile(title);
    if (picked !== null) set(picked);
  }

  function switchMode(next: Mode): void {
    mode = next;
    modeTouched = true;
    report = null;
    error = null;
  }
</script>

<PageHeader
  title="Add drive"
  description="Create a drive on this device, or join one with an invitation. The drive appears in the list once the registry names it."
>
  {#snippet actions()}
    <Button variant="outline" size="sm" onclick={onBack}>
      <ArrowLeftIcon data-icon="inline-start" />
      Back to drives
    </Button>
  {/snippet}
</PageHeader>

<Card.Root class="mt-6">
  <Card.Content>
    <!-- Toggle extracted to a child component: sibling interactive
      templates inline here break the happy-dom re-registration the
      mounted tests rely on (second mount crashes traversing the
      cloned template). The toggle flips local display state only, so
      it never needs the flight gate — the create/join submits below
      stay gated. -->
    <CredentialStorageToggle
      useStore={credentialMode === "store"}
      onSelect={(next) => (credentialMode = next ? "store" : "file")}
      storeLabel="OS credential store (recommended)"
    />
    {#if credentialMode === "store"}
      <p class="mb-5 text-xs leading-relaxed text-muted-foreground">
        New drives keep their credentials in the OS credential store under
        <span class="font-mono text-xs">{storeAccount}</span>. The drive starts at
        login without typing anything, and no passphrase file is created for the
        drive service. Seeded from this device's verified identity; no file
        choosing needed.
      </p>
    {:else}
      <p class="mb-5 text-xs leading-relaxed text-muted-foreground">
        Operator deployments only: pick the credential files the drive reads. The
        drive service will name these paths.
      </p>
    {/if}
    <Tabs.Root value={mode} onValueChange={(value) => switchMode(value as Mode)}>
      <Tabs.List>
        <Tabs.Trigger value="create">
          <PlusIcon data-icon="inline-start" />
          Create
        </Tabs.Trigger>
        <Tabs.Trigger value="join">
          <DownloadIcon data-icon="inline-start" />
          Join
        </Tabs.Trigger>
      </Tabs.List>

      <Tabs.Content value="create" class="pt-5">
        <Field.FieldGroup>
          <Field.Field>
            <Field.Label for="create-name">Drive name</Field.Label>
            <Input id="create-name" bind:value={name} disabled={busy} placeholder="alpha" autocomplete="off" spellcheck={false} />
            <Field.Description>1–58 characters: lowercase letters, digits, and dashes.</Field.Description>
          </Field.Field>

          <Field.Field>
            <Field.Label for="create-dir">Drive directory</Field.Label>
            <Input
              id="create-dir"
              bind:value={driveDir}
              oninput={() => (driveDirTouched = true)}
              disabled={busy}
              placeholder={suggestedDriveDir ?? "/drives/alpha"}
              autocomplete="off"
              spellcheck={false}
              class="font-mono text-xs"
            />
            <Field.Description>Absolute path. The keystore lives here.</Field.Description>
            {#if suggestedDriveDir !== null && driveDir !== suggestedDriveDir}
              <div class="flex flex-wrap items-center gap-2">
                <span class="min-w-0 truncate font-mono text-xs text-muted-foreground">
                  Suggested: {suggestedDriveDir}
                </span>
                <Button
                  variant="link"
                  size="xs"
                  disabled={busy}
                  onclick={() => {
                    driveDir = suggestedDriveDir;
                    driveDirTouched = false;
                  }}
                >
                  <SparklesIcon data-icon="inline-start" />
                  Use suggested path
                </Button>
              </div>
            {/if}
          </Field.Field>

          <Field.Field>
            <Field.Label for="relays" class="flex items-center gap-1.5">
              Relays
              <InfoHint label="About relays">
                <p>
                  Relays sync beyond this device. This form always runs managed, in vault service
                  mode: the drive's background process runs without a desktop file mount. At least
                  one relay is required, and an empty set is refused before anything is created —
                  that refusal reports verbatim below.
                </p>
                <p>
                  The field starts with Wyrd's suggested relays — keep them, edit the list, or
                  clear it entirely. The drive saves exactly what is listed.
                </p>
              </InfoHint>
            </Field.Label>
            <RelayEditor bind:relays disabled={busy} />
            <Field.Description>Suggested relays are pre-filled — edit or clear the list. Add at least one relay for a managed drive.</Field.Description>
            {#if relaysDiffer}
              <div class="flex flex-wrap items-center gap-2">
                <span class="min-w-0 truncate font-mono text-xs text-muted-foreground">
                  Suggested: {SUGGESTED_RELAYS.join(", ")}
                </span>
                <Button
                  variant="link"
                  size="xs"
                  disabled={busy}
                  onclick={() => {
                    relays = [...SUGGESTED_RELAYS];
                  }}
                >
                  <SparklesIcon data-icon="inline-start" />
                  Restore suggested relays
                </Button>
              </div>
            {/if}
          </Field.Field>

          {#if credentialMode === "store"}
            <Field.Field>
              <Field.Label>Store item</Field.Label>
              <p class="font-mono text-xs">{storeAccount}</p>
              <Field.Description>
                {#if seedsReady}
                  Seeded from this device's verified identity files. Converged re-runs
                  name only this item.
                {:else}
                  This device's verified identity is unavailable, so a store-backed
                  drive cannot be seeded yet. Fix the identity check first, or use
                  credential files.
                {/if}
              </Field.Description>
            </Field.Field>
          {:else}
            <Field.Field>
              <Field.Label for="create-identity" class="flex items-center gap-1.5">
                Identity file
                <InfoHint label="About the identity file">
                  <p>Path only — the file's contents never travel through this form.</p>
                </InfoHint>
              </Field.Label>
              <InputGroup.Root>
                <InputGroup.Input
                  id="create-identity"
                  bind:value={identityFile}
                  disabled={busy}
                  placeholder="/keys/identity"
                  autocomplete="off"
                  spellcheck={false}
                  class="font-mono text-xs"
                />
                <InputGroup.Addon align="inline-end">
                  <InputGroup.Button variant="secondary" size="xs" disabled={busy} onclick={() => void browse((p) => (identityFile = p), "Identity file")}>
                    <FolderOpenIcon data-icon="inline-start" />
                    Browse…
                  </InputGroup.Button>
                </InputGroup.Addon>
              </InputGroup.Root>
            </Field.Field>

            <Field.Field>
              <Field.Label for="create-passphrase" class="flex items-center gap-1.5">
                Passphrase file
                <InfoHint label="About the passphrase file">
                  <p>Path only — the file's contents never travel through this form.</p>
                </InfoHint>
              </Field.Label>
              <InputGroup.Root>
                <InputGroup.Input
                  id="create-passphrase"
                  bind:value={passphraseFile}
                  disabled={busy}
                  placeholder="/keys/passphrase"
                  autocomplete="off"
                  spellcheck={false}
                  class="font-mono text-xs"
                />
                <InputGroup.Addon align="inline-end">
                  <InputGroup.Button variant="secondary" size="xs" disabled={busy} onclick={() => void browse((p) => (passphraseFile = p), "Passphrase file")}>
                    <FolderOpenIcon data-icon="inline-start" />
                    Browse…
                  </InputGroup.Button>
                </InputGroup.Addon>
              </InputGroup.Root>
            </Field.Field>
          {/if}
        </Field.FieldGroup>

        <div class="mt-5 flex flex-wrap items-center justify-between gap-3 border-t border-border/60 pt-4">
          <p class="text-xs text-muted-foreground">
            {#if relaysEmpty}
              No relays — this managed form will refuse before creating anything.
            {:else}
              Vault service mode (platform default). Mount mode travels with the installer follow-up.
            {/if}
          </p>
          <Button
            disabled={busy || name.trim() === "" || driveDir.trim() === "" || !credentialsReady}
            onclick={() =>
              void run(() =>
                onCreate(
                  buildCreateArgs({
                    name,
                    driveDir,
                    relays,
                    credentialMode,
                    storeAccount,
                    seedIdentityFile: initialIdentityFile ?? "",
                    seedPassphraseFile: initialPassphraseFile ?? "",
                    fileIdentityFile: identityFile,
                    filePassphraseFile: passphraseFile,
                  }),
                ),
              )
            }
          >
            {#if working}
              <Spinner data-icon="inline-start" />
              Creating…
            {:else}
              <PlusIcon data-icon="inline-start" />
              Create drive
            {/if}
          </Button>
        </div>
      </Tabs.Content>

      <Tabs.Content value="join" class="pt-5">
        <Field.FieldGroup>
          <Field.Field>
            <Field.Label for="join-name">Drive name</Field.Label>
            <Input id="join-name" bind:value={name} disabled={busy} placeholder="alpha" autocomplete="off" spellcheck={false} />
            <Field.Description>1–58 characters: lowercase letters, digits, and dashes.</Field.Description>
          </Field.Field>

          <Field.Field>
            <Field.Label for="join-dir">Drive directory</Field.Label>
            <Input
              id="join-dir"
              bind:value={driveDir}
              oninput={() => (driveDirTouched = true)}
              disabled={busy}
              placeholder={suggestedDriveDir ?? "/drives/alpha"}
              autocomplete="off"
              spellcheck={false}
              class="font-mono text-xs"
            />
            <Field.Description>Absolute path. The keystore lives here.</Field.Description>
            {#if suggestedDriveDir !== null && driveDir !== suggestedDriveDir}
              <div class="flex flex-wrap items-center gap-2">
                <span class="min-w-0 truncate font-mono text-xs text-muted-foreground">
                  Suggested: {suggestedDriveDir}
                </span>
                <Button
                  variant="link"
                  size="xs"
                  disabled={busy}
                  onclick={() => {
                    driveDir = suggestedDriveDir;
                    driveDirTouched = false;
                  }}
                >
                  <SparklesIcon data-icon="inline-start" />
                  Use suggested path
                </Button>
              </div>
            {/if}
          </Field.Field>

          <Field.Field>
            <Field.Label for="join-relays" class="flex items-center gap-1.5">
              Relays
              <InfoHint label="About relays">
                <p>
                  Relays sync beyond this device. This form always runs managed, in vault service
                  mode: the drive's background process runs without a desktop file mount. At least
                  one relay is required, and an empty set is refused before anything is joined —
                  that refusal reports verbatim below.
                </p>
                <p>
                  The field starts with Wyrd's suggested relays — keep them, edit the list, or
                  clear it entirely. The drive saves exactly what is listed.
                </p>
              </InfoHint>
            </Field.Label>
            <RelayEditor id="join-relays" bind:relays disabled={busy} />
            <Field.Description>Suggested relays are pre-filled — edit or clear the list. Add at least one relay for a managed drive.</Field.Description>
            {#if relaysDiffer}
              <div class="flex flex-wrap items-center gap-2">
                <span class="min-w-0 truncate font-mono text-xs text-muted-foreground">
                  Suggested: {SUGGESTED_RELAYS.join(", ")}
                </span>
                <Button
                  variant="link"
                  size="xs"
                  disabled={busy}
                  onclick={() => {
                    relays = [...SUGGESTED_RELAYS];
                  }}
                >
                  <SparklesIcon data-icon="inline-start" />
                  Restore suggested relays
                </Button>
              </div>
            {/if}
          </Field.Field>

          <Field.Field>
            <Field.Label for="join-invitation">Invitation file</Field.Label>
            <Field.Description>
              A protected file that lets this device join a drive. Only the intended device can
              open it.
            </Field.Description>
            <InputGroup.Root>
              <InputGroup.Input
                id="join-invitation"
                bind:value={invitationFile}
                disabled={busy}
                placeholder="/tmp/invite.sealed"
                autocomplete="off"
                spellcheck={false}
                class="font-mono text-xs"
              />
              <InputGroup.Addon align="inline-end">
                <InputGroup.Button variant="secondary" size="xs" disabled={busy} onclick={() => void browse((p) => (invitationFile = p), "Invitation file")}>
                  <FolderOpenIcon data-icon="inline-start" />
                  Browse…
                </InputGroup.Button>
              </InputGroup.Addon>
            </InputGroup.Root>
          </Field.Field>

          <Field.Field>
            <Field.Label for="join-expected-owner">Expected owner (optional)</Field.Label>
            <Field.Description>
              64 hex characters naming the inviter you meant to trust. When set, the
              join refuses before touching anything if the invitation names someone else.
            </Field.Description>
            <InputGroup.Root>
              <InputGroup.Input
                id="join-expected-owner"
                bind:value={expectedOwner}
                disabled={busy}
                placeholder="64 hex characters, blank means unchecked"
                autocomplete="off"
                spellcheck={false}
                class="font-mono text-xs"
              />
            </InputGroup.Root>
          </Field.Field>

          <Field.Field>
            <Field.Label for="join-expected-drive">Expected drive (optional)</Field.Label>
            <Field.Description>
              64 hex characters naming the drive being joined. Refuses on mismatch.
            </Field.Description>
            <InputGroup.Root>
              <InputGroup.Input
                id="join-expected-drive"
                bind:value={expectedDrive}
                disabled={busy}
                placeholder="64 hex characters, blank means unchecked"
                autocomplete="off"
                spellcheck={false}
                class="font-mono text-xs"
              />
            </InputGroup.Root>
          </Field.Field>

          <Field.Field>
            <Field.Label for="join-expected-genesis">Expected genesis (optional)</Field.Label>
            <Field.Description>
              64 hex characters naming the genesis transition the invitation must
              carry. Refuses on mismatch.
            </Field.Description>
            <InputGroup.Root>
              <InputGroup.Input
                id="join-expected-genesis"
                bind:value={expectedGenesis}
                disabled={busy}
                placeholder="64 hex characters, blank means unchecked"
                autocomplete="off"
                spellcheck={false}
                class="font-mono text-xs"
              />
            </InputGroup.Root>
          </Field.Field>

          {#if credentialMode === "store"}
            <Field.Field>
              <Field.Label>Store item</Field.Label>
              <p class="font-mono text-xs">{storeAccount}</p>
              <Field.Description>
                {#if seedsReady}
                  Seeded from this device's verified identity files. Converged re-runs
                  name only this item.
                {:else}
                  This device's verified identity is unavailable, so a store-backed
                  drive cannot be seeded yet. Fix the identity check first, or use
                  credential files.
                {/if}
              </Field.Description>
            </Field.Field>
          {:else}
            <Field.Field>
              <Field.Label for="join-identity" class="flex items-center gap-1.5">
                Identity file
                <InfoHint label="About the identity file">
                  <p>Path only — the file's contents never travel through this form.</p>
                </InfoHint>
              </Field.Label>
              <InputGroup.Root>
                <InputGroup.Input
                  id="join-identity"
                  bind:value={identityFile}
                  disabled={busy}
                  placeholder="/keys/identity"
                  autocomplete="off"
                  spellcheck={false}
                  class="font-mono text-xs"
                />
                <InputGroup.Addon align="inline-end">
                  <InputGroup.Button variant="secondary" size="xs" disabled={busy} onclick={() => void browse((p) => (identityFile = p), "Identity file")}>
                    <FolderOpenIcon data-icon="inline-start" />
                    Browse…
                  </InputGroup.Button>
                </InputGroup.Addon>
              </InputGroup.Root>
            </Field.Field>

            <Field.Field>
              <Field.Label for="join-passphrase" class="flex items-center gap-1.5">
                Passphrase file
                <InfoHint label="About the passphrase file">
                  <p>Path only — the file's contents never travel through this form.</p>
                </InfoHint>
              </Field.Label>
              <InputGroup.Root>
                <InputGroup.Input
                  id="join-passphrase"
                  bind:value={passphraseFile}
                  disabled={busy}
                  placeholder="/keys/passphrase"
                  autocomplete="off"
                  spellcheck={false}
                  class="font-mono text-xs"
                />
                <InputGroup.Addon align="inline-end">
                  <InputGroup.Button variant="secondary" size="xs" disabled={busy} onclick={() => void browse((p) => (passphraseFile = p), "Passphrase file")}>
                    <FolderOpenIcon data-icon="inline-start" />
                    Browse…
                  </InputGroup.Button>
                </InputGroup.Addon>
              </InputGroup.Root>
            </Field.Field>
          {/if}
        </Field.FieldGroup>

        <div class="mt-5 flex flex-wrap items-center justify-between gap-3 border-t border-border/60 pt-4">
          <p class="text-xs text-muted-foreground">
            {#if relaysEmpty}
              No relays — this managed form will refuse before joining anything.
            {:else}
              Vault service mode (platform default). Mount mode travels with the installer follow-up.
            {/if}
          </p>
          <Button
            disabled={busy || name.trim() === "" || driveDir.trim() === "" || invitationFile.trim() === "" || !credentialsReady || !expectationsValid}
            onclick={() =>
              void run(() =>
                onJoin(
                  buildJoinArgs({
                    name,
                    driveDir,
                    relays,
                    credentialMode,
                    storeAccount,
                    seedIdentityFile: initialIdentityFile ?? "",
                    seedPassphraseFile: initialPassphraseFile ?? "",
                    fileIdentityFile: identityFile,
                    filePassphraseFile: passphraseFile,
                    invitationFile,
                    expectedOwner,
                    expectedDrive,
                    expectedGenesis,
                  }),
                ),
              )
            }
          >
            {#if working}
              <Spinner data-icon="inline-start" />
              Joining…
            {:else}
              <DownloadIcon data-icon="inline-start" />
              Join drive
            {/if}
          </Button>
        </div>
      </Tabs.Content>
    </Tabs.Root>

    {#if error !== null}
      <Alert.Root variant="destructive" class="mt-4">
        <CircleAlertIcon />
        <Alert.Title>Operation failed</Alert.Title>
        <Alert.Description>{error}</Alert.Description>
      </Alert.Root>
    {/if}

    {#if report !== null}
      <Alert.Root class="mt-4 border-primary/30 bg-primary/5">
        <TerminalIcon />
        <Alert.Title>Provisioning report</Alert.Title>
        <Alert.Description class="space-y-1 text-foreground/80 [&_p:not(:last-child)]:mb-1">
          <p>{describeProvisionResult(report)}</p>
          <p>{observedRegistrySummary(report)}</p>
          <p>{membershipSummary(report)}</p>
        </Alert.Description>
      </Alert.Root>
    {/if}
  </Card.Content>
  {#if report !== null}
    <Card.Footer class="justify-end">
      <Button variant="outline" size="sm" disabled={busy} onclick={onBack}>
        <ArrowLeftIcon data-icon="inline-start" />
        Back to drives
      </Button>
    </Card.Footer>
  {/if}
</Card.Root>
