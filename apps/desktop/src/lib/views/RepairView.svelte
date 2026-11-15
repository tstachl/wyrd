<!-- Broken-credential Repair plus broken-pairing Repair: two causes,
distinct states, one screen. The credential state names the identity
files and offers file choosing or a new identity; the pairing state
(a version-mismatched or unrunnable helper) names neither — neither
choosing files nor creating an identity can fix a pairing, so those
sections stay hidden there and only Verify again and the technical
detail remain. Human-first copy up front; raw CLI text rides only
inside the expandable technical detail (raw credentials.rs strings
never reach the renderer as the headline). A new identity from here
requires explicit destructive confirmation: the old file is preserved
where it is, never overwritten, and the consequence is stated plainly. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import * as Field from "$lib/components/ui/field/index.js";
  import * as InputGroup from "$lib/components/ui/input-group/index.js";
  import * as Collapsible from "$lib/components/ui/collapsible/index.js";
  import * as AlertDialog from "$lib/components/ui/alert-dialog/index.js";
  import PageHeader from "$lib/components/page-header.svelte";
  import DetailRow from "$lib/components/detail-row.svelte";
  import CopyButton from "$lib/components/copy-button.svelte";
  import type { KeyDefaults, OnboardingReport } from "$lib/ipc/results.js";
  import { pickExistingFile } from "$lib/ipc/files.js";
  import ShieldAlertIcon from "@lucide/svelte/icons/shield-alert";
  import CircleAlertIcon from "@lucide/svelte/icons/circle-alert";
  import ChevronDownIcon from "@lucide/svelte/icons/chevron-down";
  import FolderOpenIcon from "@lucide/svelte/icons/folder-open";
  import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
  import PlusIcon from "@lucide/svelte/icons/plus";

  interface Props {
    defaults: KeyDefaults;
    detail: string;
    kind?: "credentials" | "versionmismatch";
    pending: boolean;
    onVerify: (args: { identityFile: string; passphraseFile: string }) => Promise<OnboardingReport>;
    onAdopt: (args: { identityFile: string; passphraseFile: string }) => Promise<OnboardingReport>;
    onAdopted: (identityFile: string, passphraseFile: string) => void;
    onCreateNew: () => void;
  }

  let { defaults, detail, kind = "credentials", pending, onVerify, onAdopt, onAdopted, onCreateNew }: Props = $props();

  let pickedIdentity = $state("");
  let pickedPassphrase = $state("");
  let working = $state(false);
  let helper: string | null = $state(null);
  let showTechnical = $state(false);
  let confirmNew = $state(false);

  const busy = $derived(pending || working);

  async function browse(set: (path: string) => void, title: string): Promise<void> {
    const picked = await pickExistingFile(title);
    if (picked !== null) set(picked);
  }

  async function verifyAgain(): Promise<void> {
    working = true;
    helper = null;
    try {
      const report = await onVerify({ identityFile: defaults.identity_file, passphraseFile: defaults.passphrase_file });
      // The pairing path re-runs the version gate, not a credential
      // read: success means the pair answers, never that files are
      // readable.
      helper = report.result.result === "completed"
        ? kind === "versionmismatch"
          ? "The pair answers now — restart the app to continue."
          : "The files are readable now — restart the app to continue."
        : kind === "versionmismatch"
          ? "Still failing — the detail above says what the check reported."
          : "Still unreadable — the detail above says what the check reported.";
    } catch (failure: unknown) {
      helper = failure instanceof Error ? failure.message : String(failure);
    } finally {
      working = false;
    }
  }

  async function usePicked(): Promise<void> {
    if (pickedIdentity.trim() === "" || pickedPassphrase.trim() === "") {
      helper = "Choose both files before continuing.";
      return;
    }
    working = true;
    helper = null;
    try {
      // Adopted on success: without the record the gate would
      // re-enter Repair against the old defaults next launch.
      const report = await onAdopt({ identityFile: pickedIdentity, passphraseFile: pickedPassphrase });
      if (report.result.result === "completed") {
        onAdopted(pickedIdentity, pickedPassphrase);
      } else {
        helper = "Those files were not adopted — the detail above says what the check reported.";
      }
    } catch (failure: unknown) {
      helper = failure instanceof Error ? failure.message : String(failure);
    } finally {
      working = false;
    }
  }
</script>

