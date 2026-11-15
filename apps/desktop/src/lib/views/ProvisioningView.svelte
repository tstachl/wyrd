<!-- Runtime provisioning: create, join, and leave through the
constrained CLI adapter. Every operation returns the outcome plus
the post-run observation; observed state decides the display, the
outcome only indicates what the operation reported. Secret
contents never travel — credential and invitation inputs are paths
only, picked natively where the dialog capability answers and
entered by hand elsewhere. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Field from "$lib/components/ui/field/index.js";
  import * as Tabs from "$lib/components/ui/tabs/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import * as InputGroup from "$lib/components/ui/input-group/index.js";
  import { Input } from "$lib/components/ui/input/index.js";
  import { Checkbox } from "$lib/components/ui/checkbox/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import RelayEditor from "$lib/components/relay-editor.svelte";
  import InfoHint from "$lib/components/info-hint.svelte";
  import type { ProvisionReport } from "$lib/ipc/results.js";
  import { pickExistingFile } from "$lib/ipc/files.js";
  import type { ProvisionCreateArgs, ProvisionJoinArgs, ProvisionLeaveArgs } from "$lib/ipc/operations.js";
  import { nextDriveDir, suggestedDriveDirFor, withDevCliHint } from "./format.js";
  import PlusIcon from "@lucide/svelte/icons/plus";
  import DownloadIcon from "@lucide/svelte/icons/download";
  import LogOutIcon from "@lucide/svelte/icons/log-out";
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
    onLeave: (args: ProvisionLeaveArgs) => Promise<ProvisionReport>;
    onDone: () => void;
  }

  let { pending, initialIdentityFile, initialPassphraseFile, initialMode, onCreate, onJoin, onLeave, onDone }: Props = $props();

  type Mode = "create" | "join" | "leave";

  let mode = $state<Mode>("create");
  let name = $state("");
  let driveDir = $state("");
  let driveDirTouched = $state(false);
  let relays = $state<string[]>([]);
  let identityFile = $state("");
  let passphraseFile = $state("");
  let invitationFile = $state("");
  // Pre-fill from onboarding (first create goes through the
  // unchanged constrained CLI adapter with committed credentials).
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
  let acknowledged = $state(false);
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

  // The suggestion decision as a pure function of its inputs so it
  // pins in `format.test.ts` beside the derivation: while untouched
  // (and not leaving) follow the suggestion; entering Leave with an
  // untouched field clears it so Leave never targets a guess;
  // otherwise keep the operator's value (including an explicit
  // clear, which marks the field touched).
  $effect(() => {
    driveDir = nextDriveDir(mode, driveDirTouched, driveDir, suggestedDriveDir);
  });

  // Empty relays are refused before anything is created: this form
  // always runs managed (`noService: false`), so the CLI's
  // managed-service refusal is the honest outcome and the detail
  // below reports it verbatim. There is no local-only mode in this
  // form and no product default relay set ships in the repo — the
  // field starts empty with the example placeholder as a shape hint.
  const relaysEmpty = $derived(relays.length === 0);

  function describeResult(value: ProvisionReport): string {
    const result = value.result;
    switch (result.result) {
      case "completed":
        return `Exit 0 — the CLI reported no error. Only the observation below says what happened: ${result.detail}`;
      case "expectedfailure":
        return `Exit 2 — expected failure. Only the observation below says what actually exists: ${result.detail}`;
      case "helddrive":
        return `Held drive (another process holds it — not retried): ${result.detail}`;
      case "versionmismatch":
        return `Version mismatch (reinstall the matched pair): ${withDevCliHint(result.reason)}`;
      case "busy":
        return `Another operation is already in flight on ${result.drive_dir} — not queued.`;
      case "unknown":
        return `Outcome unknown: ${result.detail}. The observation below decides.`;
    }
  }

  function describeMembership(value: ProvisionReport): string {
    const membership = value.membership;
    if (membership === null) return "Membership: unknown (the owner did not answer).";
    const epoch = membership.epoch === null ? "no tip yet" : `epoch ${membership.epoch}`;
    return `Membership: ${membership.known_members} member(s), ${epoch}.`;
  }

  function observedSummary(value: ProvisionReport): string {
    const observed = value.observed;
    if (observed.status !== "ready") return `Registry: ${observed.status}.`;
    const names = observed.drives.map((d) => d.name).join(", ");
    return `Registry ready: ${observed.drives.length} drive(s)${names.length > 0 ? ` (${names})` : ""}.`;
  }

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
    if (next !== "leave") acknowledged = false;
  }
