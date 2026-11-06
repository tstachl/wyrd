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

## Deletion guarantee levels (from tracking issue)

The design distinguishes three levels. A `delete` operation must name which
level it promises. Cooperative deletion (hide path, throw away key) is UX,
not crypto — the issue records this explicitly.

| Level | What it means | What it requires |
|---|---|---|
| **L1 — unreferenced drop** | Ciphertext no longer referenced by any live head or pinned tree is eligible for vault drop | Reachability (above) + grace + quorum. Keys and manifests still exist; a holder with old capability could still read a copied representation. No resurrection guarantee beyond "no live head names it." |
| **L2 — epoch-key destruction** | Epoch secrets for dropped epochs are destroyed; old ciphertext becomes unrecoverable even if copied | Escrow record + sidecar destruction, owner-proof that destruction happened. Needs `trust.md` escrow lifecycle (T13) and guaranteed-deletion saga across device keystores. |
| **L3 — no-resurrection tombstone** | Deleted ContentIds never resurrect via sync; tombstone commits as a new snapshot transition | A tombstone fact type (new tag, `upgrade-contract.md` entry), monotonic tombstone state machine, and resurrection tests (`deleted_object_unfetchable_after_gc`). Strongest; needs protocol change. |

L1 is storage hygiene. L2 is cryptographic erasure for future acquisition.
L3 is the only level that makes a replicated delete safe against a lagging
peer re-announcing the object.

Recommendation for pre-v1: L1 local-only trim of *verified unreachable
orphans* (objects never referenced by any snapshot body the device
holds) MAY be considered, but must be named "orphan trim", never "GC".
L2 and L3 stay post-v1: both need epoch/escrow and tombstone protocol
changes that break the v0 freeze (`ROADMAP.md:150-152` v0.8 freeze,
`architecture.md:39` frozen protocol identity).

## Phases for distributed GC (post-v1)

```
Mark → Hold → Authorize → Sweep → Audit
```

**Mark (local, no authority).** Compute candidate set = all objects not in
reachable closure. Emit measurement report only (`wyrd policy budgets` style —
no deletion). This sizes the problem before any sweep.

**Hold (grace + acknowledgement).** Publish candidate set, start grace period
(README proposes 30 days). Poll configured vaults. An offline vault that
cannot acknowledge *blocks* sweep for its slice. This is the
retention/acknowledgement protocol `ROADMAP.md:180` requires — quorum alone
fails when a vault is offline: quorum says "store dropped", vault still holds
bytes.

**Authorize (quorum + human).** `k`-of-`N` configured peer nodes sign the
(candidate set hash, hold expiry, epoch range). Owner human confirmation is a
separate durable fact, not a flag. Authorization rides the existing
owner-proof pattern (`trust.md` mint authority vs delivery authority) — GC
authorization must bind to the pre-transition owner set like membership
transitions do.

**Sweep (generation-scoped).** Delete newest-generation first, call
`RetainedBytes::subtract` synchronously, persist tombstone fact (L3). Crash
during sweep must resume or roll-forward: candidate set is durable, sweep
state is checkpointed per generation, never re-marked from scratch.

**Audit.** Post-sweep census: reachable bytes, candidate bytes, tombstoned
bytes, refusal ledger. A later snapshot that references a tombstoned
ContentId must fail closed, never resurrect.

Crash windows follow `crash-consistency.md` pattern: mark is read-only,
authorize is atomic, sweep is idempotent per object.

## Pre-v1 slice rule

If any of this is pulled before v1, the slice is:

- Allowed: L1 orphan trim limited to objects *never* referenced by any
  snapshot body on this device, with no wire signal, no quorum claim, no
  promise shared with peers. Report only (`retention-constraints.md:202`
  — history-depth policy observable only until GC exists).
- Forbidden before v1: eviction under pressure, history-depth pruning
  enforced by deletion, quota-triggered reclamation, tombstone facts,
  epoch-key destruction, any control-plane refusal signal, any claim that
  a live head no longer exists because GC ran.

That keeps the DG-4 invariant "refused is not unavailable, neither is lost"
intact (`retention-constraints.md:121-127`).

## Interaction with existing mechanisms

- **Materialization (`sync-and-peers.md:533-546`).** `REMOTE_ONLY` /
  `CACHED` / `PINNED` is residency policy, not reclamation. Evict under a
  pin is local policy; GC is drive-level authorization. Never merge them.
- **DG-4 A/B.** GC consumes the same accounting (`RetainedBytes` +
  vault-seeing counter). A alone does not make GC safe: it bounds disk, not
  promises. B is the serving-side analogue GC replaces post-hoc.
- **Recovery.** Content recovery (v0.3) grafts stranded content into
  current state. GC must never sweep a stranded branch that content recovery
  could still graft. Reachability must include stranded branches until the
  owner explicitly voids them via a recovery snapshot.
- **Epochs.** Rotation never re-encrypts history (`object-model.md` decision
  13). Deleting an epoch's keys (L2) is therefore a separate owner-signed
  destructive transition, not a side effect of rotation.

## Open questions (must close before any normative edit)

1. Quorum membership: configured peer nodes from where? Membership log
   field vs local config? (`epochs.md` owns membership; no new field
   without a transition type.)
2. Offline-vault acknowledgement format: signed ack vs liveness poll vs
   reconciliation view extension (`sync-and-peers.md` 0x18/0x19)? Must survive
   relay retention expiry like the forget contract.
3. Tombstone representation: new `Fact` tag vs snapshot flag bit vs
   `DiscardRejectedRepresentation`-style op? Each needs an
   `upgrade-contract.md` entry and tag allocation.
4. Grace period size and reset rule: does a new snapshot referencing an
   old candidate reset hold for that slice or the whole set?
5. Marketplace ephemeral shares (timebound capability epochs `1..=N` +
   `not_after`, serving-side enforcement): same GC authority or a separate
   expiry mechanism that does not require re-encrypting history? Per the
   tracking issue, expiry ends acquisition of future material, never
   possession of already-read plaintext — the UI must state this plainly.

No implementation lands before questions 1+2 close. Until then, this file
stays Proposed.
