# Roadmap

This file tracks implementation order and stays directional. Normative contracts live in `docs/contracts/` (`object-model.md`, `trust.md`, `epochs.md`, `sync-and-peers.md`); per-capability evidence lives in tests and release records.

Milestones are capability-oriented, not issue-volume-oriented. Each names the system that must become trustworthy before the next begins: v0.2 to v0.3 make the substrate trustworthy, v0.4 to v0.7 build products around it, v0.8 to v1.0 make long-term promises about it.

| Milestone | Label | Status | Thesis |
| --------- | ----- | ------ | ------ |
| v0.2 | `release:v0.2.0-alpha` | shipped | Trustworthy live synchronization |
| v0.3 | `release:v0.3.0-alpha` | current | Durable operation and content recovery |
| v0.4 | `release:v0.4` | | Cross-platform desktop product |
| v0.5 | `release:v0.5` | | Human-scale history and conflict resolution |
| v0.6 | `release:v0.6` | | Mobile synchronization |
| v0.7 | `release:v0.7` | | Production vault infrastructure and disaster recovery |
| v0.8 | `release:v0.8` | | Protocol compatibility and security freeze |
| v0.9 | `release:v0.9.0-beta` | | Release candidate and soak (required before feature-complete Beta) |
| v1.0 | `release:v1.0` | | Full Wyrd drive |
| n/a | `release:backlog` | | Deferred beyond the immediate roadmap |

`release:v0.1.0-alpha` is retired but stays on its issues for history. The label scheme itself is in `gate.toml`.

As transitions: v0.2, sync works; v0.3, one machine can safely operate it; v0.4, people can use it; v0.5, people can understand its history; v0.6, people can use it on mobile; v0.7, infrastructure can keep it alive and recover it; v0.8, contracts become long-lived; v0.9, prove the whole thing; v1.0, ship it.

## Status vocabulary

A capability is never just "done". Here, in `architecture.md`, and in design docs, these states stay distinct:

| Status | Meaning |
| ------ | ------- |
| Specified | A contract or design has been written down. |
| Implemented | The code path exists. |
| Tested | The behavior has automated evidence. |
| Integrated | The relevant components are wired together. |
| End-to-end verified | The composed workflow has passed its required test. |
| Proposed / Open | The design or decision is not settled. |

## Placing work on the map

Issue labeling mechanics are in `docs/collaboration-workflow.md`. Placement rules:

- A new issue matches a milestone thesis, or it forces a thesis discussion first.
- **v0.2 entry test** (for work targeting v0.2 or arguing a regression there): does it alter behavior a peer can observe over the network, or prevent locally corrupted or invalid state from being accepted? Performance work qualifies only when current behavior violates an operational bound; otherwise the sequence is correctness, measure, optimize, prove equivalence.
- **Project rule, all milestones:** protocol and synchronization semantics never depend on whether the consumer is FUSE, a desktop app, iOS, Android, or a vault. Presentation surfaces are adapters around `wyrd-core` and the daemon, never the architecture. Platform clients may differ in process, lifecycle, storage and IPC. Platform-specific abstractions (persistence capabilities, lifecycle, resource budgets, key storage) are inputs to the core, not knowledge of which product is calling. Clients must still consume the same core synchronization and state semantics rather than reimplement them. This keeps v0.4 to v0.7 product work from turning the daemon into a universal application.

## Recovery terminology

"Recovery" always means one of three things. Say which:

- **Crash recovery:** a local process or device survives a crash. Shipped.
- **Content recovery:** stranded local content is grafted into current state. v0.3.
- **Root recovery:** all capable devices are lost and guardians reconstruct control and root material. v0.7.

Until root recovery lands, losing all capable devices is unrecoverable, as `docs/contracts/trust.md` states. Nothing shipped should be read as operational root recovery.

## Shipped foundations

Protocol identity, wire format, authorization model and control-plane contracts are frozen for v0. Core implementation and local durability semantics stay open where the product above them requires it.

