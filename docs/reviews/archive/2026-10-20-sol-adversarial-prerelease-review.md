# Adversarial pre-release review: architecture, code quality, security

- **Target:** `wyrd`, `/Users/thomas/workspace/wyrd/master`
- **Reviewed commit:** `483e28684be1005659291419d22ec4ca070080fa` (`0.3.0-alpha`)
- **Review date:** 2026-10-20
- **Method:** static source, contract, build-recipe, workflow, and test inspection. No tests, builds, benchmarks, integration suites, services, Lima, or microVMs were run.
- **Baseline:** HEAD matched the requested commit. The initial clean-tree check found three untracked sibling review documents; the user explicitly authorized continuing while disregarding them. Tracked working-tree and index diffs against the target were empty before writing this document. Those sibling documents were left untouched and are not evidence for the findings below.

## 1. Executive assessment

**Verdict: BLOCK, within the reviewed scope only.** Four confirmed defects warrant withholding this release: peer-controlled snapshot timestamps can permanently disable local authoring, including recovery and namespace carries (C1); export can report success after deleting a legal user filename from its output (B1); fetched facts are neither checkpointed at deadline exits nor batched to the durable commit ceiling, allowing valid input to lose progress or repeatedly fail the same oversized batch (A1); and operational verification failures after an executable swap bypass updater rollback (C2). Additional findings concern stale embedded projections, an unbound write-session API, validation-trigger omissions, and resource-exhaustion paths. The missing receive-side retention and residency ceilings are explicitly documented implementation gaps, not newly discovered breaches of a claimed total-disk bound. The intended release KDF configuration was established from repository recipes, but no particular release artifact was built, executed, or attested. This assessment is not a claim that unexamined paths are free of defects.

## 2. Ranked findings

Findings are ordered by severity, then confidence, within each track. “Confirmed” means the failure follows from the target commit's code and contracts; it does not mean a reproduction was executed. Regression tests below are proposed, not performed.

### Track A: architecture

#### A1. Fetch execution does not preserve a commit-safe prefix at its bounds

- **Status:** confirmed defect.
- **Confidence:** high.
- **Severity:** high.
- **Release disposition:** block release.
- **Scope:** shipped sync executor and public engine/node fetch APIs; availability and progress-accounting failure.

**Evidence.** `crates/wyrd-sync/src/runtime/plan/mod.rs:84-100` creates one in-memory replay and one fact vector per reconciliation iteration. Successful bodies, roots, children, and objects extend that vector at `:141-146`, `:213-217`, `:265-269`, and `:361-366`. The deadline exits at `:99-101`, `:193-195`, `:244-246`, and `:296-298` return before the sole commit at `:393-406`. That commit has no executor-side byte/count batching. The durable limits are 64 MiB and 65,536 records (`crates/wyrd-sync/src/durable/codec/tags.rs:127-134`), enforced before publication by `durable/store.rs:544-563,737-750`. Manifest records contain the complete canonical manifest (`durable/codec/facts.rs:88-98`). Per-object intake allows 64 MiB and 580,000 mappings (`ingest.rs:80-96`), not a combined-batch ceiling. The live loop calls the sliced executor at `crates/wyrd-core/src/live/pass.rs:374-403`; `WyrdNode::execute_plan` calls the unsliced API at `node.rs:154-166`.

**Failure scenario.** There are two manifestations of the same missing checkpoint boundary:

1. A first item fetches successfully and adds a fact. Its transfer/verification/import, or a subsequent attempt, spends the remaining slice. The next loop-entry deadline check returns success with the earlier fulfillment counted but no fact committed. The next invocation replays the old state and requests that prefix again. This is an ordinary time-slice exit, not a crash.
2. Two individually valid root manifests of about 40 MiB each are pending. Each fits the object and mapping ceilings. If both are fetched in one iteration, their combined fact encoding exceeds 64 MiB. The entire commit refuses; neither manifest is recorded, and the next execution reconstructs the same batch. The unsliced API reaches this condition without a timing precondition; the live path reaches it when the batch fits its fetch slice. Durable-store rejection remains correct, but the executor offers no subdivision to make progress. The failure also enters the live loop's fatal-store classification through `LiveError::Engine(EngineError::Durable(_))` (`live/config.rs:101-109`).

Physical imports can survive either sequence while durable knowledge does not. Reported fulfillment and committed progress consequently disagree. This contradicts the bounded-progress description in `runtime/plan/mod.rs:39-44` and the commit-budget coverage claim in `docs/contracts/resource-limits.md:80-96`.

**Correction.** Give the executor one bounded fact-checkpoint path: commit the successful prefix before a normal deadline return, and flush batches before either durable count or encoded-byte ceiling is reached. Preserve replay equivalence when a checkpoint fails. Report committed fulfillment, not a discarded in-memory prefix.

