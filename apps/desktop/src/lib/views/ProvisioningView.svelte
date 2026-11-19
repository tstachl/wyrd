<!-- Dedicated Add drive page: create on this device or join with an
invitation through the constrained CLI adapter. Every operation
returns the outcome plus the post-run observation; observed state
decides the display, the outcome only indicates what the operation
reported. Leaving a drive lives in that drive's Settings — a
destructive membership action does not belong in an add flow. Secret
contents never travel: credential and invitation inputs are paths
only, picked natively where the dialog capability answers and entered
by hand elsewhere. -->
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
  import type { ProvisionReport } from "$lib/ipc/results.js";
  import { pickExistingFile } from "$lib/ipc/files.js";
  import type { ProvisionCreateArgs, ProvisionJoinArgs } from "$lib/ipc/operations.js";
  import {
    describeProvisionResult,
    membershipSummary,
    nextDriveDir,
    observedRegistrySummary,
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
    onCreate: (args: ProvisionCreateArgs) => Promise<ProvisionReport>;
    onJoin: (args: ProvisionJoinArgs) => Promise<ProvisionReport>;
    /** Registry re-read after every operation; observed state decides. */
    onDone: () => void;
    /** Back to the drive list. */
    onBack: () => void;
  }

  let { pending, initialIdentityFile, initialPassphraseFile, initialMode, onCreate, onJoin, onDone, onBack }: Props = $props();

  type Mode = "create" | "join";

  let mode = $state<Mode>("create");
  let name = $state("");
  let driveDir = $state("");
  let driveDirTouched = $state(false);
  let relays = $state<string[]>([]);
  let identityFile = $state("");
  let passphraseFile = $state("");
  let invitationFile = $state("");
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
  const credentialsReady = $derived(identityFile.trim() !== "" && passphraseFile.trim() !== "");

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
  // form and no product default relay set ships in the repo — the
  // field starts empty with the example placeholder as a shape hint.
  const relaysEmpty = $derived(relays.length === 0);

  async function run(call: () => Promise<ProvisionReport>): Promise<void> {
    working = true;
    error = null;
    try {
      report = await call();
      onDone();
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
                  No product default relay set ships in the repo, so the field starts empty. The
                  example placeholder is a shape hint, not a suggestion to use.
                </p>
              </InfoHint>
            </Field.Label>
            <RelayEditor bind:relays disabled={busy} />
            <Field.Description>Add at least one relay for a managed drive.</Field.Description>
          </Field.Field>

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
                onCreate({ name, driveDir, relays, identityFile, passphraseFile, mode: "vault", noService: false }),
              )}
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
              </InfoHint>
            </Field.Label>
            <RelayEditor id="join-relays" bind:relays disabled={busy} />
            <Field.Description>Add at least one relay for a managed drive.</Field.Description>
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
            disabled={busy || name.trim() === "" || driveDir.trim() === "" || invitationFile.trim() === "" || !credentialsReady}
            onclick={() =>
              void run(() =>
                onJoin({
                  name,
                  driveDir,
                  relays,
                  identityFile,
                  passphraseFile,
                  invitationFile,
                  mode: "vault",
                  noService: false,
                }),
              )}
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
