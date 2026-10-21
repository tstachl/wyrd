# Wyrd Adversarial Security Audit

**Review date:** 2026-10-20  
**Reviewed commit:** `483e28684be1005659291419d22ec4ca070080fa`  
**Working tree:** clean on branch `master`  
**Review type:** adversarial security audit  
**Reviewer model:** GPT-6 (Codex)

## A. Executive assessment

I found **no confirmed or probable vulnerabilities** in the paths inspected. I did identify one Wyrd-specific hardening concern: the intentionally weak keystore KDF profile is a public Cargo feature, so downstream builds can enable it. The normal `wyrd-cli` dependency graph does not enable it.

This was a static review at the commit above. I did not run the test suite, construct a reproducer, or inspect every source file and operational script; the coverage limits are listed below.

## B. Confirmed vulnerabilities

**None confirmed.** No inspected code path yielded a failure sequence with both a violated security property and a credible attacker path.

## C. Probable vulnerabilities and unverified hypotheses

**None retained.** The KDF feature concern below was checked against the normal binary dependency graph and does not establish a production vulnerability in this checkout.

## D. Hardening recommendations

1. **Make the fast KDF profile unavailable to production consumers.** In `crates/wyrd-sync/src/keys/keystore.rs`, `insecure-fast-kdf` switches Argon2id from 64 MiB and three passes to 8 KiB and one pass. It is intentionally documented as test-only. The normal CLI dependency graph uses the default profile, but the feature remains explicitly enableable by downstream builds. Prefer a test-only mechanism that cannot be selected in an ordinary dependency build. Add a build-configuration check that rejects the weak profile in release artifacts.

2. **Keep the “trusted store directory” assumption explicit at composition boundaries.** `crates/wyrd-format/src/fs_store.rs` documents that symlinks and foreign entries inside the store are not defended against. This is an operator/local-filesystem assumption, not a demonstrated vulnerability under the stated threat model. Keep deployment guidance clear about who may write the drive directory.

## E. Invariant and attack-surface coverage

The review included nine workspace crates: `wyrd-format`, `wyrd-sync`, `wyrd-namespace`, `wyrd-fuse`, `wyrd-core`, `wyrd-daemon`, `wyrd-cli`, `wyrd-contracts`, and `wyrd-embed`. The only binary target identified was `wyrd` in `wyrd-cli`. No crate declared a `[features]` default set; `wyrd-sync` declares `insecure-fast-kdf`. Relevant default feature activation was checked through Cargo’s normal dependency graph.

| Area | Coverage | Evidence examined |
|---|---|---|
| Resource bounds and intake | Partially inspected | `Limits`, fetch paths, bulk transport, mailbox and intake bounds, `ResourceBudgets` documentation |
| Cryptography and identity | Partially inspected | Content/Storage ID derivation, AEAD wiring, sealing/opening, keystore KDF parameters and feature selection |
| Authorization and state machines | Partially inspected | Authorization classification, intake flow, membership contract documentation |
| Persistence and crash recovery | Partially inspected | Durable-store opening/locking, filesystem object-store assumptions, crash-consistency documentation |
| Filesystem and OS boundaries | Partially inspected | Credential file opening and permissions, export staging/walk, object-store behavior, store locking |
| Secrets and deployment | Partially inspected | CLI credentials/config parsing, secret file limits, KDF feature graph, store permission logic |
| Cross-crate composition | Partially inspected | Architecture/dependency contracts, crate manifests, CLI normal dependency graph |

Threat-model coverage: malformed or oversized peer input, validly encoded untrusted content, replay/reordering, process crashes, local filesystem manipulation, adversarial CLI/config inputs, corrupted persisted state, and resource exhaustion were considered. No confirmed exploit was established in the reviewed paths. Arbitrary trusted-process memory modification and OS-protected file modification were out of scope.

## F. Validation and blind spots

Commands actually run:

- `git rev-parse HEAD` and `git status --short --branch`: commit and clean working tree recorded above.
- Read `docs/architecture.md`, selected trust/resource/crash documentation, and source in the intake, fetch, bulk, authorization, persistence, credential, export, and identity paths.
- `cargo tree -p wyrd-cli -e features -i wyrd-sync --edges normal`: the normal CLI graph showed `wyrd-sync` with its default feature only.
- `cargo tree --workspace -e features -i wyrd-sync --edges normal`: the normal workspace graph likewise showed no `insecure-fast-kdf` activation. Test-only edges were excluded from these commands.

No tests, builds, fuzzers, crash injectors, or vulnerability reproductions were run. I did not exhaustively inspect all implementation and test files, vendored `fuser`, update logic, every operational script/configuration, or every platform-specific execution path. In particular, this review does not establish end-to-end behavior for all network and crash scenarios.

## G. Remediation order

1. Before release, remove or structurally isolate `insecure-fast-kdf` from production-consumable builds and verify release artifacts use the pinned Argon2id parameters.
2. Add a build-configuration regression check for that property.
3. Continue the audit with the uninspected update subsystem, vendored `fuser`, operational scripts, and remaining sync/runtime paths; then run targeted security tests for any concrete concern found.