**Regression test.** In the plan suite, force a deadline after one verified fulfillment while another item remains; assert that the first fact survives reopen and is not fetched again. Separately feed individually admissible manifests whose combined fact encoding exceeds 64 MiB; assert multiple admissible commits and eventual convergence, with no repeated whole-batch refusal. Existing `runtime/plan/tests_bounded.rs:229-320` exercises failed/stalling attempts, not this successful-prefix case.

#### A2. Live composition can permanently adopt an empty or stale baseline

- **Status:** confirmed defect.
- **Confidence:** high.
- **Severity:** medium.
- **Release disposition:** fix before release if feasible.
- **Scope:** public embedded composition; current CLI callers usually perform the missing refresh explicitly.

**Evidence.** `crates/wyrd-core/src/node.rs:87-95` constructs every view with zero heads even when the engine has durable eligible heads. `parts_mut` exposes an unrestricted mutable engine beside the old view (`:121-125`). `into_live` refreshes heads only if carry recovery changes the queue, then captures the engine's current revision (`:376-395`). With no pending carries, `Engine::carry_pending` returns immediately (`crates/wyrd-sync/src/runtime/engine/carry.rs:70-74`). `LiveNode::split` adopts the supplied baseline and revision, initializes `dirty: false`, and separately counts engine heads (`live/mod.rs:276-325`). An idle pass at that revision returns without installing them (`live/pass.rs:489-560`), even after its closure scan establishes they are available.

**Failure scenario.** Reopen a complete solo drive, construct `WyrdNode::new`, and immediately call `into_live`, with no pending carries or outbound work. The namespace serves an empty root. Subsequent otherwise-idle sync passes see the captured current revision and do not repair it. A second trigger is a successful engine mutation through `parts_mut` after an earlier refresh: the live baseline can retain the old namespace while claiming the new revision. This is not the accepted “retain the last good projection after a projection error” behavior in `docs/contracts/epochs.md:557-568`; no projection was attempted or failed here. The live split's own contract says the backend must not serve empty while the engine has heads (`live/mod.rs:250-253`).

**Correction.** Refresh both head and materialization projections at the `into_live` boundary after carry recovery, before recording the baseline revision. Make initialization/freshness requirements explicit for direct `view()` consumers as well; the constructor currently provides no freshness proof.

**Regression test.** Reopen a populated solo drive and enter live composition without any explicit refresh. Assert immediate namespace readability and consistency after an idle pass with zero new commits. Repeat after an engine-authored change through `parts_mut`. The existing embedded lifecycle explicitly refreshes after construction (`crates/wyrd-embed/src/embedded_lifecycle.rs:150-157,210-215`), masking this boundary failure.

#### A3. Normative vault confidentiality claims contradict the plaintext snapshot-body exception

- **Status:** confirmed defect, specifically a normative documentation contradiction.
- **Confidence:** high.
- **Severity:** medium.
- **Release disposition:** fix before release if feasible.
- **Scope:** present contract inconsistency and latent keyless-replica integration risk; no new exploit against the shipped member-hosted composition is established.

**Evidence.** `docs/architecture.md:157-159` says content IDs never reach vaults. `docs/contracts/trust.md:957-968` guarantees that the keyless-replica position never receives snapshot contents/authorship or ContentIds, and `:1055-1068` restates those guarantees. T17 says serving returns ciphertext only (`:1119`). Conversely, the normative serving contract explicitly permits plaintext snapshot bodies at `docs/design/fetch-on-open.md:99-106`. Implementation follows that exception: authoring imports `snapshot.encode()` directly (`crates/wyrd-sync/src/runtime/author/snapshot.rs:204-216`), receiving does the same (`runtime/plan/mod.rs:135-144`), and endpoint boot rebuild mirrors every vault root (`serving/endpoint.rs:92-107`). The body's clear fields include the root ContentId, author, parents, membership reference, and epoch (`crates/wyrd-format/src/snapshot.rs:135-155`). It does not itself carry path strings.

**Failure scenario.** A future keyless replica reusing this vault/mirror composition would receive metadata that the H5/H6 guarantees forbid, without decrypting any object. The current member-hosted process already legitimately holds that metadata, and `trust.md:1011-1023` explicitly says a keyless replica is a future posture. Thus this is not evidence that current unaffiliated peers can discover private hashes, read chunks, or enumerate the store. It is a real disagreement between normative contracts that must not be resolved by silently choosing the stronger promise.

**Correction.** Propagate the existing snapshot-body exception into the architectural and trust descriptions, with its exact trust-position scope. Make keyless replication establish H5/H6 separately rather than inheriting the assumption that every vault entry is ciphertext. Preserve the settled body-serving decision unless an explicit protocol change is made.

