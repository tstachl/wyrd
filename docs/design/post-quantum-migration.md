# Post-quantum migration architecture

Status: Proposed / Open. This note owns the post-quantum migration
boundaries: the substrate inventory, the suite-registry and negotiation
sketch, the Nostr-dependency design, and the forward-vs-retrospective
semantics. Normative invariants live in the contracts linked below.
Nothing here authorizes a contract change, a v0 wire-format change, or
a new dependency; `../contracts/trust.md`, `../contracts/object-model.md`,
`../contracts/upgrade-contract.md`, and `../contracts/epochs.md` win every
conflict with this note. Target: normative contract language at the v0.8
protocol compatibility and security freeze (`ROADMAP.md`, v0.8).

## Goal

Wyrd supports migration to standardized post-quantum cryptography
without changing its content-addressing model or rewriting history,
with the migration boundaries specified before the v0.8 freeze.

Done means: suite identifiers, key types, downgrade-resistant
negotiation, and epoch-scoped suite introduction are pinned in the
contracts; old classical-only envelopes stay readable under their
declared semantics; new envelopes cannot silently downgrade; and no
post-quantum dependency has landed until an audited implementation
satisfies T11 (`../contracts/trust.md`).

## Threat model

Harvest-now-decrypt-later governs: an adversary records classical
envelopes and ciphertext today and decrypts them once a capable
quantum computer exists. Wyrd's ciphertext is designed to be retained
for decades, so durable key establishment — not transport sessions —
is the primary exposure. If the confidentiality lifetime plus the
migration time exceeds the quantum-horizon (Mosca's inequality), the
data is already exposed; re-encryption after harvest cannot un-harvest.

## Substrate inventory (verified, not assumed)

Wyrd does not use X25519/Ed25519/AES-256. The actual substrate:

| Use | Primitive | Quantum impact |
|---|---|---|
| Device identity, snapshot / membership / owner-proof signatures | secp256k1 + BIP-340 Schnorr (32B pk, 64B sig), deterministic nonces | Shor breaks — High |
| Capability / bootstrap / rotation wrapping | secp256k1 ECDH (x-coordinate, parity-normalized) → HKDF-SHA256 → XChaCha20-Poly1305 | Shor breaks ECDH — Critical, durable HNDL surface |
| Control-plane transport | NIP-44 (secp256k1 ECDH + HKDF-SHA256 + ChaCha20 + HMAC) inside NIP-59 gift wrap | Same ECDH exposure; relay-retained ciphertext is harvestable |
| Object / manifest / sealed-control / escrow AEAD | XChaCha20-Poly1305, 256-bit keys | Grover only (~128-bit effective) — Low |
| Hashing / KDF / addressing | BLAKE3-derive-key everywhere; HKDF-SHA256 in wraps/NIP-44; Argon2id for the keystore | Grover only — Low |
| Bulk transport | iroh QUIC/TLS stack (pinned set), Nostr relays | Follows upstream — not Wyrd-owned work |

Two classifications the inventory must keep separate:

- **Hash uses are not one assumption.** ContentId/StorageId collision
  resistance, second-preimage resistance, and KDF output security have
  different quantum bounds. BLAKE3 is retained provisionally; the
  contract documents per-use assumptions rather than a blanket claim.
- **Escrow/keystore wraps are not key-establishment.** Argon2id plus
  XChaCha20 is password-entropy plus Grover scope, not Shor scope. A
  plaintext DriveId in escrow is privacy, not a KEM break. They are
  inventoried separately from secp256k1-ECDH envelopes.

Priority order: (1) Wyrd-owned durable key establishment; (2)
identity/authorization signatures subject to the Nostr binding below;
(3) transport follows upstream. Symmetric cryptography and content
addressing need no redesign.

## The Nostr dependency

Device identity **is** a Nostr secp256k1 public key, immutable by
contract. The membership log, the NIP-44 mailbox, and the identity root
are therefore shared with an external ecosystem Wyrd does not control.
Replacing Nostr identity is not a precondition for PQ encryption.

Migration paths differ by owner:

| Surface | Migration ownership |
|---|---|
| Capability and bootstrap wrapping | Wyrd |
| Rotation delivery | Wyrd |
| Membership and snapshot signatures | Wyrd, subject to identity binding |
| Nostr identity | Shared with external ecosystem |
| NIP-44 mailbox encryption | Nostr ecosystem / Wyrd integration |
| iroh transport | Upstream transport ecosystem |

Direction: investigate a separate PQ device-authentication key, bound
to the existing Nostr identity through authenticated membership
admission (the T14 two-key pattern is the precedent). The trust
semantics must be explicit: a PQ key bound by a classical-only
signature is a new authentication authority with a classical bootstrap
trust dependency. It does not make the original identity
quantum-resistant.

## Forward protection vs retrospective confidentiality

"New suite arrives with a new epoch, never rewrites history" is
necessary and insufficient for HNDL. An attacker who recorded a 2026
classical envelope recovers the old epoch secret whenever Shor becomes
practical, regardless of a 2030 PQ rotation. Three classes, three
requirements:

| Data | Requirement |
|---|---|
| Future objects | Encrypt under PQ-protected epoch keys |
| Existing objects with potentially exposed epoch keys | Re-encrypt under new keys *where retrospective confidentiality is required and the old representations were not already harvested* |
| Historical authorization records | Preserve immutable; anchor separately (below) |

Re-encryption preserves plaintext identity by construction
(`seal.rs`: `ContentId` over the plaintext, `StorageId` over the
sealed bytes with fresh nonces; `../contracts/object-model.md`
decision 30; upgrade invariant 8 — migration as ordinary
snapshot-producing operations). But it cannot un-harvest: copies an
adversary already recorded stay decryptable under the old epoch
secret. The v1 contracts must state what Wyrd promises about
pre-migration confidentiality in exactly these terms — that promise is
what separates a system that merely supports PQC from one that
protects decades of accumulated data.

## Historical authenticity and checkpoints

Classical-only history becomes forgeable under Shor: "genuine
signature" for pre-PQ transitions and snapshots cannot be re-proven.
The migration needs a trust transition the future attacker cannot
rewrite:

1. Classical-authenticated initial PQ key registration.
2. Dual-signed transition once the PQ key is established.
3. PQ-signed checkpoint covering the membership history and key bindings.
4. Explicit pre/post-checkpoint acceptance rules.

The checkpoint is an attestation boundary over history as observed at
checkpoint time, not a retroactive upgrade of old signatures. A record
can then be valid under its creation-era guarantees while its
authenticity is anchored by a stronger checkpoint. That distinction is
normative `trust.md` material at v1 scope — including the
forgery-of-binding case, where the attacker forges the PQ-key binding
itself rather than subsequent signatures.

## Suite registry and negotiation (sketch, not spec)

- Versioned cryptographic suites for key establishment, key wrapping,
  and signatures; key-type identifiers distinct from key material.
- Every durable envelope names its suite explicitly, and the suite
  identifier rides inside the authenticated data (AAD or signed
  preimage) — never a strippable header.
- Negotiation is capability-based with oldest-mutual selection, per
  `../contracts/upgrade-contract.md` invariant 5; no silent downgrade,
  and suite advertisement alone is not a security property.
- New constructions arrive with new epochs and new envelope versions;
  old epochs keep their construction (upgrade invariant 7). Envelope
  versioning follows the existing `SEAL_VERSION` / `ROTATION_VERSION` /
  `CONTROL_VERSION` pattern; no generic provider framework.
- Algorithm picks stay provisional: ML-KEM-768 / ML-DSA-65 as the
  conventional defaults, SLH-DSA retained as a lattice-independent
  hedge (large, slow — backup only). Finalization waits on audits.

Overhead is a design input, not an afterthought: ML-DSA-65 signatures
(3309B vs 64B) and ML-KEM-768 ciphertexts (~1088B vs 32B ephemeral)
multiply across every transition, snapshot, invitation, and
per-recipient wrap. The v1 design measures bytes per artifact,
verification time, and constrained-transport impact before freezing.

## Evidence for PQ readiness

When implementation lands, it demonstrates, at minimum:

- Old classical-only envelopes remain readable under declared semantics.
- New envelopes cannot silently downgrade (negative tests).
- A new epoch establishes without classical-only key establishment.
- Re-encryption yields same-`ContentId`/new-`StorageId`; old
  representations stay readable — without claiming harvested copies
  become secure.
- Historical checkpoints validate independently of newly presented
  classical signatures.
- `layer_contracts` and T11 gates green; no wire bytes outside the
  pinned surface.

## Staging

| Phase | Deliverable | Constraint |
|---|---|---|
| Now | This note + substrate inventory as code | Docs and tests only; no v0 changes |
| v0.8 freeze | Normative suite/negotiation/checkpoint language in `trust.md` + `upgrade-contract.md` | Contracts change, v0 envelopes don't |
| Before v1.0 | Hybrid envelopes + hybrid authorization, interop + overhead evidence | Only behind audited implementations (T11) |
| Post-v1.0 | Existing-data migration and classical retirement | Explicit re-encryption strategy; honest retrospective promise |

## Deferred

Algorithm finalization, wire-format changes, implementation
dependencies, iroh/Nostr upstream migration, classical algorithm
retirement. `ml-kem` / `ml-dsa` (RustCrypto) are tracked, not adopted:
both currently carry "never independently audited — USE AT YOUR OWN
RISK," which fails T11 by construction.
