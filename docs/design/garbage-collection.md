# Garbage Collection — design note

Status: Proposed / Open. This note does not settle a contract. Normative
constraints live in `../contracts/retention-constraints.md` and analysis in
`storage-growth.md`. Nothing here may be cited as a guarantee until the
contract it amends is updated first.

Central thesis: **GC is reclamation authority, not a janitor.** v0.x already
has refusal (DG-4 A/B), quota, and quarantine/scrub subtractions. What it
does not have — by decision — is the authority to say bytes are *safe to
delete*. That authority is quorum + grace + acknowledgement + human
confirmation (`retention-constraints.md` Invariant 4). Design the authority
before designing the sweeper.

Tracking: this note scopes `docs/design/` work for the post-v1 GC design.
It supports `ROADMAP.md` "Garbage collection stays post-v1" by recording what
*can* be pulled before v1 without breaking that line, and what cannot.

## v0 constraints this design must not violate

From `retention-constraints.md` (normative, decided):

1. **Retention is not deletion.** Refusal never deletes or un-charges stored
   bytes. Every future removal path (quarantine, scrub, GC sweep) must call
   `RetainedBytes::subtract` (`retention-constraints.md:162-169`).
2. **Residency is not availability.** One device willing to retain proves
   nothing about reclaimability elsewhere. No v0.x signal may be read as
   global reclaimability.
3. **Quota pressure causes refusal, never reclamation evidence.** Reaching a
   ceiling declines new retention; it never manufactures proof that history
   is reclaimable.
4. **Reclamation needs a separate authority.** Quorum, grace period,
   offline-vault acknowledgement, and human confirmation belong to the
   future GC. Nothing in v0.x mints that authority early.
5. **Remote admission is charged locally.** A/B split stays: A bounds disk
   at pre-admission, B bounds promises after durability. B never un-charges
   bytes (`storage-growth.md:261`).
6. **Refusal is local, has no wire representation in v0.3** (`retention-constraints.md:115-120`).
   A "peer declined" control message is an enumeration/pressure surface.
   GC needs a *new* signal design; it cannot reuse refusal semantics.

Adversary (`storage-growth.md:70-96`): state-transition churn with zero
file-content bytes (unlink+create loop), replica asymmetry (commit rate set
by peers), structural closure (snapshots imply tree/manifest closure),
remote spending of local headroom. A chunk-only GC underestimates a replica
~3x.

Three distinct facts must never be confused: (a) not needed by my current
view, (b) not currently available, (c) authorized to destroy. Local
residency proves none of them for the whole drive.

## Reachability: what GC must compute first

`RetainedBytes` counts only plaintext object-store bytes. The vault sees
ciphertext. Structural bytes (snapshot bodies, root/child manifests, trees)
are retained by replicas even with zero wants (`storage-growth.md:188-206`).
Any mark algorithm must include:

- Live heads + canonical history per snapshot classification
  (`object-model.md:298-307` — only ELIGIBLE advances the view, but VOIDED,
  SUPERSEDED, STRANDED remain valid history).
- Pinned + recovery snapshots (bit 0 flag, `object-model.md:283-295`).
- Quarantine/scrub pending sets, in-flight fetches, outbox obligations
  (`AnnouncementQueued`, `CapabilityQueued` etc. — dropping the body they
  reference strands the obligation).
- Structural closure: every reachable tree node must have a `Tree` manifest
  entry, every chunk a `Chunk` entry, every subtree a child manifest
  (`object-model.md:264-271`, verifier `verify_snapshot_manifest`).

Failure to include structural closure is the most likely GC bug: sweep
deletes a tree that a later snapshot still navigates.

### Four reachability sets, not one binary

A candidate is not a single reachable/unreachable bit. Mark must produce a
*reason* for every retained object, e.g. `retained_by: pinned_snapshot:S1,
recovery_branch:S2, outbox_obligation:O3`. Four sets overlap without
being identical:

| Set | What it protects | Example |
|---|---|---|
| **Metadata/history reachability** | Facts and manifests needed to understand and verify history | A SUPERSeded snapshot body + its membership transition; VOIDED snapshot that must stay verifiable |
| **Recovery reachability** | Content an authorized recovery graft could still reclaim | STRANDED branch not yet voided by a recovery snapshot (`epochs.md`) |
| **Materialization reachability** | Content required by current and pinned views | Live head trees, pinned file chunks, child manifests for `Music/…` materials |
| **Operational reachability** | Content referenced by in-flight ops and durable obligations | `AnnouncementQueued` bodies, `CapabilityQueued` envelopes, fetch-plan legs in flight |