**Regression test.** A prose contradiction is not meaningfully prevented by a rendered-string omission test. Before adding a keyless replica, add a composed disclosure test using real encoded snapshot bodies, proving that its admissible inputs uphold the declared H5/H6 boundary. The existing snapshot layout test already pins the clear fields (`snapshot.rs:259-286`).

### Track B: codebase quality

#### B1. Export heartbeat scratch can silently remove a user file

- **Status:** confirmed defect.
- **Confidence:** high.
- **Severity:** high.
- **Release disposition:** block release.
- **Scope:** shipped `wyrd export`; successful but incomplete offline egress.

**Evidence.** Export places its internal heartbeat inside the same staging root as the user's files (`crates/wyrd-core/src/export/walk.rs:49-54`). The heartbeat path is `.wyrd-heartbeat` (`export/staging.rs:30`), and every 64th renewal uses `self.path.with_extension("tmp")`, followed by truncating `fs::write` and rename (`:268-277`). That scratch name is exactly `.wyrd-heartbeat.tmp`. User entries are written directly into the staging root without reserving either name (`export/walk.rs:148-164,179-201`). Completion removes `.wyrd-heartbeat` and publishes the staging directory (`:90-109`). These are legal drive names under `docs/contracts/object-model.md:182-189`. The CLI reports success from the returned counters (`crates/wyrd-cli/src/commands/export.rs:33-43`).

**Failure scenario.** Export a complete tree containing a nonempty root file `.wyrd-heartbeat.tmp` followed in canonical order by 100 empty `a...` files. The user file is exported first. A later renewal overwrites that file with the heartbeat marker and renames it to `.wyrd-heartbeat`; final cleanup deletes the marker. Export returns `Ok`, counts the user file and its bytes, and publishes a tree missing it. No malicious filesystem manipulation, crash, or missing content is needed. A user file named `.wyrd-heartbeat` instead meets the already-present marker and prevents export outright. The silent omission violates the complete-copy contract in `docs/design/cli.md:84-94,108-119` and `export/mod.rs:13-29`; it can cause actual loss if an operator relies on the successful copy as the guaranteed egress before abandoning an old store.

**Correction.** Keep exporter control state outside the user-content subtree, for example in a staging container whose separate payload directory is the object renamed to the destination. All valid user names must remain representable; choosing another fixed marker name inside that subtree only moves the collision.

**Regression test.** Export a real view containing `.wyrd-heartbeat.tmp` and enough later entries to force renewal. Compare the entire destination filename-to-bytes map against the source and assert that report totals match it. Also cover a genuine `.wyrd-heartbeat` file. Existing exact-copy and staging tests use other names (`export/tests_walk.rs:9-60`, `tests_staging.rs:13-47`) and do not protect this invariant.

#### B2. `PendingWrite` does not enforce its advertised freshness or origin

- **Status:** confirmed defect.
- **Confidence:** high.
- **Severity:** medium.
- **Release disposition:** fix before release if feasible.
- **Scope:** latent public-library hazard; the current `WyrdNode::put_file` sequence holds the intended exclusive borrow.

**Evidence.** `crates/wyrd-sync/src/runtime/engine/mod.rs:851-870` says staleness is unrepresentable, but `PendingWrite` owns only `Rebuilt` and captured heads. `begin_write(&self)` returns it without borrowing the engine (`engine/authoring.rs:203-210`). `author_pending_write` accepts that owned session, checks only its captured head count, and passes its rebuilt state to authoring (`:220-240`). Authoring takes membership and epoch secrets from that supplied state (`runtime/author/snapshot.rs:156-177,212-224`), then commits through the receiving engine (`:266-285`). The durable snapshot-token encoder does not re-check current membership or session origin (`durable/codec/facts.rs:65`).

**Failure scenario.** Safe external Rust code can call `begin_write`, mutate the same engine, and then submit the saved session. For example, capture a session at epoch N, remove the local author from membership, and call `author_pending_write`: the stale member set and held secret still pass its authoring checks. It can return a durably recorded epoch-N snapshot that the current view will classify as superseded, rather than refusing the stale operation. Capturing two sessions before a normal intervening write also permits unintended forks. A session from another drive sharing the signing identity can be passed to this engine, combining one drive's replay/key material with another drive's signature namespace. These are caller-discipline failures, not an attacker modifying process memory; no network privilege escalation is claimed. They contradict the stated current-canonical local-authoring rule (`docs/contracts/epochs.md:527`, `runtime/author/mod.rs:17-22`).

**Correction.** Bind a session to its originating engine/store instance and durable revision, and reject a mismatch before any vault import or fact commit. This preserves the single-rebuild optimization while making its precondition enforceable at the public boundary.

