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

Readiness of this note, in three parts that must not be collapsed:

- **Architecture accepted (proposed):** Wyrd pursues PQ migration without
  changing its content-addressing model or rewriting history.
- **Security design unresolved:** checkpoint provenance is a specified
  requirement with an unresolved mechanism — acceptable in a proposal,
  and a blocker when writing normative checkpoint-acceptance rules at
  v0.8; negotiation floors are specified conceptually, with
  authentication and recovery behavior still needing concrete contract
  language; retrospective semantics are specified. The provenance
  mechanism is the must-resolve item before this architecture is
  called complete.
- **Implementation deferred:** until the v0.8 contract freeze and the T11
  dependency requirements are satisfied.

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
migration time exceeds the estimated quantum-threat horizon (Mosca's
inequality, read as a risk threshold — not proof of compromise),
migration must already have begun; re-encryption after harvest cannot
un-harvest.

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
  different quantum bounds. Generic bounds for an ideal n-bit hash:

  | Property | Classical generic work | Quantum generic work |
  |---|---|---|
  | Preimage | 2^n | 2^(n/2) |
  | Collision | 2^(n/2) | ~2^(n/3) |

  A 256-bit hash therefore does not automatically carry 128-bit
  quantum collision security. BLAKE3 is retained provisionally on this
  basis — no replacement is justified by PQ migration alone — but the
  contract documents the margin per use (which uses need collision vs
  preimage resistance) rather than a blanket claim.
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

NIP-44 deserves a sharper boundary than "somebody else's problem."
Wyrd does not control the Nostr identity format or the NIP-44
specification — but it does control what it places inside NIP-44
messages. Verified against the control plane (`control/message.rs`,
`trust.md` "Message set"): no NIP-44 message carries an epoch secret
or other durable secret under NIP-44-only protection today. Every
secret-carrying kind wraps its payload in an independent inner
envelope first — per-recipient ECDH capability wraps inside the
`Capability`, rotation-delivery, and bootstrap framings — with NIP-44
always the outer transport layer, never the sole confidentiality
boundary. `KeyRotation` carries only a transition id (a hint, no key
material); announcements carry ids and signatures.

All of those inner envelopes are classical ECDH, hence HNDL-exposed
in exactly the same way — which is what makes the mitigation
available without waiting on Nostr: **application-layer PQ-protected
payloads inside NIP-44.** NIP-44 stays the delivery and mailbox
mechanism; Wyrd independently encrypts sensitive control-plane
payloads to the recipient's PQ-capable device key; NIP-44 becomes an
additional outer layer, not the only boundary; the recipient verifies
the inner envelope and its authenticated sender/key binding. The v1
design documents the migration problem this creates for existing
recipients (pre-PQ devices cannot open PQ-inner envelopes — delivery
to mixed-version memberships needs explicit rules, not silent
downgrade to classical-inner).

## Forward protection vs retrospective confidentiality

"New suite arrives with a new epoch, never rewrites history" is
necessary and insufficient for HNDL. An attacker who recorded a 2026
classical envelope recovers the old epoch secret whenever Shor becomes
practical, regardless of a 2030 PQ rotation. Three classes, three
requirements:

| Data | Requirement |
|---|---|
| Future objects | Encrypt under PQ-protected epoch keys |
| Existing objects with potentially exposed epoch keys | Re-encrypt under new keys as a best-effort reduction of future exposure (below) |
| Historical authorization records | Preserve immutable; anchor separately (below) |

Whether an old representation was harvested is unknowable: silent
copying leaves no signal Wyrd can observe. Migration MUST NOT be
conditioned on harvest knowledge. Re-encryption therefore promises
exactly this, no more:

- It protects future access through the newly published representation.
- It does not protect an old representation whose wrapping key is
  subsequently recovered.
- It does not revoke copies an adversary already obtained.
- It still reduces exposure wherever the adversary never obtained the
  old ciphertext or cannot obtain the old key.

