# Credential loading (v0.4)

Status: Normative. Shipped scope: the loader boundary carries reads and the migration write half (the CLI file backend conforms; the OS credential-store backends conform on macOS, where service-context retrieval is verified by hand — the Linux service-context proof is deferred, see below); `wyrd drive migrate-credentials` runs the six gated migration steps, and the owner/drive read paths honor a migrated configuration. `wyrd bootstrap` provisions store-backed drives through the same commit boundary (input files or pipe seed the item, the configuration records the backend after the daemon accepts the material), service definitions render the store reference (`--credential-backend os-store --credential-account`), session unlock loads the store identity with a typed passphrase, and `drive reconfigure` reinstalls a managed definition on either backend. First unlock without a prior GUI visit is verified on macOS (two-phase hand probe: store in the login session, first retrieval under a no-terminal LaunchAgent); the Linux service-context proof is deferred, see below. Scope: how a drive owner obtains its secret
material — the device identity secret and the passphrase protecting
the wrapped root key — without the desktop UI alive, in the actual
OS service context. Authority: this contract owns the
platform-neutral loader boundary (secret format, lookup identity,
failure behavior, secret lifetime, zeroization) and the backend
obligations. What a secret *is* stays with
`docs/contracts/trust.md` (root custody, keystore wrap, custody
modes); window close, quit, and lock stay with
`docs/contracts/desktop-lifecycle.md`; the desktop's onboarding and
provisioning flows stay with `docs/design/desktop-shell.md` and
`docs/design/first-run-onboarding.md`, subordinate to this contract
where they touch credential storage. Where they disagree, this
contract wins.

## The boundary

One narrow loader boundary serves every consumer that reads
credentials: the CLI, the daemon-supervised owner, the desktop
backend, and the update verifier. Consumers receive secret
*material* — the device identity always, plus the keystore
passphrase where the reference names one (`None` is the session
posture, whose passphrase arrives via the unlock pipe or prompt:
a valid outcome, never a missing item) — never files, modes,
keychain handles, or OS facilities. Backends implement how that
material is kept and retrieved. The trait carries reads plus the
migration write half (provision, replace, delete, each under the
commit boundary: validate in memory, write, read back through the
production reader, compare). Two backends implement the full
boundary:

- **File backend** (ships today): the hardened Unix file reader in
  `wyrd-cli` (`credentials.rs`) plus its writer, `wyrd identity`
  (`commands/identity.rs`). Owner-only files, never overwritten,
  `O_NOFOLLOW`, uid and mode checks, bounded reads, zeroized
  buffers.
- **OS credential-store backend** (implemented per
  platform in `wyrd-credential-store`): the platform's credential
  store — Keychain on macOS, an OS credential service (Secret
  Service expected) on Linux — reached through the same loader
  boundary. Reads plus the migration writer, in the encoding this
  contract's backend section pins.

Adding a backend changes no secret format, no keystore wrap, no
membership or epoch behavior, and no wire byte. Hardware-backed
unwrapping stays a storage upgrade that changes nothing about the
format (`docs/contracts/trust.md:150-151`).

## Stored-item encoding (normative)

One OS-store item per drive, named by its account label under the
fixed `wyrd-drive-credential` service, is UTF-8 JSON:

```json
{"identity_hex":"<64 hex chars>","passphrase":"<optional>"}
```