**Regression test.** Capture a session, commit a membership change or intervening snapshot, and assert stale submission refuses without changing CURRENT or vault roots. Pass a session between two drives using the same identity and assert the same refusal. Retain a successful no-intervening-commit case so the optimization remains usable.

#### B3. CI trigger filters exclude inputs that change the binary or the validation lane

- **Status:** confirmed defect.
- **Confidence:** high.
- **Severity:** medium.
- **Release disposition:** fix before release if feasible.
- **Scope:** shipped workflow definition; latent undetected-breakage path.

**Evidence.** Both push and ready-for-review filters enumerate their accepted paths in `.ngit/act/workflows/workflow.yml:3-40`. They omit `vendor/**`, `.config/nextest.toml`, `.cargo/**`, and `devenv.nix`. The root Cargo manifest patches the production `fuser` dependency to `vendor/fuser` (`Cargo.toml:28-36`); its platform build selection is real product code (`vendor/fuser/build.rs:7-30`). Nextest profiles decide which tests execute (`.config/nextest.toml:64-65,117-122`), while CI invokes those profiles at `workflow.yml:152-176`. A CI shell test directly reads `devenv.nix` to assert the hook wiring (`tests/test-bash-syntax.sh:49-65`).

**Failure scenario.** A vendor-only change can alter production mount/FFI code without triggering the Rust or Nix validation jobs. A nextest-config-only change can remove contract or security coverage without triggering even a parse/run check of the new configuration. A hook-definition change can invalidate the existing shell assertion while the workflow never runs it. This does not show a broken target artifact, but it establishes concrete unvalidated changes, not a generic recommendation for more CI.

**Correction.** Add these actual build/test inputs to both trigger lists, including the vendored package and test-lane configuration. Keep the two lists derived from one declared set or structurally checked together.

**Regression test.** Add a workflow-policy check that representative paths such as `vendor/fuser/src/lib.rs`, `.config/nextest.toml`, `.cargo/config.toml`, and `devenv.nix` match both relevant trigger filters. It should parse/check the real workflow, not assert a second disconnected list.

### Track C: security

#### C1. One extreme snapshot timestamp permanently disables authoring

- **Status:** confirmed defect.
- **Confidence:** high.
- **Severity:** high.
- **Release disposition:** block release.
- **Scope:** shipped peer intake and all local snapshot-producing operations; persistent availability failure.

**Evidence.** A snapshot's timestamp is an unrestricted `u64` in construction and decoding (`crates/wyrd-format/src/snapshot.rs:83-107,190-219`). Intake verifies announcement framing, transition standing, and signature (`crates/wyrd-sync/src/runtime/intake/mod.rs:571-659`); body fetch verifies identity and parent-count limits (`runtime/fetch/mod.rs:168-214`); the plan verifies its signature and commits it (`runtime/plan/mod.rs:126-145`). None bounds the timestamp. Every local authoring operation computes `max_seen` over **all** recorded bodies and fails when `checked_add(1)` cannot advance it (`runtime/author/snapshot.rs:179-187,638-649`). Recovery calls the same `author_over` (`:107-132`); carries reach it through `author_with_parents` (`:65-75`, `runtime/engine/carry.rs:139`).

**Failure scenario.** A member signs and announces a snapshot with timestamp `u64::MAX`, reusing an otherwise ordinary tree. Once a recipient records the body, all its later authoring returns `TimestampExhausted`. The body need not remain an eligible head: the maximum includes retained history, so removal of the sender, advancing epochs, and restarting do not clear it. Recovery is blocked by the same maximum, and an epoch change can commit before its carry fails, leaving a pending carry that subsequently makes live composition fail closed. This is not merely display skew. It turns display-only peer metadata into a permanent local write veto, contrary to the separation in `docs/contracts/trust.md:655-657` and `docs/contracts/epochs.md:371-372`.

The primary trigger needs an authorized writer, not the owner. The recorded-body gate is only a signature gate, and the maximum is not classification-filtered, so excluding only current eligible heads would also be an incomplete remedy. No unexecuted lower-privilege exploit is needed to establish the finding.

**Correction.** Make overflow of the observed display maximum non-fatal, with an explicit overflow behavior such as saturation; update the strict-monotonicity prose in `docs/contracts/object-model.md:291-292` for that exceptional case. Preserve historical bytes and authorization independence. An intake-only future-time rejection would not repair stores already holding the offending body and would introduce wall-clock-dependent acceptance unless separately specified.

**Regression test.** Deliver a correctly signed maximum-timestamp body through announcement and body intake, reopen the receiver, and prove an ordinary write still succeeds. Then remove the sender, carry the namespace, and author recovery without rewriting history. Cover a retained non-eligible extreme body too. `runtime/author/tests_snapshot.rs:4-14` currently pins helper exhaustion but never tests the remotely supplied value's effect on drive operability.

