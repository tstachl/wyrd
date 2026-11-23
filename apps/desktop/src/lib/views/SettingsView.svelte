<!-- Settings: this drive's limits as a form, one field per limit.
Absent keys leave Wyrd's default in place, and the shell does not know
that value — so a field renders "Using Wyrd's default", never a
filled-in number. The form is the primary surface for every state;
the settings-file JSON sits behind an Advanced disclosure with no
suggested values. Edits commit one sparse update (set-list plus
clear-list for this drive only) through the installed CLI and the
shared transaction; the saved limits take effect at the drive's next
start, never live.

One removal action ends this device's membership. Its confirmation
offers two choices: keep local data (the `member leave` path) or take
local data too (the installed CLI's `drive delete`). Both are
alternatives, not a sequence — both author this device's removal, so
leaving first makes a later local-data cleanup refuse. The credential
backend is observed from this drive's deployment config through the
installed CLI: a known store account or file pair is pinned, and the
selector appears only where the backend is missing or explicitly
unknown. References only — no secret contents. The modal states both
consequences and takes an explicit choice; the local-data choice adds
a typed-name confirmation. The CLI stops the drive's managed service
after the choice and resumes it on a refusal. -->
<script lang="ts">
  import { Button } from "$lib/components/ui/button/index.js";
  import * as Card from "$lib/components/ui/card/index.js";
  import * as Alert from "$lib/components/ui/alert/index.js";
  import * as Field from "$lib/components/ui/field/index.js";
  import { Input } from "$lib/components/ui/input/index.js";
  import * as AlertDialog from "$lib/components/ui/alert-dialog/index.js";
  import { Badge } from "$lib/components/ui/badge/index.js";
  import { Checkbox } from "$lib/components/ui/checkbox/index.js";
  import { Spinner } from "$lib/components/ui/spinner/index.js";
  import DetailRow from "$lib/components/detail-row.svelte";
  import PageHeader from "$lib/components/page-header.svelte";
  import CopyButton from "$lib/components/copy-button.svelte";
  import InfoHint from "$lib/components/info-hint.svelte";
  import CredentialStorageToggle from "./credential-storage-toggle.svelte";
  import type { Stamped } from "$lib/state/presentation.svelte.js";
  import type { CredentialReference, DriveSettingsState, EnablementState, ProvisionReport, ServiceState, SessionState, SettingsWriteReport, SparseUpdateArgs, WireSoleOwnerState } from "$lib/ipc/results.js";
  import type { ProvisionDeleteArgs, ProvisionLeaveArgs } from "$lib/ipc/operations.js";
  import { buildDeleteArgs, buildLeaveArgs, type CredentialMode } from "$lib/ipc/provisioning-args.js";
  import {
    credentialModeFor,
    describeProvisionResult,
    formatBoundValue,
    membershipSummary,
    observedRegistrySummary,
    parseBoundInput,
    restartKindFor,
    SETTING_BOUNDS,
    storeAccountFor,
  } from "./format.js";
  import {
    buildSparseDraft,
    isDraftDirty,
    keepsDraftAfterVerdict,
    toggleCleared,
  } from "./settings-draft.js";
  import ObservedNote from "./ObservedNote.svelte";
  import FileQuestionIcon from "@lucide/svelte/icons/file-question";
  import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
  import CircleAlertIcon from "@lucide/svelte/icons/circle-alert";

  interface Props {
    driveDir: string;
    /** Registry entry name for this drive; derives the store account label. */
    driveName: string | null;
    snapshot: Stamped<DriveSettingsState> | null;
    /**
     * Advisory owner standing from the last status observation. Null
     * or `Unknown` keeps the conditional explanation; `Sole` carries
     * the warning; `NotSole` gets the short report. Never a gate.
     */
    soleOwner?: WireSoleOwnerState | null;
    /**
     * This drive's credential backend as observed from its deployment
     * config. `null` until observed; an explicit `unknown` answer
     * keeps the selector and names why.
     */
    credentialReference: CredentialReference | null;
    pending: boolean;
    transportNotice: string | null;
    /** The pair the identity gate verified; paths only, never contents. */
    identityFile: string | null;
    passphraseFile: string | null;
    onRefresh: () => void;
    /**
     * End membership and keep local data — the dialog's "Keep local
     * data" choice. Null on a confirmed removal: navigation took
     * over and the card unmounts, so no inline report renders.
     */
    onLeave: (args: ProvisionLeaveArgs) => Promise<ProvisionReport | null>;
    /** End membership and remove local data — the dialog's other choice. Same null-on-advance contract. */
    onDelete: (args: ProvisionDeleteArgs) => Promise<ProvisionReport | null>;
    /** Apply one sparse update; the report carries the outcome plus the post-run observation. */
    onApply: (update: SparseUpdateArgs) => Promise<SettingsWriteReport>;
    /**
     * This drive's session gate (`null` where a service definition
     * owns the drive — managed). Decides how saved bounds take
     * effect via `restartKindFor`, never by this view.
     */
    session: SessionState | null;
    /**
     * Restart the managed service with unchanged supervision so saved
     * bounds apply. Takes the toggle-named backend: store reference or
     * verified pair. Resolves the refusal detail, or null where the
     * restart completed and the post-restart probe answered — the
     * verdict consults the observation, never restart acceptance
     * alone.
     */
    onRestartService: (useStore: boolean) => Promise<string | null>;
    /**
     * The service-manager fact beside the transport verdict, where
     * the registry named it. Decides whether the login-start
     * toggle renders: managed stop states only.
     */
    serviceState?: ServiceState | null;
    /**
     * The login-start preference as the manager records it, where
     * the registry named it. Independent of `serviceState`.
     */
    enablement?: EnablementState | null;
    /**
     * Flip the login-start preference; resolves the refusal detail,
     * or null where the toggle completed and the post-toggle probe
     * answered — the verdict consults the observation, never toggle
     * acceptance alone.
     */
    onToggleEnablement?: (enable: boolean) => Promise<string | null>;
    /** Re-read the registry and return to the drive list. */
    onRefreshDrives: () => void;
  }

  let { driveDir, driveName, snapshot, credentialReference, pending, transportNotice, identityFile, passphraseFile, onRefresh, onLeave, onDelete, onApply, session, onRestartService, serviceState = null, enablement = null, onToggleEnablement, onRefreshDrives, soleOwner = null }: Props = $props();

  const restartKind = $derived(restartKindFor(session));

  const settings = $derived(snapshot?.data ?? null);

  /**
   * The settings-file shape behind the Advanced disclosure. Only
   * keys this drive actually configured appear, with their real
   * values — never suggested numbers that could read as defaults.
   * An unconfigured drive shows an empty section, which is what the
   * file looks like before anything is saved for it.
   */
  const advancedJson = $derived(
    JSON.stringify(
      {
        version: 1,
        drives: {
          [driveDir]: Object.fromEntries(
            SETTING_BOUNDS
              .map((bound) => [bound.key, configuredValue(bound.key)] as const)
              .filter(([, value]) => value !== null),
          ),
        },
      },
      null,
      2,
    ),
  );

  /** The file path is carried only by the nofile/nosection observations. */
  const settingsPath = $derived(
    settings !== null && "path" in settings ? settings.path : null,
  );

  /** Configured value by key for the row being rendered, from the latest observation. */
  function configuredValue(key: string): number | null {
    if (settings === null || settings.state !== "configured") return null;
    switch (key) {
      case "retained_bytes_quota":
        return settings.retained_bytes_quota;
      case "max_admit_per_pass":
        return settings.max_admit_per_pass;
      case "fetch_pass_budget_secs":
        return settings.fetch_pass_budget_secs;
      case "max_mutation_wait_secs":
        return settings.max_mutation_wait_secs;
      case "open_timeout_secs":
        return settings.open_timeout_secs;
      default:
        return null;
    }
  }

  // One removal surface: a two-choice dialog, a typed confirmation for
  // the destructive choice, and one report. The
  // credential backend comes from observation, never from a guess:
  // a known reference fixes both the mode and the reference it
  // carries; an unknown answer keeps the operator selector and its
  // reason. The two dialog choices stay alternatives, not a sequence
  // (see the module comment).
  let confirmRemoval = $state(false);
  let confirmDelete = $state(false);
  let deleteConfirmName = $state("");
  let removalWorking = $state(false);
  let removalReport = $state<ProvisionReport | null>(null);
  let removalError = $state<string | null>(null);
  let overrideMode = $state<CredentialMode>("store");

  const backendKnown = $derived(
    credentialReference !== null && credentialReference.backend !== "unknown",
  );
  const credentialMode = $derived(credentialModeFor(credentialReference, overrideMode));
  const storeAccount = $derived(
    credentialReference?.backend === "os-store"
      ? credentialReference.account
      : driveName === null
        ? null
        : storeAccountFor(driveName),
  );
  // Observed paths are this drive's own reference; the verified pair
  // from onboarding is only the fallback for an unobserved drive.
  const effectiveIdentityFile = $derived(
    credentialReference?.backend === "file"
      ? credentialReference.identityFile
      : identityFile,
  );
  const effectivePassphraseFile = $derived(
    credentialReference?.backend === "file"
      ? credentialReference.passphraseFile
      : passphraseFile,
  );
  const fileCredentialsReady = $derived(
    effectiveIdentityFile !== null && effectivePassphraseFile !== null,
  );
  const credentialsReady = $derived(
    credentialMode === "store" ? storeAccount !== null : fileCredentialsReady,
  );

  // Draft: one pending sparse update for this drive. `inputs` holds
  // each field's human-unit text (empty means no change), `cleared`
  // names keys the operator returned to Wyrd's default. Validation
  // runs over the whole draft on save — no partial writes — and Cancel
  // discards it untouched.
  let inputs = $state<Record<string, string>>({});
  let cleared = $state<string[]>([]);
  let fieldErrors = $state<Record<string, string>>({});
  let saving = $state(false);
  let saveReport = $state<SettingsWriteReport | null>(null);
  let saveError = $state<string | null>(null);

  /** Configured values by key for the draft helpers, from the latest observation. */
  function configuredRecord(): Record<string, number | null> {
    const record: Record<string, number | null> = {};
    for (const bound of SETTING_BOUNDS) {
      record[bound.key] = configuredValue(bound.key);
    }
    return record;
  }

  function resetDraft(): void {
    inputs = {};
    cleared = [];
    fieldErrors = {};
    saveReport = null;
    saveError = null;
  }

  /** A typed value takes the field off its default and clears its error. */
  function setInput(key: string, text: string): void {
    inputs = { ...inputs, [key]: text };
    if (cleared.includes(key)) cleared = toggleCleared(cleared, key);
    if (fieldErrors[key] !== undefined) {
      const next = { ...fieldErrors };
      delete next[key];
      fieldErrors = next;
    }
  }

  /** Per-field reset: empty text always, plus a clear only for a key that was set. */
  function resetBound(key: string): void {
    const next = { ...inputs };
    delete next[key];
    inputs = next;
    const errors = { ...fieldErrors };
    delete errors[key];
    fieldErrors = errors;
    if (configuredValue(key) !== null && !cleared.includes(key)) {
      cleared = toggleCleared(cleared, key);
    }
  }

  function toggleClear(key: string): void {
    const wasCleared = cleared.includes(key);
    cleared = toggleCleared(cleared, key);
    if (!wasCleared) {
      const next = { ...fieldErrors };
      delete next[key];
      fieldErrors = next;
    }
  }

  const dirty = $derived(
    isDraftDirty(
      inputs,
      cleared,
      configuredRecord(),
      SETTING_BOUNDS.map((bound) => bound.key),
    ),
  );

  async function save(): Promise<void> {
    const draft = buildSparseDraft(
      inputs,
      cleared,
      configuredRecord(),
      SETTING_BOUNDS.map((bound) => bound.key),
      parseBoundInput,
    );
    const { set, clear, errors } = draft;
    fieldErrors = errors;
    if (Object.keys(errors).length > 0) return;
    saving = true;
    saveError = null;
    restartReport = null;
    try {
      // The report carries the outcome; the paired re-read behind
      // `onApply` decides what the display says. Committed verdicts
      // discard the draft, uncommitted ones keep it for retry.
      saveReport = await onApply({ set, clear });
      if (!keepsDraftAfterVerdict(saveReport.result.result)) {
        // Keep the report: it carries the apply/restart verdict.
        inputs = {};
        cleared = [];
        fieldErrors = {};
      }
    } catch (failure: unknown) {
      saveError = failure instanceof Error ? failure.message : String(failure);
      saveReport = null;
    } finally {
      saving = false;
    }
  }

  /**
   * Restart-to-apply for a managed drive: saved bounds are inert
   * until the owner restarts, and the restart interrupts
   * availability — so it runs behind an explicit confirmation, and
   * the outcome renders as applied-on-restartup vs
   * saved-but-restart-failed from the post-restart observation, not
   * from restart acceptance. Null detail means the restart
   * completed; observed state decides the rest.
   */
  let confirmRestart = $state(false);
  let restarting = $state(false);
  let restartReport = $state<{ ok: boolean; detail?: string } | null>(null);
  let restartUseStore = $state(true);
  let toggling = $state(false);
  let toggleReport = $state<{ ok: boolean; detail?: string } | null>(null);

  const busy = $derived(pending || removalWorking || saving || restarting || toggling);

  async function toggleEnablement(enable: boolean): Promise<void> {
    if (onToggleEnablement === undefined) return;
    toggling = true;
    toggleReport = null;
    try {
      const detail = await onToggleEnablement(enable);
      toggleReport = detail === null ? { ok: true } : { ok: false, detail };
    } finally {
      toggling = false;
    }
  }

  async function restartService(): Promise<void> {
    confirmRestart = false;
    restarting = true;
    restartReport = null;
    try {
      const detail = await onRestartService(restartUseStore);
      restartReport = detail === null ? { ok: true } : { ok: false, detail };
    } finally {
      restarting = false;
    }
  }

  /**
   * Carry the one acknowledgement and credential reference into the
   * chosen removal command. The dialog's choice selects only whether
   * local data goes too; both author this device's removal. The
   * report carries the post-run observation — only that decides what
   * changed, a transport throw is not an outcome.
   */
  async function remove(keepLocalData: boolean): Promise<void> {
    if (!credentialsReady) return;
    if (credentialMode === "store" && storeAccount === null) return;
    removalWorking = true;
    removalError = null;
    const args = {
      driveDir,
      credentialMode,
      storeAccount,
      identityFile: effectiveIdentityFile,
      passphraseFile: effectivePassphraseFile,
      acknowledged: true,
    };
    try {
      removalReport = keepLocalData
        ? await onLeave(buildLeaveArgs(args))
        : await onDelete(buildDeleteArgs(args));
    } catch (failure: unknown) {
      removalError = failure instanceof Error ? failure.message : String(failure);
    } finally {
      removalWorking = false;
    }
  }
</script>

{#snippet credentialsHint()}
  <p>
    The pair this device's identity gate verified. Only the paths are passed to the CLI; the
    files' contents never enter the shell.
  </p>
{/snippet}

<PageHeader
  title="Settings"
  size="section"
  description="These settings let you change the limits for this drive. Any setting you leave unchanged uses Wyrd's default. The app cannot see that default value, so it does not show a number."
>
  {#snippet actions()}
    <Button variant="outline" size="sm" disabled={pending} onclick={onRefresh}>
      {#if pending}
        <Spinner data-icon="inline-start" />
        Reading…
      {:else}
        <RefreshCwIcon data-icon="inline-start" />
        Refresh
      {/if}
    </Button>
  {/snippet}
</PageHeader>

{#if transportNotice !== null}
  <Alert.Root class="mt-4 border-primary/40 bg-primary/10">
    <p>{transportNotice}</p>
  </Alert.Root>
{/if}

{#if session === null && onToggleEnablement !== undefined && serviceState !== null && serviceState.state !== "notinstalled" && serviceState.state !== "unsupported"}
  {@const toggleChecked = enablement !== null && enablement.enablement === "enabled"}
  {@const toggleUnknown = enablement === null || enablement.enablement === "unknown"}
  <Card.Root class="mt-6">
    <Card.Header>
      <Card.Title class="text-sm">Start at login</Card.Title>
      <Card.Description>
        Start this drive automatically when you log in. Turning this off doesn't delete anything.
      </Card.Description>
    </Card.Header>
    <Card.Content class="flex flex-col gap-3">
      <Field.Group>
        <Field.Field orientation="horizontal">
          <Checkbox
            id="settings-auto-start-toggle"
            checked={toggleChecked}
            indeterminate={toggleUnknown}
            disabled={busy}
            onCheckedChange={(next) => {
              if (typeof next === "boolean") void toggleEnablement(next);
            }}
          />
          <Field.Label for="settings-auto-start-toggle" class="font-normal">
            Start drive service at login
          </Field.Label>
        </Field.Field>
      </Field.Group>
      {#if toggleReport !== null}
        {#if toggleReport.ok}
          <p class="text-xs text-muted-foreground">
            Saved — this takes effect after the next check.
          </p>
        {:else}
          <Alert.Root variant="destructive">
            <p>{toggleReport.detail}</p>
          </Alert.Root>
        {/if}
      {/if}
    </Card.Content>
  </Card.Root>
{/if}

{#snippet saveAlerts()}
{#if saveReport !== null && saveReport.result.result === "applied"}
  <Alert.Root class="mt-6 border-primary/40 bg-primary/10">
    {#if restartKind === "service"}
      <Alert.Title>Limits saved — restart to apply</Alert.Title>
      <Alert.Description>
        <p>The drive service keeps its startup values until it restarts. Restarting briefly interrupts availability.</p>
        <div class="mt-3 flex flex-wrap items-center gap-2">
          <Button variant="outline" size="sm" disabled={busy} onclick={() => (confirmRestart = true)}>
            {#if restarting}
              <Spinner data-icon="inline-start" />
              Restarting…
            {:else}
              Restart service
            {/if}
          </Button>
        </div>
        {#if restartReport !== null}
          {#if restartReport.ok}
            <p class="mt-2">Service restarted and the drive is answering — the saved values apply from this start.</p>
          {:else}
            <p class="mt-2">Saved, but the restart failed: {restartReport.detail} — the saved values wait for the next start.</p>
          {/if}
        {/if}
      </Alert.Description>
    {:else if restartKind === "session"}
      <Alert.Title>Limits saved — lock and unlock to apply</Alert.Title>
      <Alert.Description>
        <p>The drive keeps its startup values until it restarts. Use Lock in the header to end this session, then unlock with the passphrase to start it again with the saved values.</p>
      </Alert.Description>
    {:else}
      <p>Limits saved. They apply the next time the drive starts — the current values stay in effect until then.</p>
    {/if}
  </Alert.Root>
{/if}
{#if saveReport !== null && saveReport.result.result === "unverified"}
  <Alert.Root class="mt-6 border-primary/40 bg-primary/10">
    <Alert.Title>Saved but unverified</Alert.Title>
    <Alert.Description>
      <p>{saveReport.result.reason} — the write landed; the observation below is the fresh re-read.</p>
    </Alert.Description>
  </Alert.Root>
{/if}
{#if saveReport !== null && saveReport.result.result !== "applied" && saveReport.result.result !== "unverified"}
  <Alert.Root
    variant={saveReport.result.result === "conflict" ? "default" : "destructive"}
    class="mt-6"
  >
    <Alert.Title>
      {#if saveReport.result.result === "conflict"}
        Settings changed under you
      {:else if saveReport.result.result === "refused"}
        Save refused
      {:else}
        Save failed
      {/if}
    </Alert.Title>
    <Alert.Description>
      {#if saveReport.result.result === "conflict"}
        <p>{saveReport.result.reason} — your draft is kept; save again to retry it against the current file.</p>
      {:else if saveReport.result.result === "refused"}
        <p>{saveReport.result.reason}</p>
      {:else}
        <p>{saveReport.result.reason}</p>
      {/if}
    </Alert.Description>
  </Alert.Root>
{/if}
{#if saveError !== null}
  <Alert.Root variant="destructive" class="mt-6">
    <Alert.Title>Save failed</Alert.Title>
    <Alert.Description>{saveError}</Alert.Description>
  </Alert.Root>
{/if}
{/snippet}

{#if snapshot === null || settings === null}
  <div class="mt-6 flex flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border bg-card/50 px-6 py-14 text-center">
    <div class="flex size-10 items-center justify-center rounded-full bg-muted text-muted-foreground">
      <FileQuestionIcon class="size-5" />
    </div>
    <p class="text-sm font-medium">No settings read yet</p>
    <p class="max-w-sm text-sm text-muted-foreground">
      Refresh to read this drive's limits. Nothing is filled in from memory.
    </p>
  </div>
  <div class="mt-3">
    <ObservedNote observedAtMs={null} />
  </div>
{:else if settings.state !== "unreadable"}
  {@render saveAlerts()}
  <Card.Root class="mt-6">
    <Card.Header>
      <Card.Title class="flex items-center gap-1.5">
        Limits for this drive
        <InfoHint label="About these limits">
          <p>
            Set each limit here, or return it to Wyrd's default. Every change is checked before
            anything is saved — there are no partial saves. Saved limits take effect the next time
            this drive starts; they do not change a drive that is already running.
          </p>
        </InfoHint>
      </Card.Title>
      <Card.Description>
        Changes take effect the next time this drive starts — they don't change a running drive.
      </Card.Description>
    </Card.Header>
    <Card.Content>
      <dl>
        {#each SETTING_BOUNDS as bound (bound.key)}
          {@const hasError = fieldErrors[bound.key] !== undefined}
          {@const current = configuredValue(bound.key)}
          <DetailRow label={bound.label} description={bound.description}>
            {#if cleared.includes(bound.key)}
              <span class="inline-flex items-center justify-end gap-2">
                <Badge variant="outline" class="text-muted-foreground">Using Wyrd's default</Badge>
                <Button variant="ghost" size="sm" disabled={busy} onclick={() => toggleClear(bound.key)}>
                  Change
                </Button>
              </span>
            {:else}
              <Field.Field data-invalid={hasError || undefined}>
                <span class="inline-flex items-center justify-end gap-1.5">
                  <Input
                    class="w-32 font-mono tabular-nums"
                    inputmode={bound.inputScale === 1 ? "numeric" : "decimal"}
                    autocomplete="off"
                    spellcheck={false}
                    disabled={busy}
                    placeholder="Using Wyrd's default"
                    value={inputs[bound.key] ?? formatBoundValue(bound, current)}
                    oninput={(event) => setInput(bound.key, event.currentTarget.value)}
                    aria-label={bound.label}
                    aria-invalid={hasError || undefined}
                  />
                  <span class="shrink-0 text-xs text-muted-foreground">{bound.inputUnit}</span>
                  {#if current !== null || (inputs[bound.key] ?? "") !== ""}
                    <Button variant="ghost" size="sm" disabled={busy} onclick={() => resetBound(bound.key)}>
                      Use Wyrd's default
                    </Button>
                  {/if}
                </span>
                {#if fieldErrors[bound.key]}
                  <Field.Error>{fieldErrors[bound.key]}</Field.Error>
                {/if}
              </Field.Field>
            {/if}
          </DetailRow>
        {/each}
      </dl>
      <details class="mt-4 rounded-lg border border-border/60 px-3 py-2">
        <summary class="cursor-pointer text-sm text-muted-foreground">
          Advanced: edit the settings file directly
        </summary>
        <div class="mt-3 flex flex-col gap-2">
          {#if settingsPath !== null}
            <p class="text-xs leading-relaxed text-muted-foreground">
              Saved limits live at
              <span class="inline-flex min-w-0 items-center gap-1.5 align-middle">
                <span class="break-all font-mono text-xs">{settingsPath}</span>
                <CopyButton text={settingsPath} label="settings file path" />
              </span>.
            </p>
          {:else}
            <p class="text-xs leading-relaxed text-muted-foreground">
              Saved limits live in this drive's per-user settings file.
            </p>
          {/if}
          <p class="text-xs leading-relaxed text-muted-foreground">
            Storage is entered in GB, where 1 GB means 1,073,741,824 bytes; the file stores that
            byte count. Every key is optional — leave one out to use Wyrd's default.
          </p>
          <pre class="w-full overflow-auto rounded-lg border border-border bg-ink px-3 py-2 text-left text-xs break-words whitespace-pre-wrap text-bone">{advancedJson}</pre>
        </div>
      </details>
    </Card.Content>
    <Card.Footer class="justify-end gap-2">
      <Button variant="outline" size="sm" disabled={busy || !dirty} onclick={resetDraft}>
        Cancel
      </Button>
      <Button size="sm" disabled={busy || !dirty} onclick={() => void save()}>
        {#if saving}
          <Spinner data-icon="inline-start" />
          Saving…
        {:else}
          Save limits
        {/if}
      </Button>
    </Card.Footer>
  </Card.Root>
{:else if settings.state === "unreadable"}
  <Alert.Root variant="destructive" class="mt-6">
    <p>
      Settings unreadable: {settings.reason} — showing nothing rather than defaults that were never
      configured.
    </p>
  </Alert.Root>
{/if}

<div class="mt-4">
  <ObservedNote observedAtMs={snapshot?.observedAtMs ?? null} />
</div>

<Card.Root class="mt-6 ring-destructive/25">
  <Card.Header>
    <Card.Title class="flex items-center gap-2">
      <CircleAlertIcon class="size-4 text-destructive" />
      Remove this drive
    </Card.Title>
    <Card.Description>
      End this device's membership. Choose whether to keep this drive's local data on the device.
    </Card.Description>
  </Card.Header>
  <Card.Content class="flex flex-col gap-4">
    <!-- The backend is observed, not chosen. Show the selector only
      when the observation is missing or explicitly unknown: a known
      store account or configured file pair is pinned by the drive's
      own deployment config. Toggle extracted to a child component:
      sibling interactive templates inline here break the happy-dom
      re-registration the mounted tests rely on. -->
    {#if !backendKnown}
      <CredentialStorageToggle
        useStore={overrideMode === "store"}
        onSelect={(next) => (overrideMode = next ? "store" : "file")}
      />
      {#if credentialReference?.backend === "unknown"}
        <p class="text-xs leading-relaxed text-muted-foreground">
          This drive's credential backend could not be observed: {credentialReference.reason}. Choose one below.
        </p>
      {:else}
        <p class="text-xs leading-relaxed text-muted-foreground">
          This drive's credential backend has not been observed yet. Choose one below.
        </p>
      {/if}
    {/if}
    {#if credentialMode === "store"}
      <p class="text-xs leading-relaxed text-muted-foreground">
        Wyrd keeps one credential entry for this drive, named from its drive name. No secret is
        shown here.
      </p>
    {:else}
      <p class="text-xs leading-relaxed text-muted-foreground">
        {#if backendKnown}
          This drive uses the credential files its configuration names; only the paths pass to the CLI.
        {:else}
          Operator deployment: use the credential paths below.
        {/if}
      </p>
    {/if}
    {#if credentialMode === "file"}
      <dl>
        <DetailRow label="Identity file" hint={credentialsHint} hintLabel="About the credential paths">
          <span class="inline-flex min-w-0 items-center gap-1.5">
            {#if effectiveIdentityFile === null}
              <span class="text-muted-foreground">Unknown</span>
            {:else}
              <span class="truncate font-mono text-xs">{effectiveIdentityFile}</span>
            {/if}
          </span>
        </DetailRow>
        <DetailRow label="Passphrase file" hint={credentialsHint} hintLabel="About the credential paths">
          <span class="inline-flex min-w-0 items-center gap-1.5">
            {#if effectivePassphraseFile === null}
              <span class="text-muted-foreground">Unknown</span>
            {:else}
              <span class="truncate font-mono text-xs">{effectivePassphraseFile}</span>
            {/if}
          </span>
        </DetailRow>
      </dl>
    {/if}

    {#if removalReport !== null}
      <Alert.Root class="border-primary/30 bg-primary/5">
        <CircleAlertIcon />
        <Alert.Title>Removal report</Alert.Title>
        <Alert.Description class="space-y-1 text-foreground/80 [&_p:not(:last-child)]:mb-1">
          <p>{describeProvisionResult(removalReport)}</p>
          <p>{observedRegistrySummary(removalReport)}</p>
          <p>{membershipSummary(removalReport)}</p>
        </Alert.Description>
      </Alert.Root>
    {/if}
    {#if removalError !== null}
      <Alert.Root variant="destructive">
        <CircleAlertIcon />
        <Alert.Title>Removal failed</Alert.Title>
        <Alert.Description>{removalError}</Alert.Description>
      </Alert.Root>
    {/if}

    {#if removalReport === null && !credentialsReady}
      <p class="text-sm text-destructive">
        {#if credentialMode === "store"}
          This drive has no name to find its credentials with, so removal is disabled.
        {:else}
          This device's verified identity pair is unavailable, so removal is disabled. Fix the
          identity gate first.
        {/if}
      </p>
    {/if}
  </Card.Content>
  <Card.Footer class="justify-end">
    {#if removalReport !== null}
      <!-- A confirmed removal already navigated away; this card keeps
        the report only for a refusal or uncertain outcome, with a
        prominent way back that never discards the text above. -->
      <Button variant="outline" size="sm" disabled={busy} onclick={onRefreshDrives}>
        <RefreshCwIcon data-icon="inline-start" />
        Back to drives
      </Button>
    {:else}
      <Button
        variant="destructive"
        size="sm"
        disabled={busy || !credentialsReady}
        onclick={() => (confirmRemoval = true)}
      >
        {#if removalWorking}
          <Spinner data-icon="inline-start" />
          Removing…
        {:else}
          <CircleAlertIcon data-icon="inline-start" />
          Remove this drive…
        {/if}
      </Button>
    {/if}
  </Card.Footer>
</Card.Root>

<AlertDialog.Root bind:open={confirmRemoval}>
  <AlertDialog.Content>
    <AlertDialog.Header>
      <AlertDialog.Title>Remove this drive?</AlertDialog.Title>
      <AlertDialog.Description>
        Choose what happens to this drive's local data. Both choices end this device's membership.
      </AlertDialog.Description>
    </AlertDialog.Header>
    <!-- Two alternatives, not a sequence: each authors this device's
      removal. The three lines keep the strong consequence copy; the
      shared fine print below holds the conditional and terminal facts. -->
    <div class="grid gap-3 py-2 sm:grid-cols-2">
      <button
        type="button"
        disabled={busy}
        class="flex flex-col rounded-xl border p-3 text-left hover:border-foreground/40 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-50"
        onclick={() => {
          confirmRemoval = false;
          void remove(true);
        }}
      >
        <span class="text-base font-medium">Keep local data</span>
        <span class="mt-2 flex flex-col gap-1 text-sm leading-relaxed text-foreground/90">
          <span><strong class="text-foreground">On this device.</strong> This drive's files and settings stay here.</span>
          <span><strong class="text-foreground">For other devices.</strong> No other member's copy changes.</span>
          <span><strong class="text-foreground">Can it be undone.</strong> Not here: removing this drive ends this device's membership going forward. It does not erase copies this device already obtained. Another device must admit this one again.</span>
        </span>
      </button>
      <button
        type="button"
        disabled={busy || driveName === null}
        class="flex flex-col rounded-xl border border-destructive/40 p-3 text-left hover:border-destructive focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-50"
        onclick={() => {
          confirmRemoval = false;
          deleteConfirmName = "";
          confirmDelete = true;
        }}
      >
        <span class="text-base font-medium text-destructive">Remove local data too</span>
        <span class="mt-2 flex flex-col gap-1 text-sm leading-relaxed text-foreground/90">
          <span><strong class="text-foreground">On this device.</strong> This takes the drive's local data off the device — the directory, registry entry, service definition, credential item and group socket and label. The removed files' blocks stay recoverable from this device until overwritten, like any removed file.</span>
          <span><strong class="text-foreground">For other devices.</strong> No other member's copy changes.</span>
          <span><strong class="text-foreground">Can it be undone.</strong> Not here: removing this drive ends this device's membership going forward. It does not erase copies this device already obtained. Local data removed here is not restored by the drive.</span>
        </span>
      </button>
    </div>
    {#if driveName === null}
      <p class="mt-1 text-sm text-destructive">
        This drive has no name to confirm, so removing its local data is unavailable.
      </p>
    {/if}
    <p class="mt-1 text-xs leading-relaxed text-foreground/80">
      Note that the drive's service is stopped for this action and restarted if the removal is
      refused. A non-owner or a frozen drive is refused before anything is removed.
    </p>
    {#if soleOwner === "Sole"}
      <Alert.Root variant="destructive" class="mt-3">
        <Alert.Title>As last reported, this device is the drive's only owner</Alert.Title>
        <Alert.Description>
          Removing it empties the owner set and no future membership or ownership change can be
          authorized — hand over ownership before removing if the drive should remain manageable.
        </Alert.Description>
      </Alert.Root>
    {:else if soleOwner === "NotSole"}
      <p class="mt-1 text-xs leading-relaxed text-foreground/80">
        As last reported, other devices also own this drive.
      </p>
    {:else}
      <p class="mt-1 text-xs leading-relaxed text-foreground/80">
        If this device is the drive's only owner, the owner set becomes empty and no future
        membership or ownership change can be authorized — hand over ownership before removing if
        the drive should remain manageable.
      </p>
    {/if}
    <AlertDialog.Footer>
      <AlertDialog.Cancel>Cancel</AlertDialog.Cancel>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>

<AlertDialog.Root bind:open={confirmDelete}>
  <AlertDialog.Content>
    <AlertDialog.Header>
      <AlertDialog.Title>Type the drive name to confirm</AlertDialog.Title>
      <AlertDialog.Description>
        This removes this drive's local data from this device and cannot be undone here. Type
        <span class="font-mono">{driveName ?? "this drive"}</span> to confirm.
      </AlertDialog.Description>
    </AlertDialog.Header>
    <div class="py-2">
      <Input
        id="remove-confirm-name"
        bind:value={deleteConfirmName}
        disabled={busy}
        autocomplete="off"
        spellcheck={false}
        aria-label="Drive name to confirm"
      />
    </div>
    <AlertDialog.Footer>
      <AlertDialog.Cancel>Cancel</AlertDialog.Cancel>
      <AlertDialog.Action
        variant="destructive"
        disabled={busy || driveName === null || deleteConfirmName !== driveName}
        onclick={() => {
          // bits-ui's Action does not close the root; close it so
          // the report lands in the card, not behind a modal.
          confirmDelete = false;
          void remove(false);
        }}
      >
        Remove local data
      </AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>

<AlertDialog.Root bind:open={confirmRestart}>
  <AlertDialog.Content>
    <AlertDialog.Header>
      <AlertDialog.Title>Restart the service?</AlertDialog.Title>
      <AlertDialog.Description>
        Restarting briefly interrupts availability while the drive service stops and starts again. The limits you saved apply when it comes back.
      </AlertDialog.Description>
      <div class="py-2">
        <CredentialStorageToggle
          useStore={restartUseStore}
          onSelect={(next) => (restartUseStore = next)}
        />
        {#if restartUseStore}
          <p class="mt-2 text-sm text-muted-foreground">
            Restarts the drive service against this drive's store item.
          </p>
        {:else}
          <p class="mt-2 text-sm text-muted-foreground">
            Operator deployments only: restarts against the verified identity pair.
          </p>
        {/if}
      </div>
    </AlertDialog.Header>
    <AlertDialog.Footer>
      <AlertDialog.Cancel>Not now</AlertDialog.Cancel>
      <AlertDialog.Action
        disabled={busy}
        onclick={() => {
          // bits-ui's Action does not close the root; close it so
          // the report lands in the card, not behind a modal.
          void restartService();
        }}
      >
        Restart service
      </AlertDialog.Action>
    </AlertDialog.Footer>
  </AlertDialog.Content>
</AlertDialog.Root>
