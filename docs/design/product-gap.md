# Product gap: Dropbox-class experience from a content-addressed store

Status: Proposed / Open. This note owns product gaps, not protocol design.
Normative invariants live in the contracts linked below. Nothing here
authorizes a contract change.

Central thesis: Wyrd can have a superior storage model and still lose if
ordinary operations require users to understand the storage model. The gap
is translation, not primitives.

This note rescopes the recent "best decentralized Dropbox" critique into
measurable work. Architectural proposals in the critique (epoch-scoped
sharing, guardian UX, transport redesign) stay proposals. They do not
become roadmap until a separate contract settles them.

## Edge cases first: trust and onboarding questions

These are not "features." They are the questions a first-time user asks in
order. Each must have a product answer before its implementation is judged
done.

1. What is a drive? Which devices hold complete copies? What happens when
   my laptop closes?
2. Where does my data live? If I run an always-on vault at home, what does
   that change?
3. How do I invite another person?
4. Who can read my data, delete it, block access? What happens if a device
   is compromised?
5. How do I recover if I lose every device?

A document skill or doc-gap pass covers obvious contradictions. User
comprehension does not need post-hoc edits to misleading prose — it needs
the answers above to have a surface, a flow, and a test.

Completion mechanism (small, repeatable): after creating a drive, the
user can answer from the app alone — in their own words, without
architecture vocabulary:

- Which devices have complete copies?
- What happens when this computer goes offline?
- Can the vault operator read my files?
- Can I recover if I lose this computer?

Displaying text is not sufficient. If the user cannot answer, first-run
is not complete.

## Definitions

- **Commit.** A local mutation is durable when the owner reports success
  after fsync. See `../contracts/crash-consistency.md` for per-subsystem
  windows.
- **Materialization.** What a device holds locally. See
  `docs/contracts/sync-and-peers.md` (materialization policy vs peer
  role) and `../contracts/object-model.md` (content vs storage identity).
- **Staging vs admitted.** Abandoned staging data (never durably
  admitted) is tracked separately from retained bytes. Do not invent a new
  lifecycle state for GC; use the transaction state the store already
  exposes. See `docs/contracts/retention-constraints.md` Invariants 1-4
  and `docs/design/garbage-collection.md` (pre-v1 slice rule).

## Packaged gaps, in dependency order

### 1. Owner lifecycle and actionable budgets (v0.4)

Goal: source-of-truth status a person can act on.

- **Owner dimensions** (visible, not log-only — do not collapse to one `stale` enum):

  | Dimension | Observation |
  |---|---|
  | Owner process | `available` / `unavailable` / `unknown` |
  | Local operations | `available` / `unavailable` |
  | Remote delivery | `connected` / `degraded` / `unavailable` / `unknown` |
  | Local data | `available` / `partially materialized` / `unavailable` |

  Human-readable status is derived from these rows. Example: `Available offline` = owner running, local work possible, remote delivery unavailable. Failed probe is not proof the owner died (see `../contracts/sync-and-peers.md` forget-contract epistemic limits).
- **Storage accounting** (report-only first, from existing probes):
  Do not let the UI inherit the GC accounting model simply because both
  discuss storage. Distinguish:
  - *GC accounting units* (internal, in `storage-growth.md` / GC note):
    retained, ciphertext, structural metadata, orphan bytes.
  - *Product measurements* (user-visible): logical retained content,
    materialized content, structural metadata, cache, physical
    ciphertext, total filesystem consumption for the drive directory.
  Do not sum rows across the two groups; the source is probes, not UI
  arithmetic. See `wyrd policy budgets` / `wyrd policy cache status`
  (materialization policy vs capacity caps).
  - First release is report-only. A user can safely evict locally cached content under local policy; they cannot infer retained content is globally reclaimable. A separate narrowly-scoped cache-evict op is allowed only where already contractually supported.

Acceptance: v0.4 minimum (must ship for a usable first desktop release)
is owner availability + local offline operation, basic report-only
storage measurements, safe install/upgrade/restart/uninstall, and
conflict detection where existing reconciliation evidence supports it.
Full consumer-friendly storage recommendations, advanced conflict
resolution, and comprehensive cross-platform packaging are longer-term
and do not block that minimum. This is a scheduling split, not a
weakening of guarantees.
- **Candidate problems for review:** desktop boot while offline, owner
  process death, malformed membership announcements surfaced as
  `unknown` instead of "sync failed."

