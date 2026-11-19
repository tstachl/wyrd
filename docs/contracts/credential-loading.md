# Credential loading (v0.4)

Status: Normative. Shipped scope: the read half is implemented (the CLI file backend conforms); provisioning, replacement, deletion, and the OS backends are specified, not yet wired. Scope: how a drive owner obtains its secret
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
material is kept and retrieved. The trait carries the read half
today; provisioning, replacement, and deletion join with the
migration slice. Two backends are specified by this contract:

- **File backend** (ships today): the hardened Unix file reader in
  `wyrd-cli` (`credentials.rs`) plus its writer, `wyrd identity`
  (`commands/identity.rs`). Owner-only files, never overwritten,
  `O_NOFOLLOW`, uid and mode checks, bounded reads, zeroized
  buffers.
- **OS credential-store backend** (specified here, implemented per
  platform): the platform's credential store — Keychain on macOS,
  an OS credential service (Secret Service expected) on Linux —
  reached through the same loader boundary.

Adding a backend changes no secret format, no keystore wrap, no
membership or epoch behavior, and no wire byte. Hardware-backed
unwrapping stays a storage upgrade that changes nothing about the
format (`docs/contracts/trust.md:150-151`).

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
provides the operations below. Lookup and open ship with the file
backend; provisioning, replacement, and deletion are specified
here and join with the migration slice. (This section states the
full boundary so backends are built once against it — it does not
claim the trait already carries what is marked specified.)

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
  real service context (launchd agent, systemd user unit — never
  a login shell). GUI-side access never proves service-side
  access: each backend is verified from the actual service
  context on each supported platform.

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
  credentials, retrievable from the launchd agent context.
  Keychain access is requested at registration; the ACLs travel
  with the settings contract. Account for the logged-in/unlocked
  session vs service-restart distinction: an item the GUI can
  read while the service context cannot is `denied`, and the
  drive reads unavailable — never broken.
- **Linux**: Secret Service (or the credential service of the
  supported desktop environment) in the systemd user-unit
  context. Never assume a D-Bus session exists because the
  desktop could reach one. Where the supported context cannot
  retrieve reliably, the drive reads unavailable.
- **File backend**: retained unchanged for CLI and
  operator-provisioned deployments, with its existing hardening.
  File paths stay out of desktop-managed service definitions
  once a drive migrates; the file backend itself is never
  weakened to make the OS backend look convenient.

Windows credential storage is out of v0.4 with the Windows
platform (`docs/architecture.md:48`).

## Migration

Existing desktop-managed drives on file credentials migrate
explicitly and failure-safe, in this order:

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
is kept, never the Drive Root Key, never the keystore wrap.

## What this contract does not own

- The secret model (root custody, keystore wrap, T11/T12/T14):
  `docs/contracts/trust.md`.
- Membership, epochs, revocation: `docs/contracts/epochs.md`.
- The owner interface and mutation queue:
  `docs/contracts/owner-interface.md`.
- Window close, quit, lock, and what survives them:
  `docs/contracts/desktop-lifecycle.md`.
- Session-posture unlock (typed passphrase, per-session owner):
  unchanged by this contract; unattended session startup is a
  separate decision.
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
reach: a new leaf crate at the first OS binding if the
dependency policy requires it, never `wyrd-sync`'s allow-list,
never `wyrd-desktop`. `wyrd-contracts` pins every new edge,
feature, and `unsafe` site before the backend lands.
