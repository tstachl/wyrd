<!-- First-run identity onboarding: three stages (Identity ->
Passphrase -> Recovery) plus Ready, with a first-class Restore
branch. Copy is normative per `docs/design/first-run-onboarding.md`:
the disclosure audit fixture asserts the load-bearing strings
verbatim. Secrets: the generated passphrase lives in component state
from write-down to Finish and is never persisted here; commit sends
it to the backend once (stdin pipe, never argv). Identity secrets
never enter this component. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import * as Field from "$lib/components/ui/field/index.js";
  import * as InputGroup from "$lib/components/ui/input-group/index.js";
  import { Input } from "$lib/components/ui/input/index.js";
  import { Checkbox } from "$lib/components/ui/checkbox/index.js";
  import * as RadioGroup from "$lib/components/ui/radio-group/index.js";
  import * as Collapsible from "$lib/components/ui/collapsible/index.js";
  import * as AlertDialog from "$lib/components/ui/alert-dialog/index.js";
  import { Separator } from "$lib/components/ui/separator/index.js";
  import type { KeyDefaults, OnboardingReport } from "$lib/ipc/results.js";
  import { pickExistingFile } from "$lib/ipc/files.js";
  import { withDevCliHint } from "./format.js";
  import CheckIcon from "@lucide/svelte/icons/check";
  import KeyRoundIcon from "@lucide/svelte/icons/key-round";
  import LockKeyholeIcon from "@lucide/svelte/icons/lock-keyhole";
  import FolderOpenIcon from "@lucide/svelte/icons/folder-open";
  import CopyIcon from "@lucide/svelte/icons/copy";
  import ChevronDownIcon from "@lucide/svelte/icons/chevron-down";
  import ShieldAlertIcon from "@lucide/svelte/icons/shield-alert";
  import CircleAlertIcon from "@lucide/svelte/icons/circle-alert";
  import PlusIcon from "@lucide/svelte/icons/plus";
  import DownloadIcon from "@lucide/svelte/icons/download";
  import ArrowRightIcon from "@lucide/svelte/icons/arrow-right";
  import CheckCircle2Icon from "@lucide/svelte/icons/check-circle-2";

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
  // Discard prompt (AlertDialog owns focus, Escape, and dismissal).
  let confirmDiscard = $state(false);

  const busy = $derived(pending || working);

  const steps = [
    { id: "identity", label: "Identity" },
    { id: "passphrase", label: "Passphrase" },
    { id: "recovery", label: "Recovery" },
  ] as const;
  const stepIndex = $derived(steps.findIndex((entry) => entry.id === stage));

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