A VOIDED payload may be freely reclaimable while its snapshot body is not.
A STRANDED payload may be outside materialization but inside recovery. Only
an object outside all four sets is a candidate.

Accounting units must stay distinct in every report:

- Logical retained bytes (quota / `RetainedBytes` plaintext).
- Physical ciphertext bytes (vault, incl. orphans charged via vault-seeing
  counter).
- Structural metadata bytes (bodies, manifests, trees, fact log).

An audit that reports logical reclamation while physical consumption is
unchanged is a reporting failure.

## Mark concurrency contract

Mark runs against a store that is changing. Grace alone does not fix this.
The following are hard invariants, not implementation details:

- Mark operates against a consistent store generation or equivalent read
  barrier (durable generation watermark, not wall-clock time).
- Every candidate carries the generation at which it was identified.
- Any new reference (commit, import, graft, reconciliation re-announce)
  to a candidate invalidates that candidate immediately.
- Authorization binds to `(candidate-set hash, generation)`. Sweep must
  revalidate eligibility against current generation immediately before
  deletion.
- A candidate cannot become reachable between authorization and deletion
  without invalidating the authorization for that candidate.

Crash windows follow `crash-consistency.md` pattern: mark is read-only,
authorize is atomic, sweep is idempotent per object and checkpoints per
generation.

## Deletion guarantee levels (from tracking issue)

The design distinguishes three levels. A `delete` operation must name which
level it promises. Cooperative deletion (hide path, throw away key) is UX,
not crypto — the issue records this explicitly.

| Level | What it means | What it requires |
|---|---|---|
| **L1 — unreferenced drop** | Ciphertext no longer referenced by any reachability set is eligible for physical reclamation | Reachability (above) + grace + holder ack + quorum + human. Keys and manifests still exist; a holder with old capability could still decrypt a copied representation. No resurrection guarantee. |
| **L2 — epoch-key destruction** | **All authorized acquisition paths for the specified epoch have been irreversibly disabled by destruction of every recoverable copy of its required secrets** | Complete set of key holders + key-derivation paths inventoried; destruction evidence from each participant; escrow sidecars handled separately (see below). |
| **L3 — no-resurrection tombstone** | Deleted ContentIds never resurrect via sync; tombstone commits as a new snapshot transition | A tombstone fact type (new tag, `upgrade-contract.md` entry), monotonic tombstone state machine, retained deletion frontier after compaction, resurrection tests (`deleted_object_unfetchable_after_gc`). Strongest; needs protocol change. |

L1 is storage hygiene. L2 is cryptographic erasure for future acquisition.
L3 is the only level that makes a replicated delete safe against a lagging
peer re-announcing the object. L1 does not imply L2; L2 does not imply L3.

### What L2 does not promise (explicit)

- Plaintext already acquired cannot be recalled.
- Previously copied plaintext cannot be erased.
- A device that retained an independent key copy (e.g. a capability
  installed before rotation, an exported keystore, an escrow sidecar copy)
  may still decrypt ciphertext. Key destruction cannot be claimed complete
  until every required destruction participant has supplied acceptable
  evidence.
- Escrow sidecars (`<store>/escrow/<epoch>`, `trust.md` T13) are *not*
  replicated as ordinary vault records today. L2 therefore needs its own
  escrow-availability and destruction-authority design. It cannot assume
  vault replication covers escrow lifecycle. This alone keeps L2 out of
  pre-v1 scope.

### What L3 does not promise (explicit)

- "Never resurrects" is a guarantee *only* when every participating peer
  enforces the tombstone contract. Peers on old protocol versions cannot be
  assumed to enforce it; the compacted deletion frontier (retained epoch
  fence / deletion watermark) must be enforced by any peer that claims L3.
- Tombstone compaction requires the frontier to survive after the
  tombstone fact itself is compacted. Quorum acknowledgement of the
  tombstone is not permission to delete the frontier while any holder that
  has not yet seen it may re-announce the object.

