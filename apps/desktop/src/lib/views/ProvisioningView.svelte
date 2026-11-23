<!-- Dedicated Add drive page: create on this device or join with an
invitation through the constrained CLI adapter. Every operation
returns the outcome plus the post-run observation; observed state
decides the display, the outcome only indicates what the operation
reported. A provision that both reported no error and is confirmed
by the observation (the registry names the new drive) resolves
null — the caller navigates off this form, so no success report
renders here. Anything else resolves the report, which renders
below with the entered values kept for fix-and-retry. Leaving a drive lives in that drive's Settings — a
destructive membership action does not belong in an add flow.
The decision (create or join) is the first surface; the default
path is name plus relays, and every operator setting — directory,
credential backend, store entry, credential files and join pins —
sits behind one Advanced disclosure. New drives use the OS
credential store by default, its entry derived from the name and
initialized from this device's verified identity; credential files
stay for operator deployments. Invitation and folder inputs are
paths only, picked natively where the dialog capability answers
and entered by hand elsewhere. Secret contents never travel. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Field from "$lib/components/ui/field/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import * as InputGroup from "$lib/components/ui/input-group/index.js";
  import * as ToggleGroup from "$lib/components/ui/toggle-group/index.js";
  import { Input } from "$lib/components/ui/input/index.js";
  import { Separator } from "$lib/components/ui/separator/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import PageHeader from "$lib/components/page-header.svelte";
  import RelayEditor from "$lib/components/relay-editor.svelte";
  import InfoHint from "$lib/components/info-hint.svelte";
  import CredentialStorageToggle from "./credential-storage-toggle.svelte";
  import type { ProvisionReport } from "$lib/ipc/results.js";
  import { pickExistingDirectory, pickExistingFile } from "$lib/ipc/files.js";
  import type { ProvisionCreateArgs, ProvisionJoinArgs } from "$lib/ipc/operations.js";
  import {
    buildCreateArgs,
    buildJoinArgs,
    type CredentialMode,
  } from "$lib/ipc/provisioning-args.js";
  import {
    describeProvisionResult,
    driveNameValid,
    membershipSummary,
    nextDriveDir,
    normalizeDriveName,
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
  // The toggle group clears a single selection when the active card is
  // clicked again. Pin its value to the form mode so a second click
  // re-affirms the choice instead of leaving neither card pressed.
  // svelte-ignore state_referenced_locally
  let selectedMode = $state<Mode | "">(mode);
  $effect(() => {
    selectedMode = mode;
  });
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

  async function browseDirectory(set: (path: string) => void): Promise<void> {
    const picked = await pickExistingDirectory("Drive directory");
    if (picked !== null) set(picked);
  }

  /** Format as the user types; the rule is enforced, never explained. */
  function onNameInput(event: Event): void {
    name = normalizeDriveName((event.currentTarget as HTMLInputElement).value);
  }

  /** Why the primary action is unavailable, in the order the person fixes it. */
  const createBlockers = $derived(
    [
      name.trim() === "" ? "Enter a drive name" : null,
      !driveNameValid(name) ? "Use a name of up to 58 lowercase letters, digits or dashes" : null,
      relaysEmpty ? "Add at least one relay" : null,
      driveDir.trim() === "" ? "Choose a drive directory under Advanced" : null,
      !credentialsReady
        ? credentialMode === "store"
          ? "This device's identity is unavailable — finish the identity check first"
          : "Add the identity and passphrase file paths"
        : null,
    ].filter((reason): reason is string => reason !== null),
  );
  const joinBlockers = $derived([
    ...createBlockers,
    invitationFile.trim() === "" ? "Choose an invitation file" : null,
    !expectationsValid ? "Check the optional expected values — each is 64 hex characters" : null,
  ]);

  function switchMode(next: Mode): void {
    mode = next;
    modeTouched = true;
    report = null;
    error = null;
  }

  function onModeChange(next: string): void {
    if (next === "create" || next === "join") {
      switchMode(next);
    } else {
      // Second click on the active card: the group cleared itself.
      selectedMode = mode;
    }
  }
</script>

<PageHeader
  title="Add drive"
  description="Create a drive on this device, or join one with an invitation. It appears in your drives once setup finishes."
>
  {#snippet actions()}
    <Button variant="outline" size="sm" onclick={onBack}>
      <ArrowLeftIcon data-icon="inline-start" />
      Back to drives
    </Button>
  {/snippet}
</PageHeader>

<Card.Root class="mt-6">
  <Card.Content class="flex flex-col gap-5">
    <!-- The decision comes first. These two cards only choose a path;
      every operator setting sits in Advanced below. ToggleGroup names
      the active choice without relying on colour alone. -->
    <ToggleGroup.Root
      type="single"
      bind:value={selectedMode}
      variant="outline"
      aria-label="Choose what to do"
      class="grid w-full grid-cols-1 gap-3 sm:grid-cols-2"
      onValueChange={onModeChange}
    >
      <ToggleGroup.Item
        value="create"
        class="h-auto w-full flex-col items-start gap-1 rounded-xl p-4 text-left whitespace-normal"
      >
        <span class="flex items-center gap-2 text-base font-medium">
          <PlusIcon data-icon="inline-start" />
          Create a new drive
        </span>
        <span class="mt-1 block text-xs leading-relaxed text-muted-foreground">
          Start a fresh drive on this device and choose its relays.
        </span>
      </ToggleGroup.Item>
      <ToggleGroup.Item
        value="join"
        class="h-auto w-full flex-col items-start gap-1 rounded-xl p-4 text-left whitespace-normal"
      >
        <span class="flex items-center gap-2 text-base font-medium">
          <DownloadIcon data-icon="inline-start" />
          Join with an invitation
        </span>
        <span class="mt-1 block text-xs leading-relaxed text-muted-foreground">
          Use an invitation from someone who already shares a drive.
        </span>
      </ToggleGroup.Item>
    </ToggleGroup.Root>

    {#if mode === "create"}
      <Field.FieldGroup class="gap-4">
          <Field.Field>
            <Field.Label for="create-name">Drive name</Field.Label>
            <Input id="create-name" value={name} oninput={onNameInput} disabled={busy} placeholder="alpha" autocomplete="off" spellcheck={false} />
          </Field.Field>

          <Field.Field>
            <Field.Label for="create-relays" class="flex items-center gap-1.5">
              Relays
              <InfoHint label="About relays">
                <p>
                  The field starts with Wyrd's suggested relays — keep them, edit the list, or
                  clear it entirely. At least one relay is needed.
                </p>
              </InfoHint>
            </Field.Label>
            <RelayEditor id="create-relays" bind:relays disabled={busy} />
            <Field.Description>
              Relays carry sync messages between devices. Wyrd saves the relays you list here;
              a relay accepting a message does not mean another device received it. Suggested
              relays are pre-filled — edit or clear the list. The drive saves exactly what is listed.
            </Field.Description>
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
      </Field.FieldGroup>

        <Separator class="my-4" />
        <div class="flex flex-wrap items-center justify-between gap-3">
          {#if createBlockers.length > 0}
            <p class="text-xs leading-relaxed text-muted-foreground">
              To create a drive: {createBlockers.join(" · ")}.
            </p>
          {/if}
          <Button
            disabled={busy || createBlockers.length > 0}
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
    {:else}
      <Field.FieldGroup class="gap-4">
          <Field.Field>
            <Field.Label for="join-name">Drive name</Field.Label>
            <Input id="join-name" value={name} oninput={onNameInput} disabled={busy} placeholder="alpha" autocomplete="off" spellcheck={false} />
          </Field.Field>

          <Field.Field>
            <Field.Label for="join-relays" class="flex items-center gap-1.5">
              Relays
              <InfoHint label="About relays">
                <p>
                  The field starts with Wyrd's suggested relays — keep them, edit the list, or
                  clear it entirely. At least one relay is needed.
                </p>
              </InfoHint>
            </Field.Label>
            <RelayEditor id="join-relays" bind:relays disabled={busy} />
            <Field.Description>
              Relays carry sync messages between devices. Wyrd saves the relays you list here;
              a relay accepting a message does not mean another device received it. Suggested
              relays are pre-filled — edit or clear the list. The drive saves exactly what is listed.
            </Field.Description>
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
      </Field.FieldGroup>

        <Separator class="my-4" />
        <div class="flex flex-wrap items-center justify-between gap-3">
          {#if joinBlockers.length > 0}
            <p class="text-xs leading-relaxed text-muted-foreground">
              To join a drive: {joinBlockers.join(" · ")}.
            </p>
          {/if}
          <Button
            disabled={busy || joinBlockers.length > 0}
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
    {/if}

    <!-- Operator settings sit behind one disclosure. The default
      path is name plus relays; nothing here is needed for either
      action to work. -->
    <details class="rounded-lg border border-border/60 px-3 py-2">
      <summary class="cursor-pointer text-sm font-medium">Advanced</summary>
      <Field.FieldGroup class="mt-4 gap-4">
        <Field.Field>
          <Field.Label for="advanced-dir">Drive directory</Field.Label>
          <InputGroup.Root>
            <InputGroup.Input
              id="advanced-dir"
              value={driveDir}
              oninput={(event) => {
                driveDir = event.currentTarget.value;
                driveDirTouched = true;
              }}
              disabled={busy}
              placeholder={suggestedDriveDir ?? "/drives/alpha"}
              autocomplete="off"
              spellcheck={false}
              class="font-mono text-xs"
            />
            <InputGroup.Addon align="inline-end">
              <InputGroup.Button variant="secondary" size="xs" disabled={busy} onclick={() => void browseDirectory((picked) => { driveDir = picked; driveDirTouched = true; })}>
                <FolderOpenIcon data-icon="inline-start" />
                Choose folder…
              </InputGroup.Button>
            </InputGroup.Addon>
          </InputGroup.Root>
          <Field.Description>Where Wyrd keeps this drive's local data on this device.</Field.Description>
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

        <!-- Extracted child: sibling interactive templates inline here
          break happy-dom's re-registration across mounts. -->
        <CredentialStorageToggle
          useStore={credentialMode === "store"}
          onSelect={(next) => (credentialMode = next ? "store" : "file")}
          storeLabel="OS credential store (recommended)"
        />
        {#if credentialMode === "store"}
          <p class="text-xs leading-relaxed text-muted-foreground">
            New drives keep their credentials in the OS credential store under
            <span class="font-mono text-xs">{storeAccount}</span>. The drive starts at
            login without typing anything, and no passphrase file is created for the
            drive service. It uses this device's verified identity — no file choosing needed.
          </p>
          <Field.Field>
            <Field.Label>Credential entry</Field.Label>
            <p class="font-mono text-xs">{storeAccount}</p>
            <Field.Description>
              {#if seedsReady}
                Wyrd creates one entry per drive from this device's verified identity.
              {:else}
                This device's verified identity is unavailable, so a store-backed
                drive cannot be set up yet. Fix the identity check first, or use
                credential files.
              {/if}
            </Field.Description>
          </Field.Field>
        {:else}
          <p class="text-xs leading-relaxed text-muted-foreground">
            Operator deployments only: pick the credential files the drive reads. The
            drive service will name these paths.
          </p>
          <Field.Field>
            <Field.Label for="advanced-identity" class="flex items-center gap-1.5">
              Identity file
              <InfoHint label="About the identity file">
                <p>Path only — the file's contents never travel through this form.</p>
              </InfoHint>
            </Field.Label>
            <InputGroup.Root>
              <InputGroup.Input
                id="advanced-identity"
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
            <Field.Label for="advanced-passphrase" class="flex items-center gap-1.5">
              Passphrase file
              <InfoHint label="About the passphrase file">
                <p>Path only — the file's contents never travel through this form.</p>
              </InfoHint>
            </Field.Label>
            <InputGroup.Root>
              <InputGroup.Input
                id="advanced-passphrase"
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

        {#if mode === "join"}
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
        {/if}
      </Field.FieldGroup>
    </details>

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
        <Alert.Description class="flex flex-col gap-1 text-foreground/80">
          <p>{describeProvisionResult(report)}</p>
          <p>{observedRegistrySummary(report)}</p>
          <p>{membershipSummary(report)}</p>
        </Alert.Description>
      </Alert.Root>
    {/if}
  </Card.Content>
</Card.Root>