- **Runtime sync:** durable membership, snapshot, manifest, capability and materialization state; crash recovery and restart reconciliation; transport identity distribution; bulk transport and fetch plan; serving router; relay interoperability across implementations (default gate; live public-relay runs are opt-in hand evidence, never CI).
- **Filesystem:** FUSE mount via the daemon with fetch-on-open demand machinery. The mount serves read-write by default.
- **Recovery foundations:** epoch-secret escrow records (T13), bootstrap custody create and reopen, and the recovery-snapshot flag and grafting rules (`docs/contracts/epochs.md`: content grafts, never lineage). Escrow is end to end for local custody, not for guardians.
- **Repair:** generation-scoped terminal fetch state with waiter completion, local quarantine with re-want, local scrub for out-of-band loss, and fetch-failure diagnostics on the `sync now` run surface (`docs/design/peer-repair.md`, Part 1).
- **Hardening:** decode-cost and allocation bounds, property coverage for canonical state transitions, fuzzing of envelope and decoder surfaces.

**Open from runtime sync** (not v0.2 blockers):

- **NIP-46 remote signing:** absent. The `SignerSession` boundary and `control/nip46.rs` codecs exist and are fake-tested, but there is no session negotiation. v0 operates with the local signer.
- **Bulk backpressure on a live network:** the mechanism landed and is loopback-tested (`docs/design/fetch-on-open.md`, property 8). No operational bound is violated on current evidence, so live measurement books to later hardening.

**Not landed, and untracked** (file tracking issues before implementing; all root recovery, see v0.7):

- guardian selection and membership-log guardian fields
- Shamir reconstruction (k-of-n share return, root rebuild, fresh capability and epoch)
- share rotation
- historical epoch restoration

The owner recovery-snapshot creation workflow is content recovery (v0.3), not root recovery; it tracks against the v0.3 milestone.

## v0.2: Trustworthy live synchronization (shipped)

Thesis: Wyrd can synchronize real drives reliably over the live network.

Definition of done: given two or more legitimate peers connected through real relay infrastructure, ordinary mutations (including history catch-up after disconnection) converge correctly, survive restart, partition and heal, never silently accept invalid state, and can be operated without a mounted FUSE filesystem. History acquisition and state convergence are separate failure domains, and the definition covers both.

Did not ship: the recovery grafting workflow (no CLI, daemon or embeddable-node entry point; v0.3), mobile, File Provider, conflict UI, production telemetry, marketplace, automated release distribution (v0.2.0-alpha archives shipped through the manual flow), major performance architecture.

`Engine::author_recovery_snapshot` stays public at the library layer only, because `wyrd-contracts` pins the grafting rule from outside the crate. That is contract enforcement, not a shipped workflow.

**Known limitations** (documented, not deferred silently):

- **No content recovery path.** Stranded local content has no sanctioned way back until v0.3. Neither content nor root recovery exists in v0.2.
- **Cooperative local store.** Concurrent non-cooperating writers are outside the supported concurrency model.
- **No peer repair or scrub.** A corrupt or unreachable representation falls back to the next recorded one in the fetch walk. Route publication records one serving peer per snapshot, so alternates are representations, not providers. When none serves, the waiter fails bounded (`EIO`) and invalid bytes are never committed or served. Local bitrot is detected on read but never re-fetched, so it is a permanent read error. Fix: shipped in v0.3.0-alpha (`docs/design/peer-repair.md`).
- **Unreachable history.** A known snapshot whose manifest chain can't be materialized in time fails `open()` with `EIO`. An announcement whose body never arrives leaves the snapshot unadopted, and a later snapshot built on it is `UnknownParent` (`ENOENT`, never a hang). Recovery needs a new announcement. Normative: `docs/design/fetch-on-open.md`.
- **Delivery is relay-accepted, not recipient-received.** A `Delivered` fact needs at least one relay to accept the write, and acceptance is not evidence of storage. A peer absent past retention states its gap and the sender retransmits from its durable outbox. Normative: `docs/contracts/sync-and-peers.md`.
- **Removal bounds acquisition, not knowledge.** A removed device keeps its local plaintext and can keep fetching content whose transport root it learned before removal. It learns no new roots, since later announcements are sealed under keys it never receives.
- **Admission cost is O(recorded history).** Admitting a device commits one announcement obligation per closure member in a single batch, with no incremental checkpointing. Past the 65,536-record per-commit ceiling the admission is refused whole. The walk covers the live epoch's chain only; whether that horizon is intended is an open question (`nostr:nevent1qqsdypczmlxwznanysne4qlk640fv6akxdskpmzpn9atjr9yvu6a6dgpz9mhxue69uhkwunpwdczuap49eehgcjgtm3`). Not a blocker: it needs a long single-epoch history plus an admission and violates no operational bound. Pinned by `admission_closure_cost_scales_with_history`; documented in `docs/contracts/resource-limits.md`.

