# Wyrd Adversarial Security Review

- **Date:** 2026-11-05
- **Reviewing model slug:** crouton-alpha
- **Commit:** `438a2f3dd2464049d38e487b2fc2baf659fed13a` (verified: `git rev-parse HEAD` matches)
- **Branch:** `pr/crouton-review`
- **Working tree:** clean except for untracked reviewer output files in `docs/reviews/archive/`; no commits, pushes, or staged changes made by this review (a temporary reproducer test was added, executed, and fully reverted; see §F).

---

## A. Executive assessment

Wyrd's wire, cryptographic, and authorization core is unusually well-guarded for a pre-alpha system. Every security-relevant decoding path I traced bounds allocation before trusting declared lengths (`Vec::with_capacity(...min(4096/1<<20))` + `need()` checks), verifies AEAD + content-identity as two independent checks (`seal::verify`), binds control, capability, bootstrap, and rotation envelopes with distinct AAD domains and inner/outer agreement checks, and orders cheap rejection before expensive signature/ECDH work in intake. The `insecure-fast-kdf` test profile is confined to dev-dependencies with contract tests pinning release manifests and build args, and the `AuthorizedSnapshot` unsafe crossing is single-sited and contract-pinned.

Within that solid core, I confirmed one real authorization defect: **genesis transitions are validated without binding the signer to the singleton owner the genesis installs**, in direct contradiction of the contract's "genesis author defines the initial owner" rule. I reproduced it dynamically (a non-owner-authored genesis classifies `Canonical`). Its two faces — an insider freezing a drive with a competing genesis, and an invitee anchoring to a genesis its owner never signed — are the most consequential verified risks in this review. A second, closely related gap (invitee-side never checks the inviter is the genesis owner) is established by static trace but was not executed end-to-end, so it is graded Probable, not Confirmed.

**Limits of this review.** This was a focused static audit plus targeted test runs, not a complete from-scratch re-verification of all ~173k lines. I did not run the full workspace suite, the Lima/Odin E2E suites, or the live-relay tests; I did not diff the four vendored `fuser` divergences against upstream; I did not inspect deployment packaging (flatpak/nix), `wyrd-app`, or the NIP-59 live-mailbox adapter's relay-pool wiring (the `Mailbox` trait boundary and in-memory fake were inspected; no live client exists in this crate). I deliberately did not claim coverage of anything I did not read. No blanket "secure / safe to ship" conclusion is intended or implied.

---

## B. Confirmed vulnerabilities

### B1. Genesis signature is not bound to the genesis owner

- **Severity:** Medium. **Confidence:** Confirmed (reproduced).
- **Attacker capability and prerequisites:** (a) a drive member (authenticated peer) able to seal a control message for the drive's current epoch; or (b) anyone able to hand a victim an invitation file (the normal out-of-band join flow). No outsider-only path: sealing a `MembershipTransition` requires an epoch control key, which only holders possess.
- **Affected crate, entry point, configuration:** `wyrd-sync`, membership validation (`MembershipLog::observe` → `chain::validate_link`, genesis arm) and invitee-side `verify_invitation`. All configurations.
- **Exact source locations:**
  - `crates/wyrd-sync/src/membership/chain.rs:128-139` — genesis arm derives from empty state + `check_genesis_shape` and returns `Link::Valid` with no `check_authority` call.
  - `crates/wyrd-sync/src/membership/validate.rs:92-94` — comment states authority is "Skipped for genesis."
  - `crates/wyrd-sync/src/membership/validate.rs:82-90` — `check_genesis_shape` asserts singleton member==owner, empty readers, but never compares `t.author` to that owner.
  - Contrast `crates/wyrd-sync/src/membership/chain.rs:164-166` — non-genesis links require the author in the pre-transition owner set.
  - Invitee side: `crates/wyrd-sync/src/runtime/bootstrap/invitation.rs:80-139` — `verify_invitation` checks invitee, genesis validity/authoritativeness, and capability binding, but never compares `invitation.inviter` to `genesis.author` or the derived owner (the `inviter` field is not read anywhere in `runtime/`).