Acceptance:
- v0.4 desktop shows owner status and can complete local work offline.
- `wyrd policy budgets` numbers match a scenario: 200 GB library,
  2 GB pinned, cold start, steady state, eviction, restart.

### 2. Conflict visibility and naming (v0.4-0.5)

Goal: detection when sufficient evidence exists, resolution later by an inspectable
snapshot flow. Do not couple UI to Merkle DAG internals.

- **Indicator:** multi-head state is visible as soon as the local system
  has sufficient evidence of multiple heads (reconcile state,
  `sync-and-peers.md` forget contract). A local `write()` alone does not
  know a remote peer concurrently wrote. Absence of an indicator does not
  prove no concurrent remote edit. Never auto-merge.
- **Resolve:** version addresses via `@N` lookup (never listed by
  `readdir`); export shows `name@N` siblings. Resolution authoring is
  v0.5 — see `docs/design/cli.md` conflict-version lookup. The desktop
  surface uses author/time context and preserves both versions; it does
  not require users to understand version numbering.
- **Guidepost:** "conflict = two heads" is a category error for users.
  Surface as "we have two versions of this file" with author + time-ordered
  display and a keep-both choice.

Acceptance:
- Concurrent edits produce an indicator in the desktop app once heads are
  known, plus an inspectable `wyrd snapshot conflict` evidence trail.
- Resolving a conflict is a new durable snapshot. It never erases the
  competing snapshot or rewrites history.
- `name@N` browsing works from the filesystem surface without
  understanding SnapshotId ordering.

### 3. Fresh-device bootstrap and snapshot restore (v0.5 compound)

Goal: a person with a new phone discovers the drive, sees its structure,
and materializes only a subset.

- **Metadata-first:** manifest/tree knowledge arrives before bulk data
  (two-phase arrival, `sync-and-peers.md`).
- **Selective first-run:** materialization policy is applied at first
  sync; pinning is a local decision, not a drive mutation.
- **Restore:** browsing arbitrary historical snapshots + restoring previous
  versions is v0.5 work (`ROADMAP.md:123-130`). Do not claim it earlier.

Acceptance:
- New device with metadata only + 2GB pinned completes first-run sync in
  bounded time and can open pinned files. A bound needs both logical
  bytes and object/path count (metadata footprint varies with file count
  and directory structure, not DB size alone).
- Interrupted bootstrap is tested: relay unavailable during discovery,
  vault unavailable after metadata, peer disappears mid pinned-content,
  restart after partial download, materialization change mid-bootstrap,
  insufficient local capacity. Partial bootstrap is recoverable and must
  never look like a complete library.
- Snapshot restore is user-visible and restorable, not just export dump.

### 4. Remote access performance contracts (v0.6)

Goal: contracts, not anecdotes. Three separates:

| Operation | Desired property |
|---|---|
| Warm local read | Filesystem/cache latency, no network |
| Cold range read | Bounded time-to-first-byte, vault-down fails soft, locally pinned content keeps working |
| Large sequential fetch | Sustained throughput, parallel chunk acquisition, resume after interruption, backpressure |

Acceptance:
- iroh range / Bao resume benchmarks published for direct connection,
  relay-mediated, CGNAT, vault-unavailable, and interrupted-transfer-
  followed-by-restart cases, with: time-to-first-byte, sustained
  throughput, peak RSS, temp disk, recovery time after interruption,
  network bytes transferred. Target scenarios must separate warm local
  cache from warm remote connection.
- Vault-unavailable does not invalidate already materialized content.
  Locally pinned content keeps working; only remote misses fail — and
  they fail explicitly, without cascading FUSE invalidation.

### 5. Time-travel demo (v0.5 public evidence)

Goal: reproducible release evidence, not architecture diagrams.

Sequence: browse a previous snapshot → open previous version → restore
one file → restore directory → concurrent-edit preservation.