## v0.3: Durable operation and content recovery (current)

Thesis: Wyrd is something an operator can safely run on one machine. Three parts, in dependency order.

**Core: durability, recovery, observability.** Device-local durability levels, mutation accumulation versus snapshot commitment, explicit transaction and commit windows, crash semantics, content-recovery snapshots and sanctioned grafting, durable outbox semantics, restart-equivalence testing. The peer-repair loop and local scrub shipped in v0.3.0-alpha: generation-scoped terminal fetch state with waiter completion, local quarantine with re-want, and fetch-failure diagnostics on the `sync now` surface. This landed before editors and mobile build on Wyrd.

Observability is real, not telemetry for its own sake: sync, peer, relay, pending work, fetch failures, materialization, queue and resource pressure, and convergence state, with explicit privacy boundaries (a vault is a different trust position from a client).

**Dogfood: unattended vault operation and self-hosted distribution.** One operator can run a reliable persistent peer: materialization policy, predictable retention, resource limits, restart recovery, health and status, relay and replication configuration, secure credential handling, upgrade behavior. `wyrd vault` on a server should be forgettable. This is not the production vault network (v0.7).

Distribution as dogfood (seeder vault, signed manifests, multi-source retrieval, `wyrd-get`, frozen public-read subset) is an optional validation track, not an exit criterion, and must never drive the schedule of the core work. The v0.3.0-alpha cut ships without it; the manual flow in `docs/releases/release-process.md` remains the mechanism.

**Architecture: consolidation after semantics stabilize.** The deferred splits (live loop, engine, mailbox supervision, serving, FUSE, contracts, CLI, tests) land once live-network semantics are stable enough that moves are safe. Refactors qualify when they reduce semantic locality cost without changing the active contract, not because a file is large. The goal is that every major invariant has a discoverable home. Don't split files while the semantic boundary is moving.

## v0.4: Cross-platform desktop product

Thesis: a normal person can use Wyrd as a desktop file-sync application.

A desktop application on macOS and Linux covering drive lifecycle (create, join, leave, device enrollment and removal or revocation, owner and member management, deletion and export), invitation and join flow, sharing and membership UX, sync status, pause and resume, selective sync and materialization, storage usage, offline and relay status, conflict indicators, settings, notifications, and key and device management. Protocol membership is not user-facing sharing: v0.4 sharing is member-only with no role picker (`docs/contracts/epochs.md`), plus removal consequences for materialized data and ownership transfer.

The test: a technically competent person installs the app and creates and manages a drive without learning what the `wyrd` CLI is. The UI talks to the drive-owner interface, not FUSE. FUSE and file-provider integrations are presentation adapters, not the product contract.

**Desktop owner (P2, decided option A):** both owner-mode composers serve the socket through the shared `OwnerService` — `wyrd vault` / `wyrd-host` headless and manual `wyrd mount` alike — so a manually mounted drive still answers a second process over `owner.sock` while the FUSE backend holds the same shared arcs. The desktop-managed surface therefore includes manually mounted drives; `wyrd mount` stays product (not CLI-only). Registry-managed `mount` mode (service template and enablement UX for desktop-registered drives) still travels with the installer follow-up. Offline commands (`member`, `snapshot`, `policy`) remain transient owners: they fail `StoreLocked` while a long-lived owner (vault, mount, or desktop service) holds the drive — stop the service first, the same rule `update` already states.

Windows presentation is a future decision; see `docs/architecture.md`.

**Conflicts:** divergence detection, preservation of both branches, and inspectable conflict state. Resolution authoring (including keep-both and conflict-name assignment) is v0.5.

## v0.5: Human-scale history and conflict resolution

Thesis: the immutable snapshot model becomes a usable human workflow.

Conflict resolution is a namespace and state UI, not a sync-engine feature: branch and head visualization, side-by-side comparison, text merge, binary selection, directory and rename/delete reconciliation, keep-mine / keep-theirs / keep-both, manual merged results, resolution audit. The UI resolves a conflict by creating a new legitimate snapshot transition and never mutates history.

Time travel is also v0.5: browsing arbitrary historical snapshots and restoring previous versions, built on the retained history v0 already keeps but renders only as live heads. Docs must not claim it earlier.

## v0.6: Mobile synchronization