### Level-dependent sweep semantics

L1, L2, and L3 have different sweep contracts. They must not share one
sweep description:

- **L1:** authorized physical reclamation. Durable, idempotent per-object
  sweep accounting. No tombstone. No key destruction.
- **L2:** authorized key-destruction saga. Numeric completion = all
  required destruction participants evidenced. Physical reclamation is
  optional after L2; cryptographic unavailability is the guarantee.
- **L3:** commit the tombstone transition *before* allowing physical
  reclamation. Tombstone durability precedes sweep. Enforce on every
  acquisition and replication path. Compaction keeps the frontier.

`RetainedBytes::subtract` needs a crash-consistency specification, not just
a synchronous call. Two windows must both survive:

- Accounting decremented, physical deletion not completed.
- Physical deletion completed, accounting not decremented.

A durable per-object sweep intent (or equivalent recoverable transaction)
is required. Synchronous invocation alone does not make filesystem deletion
plus accounting atomic.

### Deletion ordering

No prescribed ordering (e.g. "newest-generation first") is mandated. The
ordering requirement is dependency-aware:

- Never delete an object while a surviving object or obligation depends on it.
- Never expose a partially completed destructive transition as complete.
- Retries are idempotent.
- Durable state survives a crash mid-sweep.

Deletion order is therefore derived from the reachability dependency graph,
not a generation heuristic.

## Phases for distributed GC (post-v1)

```
Mark → Hold → Authorize → Sweep → Audit
```

**Mark (local, no authority).** Compute candidate set = objects outside all
four reachability sets, each with its reason set and generation. Emit
measurement report only (`wyrd policy budgets` style — no deletion). This
sizes the problem before any sweep.

**Hold (grace + holder acknowledgement).** Publish candidate set, start grace
period (README proposes 30 days). Poll *holders*, not an abstract quorum.
An offline holder that has not acknowledged blocks sweep for its slice.
This is the retention/acknowledgement protocol `ROADMAP.md:180` requires.

**Authorize (authorization quorum + human).** `k`-of-`N` authorized
decision-makers sign the `(candidate-set hash, generation, hold expiry)`.
Owner human confirmation is a separate durable fact, not a flag.
Authorization rides the existing owner-proof pattern (`trust.md` mint
authority vs delivery authority) — GC authorization must bind to the
pre-transition owner set like membership transitions do.

Holder acknowledgement and authorization answer different questions:

| Mechanism | Question answered |
|---|---|
| Holder acknowledgement | Have the devices responsible for retaining this representation acknowledged the proposed reclamation? |
| Authorization quorum | Have enough authorized decision-makers approved the reclamation? |
| Human confirmation | Has the owner explicitly approved this destructive operation? |

These are not interchangeable. A decision quorum cannot establish that
every holder stopped retaining. Every holder acknowledging does not establish
that the owner authorized it. Membership, custody, and authorization are
three different concepts — which is why quorum membership is a blocking
architecture question, not a configuration detail.

**Sweep (dependency-aware, per level).** Apply the level-specific sweep
contract above. For L3, tombstone commits first. For all levels, revalidate
generation immediately before each deletion. Candidate state is durable and
checkpointed; a crash resumes without re-marking from scratch.

**Audit.** Post-sweep census in the four accounting units above: reachable,
candidate, tombstoned, refused. A later snapshot referencing a tombstoned
ContentId (L3) must fail closed, never resurrect.

## Pre-v1 slice rule

If any of this is pulled before v1, the slice is:

- Allowed: **provably abandoned, never-admitted staging data only.** The
  precise boundary is whether the object ever crossed durable admission.
  If `wyrd-format` / `wyrd-sync` already state that provably (aborted
  staging write that never reached `RetainedBytes` or vault import), use
  that state. Do not invent a new lifecycle state just for GC. Report only
  (`retention-constraints.md:202` — history-depth policy observable only
  until GC exists).
- Forbidden before v1: durably admitted objects that are locally
  unreferenced (Device A may hold the only copy of an object Device B's
  unseen snapshot references — local mark seeing no reference is not
  evidence of global unreachability), eviction under pressure,
  history-depth pruning enforced by deletion, quota-triggered reclamation,
  tombstone facts, epoch-key destruction, any control-plane refusal signal,
  any claim that a live head no longer exists because GC ran.