</script>

<Card.Root id="provisioning" class="scroll-mt-24">
  <Card.Header>
    <Card.Title>Add or leave a drive</Card.Title>
    <Card.Description>
      Create a drive on this device, join one from a sealed invitation, or leave one this device
      already holds.
    </Card.Description>
  </Card.Header>
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
        <Tabs.Trigger value="leave">
          <LogOutIcon data-icon="inline-start" />
          Leave
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
                  Relays sync beyond this device. This form always runs managed (vault): at least
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
              Service mode: vault (platform default). Mount mode travels with the installer follow-up.
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
                  Relays sync beyond this device. This form always runs managed (vault): at least
                  one relay is required, and an empty set is refused before anything is joined —
                  that refusal reports verbatim below.
                </p>
              </InfoHint>
            </Field.Label>
            <RelayEditor id="join-relays" bind:relays disabled={busy} />
            <Field.Description>Add at least one relay for a managed drive.</Field.Description>
          </Field.Field>

          <Field.Field>
            <Field.Label for="join-invitation">Sealed invitation file</Field.Label>
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
                <InputGroup.Button variant="secondary" size="xs" disabled={busy} onclick={() => void browse((p) => (invitationFile = p), "Sealed invitation file")}>
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
              Service mode: vault (platform default). Mount mode travels with the installer follow-up.
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

      <Tabs.Content value="leave" class="pt-5">
        <Field.FieldGroup>
          <Field.Field>
            <Field.Label for="leave-dir">Drive directory</Field.Label>
            <Input
              id="leave-dir"
              bind:value={driveDir}
              oninput={() => (driveDirTouched = true)}
              disabled={busy}
              placeholder="/drives/alpha"
              autocomplete="off"
              spellcheck={false}
              class="font-mono text-xs"
            />
            <Field.Description>The exact directory this device already holds.</Field.Description>
          </Field.Field>

          <Field.Field>
            <Field.Label for="leave-identity" class="flex items-center gap-1.5">
              Identity file
              <InfoHint label="About the identity file">
                <p>Path only — the file's contents never travel through this form.</p>
              </InfoHint>
            </Field.Label>
            <InputGroup.Root>
              <InputGroup.Input
                id="leave-identity"
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
            <Field.Label for="leave-passphrase" class="flex items-center gap-1.5">
              Passphrase file
              <InfoHint label="About the passphrase file">
                <p>Path only — the file's contents never travel through this form.</p>
              </InfoHint>
            </Field.Label>
            <InputGroup.Root>
              <InputGroup.Input
                id="leave-passphrase"
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

          <Field.Field orientation="horizontal" class="rounded-xl border border-destructive/30 bg-destructive/5 p-3">
            <Checkbox id="leave-acknowledged" bind:checked={acknowledged} disabled={busy} />
            <Field.Content>
              <Field.Label for="leave-acknowledged" class="leading-relaxed font-normal">
                I understand: leaving as the sole owner is terminal — the drive freezes permanently
                and no future transition can ever be authorized.
              </Field.Label>
            </Field.Content>
          </Field.Field>
        </Field.FieldGroup>

        <div class="mt-5 flex flex-wrap items-center justify-between gap-3 border-t border-border/60 pt-4">
          <p class="text-xs text-muted-foreground">This removes this device from the drive's membership.</p>
          <Button
            variant="destructive"
            disabled={busy || driveDir.trim() === "" || !credentialsReady || !acknowledged}
            onclick={() => void run(() => onLeave({ driveDir, identityFile, passphraseFile, acknowledged }))}
          >
            {#if working}
              <Spinner data-icon="inline-start" />
              Leaving…
            {:else}
              <LogOutIcon data-icon="inline-start" />
              Leave drive
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
          <p>{describeResult(report)}</p>
          <p>{observedSummary(report)}</p>
          <p>{describeMembership(report)}</p>
        </Alert.Description>
      </Alert.Root>
    {/if}
  </Card.Content>
</Card.Root>
