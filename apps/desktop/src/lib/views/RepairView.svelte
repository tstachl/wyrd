<!-- Broken-credential Repair: dedicated state, never a toast and
never a silent invitation to generate. Human-first copy up front;
raw CLI text rides only inside the expandable technical detail (raw
credentials.rs strings never reach the renderer as the headline).
A new identity from here requires explicit destructive confirmation:
the old file is preserved where it is, never overwritten, and the
consequence is stated plainly. -->
<script lang="ts">
  import Button from "$lib/components/ui/button.svelte";
  import type { KeyDefaults, OnboardingReport } from "$lib/ipc/results.js";
  import { pickExistingFile } from "$lib/ipc/files.js";

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
          ? "Still failing — the detail below says what the check reported."
          : "Still unreadable — the detail below says what the check reported.";
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
        helper = "Those files were not adopted — the detail below says what the check reported.";
      }
    } catch (failure: unknown) {
      helper = failure instanceof Error ? failure.message : String(failure);
    } finally {
      working = false;
    }
  }
</script>

<div class="max-w-xl">
  {#if kind === "versionmismatch"}
  <h1 class="mt-4 text-2xl font-semibold tracking-tight">The app and its helper tool do not match</h1>
  <p class="mt-2 text-sm">The desktop could not run its paired <span class="font-mono text-xs">wyrd</span> helper. This says nothing about your identity files, and your drives are untouched — nothing was changed. Reinstall the matched pair, then verify again. In a development checkout that means building the CLI beside the desktop binary; the technical detail below names the exact failure.</p>
  {:else}
  <h1 class="mt-4 text-2xl font-semibold tracking-tight">Fix this device's identity</h1>
  <p class="mt-2 text-sm">Wyrd cannot use this identity file: other users on this computer could access it, or it is damaged. Your drives are untouched — nothing was changed.</p>
  {/if}
  <p class="mt-1 text-sm text-smoke">Expected files: <span class="font-mono text-xs">{defaults.identity_file}</span>, <span class="font-mono text-xs">{defaults.passphrase_file}</span></p>

  <div class="mt-3">
    <Button variant="secondary" size="sm" disabled={busy} onclick={() => (showTechnical = !showTechnical)}>{showTechnical ? "Hide technical detail" : "Show technical detail"}</Button>
    {#if showTechnical}
      <pre class="mt-2 overflow-auto rounded-md bg-ink px-3 py-2 text-xs text-bone dark:border dark:border-bone/10 dark:bg-black">{detail}</pre>
    {/if}
  </div>

  <div class="mt-4 flex gap-2">
    <Button variant="default" size="sm" disabled={busy} onclick={() => void verifyAgain()}>Verify again</Button>
  </div>

  {#if kind === "credentials"}
  <div class="mt-4 rounded-md border border-ink/15 dark:border-bone/20 px-3 py-2">
    <h2 class="text-sm font-semibold">Choose a different file</h2>
    <div class="mt-2 text-sm">
      <p>Identity file</p>
      <div class="mt-1 flex gap-2">
        <input class="flex-1 rounded-md border border-ink/15 dark:border-bone/20 px-2 py-1" bind:value={pickedIdentity} disabled={busy} placeholder={defaults.identity_file} />
        <Button variant="secondary" size="sm" disabled={busy} onclick={() => void browse((p) => (pickedIdentity = p), "Identity file")}>Browse…</Button>
      </div>
    </div>
    <div class="mt-2 text-sm">
      <p>Passphrase file</p>
      <div class="mt-1 flex gap-2">
        <input class="flex-1 rounded-md border border-ink/15 dark:border-bone/20 px-2 py-1" bind:value={pickedPassphrase} disabled={busy} placeholder={defaults.passphrase_file} />
        <Button variant="secondary" size="sm" disabled={busy} onclick={() => void browse((p) => (pickedPassphrase = p), "Passphrase file")}>Browse…</Button>
      </div>
    </div>
    <div class="mt-2">
      <Button variant="default" size="sm" disabled={busy} onclick={() => void usePicked()}>Use these files</Button>
    </div>
  </div>
  {/if}

  {#if helper !== null}<p class="mt-3 text-sm" role="alert">{helper}</p>{/if}

  {#if kind === "credentials"}
  <div class="mt-4 rounded-md border border-ink/15 dark:border-bone/20 px-3 py-2">
    {#if !confirmNew}
      <Button variant="secondary" size="sm" disabled={busy} onclick={() => (confirmNew = true)}>Create a new identity instead…</Button>
    {:else}
      <h2 class="text-sm font-semibold">Create a new identity instead?</h2>
      <p class="mt-1 text-sm">This will not restore your existing identity, and your old file will be kept where it is. Drives that trusted the old identity will not recognize this one — they would need to admit it as a new device. This cannot be undone.</p>
      <div class="mt-2 flex gap-2">
        <Button variant="secondary" size="sm" disabled={busy} onclick={() => (confirmNew = false)}>Cancel</Button>
        <Button variant="default" size="sm" disabled={busy} onclick={onCreateNew}>Create new identity</Button>
      </div>
    {/if}
  </div>
  {/if}
</div>
