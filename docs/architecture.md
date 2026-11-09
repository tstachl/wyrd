# Wyrd Architecture

Status: Descriptive (overview). Authority: the contracts below win any conflict.

Wyrd is an encrypted, content-addressed, append-only filesystem whose snapshots describe a logical drive, while individual devices materialize arbitrary subsets of that drive locally.

## Core model

- **Immutable, content-addressed objects.** ContentId names the canonical payload; StorageId names its ciphertext. Writing the same content twice is a no-op. There is no garbage collection in v0.
- **Logical identity vs physical storage.** Snapshots describe the drive; devices hold whatever subset local policy pins. Pin/evict changes what a device holds, never what the drive contains.
- **Signed, append-only history.** Snapshots are signed by the author's Nostr identity key and bound to the DriveId and the authorizing membership transition. Conflicts keep both heads; there is no automatic merge.
- **Nostr is identity and signatures only.** No Nostr event formats, no social-Nostr dependencies. Membership state is always encrypted.

## Crate boundaries

Listed in dependency order. Dependencies point only at rows above, and `wyrd-contracts` enforces the edges mechanically, not by review vigilance.

| Crate | Responsibility | Depends on |
|---|---|---|
| `wyrd-format` | Content addressing, typed identities, chunking, Merkle snapshot DAG, canonical plaintext formats | blake3, serde, thiserror; never networking, async, or FUSE |
| `wyrd-namespace` | Provider-neutral namespace and verified read surface | `wyrd-format`, thiserror only |
| `wyrd-owner-client` | Owned drive-owner protocol types and non-owning socket client; no domain edge, even transitively | serde, serde_json, hex only; never `wyrd-*` |
| `wyrd-desktop` | Desktop management UI backend: non-owning Tauri client of the drive-owner interface; never opens a drive | `wyrd-owner-client` only; reserved `tauri` toolkit edge, serde, thiserror |
| `wyrd-sync` | iroh transport, encryption, storage addressing, capabilities, authorization, replication | `wyrd-format`, `wyrd-namespace`, iroh stack, nostr crate |
| `wyrd-core` | Embeddable local node: namespace, snapshots, mutations, materialization, sync control | format, namespace, sync; never presentation |
| `wyrd-view` | Filesystem-shaped read surface; never mounts or authors content | `wyrd-format`, `wyrd-namespace` |
| `wyrd-daemon` | Composes the core, view and presentation backends | `wyrd-core`, `wyrd-view`, `wyrd-owner-client` |
| `wyrd-host` | Desktop host process: owns a drive through `wyrd-core`, runs the live loop under daemon supervision | `wyrd-core`, `wyrd-daemon` |
| `wyrd-cli` | Binary process host over the daemon's public API | `wyrd-daemon` |
| `wyrd-embed` | Proves the core lifecycle works without daemon, CLI or presentation | `wyrd-core`, `wyrd-sync`, `wyrd-format`, `wyrd-view` |
| `wyrd-contracts` | Cross-crate architectural contract tests; nothing depends on it | all of the above |

Only `wyrd-sync` authorization may mint `AuthorizedSnapshot`.

## Runtime ownership

A drive has exactly one active process owner of its durable store. All mutation flows through the owner's single mutation queue; other consumers submit operations through the drive-owner interface, which exposes operations, never mutable store internals. See `contracts/owner-interface.md`.

## Status

Pre-alpha. Protocol identity, wire format, authorization model and control-plane contracts are frozen for v0; local durability semantics stay open where the product above them requires it. `ROADMAP.md` tracks what has landed and what is next. The status vocabulary (specified / implemented / tested / integrated / end-to-end verified / proposed) is defined in `documentation.md`.

## Fixed scope

Three scope statements live here, exactly once, and nowhere else.

**Windows is out of v0.4 entirely.** The only credential reader fails closed on non-Unix builds, so a Windows build cannot open a drive: no application, no lifecycle, no native filesystem presentation. Before the Windows presentation milestone begins, the platform assumptions are audited together (credential loading, file locking, storage durability, process ownership, filesystem presentation) rather than discovered one port at a time.

**Platform mechanisms stay at the boundary.** The credential contract is platform-neutral (secret format, failure behavior, secret lifetime, zeroization), and one backend implements it today: the hardened Unix file reader in `wyrd-cli` (`credentials.rs`). Identity and vault logic consume secret material (`DeviceIdentitySecret`), never files, modes, or OS facilities. No OS credential store is the architecture.

**Content IDs never reach vaults as vault disclosure.** Content IDs must not appear in vault-visible metadata. The guarantee binds the keyless-replica position (ciphertext, sizes and timing only, never plaintext, paths, or plaintext equality). A member-hosted composition holds that metadata as a drive member, not as vault disclosure. A public release drive discloses it by a deliberate, per-drive, irreversible waiver (release-read v1). The full matrix is `contracts/trust.md`.

## Contract index

Normative contracts; each invariant lives in exactly one of these:

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

Also: subsystem workflows (subordinate to contracts) in `design/`; platform compositions and open decisions in `platforms/`; release procedure in `releases/release-process.md`; past reviews (evidence, not policy) in `reviews/`; how to change any of this in `documentation.md`.