- **Concrete exploit sequence:**
  1. (Freeze variant) Member M, holding epoch-N secrets/control key, crafts an alternate genesis G′ for the same drive: `epoch=1, prev=None, changes=[Admit(O), SetOwners([O])]` where O is the real owner (or M itself), authored and BIP-340-signed by M's own key. Roots are computed honestly so `derive_next` + `check_genesis_shape` pass.
  2. M seals G′ as a `MembershipTransition` control message under the epoch-N control key and sends it to victim V. Intake runs `check_total_len`/`check_transition` (pass), observes G′, commits it. V's log now holds two valid epoch-1 transitions with the same (absent) predecessor.
  3. Classification reports a genesis conflict: `frozen_at()=Some(1)`, `known_state()=None`. Snapshot acceptance freezes; V surfaces a drive-level error. Unfreezing requires an owner-signed resolution transition the drive owner must now author.
  4. (Bootstrap variant) M instead seals a bootstrap invitation carrying the genuine genesis bytes plus a `Capability::new` grant (public constructor; only count/curve checks) over the epoch secrets M already holds, signed by M (`inviter=M`). V's `verify_invitation` accepts: `open_bootstrap` only proves the signature matches the *claimed* inviter. V commits G and installs provisional control keys for a drive whose owner never invited V.
  5. A still simpler pre-plant: an attacker who obtains invitation delivery in transit substitutes G′ (signed by the attacker, installing the attacker as sole owner) for the genuine genesis; a fresh invitee's single-candidate log reports it `Canonical`/`authoritative`, so the invitee anchors to an attacker-owned chain with no signal distinguishing the two.
- **Security impact:** availability (membership freeze / drive-level error until owner action) in the insider variant; authorization/trust-anchor substitution in the bootstrap variant. Confidentiality of drive content is not broken: the forged genesis carries no epoch secrets, and capability authorization still gates keyring install. The contract invariant violated is epochs.md Layer 1 ("The genesis bootstrap… the genesis author defines the initial owner"; "Transition authority always comes from the pre-transition owner set") and trust.md ("the owner's signature" over bootstrap payloads).
- **Evidence and reproduction status:** Dynamic reproducer executed then reverted (temporary test in `membership/conformance/genesis.rs`, removed via `git checkout`). Test built a genesis admitting owner-1 but authored/signed by key `9` and observed `TransitionStatus::Canonical` (output: `REPRO-CONFIRMED: non-owner-authored genesis is Canonical`). Step (3) follows directly from the existing conformance test `two_valid_geneses_conflict_at_epoch_one` (two valid geneses freeze at epoch 1). The bootstrap variant (steps 4–5) was not executed end-to-end; it is included here because the same missing binding is the root cause, but the invitation-forge half is also tracked as Probable P1.
- **Smallest defensible remediation:** In `validate_link`'s genesis arm, after deriving the singleton state, require `derived.owners` (equivalently `members`) to contain exactly `t.author` (one-line `check_authority`-shaped guard, e.g. `if state.owners != {t.author} → Invalid(BadGenesis)`). Additionally, in `verify_invitation`, require `invitation.inviter == genesis.author` before touching disk. This is a protocol-behavior tightening, not a format change: honestly built geneses (owner, author, signer all one device) are unaffected.
- **Required regression test:** (1) Non-owner-authored genesis is `Invalid` and never roots (`known_state()==None`, `frozen_at()==None`); (2) forged-invitation with `inviter != genesis.author` is refused before disk; (3) existing `two_valid_geneses_conflict_at_epoch_one` still passes (two *owner*-authored geneses still conflict, as designed).

### B2 (tracked as Probable) — see §C, P1

No second finding met the Confirmed bar. In particular, I did **not** promote the following to Confirmed after attempting to falsify them: the `insecure-fast-kdf` feature (dev-dependency only + contract pins), `wyrd update` archive handling (operator-supplied file; identity-not-trust is documented, and a malicious archive already yields code execution by design), FUSE symlink handling (`readlink` refuses with `EOPNOTSUPP`; export confines), owner-socket reclaim (store lock held by composer before bind), and the store verified-stamp fast path (documented trusted-directory assumption; `get` still fails closed).