Thesis: Wyrd works as a first-class mobile storage and sync system on iOS and Android.

Lifecycle-aware sync, background wakeups, push gateway, battery and resource budgets, cellular and Wi-Fi policy, selective materialization, offline operation, local cache, upload and download queues, foreground catch-up, conflict notifications, secure key storage, device enrollment and removal, mobile-specific recovery.

The operational contract is wake, inspect durable state, perform bounded work, persist, sleep, never "keep the daemon alive forever". Mobile adds lifecycle and resource policy around the shared sync engine and must not own synchronization semantics. Client semantics develop against ordinary peers; the vault-backed wake topology (phone sleeps, vault holds, desktop converges) is supported in v0.7.

## v0.7: Production vault infrastructure and disaster recovery

Thesis: Wyrd operates as infrastructure, and infrastructure can keep a drive alive and recover it.

**Production vault network:** multiple independently operated vaults with unattended replication, replication and residency policy, storage and bandwidth quotas, peer admission and removal, relay configuration, encrypted-at-rest storage, monitoring, alerts, upgrade and migration, and mobile wake workflows as a supported topology. Vaults stay ordinary protocol participants, not privileged servers. Whether "backup" means another Wyrd vault holding replicated state or an independent disaster-recovery copy with its own retention semantics is a v0.7 design decision.

**Operational root recovery** is a committed deliverable, so v0.8 can audit and freeze it: guardian selection and membership state, Shamir reconstruction, root and control-key reconstruction, fresh capability and epoch after recovery, share rotation, historical epoch handling, recovery after loss of all capable devices or after vault or device compromise, and end-to-end recovery drills. Without it, v0.9 would polish an application whose disaster story is "don't lose all your devices", which doesn't fit the v1 promise.

## v0.8: Protocol compatibility and security freeze

Thesis: the protocol and operational model are mature enough for long-lived promises.

Exit condition: no known consumer-facing change requires changing object identity, serialization, authorization, membership, epoch, recovery, conflict, storage, compatibility, or user-visible drive semantics. After this, protocol change is exceptional.

- **Protocol:** versioned fact payloads, capability negotiation, mixed-version policy, compatibility matrix, migration machinery, retention refusal semantics, epoch escrow (finalizing the contract; the mechanism has landed), finalized key-rotation semantics.
- **Security:** full threat-model review, recovery and custody review, device-compromise and malicious-vault scenarios, relay metadata, downgrade and replay, resource exhaustion, external audit if feasible.
- **Release engineering** (subordinate): reproducible builds, signed releases, update mechanism, platform packaging, migration and compatibility testing.
- Coverage gates become meaningful here, once contracts are stable enough to measure.

## v0.9: Release candidate and soak

Thesis: no major architecture remains between this and v1. Object identity, serialization, compatibility, membership, epochs, conflict and recovery semantics, client and vault roles, storage, and upgrade and migration rules are frozen.

Hard rule: no new subsystem, protocol redesign, opportunistic refactor, platform, or recovery model. Only proving what exists: soak, upgrade and migration testing, large drives, pathological histories, large vaults, multi-device and partition scenarios, mobile lifecycle, disk-full, crash injection, relay failure, malicious peers, cross-platform runs, audit fixes, UX polish.

## v1.0: Full Wyrd drive

Defined by user capabilities, not implementation completeness:

- **Files:** install on Mac, Linux, Windows, iOS and Android; create and join drives; edit with automatic sync; work offline and reconcile; choose local materialization; recover previous versions.
- **Migration:** existing directories import as first-class drives without being destroyed or rearranged. Covers symlinks, Unix permissions and ownership, Windows semantics, case collisions, Unicode normalization, timestamps, executable bits, large files, files changing mid-import, resumable imports, and explicitly documented unsupported metadata.
- **Conflicts:** understand, compare and resolve them without understanding Merkle DAGs, preserving both sides when desired.
- **Devices:** add, remove, revoke, inspect status and materialization.
- **Vaults:** configure retention, run unattended, monitor, recover, replace, several per drive.
- **Sharing:** share with other people, with explicit roles and removal consequences.
- **Recovery:** never one disk failure away from unsupported recovery.
- **Updates:** move between releases without understanding protocol versioning.

## Post-v1

Garbage collection stays post-v1: it needs a retention/acknowledgement design separate from the core protocol boundary. The current contract is append-only history, immutable objects and explicit state changes.
