# Wyrd adversarial architecture review

## Executive assessment

Reviewed commit: `483e28684be1005659291419d22ec4ca070080fa`. The working tree was clean on `master` at review time.

The implementation largely matches the intended layered architecture. The most important dependency rules are enforced by `wyrd-contracts`: production dependencies are checked against an allowlist, the FUSE view’s member-declared dependency closure excludes sync and iroh, and the embedder closure excludes the daemon and CLI. The code also has substantial named coverage for durable commits, restart recovery, authorization, and sync obligations. These checks meaningfully reduce the risk that a routine refactor silently crosses a boundary.

No demonstrated A1 architectural defect was found in the reviewed paths. Two material concerns remain:

1. **A3 — Resource limits do not bound total local retained storage.** The mounted write quota is opt-in and does not cover remote fetches, vault storage, fact logs, or the node’s direct write API. A valid peer can therefore increase durable disk use outside that quota. This is documented, but it leaves total storage unbounded in normal use.
2. **A2 — iOS writes require a cross-process writer-ownership decision.** The mobile design says the File Provider extension runs in another process, while the ownership protocol is still open. The current single-process mutation serialization cannot, by itself, define safe multi-process commits.

The project has already taken care to make the protocol/core testable without a mount or daemon. There are still real evolution decisions ahead, particularly total retention policy and mobile writer coordination. Neither finding calls for a rewrite.

## Baseline and architecture map

### Crates and responsibilities

| Crate | Implemented responsibility |
|---|---|
| `wyrd-format` | Content identities, canonical object formats, chunking, trees, snapshots, mutations, and object-store interfaces. Its manifest has no networking, async, or FUSE dependencies. |
| `wyrd-sync` | Membership and snapshot authorization, cryptographic keys and sealing, durable facts, intake, sync planning, iroh transport, serving, and control messages. |
| `wyrd-namespace` | Namespace values, verified-head proof token, materialization policy interface, and provider-neutral read surface. |
| `wyrd-fuse` | Mount-free `DriveView`: namespace lookup, conflict resolution, open/read/stat behavior. It depends on format and namespace, not the transport. |
| `wyrd-core` | Embeddable node, mutation and read surfaces, materialization, live-loop machinery, mailbox integration, and core budgets. It depends downward on format, namespace, and sync. |
| `wyrd-daemon` | Composition library for core plus FUSE backend, lifecycle, and runtime supervision. |
| `wyrd-cli` | The `wyrd` binary: CLI parsing, credentials, mount and sync commands, diagnostics, and exit codes. |
| `wyrd-embed` | An embedder-shaped consumer demonstrating a drive lifecycle through core without daemon or CLI. It does depend on `wyrd-fuse` for its mount-free view, whose production closure excludes FUSE libraries and transport. |
| `wyrd-contracts` | Cross-crate architectural contracts. It is a workspace leaf in the actual manifests; it does not depend on the binary-only CLI. |

### Dependencies and call paths

The intended and implemented production direction is:

```text
wyrd-format
     ↑
wyrd-sync ← wyrd-namespace
     ↑              ↑
 wyrd-core          wyrd-fuse
     ↑              ↑
wyrd-daemon ─────────┘
     ↑
wyrd-cli (binary)
```

`wyrd-embed` consumes core, sync, format, and the mount-free view. `wyrd-contracts` composes the library crates in tests.

Representative paths:

- **Write:** `WyrdNode::put_file` → `Engine::begin_write` → format chunk/tree mutation → `Engine::author_pending_write` → install the authored head in the view.
- **Read:** a presentation backend → `NamespaceView`/`DriveView` → local object store; core’s live reader can register bounded demand for remote-only data.
- **Inbound sync:** mailbox → core/sync intake → durable facts and runtime reconstruction → fetch plan and verified object import → head/materialization projection refresh.
- **Authorization:** sync validates the membership/snapshot state and mints `AuthorizedSnapshot`; namespace carries that proof into heads.

The key boundary difference between architecture prose and code is naming: `wyrd-fuse` is described as a FUSE layer, but its library is a mount-free read view and has no FUSE library dependency. The actual OS-facing FUSE backend is in `wyrd-daemon`. That arrangement is consistent with the intended separation, though the crate name can mislead readers.

### Documents inspected

I read `docs/architecture.md` first, then inspected the normative contracts most relevant to this review:

- `object-model.md`, `trust.md`, and `epochs.md`
- `sync-and-peers.md`
- `write-path.md` and `fetch-on-open.md`
- `resource-limits.md` and `storage-growth.md`
- `crash-consistency.md` and `retention-constraints.md`
- `mobile.md`, `retry-policy.md`, `upgrade-contract.md`, and `release-read.md`