`identity_hex` decodes to 32 bytes forming a valid identity
scalar (the same gate as the file reader, including its ASCII
whitespace trim); `passphrase`, where present, is bounded at 4096
bytes and returned verbatim — the trailing-newline strip is the
file reader's line discipline, applied once when material is
stored, never on load, so both backends open the same keystore.
Unknown fields are refused. The wire form is bounded at 24576 +
256 bytes: the worst case is a legal 4096-byte passphrase whose
every byte escapes to 6 (`"` and `\` escape to 2, control
characters to 6), plus framing; a larger item is shape-broken.
The platform fetch itself is not bounded — the store facade
exposes no streaming read — so the refusal lands after the store
answers. Provisioning, replacement, and deletion write this
encoding through the loader's commit boundary (shipped).

How a drive's configuration comes to *name* an OS-store account
is a config key in this slice (`credential_backend` plus
`credential_account` in `default.toml`: one backend per drive,
ambiguity refused at parse) honored by the owner/drive read paths;
file flags beside a store configuration refuse. `wyrd bootstrap`
provisions into the store (input files or the pipe seed the item;
the configuration records the backend after the daemon accepts
the material), service definitions render the store reference for
managed drives, session unlock loads the store identity with a
typed passphrase, and `drive reconfigure` reinstalls the
definition on either backend.

## Credential references, never secrets, in configuration

Drive configuration, the registry, service definitions, and IPC
carry a stable credential *reference*, never secret bytes. A
reference names the backend and the item to load (for example,
backend kind plus account/service label); resolving it to material
happens only inside the loader, at the moment of use. No
credential — passphrase, identity secret, or wrapped root key —
appears in command-line arguments, environment variables, logs,
diagnostic output, or serialized configuration. The two bounded
passphrase pipes (onboarding commit stdin, per-session unlock
stdin) are the only secret transits and stay bounded, zeroized,
and fail-closed. The `drive_config` no-secret-material invariant
holds for every new selector field.

## What the loader must do

For one drive, identified by its credential reference, the loader
provides the operations below. Lookup and open run on every owner
start; provision stores a fresh credential, replace overwrites
under the same boundary, and delete removes the backend item. (This
section states the full boundary so backends are built once against
it.)

- **Lookup and open**: return the secret material or a terminal
  failure from the taxonomy below. The caller decides what the
  failure means for the drive; the loader never invents a
  fallback backend.
- **Provision**: store a fresh credential through the backend's
  commit boundary (same atomicity the file backend's commit
  provides: validate in memory, write, read back through the
  production reader, record as adopted). Provisioning must not
  report a drive ready where credential storage or owner
  retrieval has failed.
- **Replace and delete**: replace under the same commit boundary;
  delete removes the backend item. Revocation of what the secret
  *authorizes* is membership business (`docs/contracts/epochs.md`),
  not loader business.
- **Service-context retrieval**: the owner retrieves its material
  with no desktop process alive, no interactive prompt, in the
  real service context (launchd user agent, systemd user unit —
  never a login shell) and within the authenticated user session
  (see `docs/contracts/desktop-lifecycle.md`: the session bounds
  desktop owners; pre-login retrieval is out of scope).
  GUI-side access never proves service-side access: each backend
  is verified from the actual service context on each supported
  platform. Verification state: the macOS backend is verified by
  hand with the two-phase probe (`real_store_probe_round_trip`
  with `WYRD_PROBE_PHASE=store/load`): store through the commit
  boundary in the login session, then first retrieval plus delete
  under a no-terminal LaunchAgent with the material asserted and
  the post-delete `Missing` asserted; the Linux backend's
  service-context verification is deferred — no Linux host was
  available and headless CI has no bus. CI exercises the
  fail-closed path there instead (the dispatcher test routes a
  bus-less store to the OS backend, never the file backend, and
  the taxonomy test pins `PlatformFailure` to `unavailable`).

## Failure taxonomy

Retrieval fails closed, with distinguishable terminal answers.
Missing, denied, and unavailable are different facts with
different remedies; the loader reports which one it saw and
changes nothing:

| Outcome | Meaning | Drive reads |
|---|---|---|
| `ok` | Material returned, shape-checked (passphrase present exactly where the reference names one) | Proceeds to open |
| `missing` | No such item in the backend | Registered but unavailable (re-provision, never auto-create) |
| `denied` | Store reachable, access refused (ACL, locked keychain) | Registered but unavailable (fix access, retry) |
| `unavailable` | Store itself unreachable (no D-Bus session, no Secret Service) | Registered but unavailable (retry; never a weaker substitute) |
| `invalid` | Item present but shape-broken | Fail closed; never guess, never repair silently |

A credential failure is never reported as corruption, never
prompts from the owner service, and never silently falls back to
another backend. Interrupted provision/replace/migrate leaves at
least one working credential path (see "Migration" below).

## Credential availability (desktop)

Storage, availability, lifecycle, supervision, and
materialization are separate concerns: storing in a backend does
not imply the owner can retrieve at any moment, supervision does
not imply availability, and neither implies a server lifecycle.
In particular, Keychain storage never implies pre-login start,
and a systemd unit never implies unattended operation.

Each row below states three things separately: the expected
platform behavior, Wyrd's requirement, and what is verified.
Expected behavior is not a guarantee — access-control
configuration, lock state, and the supported configuration can
change it — and no row is claimed verified until a test on a
supported configuration proves it.

| Session state | Expected platform behavior | Wyrd requirement | Verified |
|---|---|---|---|
| macOS logged in, keychain unlocked | Login keychain readable from the user agent | Owner starts when enabled and the item loads | By hand: two-phase probe (store in session, first retrieval under a no-terminal LaunchAgent, material plus post-delete `Missing` asserted) |
| macOS screen locked | Login keychain stays unlocked by default | Owner keeps serving; new opens succeed | Not verified: access-control configuration can change this |
| macOS logged out | Session ends; user agents terminate; login keychain locks | Owners terminated per `desktop-lifecycle.md`; next login re-eligible | Termination behavior tested per supported configuration before it is claimed |
| Linux graphical login, collection unlocked | Secret Service reachable from the user unit | Owner starts when enabled and the item loads | Deferred: no Linux host; headless CI has no bus |
| Linux screen locked | Collection stays unlocked by default | Owner keeps serving | Deferred, same as above |
| Linux logged out | User manager stops without lingering; collection locks | Termination requested through session teardown; unresolved stays `Unconfirmed`; next login re-eligible | Termination behavior tested per supported configuration before it is claimed |

Unattended operation — retrieval with no authenticated user
session, now or ever — belongs to server deployments with
explicitly provisioned credentials, never to a desktop login
preference. A desktop drive that cannot retrieve reads
registered-but-unavailable; it never invents a weaker path.

## Secret lifetime and zeroization

- Material lives in `Zeroizing` buffers from load to use; no
  `Debug`/`Display` of secret bytes; bounded reads (the 4096
  precedent holds for every new reader and pipe).
- The loader holds nothing past the open: the daemon holds the
  identity for its lifetime only as local-key mode already
  specifies (`docs/contracts/trust.md:897-908`).
- No secret persists outside the backend that provided it: no
  temp files, no argv, no environment, no crash dumps by design
  (platform crash-report hygiene is best-effort and documented
  per backend, not a loader guarantee).

## Platform obligations

- **macOS**: Keychain items for desktop-managed drive
  credentials, retrievable from the launchd user-agent context
  within the authenticated session. Items carry default (NULL)
  access — no per-application trust list, no biometric flags —
  so there is no registration-time grant to request and none is
  needed: the first agent read succeeds with no prior GUI visit
  while the login keychain is unlocked (verified by hand, two-phase
  probe). The posture is fixed here, not in settings: per-app ACL
  calls would need a contract amendment (the facade owns the
  store — see Placement), and biometric-gated reads would
  contradict non-interactive service-context retrieval. A locked
  keychain reads as `denied` through the mapped statuses
  (unit-pinned; the locked state itself is not hand-triggered —
  locking the operator's login keychain is not part of the probe).
  Account for the session-vs-service distinction: an item the GUI
  can read while the service context cannot is `denied`, and the
  drive reads unavailable — never broken. Lock-state and
  access-control effects follow the availability matrix above;
  they are requirements on Wyrd's behavior, not promises about
  the platform.
- **Linux**: Secret Service (or the credential service of the
  supported desktop environment) in the systemd user-unit
  context, within the authenticated session and without
  lingering. Never assume a D-Bus session exists because the
  desktop could reach one. Where the supported context cannot
  retrieve reliably, the drive reads unavailable.
- **File backend**: retained unchanged for CLI and
  operator-provisioned deployments — including headless server
  deployments with independent supervision and unattended
  operation — with its existing hardening. It is the server
  credential mechanism, not a temporary compatibility layer.
  File paths stay out of desktop-managed service definitions
  once a drive migrates; the file backend itself is never
  weakened to make the OS backend look convenient.

Windows credential storage is out of v0.4 with the Windows
platform (`docs/architecture.md:48`).

## Migration

Existing desktop-managed drives on file credentials migrate
explicitly and failure-safe through `wyrd drive
migrate-credentials`, in this order:

1. Read and validate the existing credential through the
   hardened file reader.
2. Store it in the OS credential store.
3. Verify the owner retrieves it through the new backend.
4. Update the drive's credential reference and service
   definition.
5. Verify the owner opens the existing keystore.
6. Only then retire the old credential file.

Steps 3 and 5 are the gates: never delete the old path before
the new one is verified. Migration changes where the passphrase
is kept, never the Drive Root Key, never the keystore wrap. The
configuration rewrite (step 4) is the store-backend key pair in
`default.toml`; the manager's installed service definition still
names the retired files until `drive reconfigure` reinstalls it
with the store reference, so the migrate command warns where a
unit is installed and a migrated drive restarts through the
store-aware owner path until then. Re-runs converge: an identical
store item continues, a completed migration re-verifies and
reports done, and a differing store item refuses rather than
overwriting.

## What this contract does not own

- The secret model (root custody, keystore wrap, T11/T12/T14):
  `docs/contracts/trust.md`.
- Membership, epochs, revocation: `docs/contracts/epochs.md`.
- The owner interface and mutation queue:
  `docs/contracts/owner-interface.md`.
- Window close, quit, lock, and what survives them:
  `docs/contracts/desktop-lifecycle.md`.
- Session-posture unlock (typed passphrase, per-session owner):
  unchanged by this contract except that unlock loads the store
  identity for store-backed drives. Pre-login and
  user-session-independent unattended startup are server-only and
  explicitly out of scope for desktop. Desktop-managed owners may
  start non-interactively after the authenticated user-session
  domain exists, using credentials available from the OS
  credential store.
- Service enablement at login (whether the manager starts the
  unit): separate observation, separate issue. Enablement must
  never be mistaken for credential availability, and
  availability must never imply enablement.
- The desktop onboarding flow and its copy: subordinate design
  docs, amended as the backend lands.

## Placement (normative)

The loader trait lives where the secret type lives
(`wyrd-sync::keys`, beside `DeviceIdentitySecret` and the
zeroization discipline). The file backend stays in `wyrd-cli`.
Each OS backend lives in the narrowest holder that keeps
platform FFI out of the audited substrate and out of desktop
reach: the `wyrd-credential-store` leaf crate, holding the first
OS binding (`keyring`, per-OS target tables), never
`wyrd-sync`'s allow-list, never `wyrd-desktop`. `wyrd-contracts`
pins every new edge, feature, and `unsafe` site before the
backend lands.