<div class="mx-auto w-full max-w-3xl">
  {#if stage !== "ready"}
    <ol class="flex flex-wrap items-center gap-x-3 gap-y-2 text-sm" aria-label="Onboarding progress">
      {#each steps as step, index (step.id)}
        <li
          class="flex items-center gap-2"
          aria-current={stage === step.id ? "step" : undefined}
        >
          <span
            class="flex size-6 items-center justify-center rounded-full text-xs font-semibold tabular-nums transition-colors {stage ===
            step.id
              ? 'bg-primary text-primary-foreground'
              : index < stepIndex
                ? 'bg-foreground text-background'
                : 'border border-border text-muted-foreground'}"
          >
            {#if index < stepIndex}
              <CheckIcon class="size-3.5" />
            {:else}
              {index + 1}
            {/if}
          </span>
          <span
            class={stage === step.id
              ? "font-semibold text-foreground"
              : "text-muted-foreground"}
          >
            {step.label}
          </span>
        </li>
        {#if index < steps.length - 1}
          <li aria-hidden="true" class="h-px w-6 bg-border"></li>
        {/if}
      {/each}
    </ol>
  {/if}

  {#if stage === "identity"}
    <Card.Root class="mt-6">
      <Card.Header>
        <Card.Title class="text-xl">Welcome to Wyrd</Card.Title>
        <Card.Description>This device needs its own identity to join drives.</Card.Description>
      </Card.Header>
      <Card.Content class="flex flex-col gap-5">
        <div class="rounded-xl border border-border bg-muted/40 p-4">
          <p class="text-sm font-semibold">This device's identity has two parts.</p>
          <div class="mt-3 grid gap-3 sm:grid-cols-2">
            <div class="flex gap-2.5">
              <span class="flex size-8 shrink-0 items-center justify-center rounded-lg bg-background text-muted-foreground ring-1 ring-border">
                <KeyRoundIcon class="size-4" />
              </span>
              <div class="min-w-0">
                <p class="text-sm font-semibold">Identity key</p>
                <p class="mt-0.5 text-xs leading-relaxed text-muted-foreground">
                  establishes who this device is. Generated randomly; it cannot be recreated from
                  your passphrase.
                </p>
              </div>
            </div>
            <div class="flex gap-2.5">
              <span class="flex size-8 shrink-0 items-center justify-center rounded-lg bg-background text-muted-foreground ring-1 ring-border">
                <LockKeyholeIcon class="size-4" />
              </span>
              <div class="min-w-0">
                <p class="text-sm font-semibold">Unlock passphrase</p>
                <p class="mt-0.5 text-xs leading-relaxed text-muted-foreground">
                  protects access to this device's identity key. It does not replace or recover that
                  key.
                </p>
              </div>
            </div>
          </div>
          <p class="mt-3 text-sm leading-relaxed">
            You need both — a backup of the identity files <em>and</em> the passphrase — to restore
            access from a backup.
          </p>
          <Separator class="my-3" />
          <div class="grid gap-4 sm:grid-cols-2">
            <div>
              <p class="text-sm font-semibold">On this device:</p>
              <p class="mt-1 text-sm leading-relaxed text-muted-foreground">
                Wyrd remembers your passphrase so you can use your drives without entering it each time.
                Your identity files are accessible to your user account — anyone or anything with your login's file access can use them.
                Full-disk encryption and your login are the actual protection here.
              </p>
            </div>
            <div>
              <p class="text-sm font-semibold">On another device:</p>
              <p class="mt-1 text-sm leading-relaxed text-muted-foreground">
                you'll need your backed-up identity files and passphrase to restore this device's
                identity. The copy you store elsewhere is what saves you on a new machine or after a
                reinstall.
              </p>
            </div>
          </div>
        </div>

        <Field.Set>
          <Field.Legend variant="label">How should this device start?</Field.Legend>
          <RadioGroup.Root
            value={choice}
            onValueChange={(value) => (choice = value as IdentityChoice)}
            disabled={busy}
          >
            <Field.Label for="identity-create">
              <Field.Field orientation="horizontal">
                <Field.Content>
                  <Field.Title>Create a new identity</Field.Title>
                  <Field.Description>Create a new identity for this device.</Field.Description>
                </Field.Content>
                <RadioGroup.Item value="create" id="identity-create" disabled={busy} />
              </Field.Field>
            </Field.Label>
            <Field.Label for="identity-restore">
              <Field.Field orientation="horizontal">
                <Field.Content>
                  <Field.Title>Restore from backup</Field.Title>
                  <Field.Description>Bring back an identity you backed up before.</Field.Description>
                </Field.Content>
                <RadioGroup.Item value="restore" id="identity-restore" disabled={busy} />
              </Field.Field>
            </Field.Label>
          </RadioGroup.Root>
        </Field.Set>

        {#if choice === "restore"}
          <div class="rounded-xl border border-border p-4">
            <h2 class="text-sm font-semibold">Restore from backup</h2>
            <p class="mt-1 text-sm">
              Choose the files belonging to the identity you want to restore on this machine.
            </p>
            <p class="mt-1 text-sm text-muted-foreground">
              Restoring brings back this identity on this machine. A second device that stays in use needs its own identity — admit it to your drives instead of copying these files.
            </p>
            <Field.Group class="mt-3">
              <Field.Field>
                <Field.Label for="restore-identity">Identity file</Field.Label>
                <InputGroup.Root>
                  <InputGroup.Input
                    id="restore-identity"
                    bind:value={restoreIdentityFile}
                    disabled={busy}
                    placeholder={defaults.identity_file}
                    autocomplete="off"
                    spellcheck={false}
                    class="font-mono text-xs"
                  />
                  <InputGroup.Addon align="inline-end">
                    <InputGroup.Button variant="secondary" size="xs" disabled={busy} onclick={() => void browse((p) => (restoreIdentityFile = p), "Identity file")}>
                      <FolderOpenIcon data-icon="inline-start" />
                      Browse…
                    </InputGroup.Button>
                  </InputGroup.Addon>
                </InputGroup.Root>
              </Field.Field>
              <Field.Field>
                <Field.Label for="restore-passphrase">Passphrase file</Field.Label>
                <InputGroup.Root>
                  <InputGroup.Input
                    id="restore-passphrase"
                    bind:value={restorePassphraseFile}
                    disabled={busy}
                    placeholder={defaults.passphrase_file}
                    autocomplete="off"
                    spellcheck={false}
                    class="font-mono text-xs"
                  />
                  <InputGroup.Addon align="inline-end">
                    <InputGroup.Button variant="secondary" size="xs" disabled={busy} onclick={() => void browse((p) => (restorePassphraseFile = p), "Passphrase file")}>
                      <FolderOpenIcon data-icon="inline-start" />
                      Browse…
                    </InputGroup.Button>
                  </InputGroup.Addon>
                </InputGroup.Root>
              </Field.Field>
            </Field.Group>
          </div>
        {/if}

        {#if helper !== null}
          <Alert.Root class="border-primary/40 bg-primary/10">
            <CircleAlertIcon />
            <Alert.Description class="text-foreground/80">{helper}</Alert.Description>
          </Alert.Root>
        {/if}
      </Card.Content>
      <Card.Footer class="justify-between">
        <Button variant="ghost" size="sm" disabled={busy} onclick={quit}>Discard</Button>
        <Button size="sm" disabled={busy} onclick={() => void goPassphrase()}>
          Continue
          <ArrowRightIcon data-icon="inline-end" />
        </Button>
      </Card.Footer>
    </Card.Root>
  {:else if stage === "passphrase"}
    <Card.Root class="mt-6">
      {#if choice === "restore"}
        <Card.Header>
          <Card.Title class="text-xl">Restore from backup</Card.Title>
          <Card.Description>Continue to review what restore does and doesn't cover.</Card.Description>
        </Card.Header>
        <Card.Content class="flex flex-col gap-4">
          <div class="rounded-xl border border-border bg-muted/40 p-4 text-sm">
            <p>
              Identity files are readable:
              <span class="font-mono text-xs">{restoreIdentityFile}</span>
              and
              <span class="font-mono text-xs">{restorePassphraseFile}</span>.
            </p>
          </div>
          {#if helper !== null}
            <Alert.Root class="border-primary/40 bg-primary/10">
              <CircleAlertIcon />
              <Alert.Description class="text-foreground/80">{helper}</Alert.Description>
            </Alert.Root>
          {/if}
        </Card.Content>
        <Card.Footer class="justify-between">
          <Button variant="ghost" size="sm" disabled={busy} onclick={quit}>Discard</Button>
          <div class="flex gap-2">
            <Button variant="outline" size="sm" disabled={busy} onclick={() => (stage = "identity")}>Back</Button>
            <Button size="sm" disabled={busy} onclick={() => void goRecovery()}>
              Continue
              <ArrowRightIcon data-icon="inline-end" />
            </Button>
          </div>
        </Card.Footer>
      {:else}
        <Card.Header>
          <Card.Title class="text-xl">Choose a passphrase</Card.Title>
          <Card.Description>
            This device will remember it so you don't type it each launch. Short passphrases can be
            guessed if someone gets a copy of your files — 12 characters is the floor; longer is
            better.
          </Card.Description>
        </Card.Header>
        <Card.Content class="flex flex-col gap-5">
          <Field.Set>
            <Field.Legend variant="label">Where should it come from?</Field.Legend>
            <RadioGroup.Root
              value={useGenerated ? "generated" : "custom"}
              onValueChange={(value) => {
                useGenerated = value === "generated";
                helper = null;
              }}
              disabled={busy}
            >
              <Field.Label for="passphrase-generated">
                <Field.Field orientation="horizontal">
                  <Field.Content>
                    <Field.Title>Generate one for me</Field.Title>
                    <Field.Description>Generated locally; write it down for another machine.</Field.Description>
                  </Field.Content>
                  <RadioGroup.Item value="generated" id="passphrase-generated" disabled={busy} />
                </Field.Field>
              </Field.Label>
              <Field.Label for="passphrase-custom">
                <Field.Field orientation="horizontal">
                  <Field.Content>
                    <Field.Title>Choose my own</Field.Title>
                    <Field.Description>Use something you already keep safe.</Field.Description>
                  </Field.Content>
                  <RadioGroup.Item value="custom" id="passphrase-custom" disabled={busy} />
                </Field.Field>
              </Field.Label>
            </RadioGroup.Root>
          </Field.Set>

          {#if useGenerated}
            {#if generatedPassphrase !== null}
              <div class="rounded-xl border border-primary/30 bg-primary/5 p-4">
                <p class="text-sm font-semibold">
                  Your generated passphrase
                  <span class="font-normal text-muted-foreground">— generated locally on this device</span>
                </p>
                <p
                  class="mt-2 rounded-lg bg-background/70 px-3 py-2 font-mono text-base tracking-wide select-all"
                  aria-live="polite"
                >{generatedPassphrase}</p>
                <div class="mt-3 flex flex-wrap gap-2">
                  <Button variant="outline" size="sm" disabled={busy} onclick={() => void copyText(generatedPassphrase ?? "")}>
                    <CopyIcon data-icon="inline-start" />
                    Copy — for pasting into your password manager
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={busy}
                    onclick={() => {
                      generatedPassphrase = null;
                      void ensureGenerated();
                    }}
                  >
                    Generate another
                  </Button>
                </div>
                <p class="mt-3 text-sm leading-relaxed">
                  Write it down or save it somewhere safe: this copy is for a new machine or a reinstall.
                  This device remembers it for daily use. (The clipboard clears itself where the platform allows; clipboard managers may keep copies — your stored copy is the backup, not the clipboard.)
                </p>
              </div>
            {:else}
              <p class="text-sm text-muted-foreground">
                A passphrase will be generated on this device when you continue — nothing is written
                yet.
              </p>
            {/if}
          {:else}
            <Field.Group>
              <Field.Field>
                <Field.Label for="custom-passphrase">Passphrase</Field.Label>
                <Input
                  id="custom-passphrase"
                  type={showPassphrase ? "text" : "password"}
                  bind:value={customPassphrase}
                  disabled={busy}
                  autocomplete="new-password"
                />
              </Field.Field>
              <Field.Field>
                <Field.Label for="custom-confirm">Confirm passphrase</Field.Label>
                <Input
                  id="custom-confirm"
                  type={showPassphrase ? "text" : "password"}
                  bind:value={customConfirm}
                  disabled={busy}
                  autocomplete="new-password"
                />
              </Field.Field>
              <Field.Field orientation="horizontal">
                <Checkbox id="show-passphrase" bind:checked={showPassphrase} disabled={busy} />
                <Field.Label for="show-passphrase" class="font-normal">Show passphrase</Field.Label>
              </Field.Field>
            </Field.Group>
          {/if}

          {#if helper !== null}
            <Alert.Root class="border-primary/40 bg-primary/10">
              <CircleAlertIcon />
              <Alert.Description class="text-foreground/80">{helper}</Alert.Description>
            </Alert.Root>
          {/if}
        </Card.Content>
        <Card.Footer class="justify-between">
          <Button variant="ghost" size="sm" disabled={busy} onclick={quit}>Discard</Button>
          <div class="flex gap-2">
            <Button variant="outline" size="sm" disabled={busy} onclick={() => (stage = "identity")}>Back</Button>
            <Button size="sm" disabled={busy} onclick={() => void goRecovery()}>
              Continue
              <ArrowRightIcon data-icon="inline-end" />
            </Button>
          </div>
        </Card.Footer>
      {/if}
    </Card.Root>
  {:else if stage === "recovery"}
    <Card.Root class="mt-6">
      <Card.Header>
        <Card.Title class="text-xl">Plan for losing this device</Card.Title>
      </Card.Header>
      <Card.Content class="flex flex-col gap-4">
        <Alert.Root class="border-primary/40 bg-primary/10">
          <ShieldAlertIcon />
          <Alert.Title>Wyrd cannot recover this device's identity for you.</Alert.Title>
          <Alert.Description class="space-y-1 text-foreground/80 [&_p:not(:last-child)]:mb-1">
            <p>To restore it, you need a backup of your identity files and your passphrase.</p>
            <p>If you lose either one, Wyrd cannot restore your identity.</p>
            <p class="font-medium text-foreground">
              Keep your identity files and passphrase in separate places.
            </p>
          </Alert.Description>
        </Alert.Root>

        <div class="rounded-xl border border-border bg-muted/40 px-4 py-3 text-sm">
          {#if choice === "restore"}
            <p>
              Restored files:
              <span class="font-mono text-xs">{restoreIdentityFile}</span>,
              <span class="font-mono text-xs">{restorePassphraseFile}</span>
            </p>
          {:else}
            <p>
              Identity files will be created at:
              <span class="font-mono text-xs">{defaults.identity_file}</span>
              (created when you finish)
            </p>
          {/if}
        </div>

        <Collapsible.Root bind:open={showDetails}>
          <Collapsible.Trigger>
            {#snippet child({ props })}
              <Button {...props} variant="ghost" size="sm" class="-ml-2 px-2 text-muted-foreground">
                Details — what restore does and doesn't cover
                <ChevronDownIcon
                  data-icon="inline-end"
                  class={showDetails ? "rotate-180 transition-transform" : "transition-transform"}
                />
              </Button>
            {/snippet}
          </Collapsible.Trigger>
          <Collapsible.Content>
            <ul class="mt-2 flex list-disc flex-col gap-1.5 pl-5 text-sm text-muted-foreground">
              <li>
                Restoring the identity alone doesn't bring back drive contents: those need a
                surviving copy (another member's device, a vault) or a fresh invitation.
              </li>
              <li>
                An identity removed from its drives cannot regain access to them: that drive needs a
                new identity admitted by its owner.
              </li>
              <li>Guardian recovery (trusted contacts) is planned for a later release.</li>
            </ul>
          </Collapsible.Content>
        </Collapsible.Root>

        {#if choice === "create" && useGenerated && generatedPassphrase !== null}
          <div class="rounded-xl border border-border px-4 py-3">
            <p class="text-sm">
              Your passphrase, again:
              <span class="font-mono">{generatedPassphrase}</span>
            </p>
            <div class="mt-2">
              <Button variant="outline" size="sm" disabled={busy} onclick={() => void copyText(generatedPassphrase ?? "")}>
                <CopyIcon data-icon="inline-start" />
                Copy — for your password manager
              </Button>
            </div>
            <p class="mt-2 text-xs text-muted-foreground">
              The clipboard clears itself where the platform allows, but clipboard managers may keep
              copies, so your stored copy is the backup.
            </p>
          </div>
        {/if}

        <Field.Field orientation="horizontal" class="rounded-xl border border-border p-3">
          <Checkbox id="recovery-acknowledged" bind:checked={acknowledged} disabled={busy} />
          <Field.Content>
            <Field.Label for="recovery-acknowledged" class="leading-relaxed font-normal">
              I've stored my passphrase somewhere safe, and I understand Wyrd cannot recover this device's identity if I lose my identity-file backup or my passphrase.
            </Field.Label>
          </Field.Content>
        </Field.Field>

        {#if helper !== null}
          <Alert.Root class="border-primary/40 bg-primary/10">
            <CircleAlertIcon />
            <Alert.Description class="text-foreground/80">{helper}</Alert.Description>
          </Alert.Root>
        {/if}
        {#if commitError !== null}
          <Alert.Root variant="destructive">
            <CircleAlertIcon />
            <Alert.Description>{commitError}</Alert.Description>
            <div class="mt-2">
              <Button variant="outline" size="sm" disabled={busy} onclick={() => void verifyExisting()}>
                Verify the existing files
              </Button>
            </div>
          </Alert.Root>
        {/if}
      </Card.Content>
      <Card.Footer class="justify-between">
        <Button variant="ghost" size="sm" disabled={busy} onclick={quit}>Quit</Button>
        <div class="flex gap-2">
          <Button variant="outline" size="sm" disabled={busy} onclick={() => (stage = "passphrase")}>Back</Button>
          <Button size="sm" disabled={busy} onclick={() => void finish()}>{working ? "Finishing…" : "Finish"}</Button>
        </div>
      </Card.Footer>
    </Card.Root>
  {:else}
    <Card.Root class="mt-6">
      <Card.Content class="flex flex-col items-center gap-3 py-8 text-center">
        <span class="flex size-14 items-center justify-center rounded-full bg-primary/15 text-primary ring-1 ring-primary/30">
          <CheckCircle2Icon class="size-7" />
        </span>
        <div>
          <p class="text-xl font-semibold tracking-tight">This device is ready</p>
          <p class="mt-1 text-sm text-muted-foreground">Your device has its own Wyrd identity.</p>
        </div>
      </Card.Content>
      <Card.Content class="flex flex-col gap-2">
        <Button
          variant="outline"
          class="h-auto w-full justify-start gap-4 p-4 text-left"
          disabled={busy}
          onclick={() => onDone(committedIdentityFile, committedPassphraseFile, "create")}
        >
          <span class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary text-primary-foreground">
            <PlusIcon class="size-4" />
          </span>
          <span class="flex min-w-0 flex-1 flex-col gap-0.5">
            <span class="font-medium">Create a drive</span>
            <span class="text-xs font-normal text-muted-foreground">
              Start a private collection of files and history.
            </span>
          </span>
          <ArrowRightIcon class="size-4 text-muted-foreground" />
        </Button>
        <Button
          variant="outline"
          class="h-auto w-full justify-start gap-4 p-4 text-left"
          disabled={busy}
          onclick={() => onDone(committedIdentityFile, committedPassphraseFile, "join")}
        >
          <span class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted text-foreground ring-1 ring-border">
            <DownloadIcon class="size-4" />
          </span>
          <span class="flex min-w-0 flex-1 flex-col gap-0.5">
            <span class="font-medium">Join with an invitation</span>
            <span class="text-xs font-normal text-muted-foreground">
              Join a drive someone has shared with this device.
            </span>
          </span>
          <ArrowRightIcon class="size-4 text-muted-foreground" />
        </Button>
      </Card.Content>
      <Card.Footer class="flex-col items-start gap-3">
        <div class="w-full">
          <p class="text-sm font-medium">
            Identity files <span class="font-normal text-muted-foreground">— Stored on this device:</span>
          </p>
          <div class="mt-1.5 flex flex-wrap items-center gap-2">
            <span class="min-w-0 truncate rounded-md bg-muted/60 px-2 py-1 font-mono text-xs">
              {committedIdentityFile}
            </span>
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onclick={() => void onReveal(committedIdentityFile).then((ok) => { if (!ok) helper = "Could not open the file manager — the files are still at the path above."; })}
            >
              <FolderOpenIcon data-icon="inline-start" />
              Show in Finder
            </Button>
          </div>
          {#if helper !== null}
            <Alert.Root class="mt-3 border-primary/40 bg-primary/10">
              <CircleAlertIcon />
              <Alert.Description class="text-foreground/80">{helper}</Alert.Description>
            </Alert.Root>
          {/if}
        </div>
      </Card.Footer>
    </Card.Root>
  {/if}

    <AlertDialog.Root bind:open={confirmDiscard}>
      <AlertDialog.Content size="sm">
        <AlertDialog.Header>
          <AlertDialog.Title>Discard and start over?</AlertDialog.Title>
          <AlertDialog.Description>
            The identity is not saved yet{useGenerated && generatedPassphrase !== null ? " and anything generated will be lost" : ""}.
            Starting over leaves nothing behind.
          </AlertDialog.Description>
        </AlertDialog.Header>
        <AlertDialog.Footer>
          <AlertDialog.Cancel>Keep going</AlertDialog.Cancel>
          <AlertDialog.Action
            variant="destructive"
            onclick={() => {
              // bits-ui's Action does not close the root; the caller
              // owns the bound open state.
              confirmDiscard = false;
              onQuit();
            }}
          >
            Discard
          </AlertDialog.Action>
        </AlertDialog.Footer>
      </AlertDialog.Content>
    </AlertDialog.Root>
</div>