I also inspected workspace and crate manifests, the layer-contract implementation, core node/namespace/view code, durable-store code, and the relevant budget configuration and references. I did not execute tests or build commands.

## Ranked findings

### 1. A3 — The configured retained-bytes quota is not a total storage bound

**Confidence: High.** Both the documented exclusions and the quota wiring are explicit.

**Intended property:** Resource limits should make storage policy and its scope clear, and a configured bound should cover the operation or state it claims to bound.

**Evidence:** `resource-limits.md` says the retained-bytes quota is enforced only on the mounted write path. It explicitly excludes fetched bytes, the vault, the fact log, and `WyrdNode::put_file`/`remove`. The same section notes that a remote author can consume a peer’s local-write headroom. In code, the quota is wired through `LiveConfig` and checked during the mounted commit path; it is not a universal limit on every object-store or fact-log write.

**Scenario and consequence:** A peer sends valid snapshots and the local node fetches their content. The data becomes durable even if the mounted-write quota is configured or exhausted. The quota therefore cannot be relied on as a bound for total disk consumption; in the default CLI configuration it is also unset. This is an operational and resource-exhaustion risk, not a contradiction of the append-only contract.

**Smallest correction:** Define a separate retention/residency policy that covers fetched objects and vault storage, and explicitly decide what happens when that policy is reached. With append-only storage and no GC, that decision may be to stop accepting or fetching some content; a quota alone cannot reclaim space.

**Recurrence guard:** Add a resource contract that enumerates every durable byte-producing ingress and asserts the configured policy’s scope. Keep the current mounted-path tests, but do not describe them as a total-store bound. The open accounting work in `storage-growth.md` is the appropriate place to resolve the policy before widening claims.

### 2. A2 — iOS write ownership is unresolved across process boundaries

**Confidence: High for iOS writes; this is a documented blocker, not a defect in current desktop behavior.**

**Intended property:** There must be one authoritative mutation sequence constructing and publishing snapshots for a local store.

**Evidence:** `mobile.md` specifies that the iOS File Provider extension runs in a separate process and states that shared storage does not authorize multiple writers. Its mutation-ownership section requires an equivalent cross-process protocol if multiple processes may write, and its decision record leaves the iOS writer model open. The current mutation queue and `&mut Engine` serialization are in-process mechanisms. The durable store’s lock prevents a second engine open from owning the same drive concurrently, but it does not provide a shared writer service that lets independently running app and extension processes safely coordinate writes.

**Scenario and consequence:** The iOS extension and application each attempt to operate on the same drive. Without a defined ownership protocol, one process must either fail to open while the other owns the store or risk stale observations if an implementation works around that exclusion. A simple shared directory does not make snapshot authoring transactional across processes. This blocks a clean iOS write implementation; it does not block mobile read surfaces.

**Smallest correction:** Decide and document the iOS writer owner before implementing writes—most simply, have one process own the engine and delegate extension mutations to it. If independent writers are required, specify the lease, refresh, commit, and crash-recovery protocol first.

**Recurrence guard:** Add the mobile cross-process contracts already listed in `mobile.md`: exclusive writer ownership, head refresh after ownership acquisition, stale-observation refusal, process-death recovery, and durable outbox recovery. Do not treat a file lock alone as proof of the complete protocol.

### 3. A3 — Core exposes a mutable engine alongside a separately refreshed view

**Confidence: Medium.** The API permits projection lag; the reviewed code does not establish that callers routinely misuse it.

**Intended property:** Durable state is authoritative, and published namespace/materialization views are derived projections whose refresh boundary is explicit.

**Evidence:** `WyrdNode` stores an `Engine` and a view separately. `parts_mut()` exposes `&mut Engine` alongside `&V`, and public engine operations can commit durable state. Core refreshes projections in selected paths such as `drain`, `execute_plan`, and `refresh_live_heads`; `put_file` installs its new head directly. Thus an embedder can mutate the engine through the public lower-level surface and then continue reading a view that has not been refreshed.

**Scenario and consequence:** An embedder authors or ingests a snapshot directly through the engine and immediately queries `node.view()`. Durable state has advanced, but the namespace projection may still show the previous head until the caller knows which refresh method to invoke. That is a change-amplification and stale-read risk at the embedder boundary.

**Smallest correction:** Clarify which Engine mutations require which projection refreshes, or provide node-level operations for mutations that must become visible synchronously. Keep raw engine access for operations whose results are intentionally observed only through durable APIs.

