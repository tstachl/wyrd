<!-- First-run identity onboarding: three stages (Identity ->
Passphrase -> Recovery) plus Ready, with a first-class Restore
branch. Copy is normative per `docs/design/first-run-onboarding.md`:
the disclosure audit fixture asserts the load-bearing strings
verbatim. Secrets: the generated passphrase lives in component state
from write-down to Finish and is never persisted here; commit sends
it to the backend once (stdin pipe, never argv). Identity secrets
never enter this component. -->
<script lang="ts">
  import Button from "$lib/components/ui/button.svelte";
  import type { KeyDefaults, OnboardingReport } from "$lib/ipc/results.js";
  import { pickExistingFile } from "$lib/ipc/files.js";
  import { withDevCliHint } from "./format.js";

  type Stage = "identity" | "passphrase" | "recovery" | "ready";
  type IdentityChoice = "create" | "restore";

  interface Props {
    defaults: KeyDefaults;
    pending: boolean;
    onNewPassphrase: () => Promise<OnboardingReport>;
    onCommit: (args: { identityFile: string; passphraseFile: string; passphrase: string }) => Promise<OnboardingReport>;
    onVerify: (args: { identityFile: string; passphraseFile: string }) => Promise<OnboardingReport>;
    onAdopt: (args: { identityFile: string; passphraseFile: string }) => Promise<OnboardingReport>;
    onReveal: (path: string) => Promise<boolean>;
    onDone: (identityFile: string, passphraseFile: string, mode: "create" | "join") => void;
    onQuit: () => void;
  }

  let { defaults, pending, onNewPassphrase, onCommit, onVerify, onAdopt, onReveal, onDone, onQuit }: Props = $props();

  let stage = $state<Stage>("identity");
  let choice = $state<IdentityChoice>("create");
  // Restore branch: user-picked existing files (paths only).
  let restoreIdentityFile = $state("");
  let restorePassphraseFile = $state("");
  // Passphrase branch: generated (default per design) vs custom.
  let useGenerated = $state(true);
  let generatedPassphrase: string | null = $state(null);
  let customPassphrase = $state("");
  let customConfirm = $state("");
  let showPassphrase = $state(false);
  // Recovery: single acknowledgement + expandable detail.
  let acknowledged = $state(false);
  let showDetails = $state(false);
  // Commit.
  let working = $state(false);
  let helper: string | null = $state(null);
  let commitError: string | null = $state(null);
  let committedIdentityFile = $state("");
  let committedPassphraseFile = $state("");
  // Discard prompt. The modal focuses its dialog on open so Escape
  // reaches it; the dialog closes on Escape and swallows other keys
  // so they never dismiss from behind the overlay.
  let confirmDiscard = $state(false);
  let confirmDialogEl: HTMLDivElement | null = $state(null);
  $effect(() => {
    if (confirmDiscard) confirmDialogEl?.focus();
  });

  const busy = $derived(pending || working);

  function effectivePassphrase(): string | null {
    if (choice === "restore") return null;
    return useGenerated ? generatedPassphrase : customPassphrase;
  }

  function customError(): string | null {
    if (useGenerated) return null;
    if (customPassphrase === "") return "Enter a passphrase, or choose Generate one for me.";
    if (customPassphrase.length < 12) return "Short passphrases can be guessed if someone gets a copy of your files — 12 characters is the floor; longer is better.";
    if (customPassphrase !== customConfirm) return "The two passphrases do not match.";
    if (customPassphrase.includes("\n") || customPassphrase.includes("\r")) return "The passphrase must not contain line breaks.";
    return null;
  }

  async function ensureGenerated(): Promise<boolean> {
    if (!useGenerated || generatedPassphrase !== null) return true;
    working = true;
    helper = null;
    try {
      const report = await onNewPassphrase();
      const result = report.result;
      if (result.result === "completed" && result.passphrase !== null) {
        generatedPassphrase = result.passphrase;
        return true;
      }
      helper = describeReport(report, "Could not generate a passphrase.");
      return false;
    } catch (failure: unknown) {
      helper = failure instanceof Error ? failure.message : String(failure);
      return false;
    } finally {
      working = false;
    }
  }

  async function goPassphrase(): Promise<void> {
    helper = null;
    commitError = null;
    if (choice === "restore") {
      if (restoreIdentityFile.trim() === "" || restorePassphraseFile.trim() === "") {
        helper = "Choose both files belonging to the identity you want to restore on this machine.";
        return;
      }
      working = true;
      try {
        const report = await onVerify({ identityFile: restoreIdentityFile, passphraseFile: restorePassphraseFile });
        if (report.result.result === "completed") {
          stage = "passphrase";
        } else {
          helper = describeReport(report, "Those files did not verify.");
        }
      } catch (failure: unknown) {
        helper = failure instanceof Error ? failure.message : String(failure);
      } finally {
        working = false;
      }
      return;
    }
    if (await ensureGenerated()) stage = "passphrase";
  }

  async function goRecovery(): Promise<void> {
    helper = null;
    if (choice === "restore") {
      stage = "recovery";
      return;
    }
    if (useGenerated) {
      if (await ensureGenerated()) stage = "recovery";
      return;
    }
    const problem = customError();
    if (problem !== null) {
      helper = problem;
      return;
    }
    stage = "recovery";
  }

  async function finish(): Promise<void> {
    helper = null;
    commitError = null;
    if (!defaults.usable) {
      helper = "Could not determine this device's credential locations: no home directory resolves. Fix the environment and retry — nothing was written.";
      return;
    }
    if (!acknowledged) {
      helper = "Confirm you have stored your passphrase somewhere safe before finishing.";
      return;
    }
    if (choice === "restore") {
      // Nothing to write: the restored files are already in place.
      // Adopt verifies once more (read-back) and records the pair,
      // so the gate resolves it after the session.
      working = true;
      try {
        const report = await onAdopt({ identityFile: restoreIdentityFile, passphraseFile: restorePassphraseFile });
        if (report.result.result === "completed") {
          committedIdentityFile = restoreIdentityFile;
          committedPassphraseFile = restorePassphraseFile;
          stage = "ready";
        } else {
          commitError = describeReport(report, "The restored files were not adopted.");
        }
      } catch (failure: unknown) {
        commitError = failure instanceof Error ? failure.message : String(failure);
      } finally {
        working = false;
      }
      return;
    }
    const passphrase = effectivePassphrase();
    if (passphrase === null || passphrase === "") {
      helper = useGenerated
        ? "No generated passphrase is ready — go back and generate one."
        : (customError() ?? "Enter a passphrase.");
      return;
    }
    working = true;
    try {
      // Atomic step: commit writes, verifies, and adopts in one
      // backend call, so no Ready-close window can orphan the pair.
      const report = await onCommit({ identityFile: defaults.identity_file, passphraseFile: defaults.passphrase_file, passphrase });
      if (report.result.result === "completed") {
        committedIdentityFile = defaults.identity_file;
        committedPassphraseFile = defaults.passphrase_file;
        generatedPassphrase = null;
        customPassphrase = "";
        customConfirm = "";
        stage = "ready";
      } else {
        commitError = describeReport(report, "Could not write the credential files.");
      }
    } catch (failure: unknown) {
      commitError = failure instanceof Error ? failure.message : String(failure);
    } finally {
      working = false;
    }
  }

  // Verify-only recovery after a failed commit: the commit refuses
  // to overwrite, so re-running it refuses too. Adopt verifies the
  // existing files and records them without writing anything.
  async function verifyExisting(): Promise<void> {
    commitError = null;
    working = true;
    try {
      const check = await onAdopt({ identityFile: defaults.identity_file, passphraseFile: defaults.passphrase_file });
      if (check.result.result === "completed") {
        committedIdentityFile = defaults.identity_file;
        committedPassphraseFile = defaults.passphrase_file;
        stage = "ready";
      } else {
        commitError = describeReport(check, "The files are still not readable.");
      }
    } catch (failure: unknown) {
      commitError = failure instanceof Error ? failure.message : String(failure);
    } finally {
      working = false;
    }
  }

  function describeReport(report: OnboardingReport, prefix: string): string {
    const result = report.result;
    switch (result.result) {
      case "completed":
        return `${prefix} ${result.detail}`;
      case "expectedfailure":
        return `${prefix} ${result.detail}`;
      case "versionmismatch":
        return `Version mismatch (reinstall the matched pair): ${withDevCliHint(result.reason)}`;
      case "unknown":
        return `Outcome unknown: ${result.detail}`;
    }
  }

  async function browse(set: (path: string) => void, title: string): Promise<void> {
    const picked = await pickExistingFile(title);
    if (picked !== null) set(picked);
  }

  async function copyText(text: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      helper = "Copied — for pasting into your password manager. Your stored copy is the backup, not the clipboard.";
      // Best-effort clear where the platform allows; clipboard
      // managers may keep copies, so the stored copy stays the
      // backup. Never promised as a guarantee.
      window.setTimeout(() => {
        void navigator.clipboard.writeText("").catch(() => {});
      }, 30000);
    } catch {
      helper = "Copy failed — select the passphrase and copy it by hand.";
    }
  }

  function quit(): void {
    if (stage === "ready") {
      onDone(committedIdentityFile, committedPassphraseFile, "create");
      return;
    }
    confirmDiscard = true;
  }
