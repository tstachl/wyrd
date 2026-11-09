# Codebase Tour

Where to go after `docs/architecture.md`. The crate table there names
every crate's responsibility; this file names the files inside them,
the words that mislead, and the order to read them in.

## Reading order

1. `docs/architecture.md` — crate boundaries and fixed scope.
2. The two flow narratives, one vertical slice each:
   - write side: `docs/contracts/write-path.md` (POSIX mutation
     through the mount to a committed snapshot);
   - read side: `docs/design/fetch-on-open.md` (announcement to
     serving to `open()` unblocking on arrived bytes).
3. `wyrd-format` — content addressing, typed identities, the snapshot
   DAG (`crates/wyrd-format/src/snapshot.rs`). Canonical plaintext
   representations live here and nowhere else.
4. `wyrd-sync` — transport, encryption, storage addressing,
   capabilities, authorization, replication. Start at
   `runtime/engine/` (membership intake, authoring, fetch plans),
   then `serving/` (endpoint, mirror, vault) and `control/`
   (sealed envelopes, mailbox).
5. `wyrd-core` — the embeddable local node. Start at `live/`
   (`pass.rs` runs the loop, `apply.rs` applies mutations,
   `config.rs` the pacing surface), then `mutation/` (user write
   ops) and `node.rs` (head projection).
6. `wyrd-daemon` — composes core, view, and backends. `vault.rs`
   runs the headless owner; `fuse/backend/` is the FUSE backend;
   `owner/` is the cross-process op-submission socket.
7. `wyrd-cli` — the binary process host. `main.rs` dispatches; one
   module per command family in `commands/`; `composer.rs` holds
   the live-node plumbing shared by `mount`, `sync now`, and
   `vault`; `update/` holds the updater and its durable-file
   capture. `wyrd-host` (desktop host) sits on `wyrd-core` plus
   `wyrd-daemon`; `wyrd-embed` proves the core lifecycle without the
   daemon or presentation, over `wyrd-core`, `wyrd-sync`,
   `wyrd-format`, and `wyrd-view`.
8. `wyrd-contracts` — the cross-crate contract suite (below).

Normative contracts live in `docs/contracts/` (index in
`architecture.md`); subsystem workflows subordinate to them live
in `docs/design/`. When code and prose disagree, the code wins —
except against a normative contract, which wins against both
(see `docs/documentation.md`).

## Test layout

Tests live beside the code they pin, as `tests_*.rs` siblings —
not under a `tests/` directory. Roughly half the `.rs` lines in
the tree are these siblings. The `tests/` top level is not unit
coverage: `drive`, `gate`, `harness`, `lima`, `microvm` are
environment harnesses (end-to-end and device runs).

`wyrd-contracts` is the contract suite: one named test per
architectural invariant, composed through the public APIs of
`wyrd-format`, `wyrd-namespace`, `wyrd-sync`, `wyrd-core`,
`wyrd-view`, and `wyrd-daemon`, so an invariant regression fails a
test instead of a deployment. It is a workspace leaf — it depends on every crate and nothing depends
on it — and its dependency-edge test fails closed on any new
member, edge, or external dependency.

## The `mount` feature matrix

FUSE is a default-on feature, not a property of the node. `mount`
in each of `wyrd-daemon`, `wyrd-cli`, and `wyrd-contracts`
re-enables the daemon's mount explicitly (`wyrd-daemon/mount`),
so `--no-default-features` drops the whole FUSE stack instead of
re-enabling it through an edge. Deliberately one feature, not a
mount/presentation pair: a mount-free errno vocabulary would need
a second POSIX-error mapping, and the contract suite's errno
assertions stay with the mount by design. A machine without
system libfuse builds and runs everything above the mount.

## Crate-name caveats

- `wyrd-view` holds no FUSE. It is the mount-free read view
  (`view::DriveView`) plus conflict-version lookup; the actual
  FUSE backend is `wyrd-daemon::fuse`. Its description says so.