{#if kind === "versionmismatch"}
  <PageHeader
    title="The app and its helper tool do not match"
    description="The desktop and its paired wyrd helper do not match: either the helper could not be run, or it reported a different build. This says nothing about your identity files, and your drives are untouched — nothing was changed. Reinstall the matched pair, then verify again. In a development checkout that means building the CLI beside the desktop binary; the technical detail below names the exact failure."
  />
{:else}
  <PageHeader
    title="Fix this device's identity"
    description="Wyrd cannot use this identity file: other users on this computer could access it, or it is damaged. Your drives are untouched — nothing was changed."
  />
{/if}

<div class="mt-6 flex flex-col gap-4">
  <Card.Root>
    <Card.Header>
      {#if kind === "versionmismatch"}
        <Card.Title>Helper pairing</Card.Title>
        <Card.Description>The paired helper must answer with the same build as the desktop.</Card.Description>
      {:else}
        <Card.Title>Expected files</Card.Title>
        <Card.Description>The two files this device needs before it can use any drive.</Card.Description>
      {/if}
    </Card.Header>
    <Card.Content>
      {#if kind === "credentials"}
        <dl>
          <DetailRow label="Identity file">
            <span class="inline-flex min-w-0 items-center gap-1.5">
              <span class="truncate font-mono text-xs">{defaults.identity_file}</span>
              <CopyButton text={defaults.identity_file} label="identity file path" />
            </span>
          </DetailRow>
          <DetailRow label="Passphrase file">
            <span class="inline-flex min-w-0 items-center gap-1.5">
              <span class="truncate font-mono text-xs">{defaults.passphrase_file}</span>
              <CopyButton text={defaults.passphrase_file} label="passphrase file path" />
            </span>
          </DetailRow>
        </dl>
      {/if}

      <Collapsible.Root bind:open={showTechnical}>
        <Collapsible.Trigger>
          {#snippet child({ props })}
            <Button {...props} variant="ghost" size="sm" class="-ml-2 mt-3 px-2 text-muted-foreground">
              {showTechnical ? "Hide technical detail" : "Show technical detail"}
              <ChevronDownIcon
                data-icon="inline-end"
                class={showTechnical ? "rotate-180 transition-transform" : "transition-transform"}
              />
            </Button>
          {/snippet}
        </Collapsible.Trigger>
        <Collapsible.Content>
          <pre class="mt-2 overflow-auto rounded-lg border border-border bg-ink px-3 py-2 text-xs break-words whitespace-pre-wrap text-bone">{detail}</pre>
        </Collapsible.Content>
      </Collapsible.Root>
    </Card.Content>
    <Card.Footer class="justify-between gap-3">
      <p class="text-xs text-muted-foreground">
        {kind === "versionmismatch"
          ? "Verify again re-runs the version gate; it never changes a file."
          : "The check reports what it found; it never modifies the files."}
      </p>
      <Button variant="outline" size="sm" disabled={busy} onclick={() => void verifyAgain()}>
        {#if working}
          Checking…
        {:else}
          <RefreshCwIcon data-icon="inline-start" />
          Verify again
        {/if}
      </Button>
    </Card.Footer>
  </Card.Root>

  {#if kind === "credentials"}
    <Card.Root>
      <Card.Header>
        <Card.Title>Choose a different file</Card.Title>
        <Card.Description>Point this device at the identity files you want it to use.</Card.Description>
      </Card.Header>
      <Card.Content>
        <Field.FieldGroup>
          <Field.Field>
            <Field.Label for="repair-identity">Identity file</Field.Label>
            <InputGroup.Root>
              <InputGroup.Input
                id="repair-identity"
                bind:value={pickedIdentity}
                disabled={busy}
                placeholder={defaults.identity_file}
                autocomplete="off"
                spellcheck={false}
                class="font-mono text-xs"
              />
              <InputGroup.Addon align="inline-end">
                <InputGroup.Button variant="secondary" size="xs" disabled={busy} onclick={() => void browse((p) => (pickedIdentity = p), "Identity file")}>
                  <FolderOpenIcon data-icon="inline-start" />
                  Browse…
                </InputGroup.Button>
              </InputGroup.Addon>
            </InputGroup.Root>
          </Field.Field>
          <Field.Field>
            <Field.Label for="repair-passphrase">Passphrase file</Field.Label>
            <InputGroup.Root>
              <InputGroup.Input
                id="repair-passphrase"
                bind:value={pickedPassphrase}
                disabled={busy}
                placeholder={defaults.passphrase_file}
                autocomplete="off"
                spellcheck={false}
                class="font-mono text-xs"
              />
              <InputGroup.Addon align="inline-end">
                <InputGroup.Button variant="secondary" size="xs" disabled={busy} onclick={() => void browse((p) => (pickedPassphrase = p), "Passphrase file")}>
                  <FolderOpenIcon data-icon="inline-start" />
                  Browse…
                </InputGroup.Button>
              </InputGroup.Addon>
            </InputGroup.Root>
          </Field.Field>
        </Field.FieldGroup>
      </Card.Content>
      <Card.Footer class="justify-end">
        <Button
          size="sm"
          disabled={busy || pickedIdentity.trim() === "" || pickedPassphrase.trim() === ""}
          onclick={() => void usePicked()}
        >
          {working ? "Checking…" : "Use these files"}
        </Button>
      </Card.Footer>
    </Card.Root>
  {/if}

  {#if helper !== null}
    <Alert.Root class="border-primary/40 bg-primary/10">
      <CircleAlertIcon />
      <Alert.Description class="text-foreground/80">{helper}</Alert.Description>
    </Alert.Root>
  {/if}

  {#if kind === "credentials"}
    <Card.Root class="ring-destructive/25">
      <Card.Header>
        <Card.Title class="flex items-center gap-2">
          <ShieldAlertIcon class="size-4 text-destructive" />
          Start over with a new identity
        </Card.Title>
        <Card.Description>
          A fresh identity is written to new files. The old file stays where it is, untouched.
        </Card.Description>
      </Card.Header>
      <Card.Footer class="justify-end">
        <Button variant="destructive" size="sm" disabled={busy} onclick={() => (confirmNew = true)}>
          <PlusIcon data-icon="inline-start" />
          Create a new identity instead…
        </Button>
      </Card.Footer>
    </Card.Root>
  {/if}
</div>

{#if kind === "credentials"}
  <AlertDialog.Root bind:open={confirmNew}>
    <AlertDialog.Content>
      <AlertDialog.Header>
        <AlertDialog.Title>Create a new identity instead?</AlertDialog.Title>
        <AlertDialog.Description>
          This will not restore your existing identity, and your old file will be kept where it is.
          Drives that trusted the old identity will not recognize this one — they would need to admit it as a new device.
          This cannot be undone.
        </AlertDialog.Description>
      </AlertDialog.Header>
      <AlertDialog.Footer>
        <AlertDialog.Cancel>Cancel</AlertDialog.Cancel>
        <AlertDialog.Action variant="destructive" onclick={onCreateNew}>Create new identity</AlertDialog.Action>
      </AlertDialog.Footer>
    </AlertDialog.Content>
  </AlertDialog.Root>
{/if}