---

## C. Probable vulnerabilities and unverified hypotheses

### P1 (Probable). Bootstrap invitee never verifies the inviter is the genesis owner

- **Severity if confirmed:** Medium (authorization bypass bounded to control-plane visibility; no content decrypt without an authorized capability).
- **Code evidence:** `verify_invitation` (`crates/wyrd-sync/src/runtime/bootstrap/invitation.rs:80-139`) opens the bootstrap envelope (proving only that the signature matches the *claimed* `inviter`), validates the genesis as authoritative, unwraps the capability and checks device/drive/encryption-key — but never reads `invitation.inviter` against `genesis.author`/derived owner. `grep '\.inviter'` over `crates/wyrd-sync/src/runtime` returns nothing; the only subsequent inviter mention is a comment. Honest admission is owner-gated (`admit()` → `NotOwner`), but that gate is on the *minter's engine*, not the invitee's verifier: `seal_bootstrap` accepts any caller-supplied `DeviceIdentitySecret`, and `Capability::new` is public with only count/curve checks — a member can mint a grant over secrets it already holds without owner authority.
- **Attack sequence (traced, not executed):** member M crafts `Capability::new(drive, victim, victim_enc_key, arbitrary_transition, N, keyring_secrets_1..=N)` over held secrets, wraps it, seals a bootstrap invitation signed by M (`inviter=M`), and gives the file to victim V. V runs `device join`; `verify_invitation` passes; V commits the genuine genesis and installs provisional epoch control keys (`install_invitation_keys`), then opens epoch-sealed control traffic (announcements: snapshot ids, authors, epochs, transport roots — member-visible metadata) despite never being admitted by the owner. No content access follows: manifest/object keys require an *authorized* capability, which requires an owner-signed admission transition naming V, which does not exist; the provisional keys gate correctly against the (empty) authorized keyring on resync.
- **Precise missing evidence / next action:** Execute the forge→accept sequence in a test: build an engine, admit member M, forge an invitation from M to a fresh device V (genuine genesis bytes are member-visible), call `Engine::join`/`accept_invitation` as V, and assert whether accept succeeds. If it succeeds, promote to Confirmed and land the §B1 fix (`inviter == genesis.author`); if something downstream I did not trace (e.g. resync refusal) blocks it, downgrade.

### P2 (Unverified hypothesis, not a finding). Owner-interface frame allocation

- `read_frame` (`crates/wyrd-daemon/src/owner/server.rs:770-783`) allocates `vec![0u8; len]` up to `MAX_FRAME_BYTES` (96 MiB) per connection, with one unbounded thread per connection and no concurrency cap. A same-uid local process could open many connections each announcing a 96 MiB frame and pressure memory. I did **not** verify this as a vulnerability: cross-uid peers are rejected at `check_peer` before `read_frame` runs (same-user is the complete credential), and a same-user attacker already has drive-directory access. This is listed only as a hypothesis needing a threat-model decision (is same-uid local DoS in scope?); a connection cap would be the hardening fix. No action required beyond that decision.

---

## D. Hardening recommendations

Prioritized; none is a demonstrated vulnerability.

1. **Sign update archives (or pin hashes out-of-band).** `wyrd update` resolves archives by canonical name + blake3 identity, explicitly "identity, not trust" (`crates/wyrd-cli/src/update/identity.rs`). Today, anyone able to supply/swap the archive file (local FS write, compromised download) yields arbitrary code execution as the operator. The operator-supplied-file model makes this low-risk by design, but a detached signature over the archive (verified in `resolve_archive` before `stage_archive`) would make the transaction safe against misdirected or tampered downloads. *(Reduces: silent compromise of the update channel. Low priority.)*
2. **Bound owner-interface concurrency.** Cap simultaneous connections (or total in-flight frame bytes) in `OwnerServer::serve_on`, since one thread per connection is unbounded. *(Reduces: same-user local memory/thread exhaustion. Low priority — see P2 decision.)*
3. **Bound hydrated projections.** The committed-capability and announcement projections are in-memory hydrated copies of the durable log, explicitly unbounded pre-GC (`docs/contracts/resource-limits.md`; `storage-growth.md`). With remote-triggerable commit pacing (≤1024 facts/pass/sender) this is a long-horizon retention issue, not a live-exhaustion bug. GC/retention policy is the fix; no code change needed now. *(Low priority.)*