#### C2. Post-swap verification errors escape without restoring the old executable

- **Status:** confirmed defect.
- **Confidence:** high.
- **Severity:** high.
- **Release disposition:** block release.
- **Scope:** shipped updater's operational failure handling; fail-closed lifecycle violation, not an arbitrary-code-execution claim.

**Evidence.** `crates/wyrd-cli/src/update/run.rs:175-188` performs both executable renames, then applies `?` to `run_verify_child`. Rollback occurs only in the returned `VerifyOutcome::Rejected` arm (`:190-223`). `update/supervise.rs:41-53,69-82` instead returns `Err` on spawn/wait failure or timeout. The production timeout is 300 seconds (`:9-14`). The CLI propagates `update_run` errors directly (`src/main.rs:265-273`), with no outer rollback. Recovery on a later update can restore `prev` (`update/state.rs:265-284`), but that is not restoration before this invocation exits.

**Failure scenario.** The staged binary passes early verification. After the swap, the authoritative child cannot spawn because of process/resource exhaustion, or its replay exceeds the timeout because of a stalled disk or changed load. The updater returns an operational error, leaving the **new, unverified** executable at the ordinary service path, the old binary only at `.prev`, and state at `Swapped`. `--restart` also exits instead of supervising the restored old binary. A service owner following the handoff can start the unverified replacement. The timeout message says it never reported, not that the service path still contains that replacement. `docs/contracts/update.md:172-179,267-274` explicitly requires restoration for nonzero, timeout, and signal outcomes; the timeout function's own comment also promises rollback (`update/supervise.rs:9-13`).

**Correction.** Route all post-swap unsuccessful outcomes, including returned operational errors, through restoration and terminal-verdict recording before returning or handing off supervision. Use a transaction guard spanning the swap so errors during phase recording or the second rename cannot bypass the same cleanup discipline. Preserve the operational cause rather than falsely calling it format incompatibility.

**Regression test.** Exercise the complete transaction with a verifier double that succeeds early and times out or fails to spawn after swap; assert old executable bytes, cleaned transaction state, and a rolled-back verdict. Under `--restart`, assert supervision targets the restored executable. `update/tests.rs:804-864` covers returned rejection; the misleadingly named timeout test at `:923-945` calls only `run_verify_child` and never asserts rollback.

#### C3. Poison traffic can monopolize a sync pass because intake has no service bound

- **Status:** confirmed defect.
- **Confidence:** high for the unbounded control flow and conditional starvation sequence.
- **Severity:** medium.
- **Release disposition:** fix before release if feasible.
- **Scope:** shipped mailbox/engine boundary; CPU starvation and potentially unbounded per-pass bookkeeping under sustained arrivals. Required traffic rate was not measured.

**Evidence.** `crates/wyrd-core/src/mailbox/mod.rs:962-995` repeatedly consumes duplicate or malformed wraps until it finds a deliverable one or the incoming channel is momentarily empty. It has no scan budget. The engine drain similarly loops until `recv` returns empty or repeats a delivery ID (`crates/wyrd-sync/src/runtime/intake/mod.rs:152-211`), retaining every offered ID in a per-pass `HashSet`. Its 1,024/256 budgets bound committed facts (`:22-74`), not discarded envelopes or total work. The live loop performs intake before fetch or mutations (`crates/wyrd-core/src/live/pass.rs:210,374-403,410-441`). The trait's claim that one-off offering guarantees termination (`wyrd-sync/src/transport/mailbox.rs:262-271`) does not hold for an actively replenished stream of unique IDs.

**Failure scenario.** A sender knowing the recipient pubkey continuously supplies syntactically valid NIP-59 wraps containing invalid Wyrd material. Such mail needs no drive membership to construct. As long as arrivals keep the channel nonempty, pre-envelope poison can keep a single `recv` inside its loop, or engine-discarded unique deliveries keep the outer drain running. Terminal settlement frees each held slot, so the 1,024-unacked limit does not force a yield. Fetch, publication, mutation execution, and the loop's stop check cannot run until intake returns. Finite large backlogs cause the corresponding long pause; persistent replenishment has no code-imposed endpoint. This is distinct from the documented absence of a time-based commit-rate cap (`docs/contracts/resource-limits.md:167`).

**Correction.** Add a bounded mailbox scan/yield mechanism and a per-drain handover/work budget independent of facts committed. Keep unprocessed mail retained and allow the remaining local pass phases and shutdown observation to execute before continuing intake.

**Regression test.** Use a deterministic mailbox double that offers fresh poison IDs indefinitely and fails the test if more than the declared budget are requested in one pass. Assert the pass returns and a queued local mutation is serviced. Separately feed more malformed/duplicate outer wraps than one mailbox scan budget and assert `recv` yields without consuming the whole backlog. Existing finite flood tests do not establish these service bounds.