- `wyrd-cli/src/composer.rs` (formerly `live.rs`) is composer
  plumbing shared by the `mount`, `sync now`, and `vault`
  commands — not the sync loop. The loop is
  `wyrd-core::live`.
- `wyrd-cli/src/update/snapshot.rs` captures durable files for
  update verification (`DurableFiles`); it never touches the
  snapshot DAG.
- `wyrd-sync/src/close.rs` is transport teardown;
  `wyrd-sync/src/closure.rs` is tree/manifest correspondence.
  Different words, adjacent modules.

## Vocabulary

The same word names different things in different crates. The
table gives the meanings that collide most often; each row names
the crate or file where that meaning lives.

| Word | Meanings (where) |
|---|---|
| `owner` | The single writer process holding `LOCK` (`wyrd-daemon/src/vault.rs`, `docs/contracts/owner-interface.md`); the `owner.sock` op-submission API (`wyrd-daemon/src/owner/`); the outgoing owner of a handover pre-state (`docs/contracts/capability-bindings.md`); a filesystem UID (`vendor/fuser`, packaging) — never the Rust borrow checker |
| `ownership` | Runtime process ownership of the durable store (exactly one writer; `docs/architecture.md`); distinct from the owner role above only in that it names the invariant, not the process |
| `vault` | The durable sealed-representation CAS (`wyrd-sync/src/serving/vault.rs`); the serving adapters over it (`serving/source.rs`, `serving/mirror.rs`); the vault peer role vs mirror (a replica that keeps history but cannot decrypt); `wyrd vault`, the headless-owner run (`wyrd-daemon/src/vault.rs`); the on-disk `vault/` dir and `vault.posture` status file |
| `snapshot` | The signed format body (`wyrd-format/src/snapshot.rs`); the DAG and its eligible/live heads (`wyrd-sync/src/authorization`, `wyrd-core/src/node.rs`); the fetch units body vs manifest vs announcement (`wyrd-sync/src/runtime/`); `wyrd snapshot`, the CLI surface including content-recovery grafts |
| `live` | The mounted projection: live heads, the live view (`wyrd-core/src/node.rs`, `wyrd-view/src/view/drive.rs`); the live network/relay/sync: real iroh vs loopback (`ROADMAP.md`, `cargo nextest --profile slow`) |
| `durable` | The durable fact log: commits plus the `CURRENT` marker (`wyrd-sync/src/durable/`); bytes or state that survive restart: the vault, the outbox (`docs/contracts/sync-and-peers.md`) |
| `mutation` | A user write op folded per snapshot (`wyrd-core/src/mutation/`); one durable commit-log record (`wyrd-sync/src/durable/`) |
| `view` | `DriveView`, the mounted read-only projection (`wyrd-view/src/view/drive.rs`); the `NamespaceView` trait and `ViewHead` capability types (`wyrd-namespace/src/view.rs`, `view/head.rs`); the live view as policy over heads; the reconciliation view, a signed projection of durable state (`docs/contracts/sync-and-peers.md`) |
| `close` vs `closure` | `close`: graceful-or-abort transport teardown (5s `GRACEFUL_CLOSE_DEADLINE`); `closure`: tree/manifest correspondence completeness, plus the history/admission ancestor set |
| `ingest` vs `intake` | `ingest`: one sealed control envelope in (`wyrd-sync/src/control/`); `intake`: the mailbox-drain-to-durable-facts pipeline (`wyrd-sync/src/runtime/engine/intake.rs`) |
| `plan` | The fetch plan: what to fetch next (`wyrd-sync/src/runtime/engine/plan.rs`); the merge/snapshot dry-run preview (`wyrd snapshot plan`) |
| `materialization` | Per-content residency policy: pinned vs remote-only (`wyrd-sync/src/runtime/engine/materialization.rs`); the vault-vs-mirror role file the loop reads; the `wyrd export` projection to a directory tree; the FUSE/owner read verdict for remote-only content (bounded `EIO`/`ENOENT`, never a hang) |