---

## E. Invariant and attack-surface coverage

Inspection marks mean "read and traced," not "proven secure."

| Area | Status | Evidence (representative paths read) |
|---|---|---|
| **A. Resource exhaustion / bounded work** | Inspected | `wyrd-sync/src/ingest.rs` (`Limits::V0`); decoders in `wyrd-format` (`tree.rs`, `manifest.rs`, `snapshot.rs`, `membership.rs`, `envelope.rs`) — `min(4096)`/`1<<20` pre-allocation caps + `checked_mul` + `need()`; `control/message.rs` blob decode; mailbox gates (`transport/mailbox.rs`); bulk bounded accumulator (`bulk/iroh.rs::bounded_blob_bytes` + verified-size gate); intake budgets (`runtime/intake/mod.rs`); fetch `max_admit_per_pass` chain (`runtime/plan`, `runtime/fetch`); FUSE budgets (`wyrd-core/session.rs`, `daemon/fuse/backend/handles.rs`); export depth + symlink budgets (`export/walk.rs`, `namespace/view.rs`); owner frame cap (`owner/server.rs`); invitation bounded read (`cli/commands/device.rs`) |
| **B. Crypto / identity invariants** | Inspected | `seal.rs` (AEAD+hash double check, `verify` whole-entry); `keys/epoch.rs` (pinned contexts, drive/epoch scoping); `keys/capability/model.rs` (AAD, inner agreement, `proven_pk`, `authorize_against`); `control/mod.rs`, `control/bootstrap.rs`, `control/rotation.rs` (version-in-AAD, header agreements); `keys/owner_proof.rs` via intake; `keys/keystore.rs` (Argon2id table, test-profile gating); `keys/escrow.rs` (via tests); `signing.rs` challenge pinning; `public_read.rs` frozen subset |
| **C. Authorization / protocol state machines** | Inspected | `membership/chain.rs`, `validate.rs`, `state.rs`; `authorization/classify.rs`, `predicates.rs`; intake arms (`message_action`, `capability_action`, `rotation_commit`, `request_action`) incl. reader-author suppression, announcement cheap-reject-before-verify, rotation mint-authority (pre-state owners) + sender-membership checks; replay/dedupe layering (`ControlInbox`, suppression FIFO, `note_committed_facts` fill-vacant) |
| **D. Persistence / crashes / recovery** | Partially inspected | `durable/store.rs` (commit protocol, hash chain, `check_commit_fits`, fsync ordering), `fs_store.rs` (temp+fsync+rename, stamp/heal, `get` verify), `durable/codec/` (limits/tag fns — framing read, full per-fact audit not completed), crash-stage enum present; authoring/carry/escrow crash paths skimmed, not exhaustively traced |
| **E. Filesystem / OS boundaries** | Inspected | `cli/credentials.rs` (`O_NOFOLLOW`, uid/mode, 4 KiB cap); `durable/store.rs` file/dir modes + lock; `export/staging.rs` + `walk.rs` (heartbeat-scoped sweep, rename-publish, size reconciliation); FUSE `DriveView::read` (declared-size clamp, whole-list validation at EOF); `readlink` refusal (`daemon/fuse/backend.rs`); owner peer-uid check (`owner/auth.rs`, `server.rs`) |
| **F. Secrets / config / deployment** | Inspected | `keys/keystore.rs` + `Cargo.toml` feature gating (dev-deps only); `layer_contracts.rs` release-manifest/build-arg pins (read, not run); CLI error taxonomy (no secret material in variants); logging (`cli/logging.rs`, mount/vault diagnostics — ids/paths/stages only); `update/` transaction (identity-only hash, `tar -xzf` staging, `find_staged_binary` shape gate) |
| **G. Cross-crate composition** | Inspected | `AuthorizedSnapshot` crossing (`durable/mod.rs` single production call site); closure gate (`closure.rs::verify_snapshot_manifest` / `verify_head_closure`) vs fetch (fetch records, gate installs); `wyrd-namespace` token model; `wyrd-contracts` layer policies (read); fetch key availability vs manifest keys |
| **Not inspected** | — | `vendor/fuser` divergences vs upstream (not diffed); `wyrd-app`; flatpak/nix packaging; NIP-59 live mailbox relay-pool wiring in `wyrd-core` beyond the `Mailbox` trait (no live client in `wyrd-sync`); E2E (Lima/Odin) suites; live-relay tests; `tests/` top-level harness |