#### C4. One retried delivery makes settlement bookkeeping grow for the process lifetime

- **Status:** confirmed defect.
- **Confidence:** high.
- **Severity:** medium.
- **Release disposition:** post-release hardening.
- **Scope:** shipped mailbox memory retention; explicitly acknowledged in source, not a novel observation or a failure of the durable seen-ID bound.

**Evidence.** `crates/wyrd-core/src/mailbox/mod.rs:473-487` documents the unbounded `outstanding` set. `forget_held` inserts each terminally settled ID and can remove it only by advancing a contiguous watermark (`:843-863`). `Retry` preserves the old delivery (`:1030-1033`), while both Ack and Poison add later IDs through `forget_held` (`:1013-1028`). Unknown-epoch mail is a reproducible retry source (`crates/wyrd-sync/src/runtime/intake/mod.rs:247-252`).

**Failure scenario.** Hold delivery 1 indefinitely on an unavailable epoch, then deliver and poison-settle N distinct later messages. The held queue and poison cache remain bounded, but `outstanding` grows to N entries because the watermark cannot pass 1. This works across finite drain passes and does not require C3's continuously nonempty channel. A peer knowing the drive ID can construct the retryable header; later poison delivery itself requires no membership. Restart clears this particular state. The exact time or number of messages needed for process OOM was not measured; the confirmed consequence is unbounded attacker-influenced memory growth, with hash-table overhead beyond the source comment's approximate per-ID payload cost.

**Correction.** Exploit dense, monotonically minted session IDs: an ID satisfying `0 < id < next_delivery` that is not currently held is already settled. This preserves repeated-settlement idempotence without retaining every settled successor behind a gap. Keep future and zero IDs invalid.

**Regression test.** Leave the first delivery on Retry while terminally settling substantially more than the poison-cache/window limits. Assert bounded settlement bookkeeping, that the first delivery still reoffers, that repeated settlements succeed, and that never-issued IDs fail. `mailbox/tests_dedupe.rs:138-172` closes its three-ID gap; `tests_poison.rs:19-117,251-328` tests pure poison and pure retry separately, missing the mixed shape.

#### C5. Merged-directory enumeration is quadratic within admitted object sizes

- **Status:** confirmed defect.
- **Confidence:** high.
- **Severity:** medium.
- **Release disposition:** fix before release if feasible.
- **Scope:** shipped namespace presentation; adversarial member input or ordinary large conflicted directories. Wall-clock impact was not benchmarked.

**Evidence.** Differing directory nodes across heads produce `Node::MergedDir` (`crates/wyrd-fuse/src/view/merge.rs:26-33,45-54`). `DriveView::readdir` routes those to `readdir_union` (`view/drive.rs:175-203`). That function collects every name, then performs `tree.entries().iter().find(...)` over every source tree for every name (`:211-241`). Canonical trees already support binary lookup (`crates/wyrd-format/src/tree.rs:211-222`). The admitted tree limit is 1,000,000 entries (`crates/wyrd-sync/src/ingest.rs:80-87`), and flat directories are an explicitly supported simplification (`docs/contracts/object-model.md:218-225`).

**Failure scenario.** Two valid eligible heads have almost identical million-entry directories of empty files, differing at one entry. Such trees fit below the 64 MiB object ceiling with short names. Listing their merged directory performs roughly a trillion name comparisons across the two sorted vectors, even though the incoming data is only tens of MiB per tree and has already passed closure validation. An authorized member can supply this shape; an honest offline fork can also create it. Kernel pagination does not help because the union is assembled before the caller receives entries. The listing/consumer thread is occupied by avoidable superlinear work, with no corruption or oversized input required.

**Correction.** Replace each repeated linear search with the existing canonical-tree binary lookup, preserving the current absence-versus-presence conflict semantics.

**Regression test.** Exercise merged enumeration with large overlapping source directories, present/absent names, and one differing leaf. Instrument comparison work at the lookup seam and assert subquadratic growth rather than a machine-dependent elapsed-time threshold. Preserve exact union and conflict-version results.

## 3. Coverage record

All rows below have **no dynamic verification**. Reading an existing test establishes what it asserts, not whether it passes at this revision.