Acceptance:
- 2-minute walk-through reproducible from a clean snapshot.
- Restore authors a new forward state. Previously authored snapshots
  remain immutable; restore does not silently delete or rewrite competing
  history. Retention determines whether older content remains available;
  the UI must not imply indefinite availability unless guaranteed.

### 6. Installation and upgrade lifecycle (v0.4)

Goal: predictable behavior across application lifecycle, not just daemon
lifecycle.

- Cover: initial install, drive creation, first login/start, sleep/wake,
  application upgrade, owner restart, uninstallation, reinstall.
- Uninstallation must not imply deletion of the durable store.
- Upgrade / reinstall preserves the existing drive and the owner recovers
  state without reinitializing.

Acceptance:
- Upgrade and reinstall test preserves `wyrd export` round-trip (files,
  permissions, symlinks, exec bit) and the store's crash-consistency
  posture per `../contracts/crash-consistency.md`.
- Existing drive identity and membership remain intact; existing
  snapshots and retained objects remain accessible; pending durable
  obligations (outbox obligations, undelivered announcements) survive
  restart; no reinitialization or implicit destructive migration occurs.
- Uninstall leaves the store intact (explicitly separate from any
  `wyrd vault` retention decision).

### 7. Differentiator verification (v0.9 soak)

Goal: separate integrity / confidentiality / availability claims.

- **Integrity:** every retrieved object matches its expected identity.
  Content-identity check (plaintext hashes to expected `ContentId`) and
  AEAD check (ciphertext authenticates under the expected key/nonce/AAD)
  are distinct and both required. See `../contracts/trust.md` AEAD
  binding and `../contracts/object-model.md` two-identity rule.
- **Confidentiality:** vault cannot decrypt under the threat model in
  `../contracts/trust.md`. No data-plumbing claim beyond the defined
  boundaries.
- **Availability:** killed-peer, partition-heal, serving-restart legs in
  CI (`sync-and-peers.md` and Lima microVM legs).

Acceptance:
- Chaos legs run in CI with peer-teardown mid-transfer, relay death, and
  restart reconciliation; remote failure produces a bounded, explicit
  error appropriate to the operation and never returns corrupted or
  incomplete content as successful. Local operations with required data
  available stay successful.

## Explicit non-goals

- **No eviction-as-GC.** Cache eviction is local policy. GC is reclamation
  authority. Do not ship a local "free space" button that implies global
  deletion.
- **No auto-merge.** Conflicts preserve both versions; merge is operator
  intent.
- **No photo/notes surface before import semantics are solid**. Camera-roll
  ingestion without dedup and crash idempotency is a reliability bug, not
  a feature.
- **No post-v1 contract minting in v0.x.** No control-plane refusal
  signal, no tombstone tag, no key-destruction saga before `ROADMAP.md`
  v0.8 freeze.

## Open questions (blocking v0.4 product definition)

1. `wyrd policy budgets` rendering: which of the four accounting units
   does a person see by default? "What can I safely remove?" needs
   actionable guidance, not diagnostics.
2. Conflict indicator surface name and snapshot hierarchy grouping:
   per-path vs per-drive, and where it lives in the desktop app.
3. First-run copy language for "drive", "vault," and "custodian" that
   never uses ContentId / epoch / Merkle DAG vocabulary.
4. Owner status states across process death/upgrade: what is durable
   evidence vs transient liveness (`sync-and-peers.md` forget contract)?

No implementation issue should start from this note until questions 1-4
have a decided answer.

## Traceability

- Product owner: `docs/design/cli.md` (`wyrd policy`, `wyrd snapshot`)
- Retention/storage: `docs/contracts/retention-constraints.md`,
  `docs/design/storage-growth.md`, `docs/design/garbage-collection.md`
- Identity/membership: `docs/contracts/trust.md`, `docs/contracts/epochs.md`
- Versioning: `docs/contracts/object-model.md`, `ROADMAP.md`
- Tracked by: PR `pr/product-gap` Refs the GC marketplace issue
  `nostr:nevent1qqsqm7ay7hydgly3lrs0vqktn5rplwk9nmzjv5ev0lguk9qgkmuj8hcpz9mhxue69uhkwunpwdczuap49eehgxp25us`.
