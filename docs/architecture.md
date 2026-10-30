# Wyrd Architecture

Status: Descriptive (overview). Authority: the contracts below win any conflict.

Wyrd is an encrypted, content-addressed, append-only filesystem whose
snapshots describe a logical drive, while individual devices materialize
arbitrary subsets of that drive locally.

## Core model

- **Immutable, content-addressed objects.** ContentId names the canonical
  payload; StorageId names its ciphertext. Writing the same content twice
  is a no-op. There is no garbage collection in v0.
- **Logical identity vs physical storage.** Snapshots describe the drive;
  devices hold whatever subset local policy pins. Pin/evict changes what a
  device holds, never what the drive contains.
- **Signed, append-only history.** Snapshots are signed by the author's
  Nostr identity key and bound to the DriveId and the authorizing
  membership transition. Conflicts keep both heads; there is no automatic
  merge.
- **Nostr is identity and signatures only.** No Nostr event formats, no
  social-Nostr dependencies. Membership state is always encrypted.

## Crate boundaries

| Crate | Responsibility | Depends on |
|---|---|---|
| `wyrd-format` | Content addressing, typed identities, chunking, Merkle snapshot DAG, canonical plaintext formats | blake3, serde, thiserror — never networking, async, or FUSE |
| `wyrd-sync` | iroh transport, encryption, storage addressing, capabilities, authorization, replication | `wyrd-format`, `wyrd-namespace`, iroh stack, nostr crate |
| `wyrd-namespace` | Provider-neutral namespace and verified read surface | `wyrd-format`, thiserror only |
| `wyrd-core` | Embeddable local node: namespace, snapshots, mutations, materialization, sync control | format, namespace, sync — never presentation |
| `wyrd-fuse` | Filesystem-shaped read surface; never mounts or authors content | `wyrd-format`, `wyrd-namespace` |
| `wyrd-daemon` | Composes the core, view and presentation backends | `wyrd-core`, `wyrd-fuse` |
| `wyrd-cli` | Binary process host over the daemon's public API | `wyrd-daemon` |
| `wyrd-embed` | Proves the core lifecycle works without daemon, CLI or presentation | `wyrd-core`, `wyrd-sync`, `wyrd-format`, `wyrd-fuse` |
| `wyrd-contracts` | Cross-crate architectural contract tests; nothing depends on it | all of the above |

Dependency arrows point downward only. Only `wyrd-sync` authorization may
mint `AuthorizedSnapshot`. Content IDs must not appear in vault-visible
metadata. These edges are machine-enforced by `wyrd-contracts`, not by
review vigilance.

## Runtime ownership

A drive has exactly one active process owner of its durable store. All
mutation flows through the owner's single mutation queue; other consumers
submit operations through the drive-owner interface, which exposes
operations, never mutable store internals. See
`contracts/owner-interface.md`.

## Current status

Pre-alpha. Protocol identity, wire format, authorization model, and
control-plane contracts are frozen for v0; local durability semantics
stay open where the product above them requires it. The runtime sync
engine, durable local state, crash recovery, and the mounted write path
have landed and are under test. Still pending: signer-client wiring,
automatic peer repair, and garbage collection (post-v1 by contract).
The frontier is the live network and the v0.4 desktop product.
`ROADMAP.md` tracks the order; the status vocabulary (specified /
implemented / tested / integrated / end-to-end verified / proposed) is
defined in `documentation.md`.

## Contract index

Normative contracts — each invariant lives in exactly one of these:

- Format and identity semantics: `contracts/object-model.md`
- Cryptographic and authorization boundaries: `contracts/trust.md`, `contracts/capability-bindings.md`
- Membership and epoch state: `contracts/epochs.md`
- Sync protocol, peers, materialization: `contracts/sync-and-peers.md`
- Mutation, commit, durability ordering: `contracts/write-path.md`
- Cross-process ownership boundary: `contracts/owner-interface.md`
- Crash windows per subsystem: `contracts/crash-consistency.md`
- Error taxonomy: `contracts/error-conventions.md`
- Live-operation bounds: `contracts/resource-limits.md`
- Retention constraints: `contracts/retention-constraints.md`
- Compatibility guarantees: `contracts/upgrade-contract.md`
- Update transaction: `contracts/update.md`
- Public-read subset: `contracts/release-read.md`

Subsystem workflows (subordinate to contracts): `design/` — fetch-on-open,
peer repair, retry policy, storage growth, CLI reference.
Platform compositions and open decisions: `platforms/`.
Release procedure: `releases/release-process.md`.
Past reviews (evidence, not policy): `reviews/`.
How to change any of this: `documentation.md`.