Re-encryption preserves the `ContentId` of objects whose identity is
derived independently of their sealed representation
(`seal.rs`: `ContentId` over the plaintext, `StorageId` over the
sealed bytes with fresh nonces; `../contracts/object-model.md`
decision 30; upgrade invariant 8 — migration as ordinary
snapshot-producing operations). Manifest identities are the documented
exception: manifest entries carry the referenced representation's seal
version inside the hashed payload, so a seal-version change can move a
manifest `ContentId`. Such changes are ordinary snapshot-producing
operations, not a change to the content-addressing model. But
re-encryption cannot un-harvest: copies an adversary already recorded
stay decryptable under the old epoch secret. Migration also revokes
nothing silently: old representations keep their existing access and
retention semantics under the epoch and retention contracts —
cryptographic migration MUST NOT quietly introduce a different
retention or authorization model.

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

A checkpoint cannot establish its own authority, and this note does
not claim otherwise. A PQ-signed checkpoint whose signing key was
itself legitimized only by a now-forgeable classical signature moves
the classical trust problem one level up: the attacker mints their own
PQ keypair, forges the classical binding, signs an alternative history,
and produces a perfectly genuine PQ signature over a false checkpoint.
The v1 contract MUST therefore require independently verifiable
checkpoint provenance — trust established outside the checkpoint being
checked. Candidate mechanisms (no choice made here):

- Contemporaneous checkpoints retained by existing devices.
- Quorum-attested checkpoints from independently trusted devices.
- External timestamping or transparency witnesses.
- Cross-peer checkpoint gossip with consistency verification.

Post-quantum signatures are not enough to establish post-quantum
trust: Wyrd must prove the authority that created a PQ key was
legitimate even after the cryptography that originally established
that authority becomes forgeable. Resolving the provenance mechanism
is the must-resolve item before this architecture is called complete.

## Suite registry and negotiation (sketch, not spec)

- Versioned cryptographic suites for key establishment, key wrapping,
  and signatures; key-type identifiers distinct from key material.
- Every durable envelope names its suite explicitly, and the suite
  identifier rides inside the authenticated data (AAD or signed
  preimage) — never a strippable header.
- Negotiation separates two jobs that must not be collapsed.
  **Compatibility selection** chooses the oldest mutually supported
  representation where the upgrade contract requires it
  (`../contracts/upgrade-contract.md` invariant 5). **Cryptographic
  security policy** refuses suites below the locally required floor.
  Suppressing a peer's advertised PQ capabilities and falling back to
  classical-only protection is a downgrade attack even though no
  envelope byte was modified — oldest-mutual selection alone permits
  it. Each participant therefore enforces a locally configured minimum
  security level and fails closed rather than silently falling back.
- The negotiation transcript authenticates the advertised capability
  sets, the selected suite, the participant identities, and the
  relevant policy. The selected suite is bound into the resulting key
  derivation and the authenticated envelope. A mismatch, a suppressed
  capability, or a selection below either participant's floor fails
  closed. Capability-based negotiation never authorizes security
  downgrade: suite advertisement alone is not a security property.
- New constructions arrive with new epochs and new envelope versions;
  old epochs keep their construction (upgrade invariant 7). Envelope
  versioning follows the existing `SEAL_VERSION` / `ROTATION_VERSION` /
  `CONTROL_VERSION` pattern; no generic provider framework.
- Hybrid key establishment combines both contributions in a
  standardized, domain-separated KDF combiner: the recipient identity,
  sender identity, suite identifier, protocol context, and negotiation
  transcript are bound into derivation or authenticated context;
  failure of either required component fails the operation.
  Concatenating two secrets without a specified combiner is not an
  acceptable construction, and this note specifies no home-grown
  combiner.
- Suite identifiers are never reused. Deprecated suites stay
  identifiable for historical decoding; unsupported suites fail closed.
  Retirement removes use, never readability.
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
- New envelopes cannot silently downgrade: negative tests cover both
  envelope-header stripping and capability-advertisement stripping
  during negotiation.
- A new epoch establishes without classical-only key establishment.
- A device with a PQ-only security floor refuses classical-only
  negotiation fail-closed instead of falling back.
- Re-encryption yields same-`ContentId`/new-`StorageId`; old
  representations stay readable — without claiming harvested copies
  become secure.
- Historical checkpoints validate against their independent provenance
  anchor, not against newly presented classical signatures alone.
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
