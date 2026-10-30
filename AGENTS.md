# AGENTS.md

Wyrd is a pre-alpha decentralized, append-only, content-addressed drive.

Read `docs/architecture.md` first. The README describes the vision; `docs/` defines the current contracts.

## Architecture

- `wyrd-format`: content addressing, typed identities, chunking, Merkle snapshot DAG, canonical plaintext formats.
- `wyrd-sync`: iroh transport, encryption, storage addressing, capabilities, authorization and replication.
- `wyrd-namespace`: provider-neutral namespace and verified read surface; depends only on `wyrd-format` and `thiserror`.
- `wyrd-core`: embeddable local node; depends on format, namespace and sync, not presentation layers.
- `wyrd-fuse`: filesystem-shaped read surface; never mounts or authors content.
- `wyrd-daemon`: composes the core, view and presentation backends.
- `wyrd-cli`: binary process host over the daemon's public API.
- `wyrd-embed`: proves the core lifecycle works without the daemon, CLI or presentation layers.
- `wyrd-contracts`: cross-crate architectural contract tests; nothing depends on it.

## Invariants

- Keep `wyrd-format` free of networking, async and FUSE dependencies. Keys, ciphertext, encryption, storage addressing and capability semantics belong in `wyrd-sync`; canonical plaintext representations belong in `wyrd-format`.
- Content is immutable and append-only. Existing-content `put` is a no-op. There is no garbage collection in v0.
- Snapshots are immutable, signed by the author's Nostr identity key, and bound to the DriveId and authorizing membership transition.
- Nostr provides identity and signatures only. Do not introduce Nostr event formats or social-Nostr dependencies. Membership state is always encrypted.
- Device identity is an immutable Nostr public key. Rotation means removing the old device membership and admitting the new key.
- Follow `docs/contracts/trust.md` and `docs/contracts/epochs.md` as normative contracts for root custody, epoch secrets, membership transitions and the Nostr/iroh control-plane split. Do not redesign settled decisions.
- Never implement cryptographic primitives yourself. Use audited crates; see decision T11 in `docs/contracts/trust.md`.
- Change iroh dependency versions as a set.
- Only `wyrd-sync` authorization may mint `AuthorizedSnapshot`. Preserve the contract-34 enforcement.
- Content IDs must not appear in vault-visible metadata.
- Resolve open format decisions in `docs/contracts/object-model.md` before implementing behavior that depends on them. Keep normative docs synchronized with behavior.

## Development

Use the configured devenv and toolchain.

```sh
cargo check
cargo fast-sync
cargo fast-core
cargo fast-daemon
cargo fast-all
cargo nextest run
cargo test --workspace --doc
cargo nextest run --profile slow
cargo test -p wyrd-cli
gate help
```

Use `cargo nextest run --profile slow` for live-relay tests excluded from the default PR gate.

For Lima and microVM end-to-end testing, follow the `lima-e2e` skill. Prefer the microVM suite for topology, convergence and restart evidence; use Lima for macOS development and the refactor guard.

## Workflow

- Use Conventional Commits and `pr/<name>` branches. Never commit directly to `master`.
- Follow `docs/collaboration-workflow.md` for issue triage, review, merge and resolution.
- Apply issue labels after creation: exactly one type (`bug`, `enhancement`, `chore`), one priority (`P0`–`P4`), and one `release:*` milestone. Use `release:backlog` for deferred work.
- Comments should explain why or how code is used, not narrate each line.
- Follow the `gate-workflow` skill for Gate-managed development and remote operations.

## Remote publication safety

After a failed, timed-out or aborted remote publication, stop. Do not retry, force, switch signer/server, use an alternate command or delete refs without fresh explicit approval. Report the exact local and remote state before asking how to proceed.