| Area | Inspection depth | Evidence inspected | Dynamic verification |
| --- | --- | --- | --- |
| Required normative baseline | In depth | `docs/architecture.md`, `object-model.md`, `trust.md`, `epochs.md`, `upgrade-contract.md`, `release-process.md` | No |
| Retention/residency scope | In depth for contract; selective implementation | `docs/design/storage-growth.md`, `retention-constraints.md`, `resource-limits.md`; `live/apply.rs` quota gate; object accountant/open; vault imports and serving mirror boundary | No |
| Dependency direction and proof-token boundary | In depth for declared member edges; selective checker internals | Root and all nine member Cargo manifests; `wyrd-contracts/src/layer_contracts.rs` policies, target-dependency collection, closure, mint-count, and KDF guards; namespace token and sync mint | No |
| Store writer ownership | In depth for durable log; selective object store | `durable/store.rs` lock/open/commit/load; bootstrap custody lock ordering; mount/export opening order; `FsObjectStore` concurrency assumptions | No |
| Embedded construction and projection freshness | In depth | `wyrd-core/src/node.rs`, `view.rs`, live split and publication gates; embedded lifecycle and composition tests | No |
| Authoring sessions, membership use, timestamps | In depth for named call paths | `engine/authoring.rs`, `PendingWrite`, `author/snapshot.rs`, recovery/carry entry paths, removal, fact encoding, snapshot codec | No |
| Export egress, staging, and symlink handling | In depth for staging/walk; selective confinement implementation | `export/{mod,staging,walk}.rs`, staging/walk tests, CLI export command and contract, namespace confinement surface | No |
| Error conversions and caller discipline | Selective | `live/apply.rs::authoring_error`, `FailureClass`, FUSE mutation/view errno mappings, typed store failures, updater error propagation | No |
| CI and release configuration | In depth for main/distribution recipes; selective surrounding scripts | `.ngit/act/workflows/{workflow,dist}.yml`, `.ngit/release.yaml`, `flake.nix`, build script, `.cargo/config.toml`, `.config/nextest.toml`, hook-reading shell test, vendored FUSE build selection | No |
| KDF selection and at-rest custody | In depth for selection; selective lifecycle | `wyrd-sync/Cargo.toml`, all consuming member edges, `keys/keystore.rs`, durable store-key wrapping, bootstrap owner/member custody, escrow records | No |
| Object sealing and contextual binding | Selective, following open/verify call paths | `seal.rs`, `keys/epoch.rs`, capability model, rotation framing and intake owner-proof/pre-state checks, escrow AAD | No |
| Credential and config input | In depth for CLI loading | `credentials.rs` bounded descriptor-based validation and zeroizing buffers; `drive_config.rs` bounded strict TOML and precedence | No |
| Control intake, replay/reordering, mailbox resource use | In depth for drain and settlement; selective state machines | `runtime/intake/mod.rs`, announcement/capability/body projections, rotation authorization, mailbox recv/settle and finite flood/dedupe tests | No |
| Bulk intake bounds and fetch persistence | In depth for executor; selective network implementation | `runtime/plan/mod.rs`, fetch validators, `ingest.rs`, iroh size probe/accumulator/deadlines, tree/manifest decoders, bounded-plan tests | No |
| Directory conflict/read presentation | In depth for merge/enumeration; selective other reads | `wyrd-fuse/src/view/{drive,merge}.rs`, canonical tree binary lookup, FUSE read/attribute/error seams | No |
| Update transaction and fail-closed behavior | In depth | All modules under `wyrd-cli/src/update/`, dispatcher propagation, update contract, full-transaction rejection tests and helper-only timeout test | No |
| Persistent corruption/upgrade checks | Selective | Commit hash-chain and ceiling checks, version/tag framing, replay/body records, upgrade contract and ignored-gap declarations, updater snapshots | No |
| Public release-read profile | Contract only | `docs/contracts/release-read.md`; public-drive confidentiality waiver distinguished from private drives | No |
| External crypto/transport internals and full vendored FUSE implementation | Not inspected | Dependency/version declarations and boundary calls only; `vendor/fuser/build.rs` was inspected | No |

### Contract and scope rulings

- **No total-disk ceiling was inferred from `retained_bytes_quota`.** `docs/design/storage-growth.md:10-14,258-261,514-546` and `docs/contracts/retention-constraints.md:24-36,124-135` explicitly record the limited plaintext mounted-write quota, untallied auxiliary bytes, and unimplemented receive/residency refusal. `ResourceBudgets` defaults to unlimited and the binary exposes no quota flag (`docs/contracts/resource-limits.md:41-43,70`). These remain consequential accepted implementation gaps. Reporting them as newly bypassed shipped A/B gates would be incorrect.
- **Cross-process log ownership is enforced.** The inspected durable-store open holds a kernel-backed exclusive lock for the store lifetime (`durable/store.rs:325-386`), and production CLI paths inspected open the engine before the plaintext store. The plaintext store separately assumes a trusted directory and cooperative users (`wyrd-format/src/fs_store.rs:26-34`); no attack requiring modification of OS-protected store files is included here.
- **Mobile support is not a shipped promise.** Architecture lists mobile/web/WASM as non-goals (`docs/architecture.md:189-199`). Platform-neutral layering was checked as a dependency contract, not as evidence that current Tokio/process/FUSE compositions work on mobile.
- **The current KDF recipe is not evidence of a weak shipped artifact.** Resolver 2 is selected (`Cargo.toml:1-2`); production consumers declare normal `wyrd-sync` edges while the fast profile appears on dev edges; the flake builds only `-p wyrd-cli` with checks disabled (`flake.nix:48-61`). The production table is Argon2id, 65,536 KiB, t=3, p=1; the test/feature table is **8 KiB**, t=1, p=1 (`keys/keystore.rs:35-59,87-116`). The actual x86_64 distribution workflow and other-platform build script use that flake. No observed release build argument enables the weak feature. This establishes repository intent and selection logic, not artifact provenance or a resolved artifact feature graph.
- **Accepted crypto/protocol scope was retained.** Local identity-key custody, historical keys for newly admitted devices, no GC, no instantaneous revocation, first-wins conflicting capability vectors, no per-object serving ACL, and the two named upgrade-verification gaps are documented decisions. None is independently labeled a new vulnerability here. The plaintext-body exception is reported only because its normative descriptions disagree (A3).

