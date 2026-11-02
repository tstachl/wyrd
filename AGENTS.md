# AGENTS.md

Wyrd is a pre-alpha decentralized, append-only, content-addressed drive. Protocol identity, wire format, authorization and control-plane contracts are frozen for v0; local durability semantics stay open where the product above them requires it.

Read `docs/architecture.md` first. It owns the crate boundaries, core model and fixed scope, and `docs/contracts/` owns every invariant. The README describes the vision, and `ROADMAP.md` tracks status and what is next.

## Rules agents tend to break

- **Placement:** keys, ciphertext, encryption, storage addressing and capability semantics belong in `wyrd-sync`. Canonical plaintext representations belong in `wyrd-format`.
- **Layering:** `wyrd-contracts` rejects dependency edges that cross the architecture table. Don't add a dependency to make something compile; fix the placement.
- **Contracts:** `docs/contracts/trust.md` and `docs/contracts/epochs.md` are normative. Don't redesign settled decisions. Resolve open decisions (status "Proposed / Open", see `docs/documentation.md`) before implementing behavior that depends on them, starting with `docs/contracts/object-model.md`. Keep normative docs synchronized with behavior.
- **Crypto:** never implement primitives yourself. Use audited crates (decision T11, `docs/contracts/trust.md`).
- **Identity:** device identity is an immutable Nostr public key. Rotation means removing the old device membership and admitting the new key.
- **Dependencies:** change iroh versions as a set.

## Development

Use the configured devenv and toolchain.

```sh
cargo check                          # fast type check
cargo fast-sync                      # per-crate loops: sync, core, daemon
cargo fast-core
cargo fast-daemon
cargo fast-all                       # run before pushing
cargo nextest run                    # default PR gate
cargo test --workspace --doc
cargo nextest run --profile slow     # live-relay tests, excluded from the PR gate
cargo test -p wyrd-cli
```

End-to-end testing:

- microVM suite (`gate task run wyrd-gate-e2e`): preferred evidence for topology, convergence and restart behavior.
- Lima (`lima-e2e` skill): macOS development and the refactor guard.

## Workflow

Follow `docs/collaboration-workflow.md` for commits, issues, triage, review and merge, and the `gate-workflow` skill for Gate operations and remote publication. `gate help` lists the verbs.