That keeps the DG-4 invariant "refused is not unavailable, neither is lost"
intact (`retention-constraints.md:121-127`) and honors Invariant 4: no
reclamation authority is minted early.

## Interaction with existing mechanisms

- **Materialization (`sync-and-peers.md:533-546`).** `REMOTE_ONLY` /
  `CACHED` / `PINNED` is residency policy, not reclamation. Evict under a
  pin is local policy; GC is drive-level authorization. Never merge them.
- **DG-4 A/B.** GC consumes the same accounting (`RetainedBytes` +
  vault-seeing counter) but *does not replace* B. A alone bounds disk, not
  promises. B remains necessary to account for promises as they are created.
  GC provides a separate, authorized way to retire previously retained
  material.
- **Recovery.** Content recovery (v0.3) grafts stranded content into
  current state. GC must never sweep a stranded branch that content recovery
  could still graft. Reachability must include stranded branches until the
  owner explicitly voids them via a recovery snapshot.
- **Epochs.** Rotation never re-encrypts history (`object-model.md` decision
  13). Deleting an epoch's keys (L2) is therefore a separate owner-signed
  destructive transition, not a side effect of rotation.
- **Marketplace expiry.** Time-bounded capabilities answer a different
  question from GC. Expiry says "can someone acquire this material through
  this authorization after time T?" GC says "may previously retained
  material be physically reclaimed?" Expiry does not prove holders
  discarded copies. Keep marketplace expiry as an independent
  serving-side enforcement mechanism. UI wording must be: *Expiry prevents
  future acquisition under this capability. It does not revoke plaintext
  or copies already acquired.*

## Scopes: pre-v1, post-v1 L1, post-v1 L2/L3

| Scope | Allowed | Requires new authority |
|---|---|---|
| **Pre-v1** | Measurement-only mark, reachability reasons, storage census in the four accounting units, cleanup of provably abandoned never-admitted staging data. No reclamation authority. | None |
| **Post-v1 L1** | Distributed reachability, holder acknowledgement, grace period, authorization quorum + human confirmation, crash-safe physical reclamation. No claim of cryptographic erasure or anti-resurrection. | Yes — distributed reclamation contract |
| **Post-v1 L2/L3** | Epoch-key destruction, escrow lifecycle + destruction evidence, tombstone protocol, anti-resurrection enforcement + compaction, explicit guarantees across offline and outdated peers. | Yes — protocol / trust changes |

## Open questions (must close before any normative edit)

| # | Question | Priority | Why |
|---|---|---|---|
| 1 | Quorum membership and authority: configured peer nodes from where? Membership log field vs local config? (`epochs.md` owns membership; no new field without a transition type.) | **Blocking** | Determines who may authorize destructive transitions |
| 2 | Holder acknowledgement semantics: signed ack vs liveness poll vs reconciliation-view extension (`sync-and-peers.md` 0x18/0x19)? Must survive relay retention expiry like the forget contract. | **Blocking** | Determines which replicas can prevent reclamation |
| 3 | Tombstone representation and compaction: new `Fact` tag vs snapshot flag bit vs `DiscardRejectedRepresentation`-style op? Retained deletion frontier after compaction? | L3 blocker | Determines whether no-resurrection is enforceable |
| 4 | Grace period and reset semantics: does a new snapshot referencing an old candidate reset hold for that slice or the whole set? | **Blocking** | Determines candidate invalidation under concurrent commits |
| 5 | Marketplace expiry: separate serving-side mechanism (this note recommends independent); state its contract separately. | Independent — do not block GC on it | Avoids coupling expiry to reclamation authority |
| 6 | Exact boundary between admitted data and abandoned staging data | **Blocking for any pre-v1 trim** | Determines whether trim is legitimate (see Pre-v1 slice rule) |
| 7 | Atomicity and recovery for accounting plus physical deletion | **Blocking for any sweep** | Determines whether a crash can leave retained-byte accounting inconsistent with storage |
| 8 | History metadata retention vs payload retention: which facts must survive payload reclamation to keep history verifiable? | Must close before final reachability semantics | Metadata/history vs payload reachability sets overlap without being identical |

No implementation lands before questions 1, 2, and 4 close. Questions 6
and 7 block their respective slices. Until then, this file stays Proposed.
