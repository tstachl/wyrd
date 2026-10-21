# Wyrd adversarial codebase quality review

Reviewer: Codex (GPT-6)
Tree reviewed: `master` @ `483e28684be1005659291419d22ec4ca070080fa`
(2026-10-20). Working tree: clean at start; this review adds only this file.

## 1. Assessment and baseline

The repository has strong architectural boundaries: nine workspace crates, a documented dependency direction, typed identity and error boundaries, and cross-crate contracts for important composition rules. The structure is generally reviewable, and the code inspected is organized around the documented ownership model.

The main risks found are at seams where a contract still depends on caller discipline or a catch-all mapping, and in CI path filters that can skip validation when test configuration changes. No evidence was found that the broad module layout itself is causing defects.

- **Commit:** `483e28684be1005659291419d22ec4ca070080fa`
- **Working tree:** clean at start and end.
- **Crates inspected:** all nine workspace members, with targeted source review of `wyrd-core` node construction and mutation error mapping, `wyrd-sync` features, `wyrd-contracts` upgrade tests, CLI construction, and CI/build configuration.
- **Major modules inspected:** `live/apply`, `node`, mailbox shutdown, CLI argument and command dispatch, contract tests, and dependency/configuration files.
- **Checks run:** `cargo check --workspace` passed. `devenv shell -- cargo check --workspace` could not start because Nix could not access its cache database and daemon socket. Tests, Clippy, and e2e suites were not run.

## 2. Ranked findings

### 1. Engine error mapping silently absorbs new variants — C3

**Confidence: high. Impact: moderate.**

In `crates/wyrd-core/src/live/apply.rs:31`, `authoring_error` explicitly maps selected `EngineError` cases, then maps every other case to `MutationError::Engine` at line 72.

That fallback is consistent with the current comment’s intent to keep some failures opaque, but it also means adding a new engine failure never forces the mutation boundary’s errno classification to be reviewed. For example, a future storage-capacity or retryable failure could become generic `EIO`, losing a more useful POSIX result and concealing the intended failure semantics.

Existing types and callers do not prevent this: the wildcard makes the match compile when `EngineError` gains variants, and there is no exhaustive mapping check. The mapping appears deliberately narrow, so every engine error need not have a unique errno.

**Smallest useful correction:** use an exhaustive match, explicitly grouping variants that should remain opaque. Add a focused test for representative mappings, including capacity and transient I/O. The compiler will then require reconsideration when the error enum grows.

### 2. Node construction permits an apparently valid but empty view — C3

**Confidence: medium-high. Impact: moderate.**

`crates/wyrd-core/src/node.rs:87` opens the view with an empty head list at lines 90–94. Production callers in the mount and embed lifecycle explicitly call `refresh_live_heads()` afterward, so no evidence was found that those paths currently serve the empty view.

The structural risk is that the public constructor returns a usable `WyrdNode`, and `view()` is immediately available. An embedder that reads between construction and refresh can see an empty namespace and mistake it for durable empty state. The type does not represent “constructed but not initialized,” and the embedded lifecycle contract always refreshes before reading.

**Smallest useful correction:** make construction install classified live heads and return an error if that fails, or expose an explicit uninitialized/opening state that cannot serve reads. Add a contract that creates a node over a non-empty drive and reads its authored head immediately after construction.

### 3. CI can skip changes to test and build configuration — C2

**Confidence: high. Impact: moderate.**

The path filters in `.ngit/act/workflows/workflow.yml:6` omit `.cargo/config.toml`, `.config/nextest.toml`, `devenv.nix`, `devenv.yaml`, and `devenv.lock`. These files affect local test aliases, test selection, or the documented build environment, but changes to them alone do not match the workflow’s push or pull-request path filters.

A change to a nextest profile could therefore alter which tests the documented gate runs without triggering that gate. Likewise, a change to a local alias could invalidate the developer feedback path without CI checking the resulting configuration.

**Smallest useful correction:** include the relevant configuration paths in both filters. Add a lightweight shell/config validation step only if it checks a concrete failure mode, such as ensuring configured fast aliases and nextest profiles remain parseable.

## 3. Explicit test gaps, not additional findings

`crates/wyrd-contracts/src/upgrade_contracts.rs:354` explicitly marks fact-payload replay as blocked pending versioning, and lines 681–687 similarly defer mixed-version negotiation. Both tests are ignored with `todo!()`. The source is candid about the missing coverage, so these are not counted as defects in implemented behavior. They do mean those future compatibility claims are not currently established by tests.

## 4. Test-effectiveness table

| Invariant or behavior | Relevant test or guard | What it proves | What it does not prove |
|---|---|---|---|
| Embedded lifecycle works without presentation layers | `embedded_drive_lifecycle_needs_no_fuse_and_no_daemon` | The tested lifecycle composes headlessly across reopen and shutdown | A newly constructed node serves its durable heads before the test’s explicit refresh |
| Engine authoring failures map to mutation outcomes | Tests at `authoring_error` call sites | The exercised error cases map to expected mutation results | A newly added `EngineError` variant receives deliberate classification |
| Production builds avoid the weak KDF profile | Workflow feature-graph check; keystore parameter tests | The normal dependency graph excludes `insecure-fast-kdf`, and default-profile tests pin production values | An `--all-features` artifact is safe for production; the feature is intentionally additive |
| Upgrade compatibility | Non-ignored upgrade contracts | The implemented compatibility clauses covered by those tests | Fact-payload version replay and capability negotiation; those tests are explicitly blocked |
| CI coverage follows configuration changes | Workflow path filters | Matching source/config paths trigger the declared jobs | Changes limited to omitted build or test configuration trigger CI |

## 5. Feature, configuration, and CI coverage

`insecure-fast-kdf` is an additive feature in `wyrd-sync`, documented as unsuitable for production. The workflow checks the **normal** dependency graph for that feature, while downstream dev-dependencies enable it to keep integration tests affordable. That distinction is intentional and covered by CI; an all-features build should not be treated as a production build.

The workspace uses local `fast-*` aliases for selected crates and a broader default nextest gate, with slow tests on master. The aliases are explicitly documented as non-transitive. The main coverage issue is the CI path-filter gap above: edits to test/build configuration can bypass the gate altogether.

The project instructions say documentation carries current design contracts. The workflow also omits `docs/**` from its filters, so documentation-only changes do not run CI. No concrete documentation contradiction was identified in the focused contracts reviewed, so this is treated as a limitation of automated validation rather than a separate finding.

## 6. Prioritized improvement plan

### Immediate correctness or release-risk fixes

- Make `authoring_error` exhaustive while retaining explicit groups for intentionally opaque failures.
- Ensure consumers cannot accidentally treat a newly constructed node’s empty view as the durable namespace.

### Small independent refactors

- Add CI path coverage for `.cargo`, `.config`, and the devenv files that affect validation commands.
- Add a focused regression contract for node construction and immediate reads.

### Longer-term structural improvements

- Keep blocked upgrade clauses explicitly identified as unproven, and activate their tests when fact-payload versioning and capability negotiation land.
- Consider a small configuration validation check if the fast aliases and profiles are expected to be reliable developer entry points.

## 7. Limitations

This was a read-only review at one commit. Only `cargo check --workspace` was run; tests, Clippy, doctests, slow tests, and e2e suites were not run. The `devenv` check was blocked by local Nix access, so the successful workspace check used the available direct Cargo environment. Findings about test behavior are based on source inspection, not execution.
