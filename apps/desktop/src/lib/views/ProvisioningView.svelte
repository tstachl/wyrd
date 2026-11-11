<!-- Runtime provisioning: create, join, and leave through the
constrained CLI adapter. Every operation returns the outcome plus
the post-run observation; observed state decides the display, the
outcome only indicates what the operation reported. Secret
contents never travel — credential and invitation inputs are paths
only, picked natively where the dialog capability answers and
entered by hand elsewhere. -->
<script lang="ts">
  import Button from "$lib/components/ui/button.svelte";
  import type { ProvisionReport } from "$lib/ipc/results.js";
  import { pickExistingFile } from "$lib/ipc/files.js";
  import type { ProvisionCreateArgs, ProvisionJoinArgs, ProvisionLeaveArgs } from "$lib/ipc/operations.js";

  interface Props {
    pending: boolean;
    onCreate: (args: ProvisionCreateArgs) => Promise<ProvisionReport>;
    onJoin: (args: ProvisionJoinArgs) => Promise<ProvisionReport>;
    onLeave: (args: ProvisionLeaveArgs) => Promise<ProvisionReport>;
    onDone: () => void;
  }

  let { pending, onCreate, onJoin, onLeave, onDone }: Props = $props();

  let mode: "create" | "join" | "leave" = $state("create");
  let name = $state("");
  let driveDir = $state("");
  let relays = $state("");
  let identityFile = $state("");
  let passphraseFile = $state("");
  let invitationFile = $state("");
  let acknowledged = $state(false);
  let working = $state(false);
  let report: ProvisionReport | null = $state(null);
  let error: string | null = $state(null);

  const busy = $derived(pending || working);

  function relayList(): string[] {
    return relays.split(/[,\s]+/).map((r) => r.trim()).filter((r) => r.length > 0);
  }

  function describeResult(value: ProvisionReport): string {
    const result = value.result;
    switch (result.result) {
      case "verifiedavailable":
        return `Ready: ${result.detail}`;
      case "registeredbutunavailable":
        return `Registered but unavailable: ${result.reason} — the observation below says what actually answers.`;
      case "incomplete":
        return `Incomplete at ${result.stage}: ${result.reason}.`;
      case "helddrive":
        return `Held drive (another process holds it — not retried): ${result.detail}`;
      case "versionmismatch":
        return `Version mismatch (reinstall the matched pair): ${result.reason}`;
      case "busy":
        return `Another operation is already in flight on ${result.drive_dir} — not queued.`;
      case "unknown":
        return `Outcome unknown: ${result.detail}. The observation below decides.`;
    }
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
</script>

<section aria-label="Runtime provisioning" class="mt-6 rounded-md border border-ink/15 px-3 py-2">
  <h2 class="text-sm font-semibold tracking-wide text-smoke uppercase">Create, join, or leave</h2>
  <div class="mt-2 flex gap-2" role="tablist" aria-label="Provisioning operation">
    {#each [["create", "Create"], ["join", "Join"], ["leave", "Leave"]] as [id, label] (id)}
      <Button
        variant={mode === id ? "default" : "secondary"}
        size="sm"
        disabled={busy}
        onclick={() => { mode = id as typeof mode; report = null; error = null; }}
      >
        {label}
      </Button>
    {/each}
  </div>

  {#if mode === "leave"}
    <label class="mt-3 block text-sm">
      Drive directory
      <input class="mt-1 w-full rounded-md border border-ink/15 px-2 py-1" bind:value={driveDir} disabled={busy} placeholder="/drives/alpha" />
    </label>
  {:else}
    <label class="mt-3 block text-sm">
      Drive name <span class="text-smoke">(1–58 chars, lowercase, digits, dashes)</span>
      <input class="mt-1 w-full rounded-md border border-ink/15 px-2 py-1" bind:value={name} disabled={busy} placeholder="alpha" />
    </label>
    <label class="mt-2 block text-sm">
      Drive directory <span class="text-smoke">(absolute, holds the keystore)</span>
      <input class="mt-1 w-full rounded-md border border-ink/15 px-2 py-1" bind:value={driveDir} disabled={busy} placeholder="/drives/alpha" />
    </label>
    <label class="mt-2 block text-sm">
      Relays <span class="text-smoke">(comma-separated; a managed service needs at least one)</span>
      <input class="mt-1 w-full rounded-md border border-ink/15 px-2 py-1" bind:value={relays} disabled={busy} placeholder="wss://relay.example" />
    </label>
    {#if mode === "join"}
      <div class="mt-2 text-sm">
        <p>Sealed invitation file</p>
        <div class="mt-1 flex gap-2">
          <input class="flex-1 rounded-md border border-ink/15 px-2 py-1" bind:value={invitationFile} disabled={busy} placeholder="/tmp/invite.sealed" />
          <Button variant="secondary" size="sm" disabled={busy} onclick={() => void browse((p) => (invitationFile = p), "Sealed invitation file")}>
            Browse…
          </Button>
        </div>
      </div>
    {/if}
  {/if}

  <div class="mt-2 text-sm">
    <p>Identity file <span class="text-smoke">(path only — contents never travel)</span></p>
    <div class="mt-1 flex gap-2">
      <input class="flex-1 rounded-md border border-ink/15 px-2 py-1" bind:value={identityFile} disabled={busy} placeholder="/keys/identity" />
      <Button variant="secondary" size="sm" disabled={busy} onclick={() => void browse((p) => (identityFile = p), "Identity file")}>
        Browse…
      </Button>
    </div>
  </div>
  <div class="mt-2 text-sm">
    <p>Passphrase file <span class="text-smoke">(path only — contents never travel)</span></p>
    <div class="mt-1 flex gap-2">
      <input class="flex-1 rounded-md border border-ink/15 px-2 py-1" bind:value={passphraseFile} disabled={busy} placeholder="/keys/passphrase" />
      <Button variant="secondary" size="sm" disabled={busy} onclick={() => void browse((p) => (passphraseFile = p), "Passphrase file")}>
        Browse…
      </Button>
    </div>
  </div>

  {#if mode === "leave"}
    <label class="mt-3 flex items-start gap-2 text-sm">
      <input type="checkbox" class="mt-1" bind:checked={acknowledged} disabled={busy} />
      <span>
        I understand: leaving as the sole owner is terminal — the drive freezes permanently and no
        future transition can ever be authorized.
      </span>
    </label>
    <Button
      variant="default"
      size="sm"
      disabled={busy || driveDir.trim() === ""}
      onclick={() => void run(() => onLeave({ driveDir, identityFile, passphraseFile, acknowledged }))}
    >
      {working ? "Leaving…" : "Leave drive"}
    </Button>
  {:else if mode === "create"}
    <p class="mt-3 text-xs text-smoke">Service mode: vault (platform default). Mount mode travels with the installer follow-up.</p>
    <Button
      variant="default"
      size="sm"
      disabled={busy || name.trim() === "" || driveDir.trim() === ""}
      onclick={() =>
        void run(() =>
          onCreate({ name, driveDir, relays: relayList(), identityFile, passphraseFile, mode: "vault", noService: false }),
        )}
    >
      {working ? "Creating…" : "Create drive"}
    </Button>
  {:else}
    <p class="mt-3 text-xs text-smoke">Service mode: vault (platform default). Mount mode travels with the installer follow-up.</p>
    <Button
      variant="default"
      size="sm"
      disabled={busy || name.trim() === "" || driveDir.trim() === "" || invitationFile.trim() === ""}
      onclick={() =>
        void run(() =>
          onJoin({
            name,
            driveDir,
            relays: relayList(),
            identityFile,
            passphraseFile,
            invitationFile,
            mode: "vault",
            noService: false,
          }),
        )}
    >
      {working ? "Joining…" : "Join drive"}
    </Button>
  {/if}

  {#if error !== null}
    <p class="mt-3 text-sm" role="alert">The shell backend did not answer: {error}</p>
  {/if}
  {#if report !== null}
    <div class="mt-3 rounded-md bg-ink px-3 py-2 text-sm text-bone" role="status">
      <p>{describeResult(report)}</p>
      <p class="mt-1 text-bone/70">{observedSummary(report)}</p>
    </div>
  {/if}
</section>