**Recurrence guard:** Add a contract that commits through every supported public mutation path and asserts the documented read behavior afterward—either immediate projection update or explicitly required refresh. The existing headless lifecycle contract is useful, but it does not by itself prove every `parts_mut()` call preserves view freshness.

## Invariant ownership

| Invariant | Authoritative owner | Enforcement point | Durable evidence | Recovery behavior | Coverage inspected |
|---|---|---|---|---|---|
| Canonical content identity and immutable objects | `wyrd-format` | Typed IDs, canonical codecs, store insert/get validation | Content-addressed object files | Re-derive on read; corrupt objects fail closed | Format tests and object-store code; did not inspect every codec test |
| Membership and snapshot authorization | `wyrd-sync` | Transition validation and authorization; single `AuthorizedSnapshot` mint crossing | Durable transition/snapshot facts | Rebuild runtime state from durable log; retain invalid/voided history by contract | Epoch conformance tests; proof-mint/dependency contracts |
| Durable sync obligations | `wyrd-sync` | Outbox facts, delivery and reconciliation logic | Fact log through `CURRENT` | Replay pending obligations and resend byte-identical sealed messages | Crash-consistency document and engine test inventory |
| Local materialization policy and residency | Sync engine plus core composition | Durable policy facts and fetch/materialization accounting | Facts plus local object bytes | Reconstruct projection; fetch missing bytes when policy/runtime calls for them | Resource docs and core live code |
| Namespace projection | `wyrd-core` programs; namespace/view consume | Verified-head token and view publication methods | Snapshot bodies and durable authorization facts; projection itself is derived | Rebuild after reopen; retain last known good view while a closure is pending | Node methods and crash/restart inventory |
| Mounted write serialization | `wyrd-core` live mutation path | Queue and daemon loop | Snapshot/fact commit | Pending volatile writes are lost on crash; committed writes replay | Write-path contract and daemon test inventory |
| Process-exclusive durable engine ownership | `wyrd-sync` | `DurableStore::open` lock | Kernel-held lock, not a durable record | Lock releases on process death; reopen reconstructs from disk | Durable-store code and crash-consistency docs |
| Total retained disk use | **No single owner today** | Mounted write quota covers only one path | Objects, vault, and fact log persist independently | No GC; recovery does not reduce retained bytes | Explicitly scoped and left open in resource/storage-growth docs |

## Future evolution

- **Desktop:** The current composition supports a daemon library and a separate CLI host. FUSE behavior remains at the presentation edge. No desktop boundary blocker was demonstrated.
- **History and conflict resolution:** The format and epoch contracts distinguish DAG conflicts from path conflicts, preserve heads, and refuse implicit resolution. The reviewed surfaces support the stated conflict visibility model.
- **Mobile reads:** The provider-neutral namespace and mount-free view are usable without linking transport through the view. The existing architecture provides a plausible read boundary; platform integration itself is not implemented.
- **Mobile writes:** iOS writer coordination is a genuine prerequisite, as described above. Android’s initially same-process model does not raise the same immediate cross-process issue, though platform lifecycle contracts remain unimplemented.
- **Vault and recovery:** Root custody, epoch secrets, and recovery boundaries are documented. I did not audit the cryptographic implementation in depth; no security conclusion should be inferred from this architecture review.
- **Security freeze and stable release:** The project has explicit format, trust, upgrade, and release-read contracts, but remains pre-alpha. The open total-retention policy and platform lifecycle contracts should be resolved before claims of bounded storage or multi-process mobile writes.
- **Premature abstractions:** I found no strong case for extracting another crate or introducing a new general abstraction. The current core/view split is backed by dependency checks and a headless consumer.

## Remediation sequence

**Resolve before the dependent feature work:**

1. Decide the iOS write-ownership protocol before implementing iOS writes.
2. Decide whether total local retained storage needs a bound and define behavior at the bound before advertising storage as bounded.

**Later contract hardening:**

3. Pin projection freshness expectations for public engine mutations exposed through `WyrdNode::parts_mut()`.

None of these findings warrants a broad refactor.

## Coverage limitations

This is a source and contract review, not a full verification run. I did not run tests, compile the workspace, execute Lima/microVM e2e, or exercise live relay behavior. I did not read every source file or enumerate every state-machine transition. Android and iOS adapters are not present to review; cryptographic primitives and protocol vectors were not independently audited. The findings above are limited to the inspected manifests, architectural contracts, core composition surfaces, and durable/resource-policy paths.