</script>

<div class="max-w-xl">
  {#if stage !== "ready"}
    <ol class="flex gap-2 text-xs text-smoke" aria-label="Onboarding progress">
      <li aria-current={stage === "identity" ? "step" : undefined} class={stage === "identity" ? "font-semibold text-ink dark:text-bone" : ""}>Identity</li>
      <li aria-hidden="true">→</li>
      <li aria-current={stage === "passphrase" ? "step" : undefined} class={stage === "passphrase" ? "font-semibold text-ink dark:text-bone" : ""}>Passphrase</li>
      <li aria-hidden="true">→</li>
      <li aria-current={stage === "recovery" ? "step" : undefined} class={stage === "recovery" ? "font-semibold text-ink dark:text-bone" : ""}>Recovery</li>
    </ol>
  {/if}

  {#if stage === "identity"}
    <h1 class="mt-4 text-2xl font-semibold tracking-tight">Welcome to Wyrd</h1>
    <p class="mt-2 text-sm">This device needs its own identity to join drives.</p>

    <div class="mt-4 rounded-md border border-ink/15 dark:border-bone/20 px-3 py-2 text-sm">
      <p class="font-semibold">This device's identity has two parts.</p>
      <p class="mt-1"><span class="font-semibold">Identity key</span> — establishes who this device is. Generated randomly; it cannot be recreated from your passphrase.</p>
      <p class="mt-1"><span class="font-semibold">Unlock passphrase</span> — protects access to this device's identity key. It does not replace or recover that key.</p>
      <p class="mt-1">You need both — a backup of the identity files <em>and</em> the passphrase — to restore access from a backup.</p>
      <p class="mt-2 font-semibold">On this device:</p>
      <p>Wyrd remembers your passphrase so you can use your drives without entering it each time. Your identity files are accessible to your user account — anyone or anything with your login's file access can use them. Full-disk encryption and your login are the actual protection here.</p>
      <p class="mt-2 font-semibold">On another device:</p>
      <p>you'll need your backed-up identity files and passphrase to restore this device's identity. The copy you store elsewhere is what saves you on a new machine or after a reinstall.</p>
    </div>

    <div class="mt-4 space-y-2" role="radiogroup" aria-label="Identity choice">
      <label class="flex items-start gap-2 rounded-md border border-ink/15 dark:border-bone/20 px-3 py-2 text-sm">
        <input type="radio" class="mt-1" name="identity-choice" checked={choice === "create"} onchange={() => (choice = "create")} disabled={busy} />
        <span><span class="font-semibold">Create a new identity</span><br /><span class="text-smoke">Create a new identity for this device.</span></span>
      </label>
      <label class="flex items-start gap-2 rounded-md border border-ink/15 dark:border-bone/20 px-3 py-2 text-sm">
        <input type="radio" class="mt-1" name="identity-choice" checked={choice === "restore"} onchange={() => (choice = "restore")} disabled={busy} />
        <span><span class="font-semibold">Restore from backup</span><br /><span class="text-smoke">Bring back an identity you backed up before.</span></span>
      </label>
    </div>

    {#if choice === "restore"}
      <div class="mt-4 rounded-md border border-ink/15 dark:border-bone/20 px-3 py-2">
        <h2 class="text-sm font-semibold">Restore from backup</h2>
        <p class="mt-1 text-sm">Choose the files belonging to the identity you want to restore on this machine.</p>
        <p class="mt-1 text-sm text-smoke">Restoring brings back this identity on this machine. A second device that stays in use needs its own identity — admit it to your drives instead of copying these files.</p>
        <div class="mt-2 text-sm">
          <p>Identity file</p>
          <div class="mt-1 flex gap-2">
            <input class="flex-1 rounded-md border border-ink/15 dark:border-bone/20 px-2 py-1" bind:value={restoreIdentityFile} disabled={busy} placeholder={defaults.identity_file} />
            <Button variant="secondary" size="sm" disabled={busy} onclick={() => void browse((p) => (restoreIdentityFile = p), "Identity file")}>Browse…</Button>
          </div>
        </div>
        <div class="mt-2 text-sm">
          <p>Passphrase file</p>
          <div class="mt-1 flex gap-2">
            <input class="flex-1 rounded-md border border-ink/15 dark:border-bone/20 px-2 py-1" bind:value={restorePassphraseFile} disabled={busy} placeholder={defaults.passphrase_file} />
            <Button variant="secondary" size="sm" disabled={busy} onclick={() => void browse((p) => (restorePassphraseFile = p), "Passphrase file")}>Browse…</Button>
          </div>
        </div>
      </div>
    {/if}

    {#if helper !== null}<p class="mt-3 text-sm" role="alert">{helper}</p>{/if}
    <div class="mt-4 flex gap-2">
      <Button variant="default" size="sm" disabled={busy} onclick={() => void goPassphrase()}>Continue</Button>
      <Button variant="secondary" size="sm" disabled={busy} onclick={quit}>Discard</Button>
    </div>
  {:else if stage === "passphrase"}
    {#if choice === "restore"}
      <h1 class="mt-4 text-2xl font-semibold tracking-tight">Restore from backup</h1>
      <p class="mt-2 text-sm">Identity files are readable: <span class="font-mono text-xs">{restoreIdentityFile}</span> and <span class="font-mono text-xs">{restorePassphraseFile}</span>. Continue to review what restore does and doesn't cover.</p>
      {#if helper !== null}<p class="mt-3 text-sm" role="alert">{helper}</p>{/if}
      <div class="mt-4 flex gap-2">
        <Button variant="secondary" size="sm" disabled={busy} onclick={() => (stage = "identity")}>Back</Button>
        <Button variant="default" size="sm" disabled={busy} onclick={() => void goRecovery()}>Continue</Button>
        <Button variant="secondary" size="sm" disabled={busy} onclick={quit}>Discard</Button>
      </div>
    {:else}
      <h1 class="mt-4 text-2xl font-semibold tracking-tight">Choose a passphrase</h1>
      <div class="mt-3 space-y-2" role="radiogroup" aria-label="Passphrase source">
        <label class="flex items-start gap-2 text-sm">
          <input type="radio" class="mt-1" name="passphrase-source" checked={useGenerated} onchange={() => { useGenerated = true; helper = null; }} disabled={busy} />
          <span>Generate one for me</span>
        </label>
        <label class="flex items-start gap-2 text-sm">
          <input type="radio" class="mt-1" name="passphrase-source" checked={!useGenerated} onchange={() => { useGenerated = false; helper = null; }} disabled={busy} />
          <span>Choose my own</span>
        </label>
      </div>
      <p class="mt-2 text-sm text-smoke">This device will remember it so you don't type it each launch. Short passphrases can be guessed if someone gets a copy of your files — 12 characters is the floor; longer is better.</p>

      {#if useGenerated}
        {#if generatedPassphrase !== null}
          <div class="mt-3 rounded-md border border-ink/15 dark:border-bone/20 px-3 py-2">
            <p class="text-sm font-semibold">Your generated passphrase <span class="font-normal text-smoke">— generated locally on this device</span></p>
            <p class="mt-1 font-mono text-base tracking-wide" aria-live="polite">{generatedPassphrase}</p>
            <div class="mt-2 flex gap-2">
              <Button variant="secondary" size="sm" disabled={busy} onclick={() => void copyText(generatedPassphrase ?? "")}>Copy — for pasting into your password manager</Button>
              <Button variant="secondary" size="sm" disabled={busy} onclick={() => { generatedPassphrase = null; void ensureGenerated(); }}>Generate another</Button>
            </div>
            <p class="mt-2 text-sm">Write it down or save it somewhere safe: this copy is for a new machine or a reinstall. This device remembers it for daily use. (The clipboard clears itself where the platform allows; clipboard managers may keep copies — your stored copy is the backup, not the clipboard.)</p>
          </div>
        {:else}
          <p class="mt-3 text-sm text-smoke">A passphrase will be generated on this device when you continue — nothing is written yet.</p>
        {/if}
      {:else}
        <label class="mt-3 block text-sm">
          Passphrase
          <input class="mt-1 w-full rounded-md border border-ink/15 dark:border-bone/20 px-2 py-1" type={showPassphrase ? "text" : "password"} bind:value={customPassphrase} disabled={busy} autocomplete="new-password" />
        </label>
        <label class="mt-2 block text-sm">
          Confirm passphrase
          <input class="mt-1 w-full rounded-md border border-ink/15 dark:border-bone/20 px-2 py-1" type={showPassphrase ? "text" : "password"} bind:value={customConfirm} disabled={busy} autocomplete="new-password" />
        </label>
        <label class="mt-2 flex items-center gap-2 text-sm">
          <input type="checkbox" bind:checked={showPassphrase} disabled={busy} /> Show passphrase
        </label>
      {/if}

      {#if helper !== null}<p class="mt-3 text-sm" role="alert">{helper}</p>{/if}
      <div class="mt-4 flex gap-2">
        <Button variant="secondary" size="sm" disabled={busy} onclick={() => (stage = "identity")}>Back</Button>
        <Button variant="default" size="sm" disabled={busy} onclick={() => void goRecovery()}>Continue</Button>
        <Button variant="secondary" size="sm" disabled={busy} onclick={quit}>Discard</Button>
      </div>
    {/if}
  {:else if stage === "recovery"}
    <h1 class="mt-4 text-2xl font-semibold tracking-tight">Plan for losing this device</h1>
    <p class="mt-2 text-sm font-semibold">Wyrd cannot recover this device's identity for you.</p>
    <p class="mt-1 text-sm">To restore it, you need a backup of your identity files and your passphrase.</p>
    <p class="mt-1 text-sm">If you lose either one, Wyrd cannot restore your identity.</p>
    <p class="mt-2 text-sm font-semibold">Keep your identity files and passphrase in separate places.</p>
    {#if choice === "restore"}
      <p class="mt-1 text-sm text-smoke">Restored files: <span class="font-mono text-xs">{restoreIdentityFile}</span>, <span class="font-mono text-xs">{restorePassphraseFile}</span></p>
    {:else}
      <p class="mt-1 text-sm text-smoke">Identity files will be created at: <span class="font-mono text-xs">{defaults.identity_file}</span> (created when you finish)</p>
    {/if}

    <div class="mt-3">
      <Button variant="secondary" size="sm" disabled={busy} onclick={() => (showDetails = !showDetails)}>{showDetails ? "Hide details" : "Details — what restore does and doesn't cover"}</Button>
      {#if showDetails}
        <ul class="mt-2 list-disc space-y-1 pl-5 text-sm">
          <li>Restoring the identity alone doesn't bring back drive contents: those need a surviving copy (another member's device, a vault) or a fresh invitation.</li>
          <li>An identity removed from its drives cannot regain access to them: that drive needs a new identity admitted by its owner.</li>
          <li>Guardian recovery (trusted contacts) is planned for a later release. <span class="text-smoke">Learn more</span></li>
        </ul>
      {/if}
    </div>

    {#if choice === "create" && useGenerated && generatedPassphrase !== null}
      <div class="mt-3 rounded-md border border-ink/15 dark:border-bone/20 px-3 py-2">
        <p class="text-sm">Your passphrase, again: <span class="font-mono">{generatedPassphrase}</span></p>
        <div class="mt-2">
          <Button variant="secondary" size="sm" disabled={busy} onclick={() => void copyText(generatedPassphrase ?? "")}>Copy — for your password manager</Button>
        </div>
        <p class="mt-1 text-xs text-smoke">The clipboard clears itself where the platform allows, but clipboard managers may keep copies, so your stored copy is the backup.</p>
      </div>
    {/if}

    <label class="mt-4 flex items-start gap-2 text-sm">
      <input type="checkbox" class="mt-1" bind:checked={acknowledged} disabled={busy} />
      <span>I've stored my passphrase somewhere safe, and I understand Wyrd cannot recover this device's identity if I lose my identity-file backup or my passphrase.</span>
    </label>

    {#if helper !== null}<p class="mt-3 text-sm" role="alert">{helper}</p>{/if}
    {#if commitError !== null}
      <p class="mt-3 text-sm" role="alert">{commitError}</p>
      <div class="mt-2">
        <Button variant="secondary" size="sm" disabled={busy} onclick={() => void verifyExisting()}>Verify the existing files</Button>
      </div>
    {/if}
    <div class="mt-4 flex gap-2">
      <Button variant="secondary" size="sm" disabled={busy} onclick={() => (stage = "passphrase")}>Back</Button>
      <Button variant="default" size="sm" disabled={busy} onclick={() => void finish()}>{working ? "Finishing…" : "Finish"}</Button>
      <Button variant="secondary" size="sm" disabled={busy} onclick={quit}>Quit</Button>
    </div>
  {:else}
    <h1 class="mt-4 text-2xl font-semibold tracking-tight">This device is ready</h1>
    <p class="mt-2 text-sm">Your device has its own Wyrd identity.</p>
    <div class="mt-4 space-y-2">
      <Button variant="default" size="sm" disabled={busy} onclick={() => onDone(committedIdentityFile, committedPassphraseFile, "create")}>Create a drive — Start a private collection of files and history.</Button>
      <Button variant="secondary" size="sm" disabled={busy} onclick={() => onDone(committedIdentityFile, committedPassphraseFile, "join")}>Join with an invitation — Join a drive someone has shared with this device.</Button>
    </div>
    <div class="mt-4 rounded-md border border-ink/15 dark:border-bone/20 px-3 py-2 text-sm">
      <p class="font-semibold">Identity files <span class="font-normal text-smoke">— Stored on this device:</span></p>
      <p class="mt-1 font-mono text-xs">{committedIdentityFile}</p>
      <div class="mt-2">
        <Button variant="secondary" size="sm" disabled={busy} onclick={() => void onReveal(committedIdentityFile).then((ok) => { if (!ok) helper = "Could not open the file manager — the files are still at the path above."; })}>Show in Finder</Button>
      </div>
      {#if helper !== null}<p class="mt-2 text-sm" role="alert">{helper}</p>{/if}
    </div>
  {/if}

  {#if confirmDiscard}
    <div
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
      onclick={() => (confirmDiscard = false)}
      onkeydown={(event) => {
        if (event.key === "Escape") confirmDiscard = false;
      }}
      role="presentation"
    >
      <div
        class="w-full max-w-sm rounded-md border border-ink/15 bg-bone px-4 py-3 text-sm text-ink shadow-lg dark:border-bone/20 dark:bg-ink dark:text-bone"
        role="alertdialog"
        aria-label="Discard onboarding"
        aria-modal="true"
        tabindex={-1}
        bind:this={confirmDialogEl}
        onclick={(event) => event.stopPropagation()}
        onkeydown={(event) => {
          if (event.key === "Escape") confirmDiscard = false;
          else event.stopPropagation();
        }}
      >
        <p class="font-semibold">Discard and start over?</p>
        <p class="mt-1">The identity is not saved yet{useGenerated && generatedPassphrase !== null ? " and anything generated will be lost" : ""}. Starting over leaves nothing behind.</p>
        <div class="mt-3 flex justify-end gap-2">
          <Button variant="secondary" size="sm" onclick={() => (confirmDiscard = false)}>Keep going</Button>
          <Button variant="default" size="sm" onclick={onQuit}>Discard</Button>
        </div>
      </div>
    </div>
  {/if}
</div>