---

## F. Validation and blind spots

Commands actually run at `438a2f3dd2464049d38e487b2fc2baf659fed13a`:

| Command | Result |
|---|---|
| `git rev-parse HEAD` / `git status --porcelain` / `git branch --show-current` | `438a2f3dd2464049d38e487b2fc2baf659fed13a` / clean (pre-report) / `pr/crouton-review` |
| `cargo test -p wyrd-format --lib` | **174 passed, 0 failed** |
| `cargo test -p wyrd-sync --lib seal::tests` | **12 passed, 0 failed** |
| `cargo test -p wyrd-sync --lib capability` (69 matched) | **69 passed, 0 failed** |
| `cargo test -p wyrd-sync --lib membership::conformance` | **50 passed, 0 failed** |
| `cargo test -p wyrd-sync --lib bootstrap` | **37 passed, 0 failed** |
| Temporary reproducer test (added to `membership/conformance/genesis.rs`, run, then reverted via `git checkout`) | **Executed: `REPRO-CONFIRMED: non-owner-authored genesis is Canonical`** |

- **Reproducer status:** B1 has an executed (now-reverted) reproducer. P1's end-to-end forge→accept was **not** executed (hence Probable). No other reproducer was run; nothing in this report implies acceptance of a reproducer that does not exist.
- **Tests providing meaningful security evidence:** seal double-check tests (`the_second_check_catches_a_hand_crafted_mismatch`, `verify_enforces_the_whole_entry`), capability intake suppression tests, membership conformance genesis/conflict tests, bootstrap refusal tests (`malformed_bootstraps_are_rejected`, substituted-key refusal), closure correspondence tests. I did not run the contract suite (`wyrd-contracts`) or the full workspace suite.
- **Blind spots:** full-suite results unknown; E2E (microVM/Lima) convergence and restart behavior not exercised; live-relay NIP-59 behavior untested here; the vendored `fuser` patch set not re-audited; performance/DoS under sustained adversarial floods (budgets are per-pass work ceilings, not rate caps — by contract) not load-tested.

---

## G. Remediation order

**Block the next release (with regression tests):**

1. **B1 fix + test** — bind genesis author to the singleton owner in `validate_link` (`chain.rs`) and require `inviter == genesis.author` in `verify_invitation` (`invitation.rs`). Small, format-preserving, contract-restoring. A non-owner-authored genesis must be `Invalid` and never root; the existing two-genesis conflict semantics for *owner*-authored geneses must be preserved (they are the designed conflict signal, not the bug).
2. **P1 resolution** — execute the member-forge→victim-accept test; if accept succeeds, land the `verify_invitation` arm from (1) and promote P1 to Confirmed in a follow-up note. (The (1) fix already closes P1; the test decides severity accounting.)

**Follow-up hardening (after the release-blocking fixes):**

3. Update-archive signatures (D1); owner-interface concurrency cap pending the P2 threat-model decision (D2); projection retention (D3).

**Dependency note:** (1) and (2) share the remediation site (`verify_invitation`), so fixing B1 and re-testing P1 in the same change is the efficient order. No contract document changes are needed: the fix restores the behavior epochs.md/trust.md already promise.