## 4. Limitations

- No reproduction, compile check, test, fuzz run, benchmark, live relay exchange, filesystem fault injection, or kernel mount was executed. Confidence comes from static control-flow and data-flow evidence. Proposed tests may require small injection seams before they can be deterministic.
- No release tarball was acquired or inspected. No artifact hash, signature, build log, binary KDF behavior, platform linkage, or reproducibility claim was verified. `cargo tree` was not run and would not by itself establish a particular published artifact's configuration.
- The membership chain/conflict-resolution algorithm, complete authorization fixed-point behavior, all mutation-fold interleavings, every FUSE callback, route reannouncement/withdrawal scheduling, mailbox supervisor reconnect races, and full cross-release fixture replay were not exhaustively audited. Relevant entry paths and adjacent checks were inspected selectively.
- No external crate implementation or current advisory database was audited. In particular this review does not certify secp256k1, Argon2, AEAD, nostr-sdk, iroh, iroh-blobs, or vendored FUSE behavior beyond the repository boundaries examined.
- OS/filesystem assumptions were read, not experimentally verified: Unix locking, atomic rename, directory fsync, credential permissions, and protection of drive directories. Findings do not assume an attacker can modify files protected by the stated OS boundary or trusted-process memory.
- No throughput, allocation peak, time-to-OOM, or million-entry listing duration was measured. C3 describes conditional starvation if arrivals maintain a backlog; C4 proves unbounded bookkeeping growth; C5 proves algorithmic amplification. These are not measured deployment-capacity claims.
- The review does not establish whether any reported condition has occurred in a user's drive. No alleged weakness in the cryptographic primitives, artifact supply chain, or keyless-replica deployment is inferred from an untested hypothesis.

## 5. Remediation order

### Must block release

1. **B1: preserve every legal filename in export.** Move control files outside the payload namespace, and pin complete byte-identical egress across heartbeat renewal.
2. **C1: remove the peer timestamp's permanent write veto.** Cover ordinary authoring, removal/carry, recovery, and reopen while preserving existing history.
3. **A1: make fetch checkpoints both deadline-safe and commit-size-safe.** Prove a completed prefix survives slicing and that individually admissible inputs converge through bounded durable batches.
4. **C2: restore the old executable on every unsuccessful post-swap exit.** Verify complete-transaction timeout and operational-error paths, including `--restart` continuity.

These are independent release conditions. The absence of an attacker, or the availability of a later manual retry, does not make successful incomplete egress or a broken fail-closed transition acceptable.

### Fix before release if feasible

- **A2:** establish projection freshness at live composition rather than relying on callers' refresh order.
- **B2:** enforce write-session origin and revision at the public boundary.
- **B3:** cover actual binary and validation inputs in both CI trigger sets.
- **C3:** give mailbox scans and engine intake finite service budgets so local work can run under poison traffic.
- **C5:** remove repeated linear scans from merged-directory enumeration.
- **A3:** reconcile the plaintext-body exception with the normative privacy guarantees before those guarantees guide new replica implementations.

### Post-release hardening and explicitly deferred scope

- **C4:** replace the unbounded settlement-successor set with bounded/density-based idempotence bookkeeping. Its eventual memory pressure is real, but no time-to-exhaustion was established and process restart clears this particular state.
- Implement the already-decided receive-side retention and provider-residency refusals, full accounting prerequisites, and their negative contracts on the documented roadmap. Their absence is important exposure, but this review does not reinterpret them as currently implemented guarantees.
- Preserve the explicit artifact-verification and broader dynamic-review limitations. This static report is not a substitute for revision-bound release evidence or for verification of the actual distributed binaries.

The review document is the only intended workspace change and is to remain uncommitted.
