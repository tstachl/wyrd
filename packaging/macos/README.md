# macOS packaging (`packaging/macos/`)

Status: Implemented (daemon socket half and bundled domain registrar; distribution signing owned separately).
This note
holds the adapter layout and the sandbox/socket scheme; the
normative transport (both sockets, the alias, authentication, and
the shared connection bound) lives in
`docs/contracts/owner-interface.md` (Transport), which this note
defers to on any conflict — the drive-owner interface for
transport and the File Provider issue for OD-17-A/B/C and the
OD-8 outcome.

Platform packaging outside Cargo and Nix: the Swift File Provider
adapter and its build script. Nothing here is a Cargo workspace
member (a member must be a Rust crate); the adapter's boundary is
enforced by the `fileprovider_swift_stays_within_the_owner_boundary`
contract instead of a `MemberPolicy` allow-list — the OD-8
refinement, proposed on the issue thread and awaiting decision.
See the File Provider issue for the OD-17-A/B/C
decisions (`nostr:nevent1qqsqz3tuhx8vvucgzrdtfc0y49sfs0mc3v7xratvxhg999plxp97e0gpz9mhxue69uhkwunpwdczuap49eehgvak3m0`); the identity and conflict-surface decisions gated
Stage 2 and are recorded — Stage 2 implements against them.

## Layout

- `fileprovider/Sources/` — the Swift adapter. One File Provider
  domain per registered drive; item identifiers are drive-scoped.
- `fileprovider/Info.plist` — the appex manifest (`com.apple.fileprovider-nonui`,
  principal class `WyrdFileProvider.FileProviderExtension`).
- `fileprovider/WyrdFileProvider.entitlements` — development
  entitlements: App Sandbox plus the App Group. The group id is
  granted literally as written — Xcode does not team-prefix App
  Group ids — so the script performs no substitution. The container
  materializes only under a team-signed build with provisioning.
  Production signing, notarization, and the production group
  container are the license-gated tail.
- `fileprovider/build-appex.sh` — script-driven build: `swiftc`
  against the macOS SDK, manual appex assembly, `codesign`. No
  `.xcodeproj`, no Xcode IDE, no XcodeGen. Verify with
  `packaging/macos/fileprovider/build-appex.sh`; CI cannot run this
  (Linux-only runners), so the contract test is the automated leg
  and the script is human-verified evidence.

## Toolchain shells (devenv)

Two shells, one default. The default devenv shell is the
reproducible Rust environment and injects the Nix Apple SDK
(`DEVELOPER_DIR`/`SDKROOT`), which hijacks the `/usr/bin` shims
away from the system Apple toolchain. `build-appex.sh` undoes
that injection process-locally and discovers the toolchain
conventionally (`xcode-select -p`, `xcrun --sdk macosx`), so it
builds in both shells. For interactive Apple-native work (Swift
outside the script, Xcode runs, future signing), enter the
explicit escape hatch instead:

```sh
devenv --profile native-apple shell
```

which nulls the Nix SDK, unsets the injected variables (genuinely
unset, per `test -z "${VAR+x}"`), and drops the Nix `xcrun`
shadow from `PATH`. No hostname/user auto-activation, direnv
stays on the default shell, at most two profiles. A deliberate
Xcode selection is `TOOLCHAIN=<developer dir>` (with `SWIFTC` and
`SDK` as finer overrides); never export `DEVELOPER_DIR` for it.

## Bundle builds (macOS link-order pin)

Build the macOS bundle only through `packaging/macos/build-bundle.sh`
(the release runbook does). Under the default devenv shell the Nix cc
wrapper puts `-L/nix/store/...-libiconv.../lib` on the link line, so
the Rust binary records a `/nix/store` libiconv — and a team-signed
bundle with hardened runtime fails library validation at launch
(`different Team IDs`, dead before main; observed 2026-11-14,
`slice_uuid 38ac3d76-…`). The wrapper discovers the Xcode SDK via
`/usr/bin/xcrun` (same DEVELOPER_DIR/SDKROOT-tolerant pattern as
`build-appex.sh`), appends `-C link-arg=-L<MacOSX.sdk>/usr/lib` to
`RUSTFLAGS` so the SDK `libiconv.tbd` (`/usr/lib/libiconv.2.dylib`,
same ABI 7.0.0) wins, exports `SDKROOT=<MacOSX.sdk>` so `rustc`
uses the discovered SDK directly instead of its own bare `xcrun`
query (which under devenv resolves to the Nix shadow and warns),
runs `cargo tauri build` from
`crates/wyrd-desktop` (default `--bundles app`), reseals the outer
app shallowly (the bundler's linker-only seal leaves the staged
files unverified; never `--deep`), then guards with
`packaging/macos/check-no-nix-links.sh`: every file under the bundle
app's `Contents/MacOS` (no hardcoded binary name), plus the
standalone binary, the embedded appex binary, and the bundled
co-installed CLI when present. Any
`/nix/store` linkage — or nothing to inspect at all — fails the
build, and the notes name the release dir examined. The dir
defaults to `CARGO_TARGET_DIR`-aware `target/release`, moves with
a forwarded `--target <triple>`, and falls back to
`CARGO_BUILD_TARGET`. No source or entitlement changes — purely link-search
order. The pin is Darwin-only; elsewhere the build
forwards untouched. Under `devenv --profile native-apple` the
inherited-value note may print while `SDKROOT` itself is unset at
entry — stderr noise only, the export still pins the discovered
SDK for the cargo invocations below. Signing env (`SIGN_IDENTITY`,
`PROVISIONING_PROFILE`) passes through to the `stage-appex.sh` hook. The
wrapper stages `wyrd-finder-capability.txt` in `Contents/Resources`: set
`WYRD_FINDER_CAPABILITY` to `adhoc`, `identity` or `profile` to declare the
mode, or leave it unset and the wrapper infers it from the signing inputs
so a direct build matches the signature it made.

Bundle identities: app `md.enesis.wyrd` (from `tauri.conf.json`),
extension `md.enesis.wyrd.fileprovider`, group default
`JU2RFHWQPH.group.md.enesis.wyrd`. The group id is one
environment-configurable value: `WYRD_APP_GROUP` overrides the
default at build time (rendered into the entitlements, the appex
`Info.plist` and the registrar launch agent) and at runtime (Rust
path derivation). The default is team-prefixed so a free Personal
Team can resolve the container without registering a group, and
distribution with the same team needs no change; a future signing
team sets the one variable. Historical observation, 2026-11-11
under development signing on macOS 15.8.2: with the then-current
unprefixed id the system materialised the container at
`~/Library/Group Containers/group.md.enesis.wyrd/`, with no team
prefix — confirmed by the probe log and `fileproviderd`'s own
dump. Treat that as the resolved path of that build, not a
platform law: it was verified for development signing only, and
other signing types (Developer ID, notarised distribution) are
unverified. The group id is part of every group-socket path:
`<account home>/Library/Group Containers/<group>/Sockets/<16 hex>.sock`.
With the team-prefixed default this is 94 bytes plus the short account
name, against 103 usable bytes of `sun_path`, so accounts of ten
characters or more exceed the ceiling. The bind fails closed with an
error naming `WYRD_APP_GROUP`; choose a shorter override for such an
account. Accordingly: the extension resolves through `containerURL` and
never hardcodes, and Stage 2 must confirm the daemon-side bind
lands where the extension resolves before the scheme settles —
an unsandboxed daemon without the group entitlement cannot call
`containerURL`, so whatever rule it uses to compute the directory
needs the same live confirmation this build gave the extension
side. The container materializes only under a team-signed build
with provisioning. Deployment target macOS 13.0.

## Developer install (ad-hoc, no fee)

One command builds ad-hoc and installs to an Applications dir,
ready to launch — no signing assets, no paid Developer Program:

```sh
packaging/macos/install-dev.sh
```

Prerequisites: Xcode/CLT (`xcode-select --install`), the devenv
shell (cargo), `bun` (the desktop `beforeBuildCommand`), `python3`.
For a Finder-capable build, also pass a development identity and an
App Group provisioning profile kept outside the repository:

```sh
packaging/macos/install-dev.sh \
  --sign-identity "Apple Development: …" \
  --provisioning-profile /path/to/WyrdFileProvider.provisionprofile
```

`SIGN_IDENTITY`, `APPLE_SIGNING_IDENTITY`, and `PROVISIONING_PROFILE`
in the environment are equivalent. A real identity without a profile
is refused before any build: the App Group grant would not be
effective and the appex would stay inert. The script verifies the
embedded appex's identifier, team, entitlement, and profile, plus the
domain registrar, before the app reaches /Applications.

`WYRD_APP_GROUP` selects the App Group id for the whole build; the
default is the team-prefixed `JU2RFHWQPH.group.md.enesis.wyrd`. The
build renders it into the entitlements, the appex `Info.plist` and
the registrar launch agent, and Rust reads the same variable at
runtime, so one setting moves every consumer. A future signing team
changes one value, not 37 literals.

### Stable identity for repeat installs (no fee, Finder stays inert)

An ad-hoc rebuild changes the code signature each time. macOS then
treats the rebuilt binary as a new keychain client and asks again for
access to the `wyrd-drive-*` items. A free Personal Team can issue an
Apple Development certificate at no cost. Supplying it keeps one signer
across rebuilds, so an "Always Allow" grant survives a reinstall:

```sh
packaging/macos/install-dev.sh \
  --stable-identity "Apple Development: …"
```

`STABLE_SIGN_IDENTITY` in the environment is equivalent. The mode signs
the app, the bundled `wyrd` CLI, the `wyrd-supervise` wrapper and the
domain registrar with that identity. The File Provider appex stays
ad-hoc and no provisioning profile is embedded. Finder stays inert
(`no-group-container`), as with the ad-hoc default; use
`--sign-identity` with both profiles when a Finder-capable build is
needed. The two modes cannot be combined, and a profile passed with
`--stable-identity` is refused before any build. The installed app, CLI
and registrar are checked for a real signature, and a silent fallback
to ad-hoc fails the install.

Nothing here changes the keychain item ACL. Items keep default (NULL)
access: no per-application trust list and no biometric flags, so the
non-interactive owner can still read them from its launchd agent. The
stable signer only keeps macOS recognising the same client. A Personal
Team certificate expires; `codesign` refuses an expired identity with
the toolchain message, so renew it in Xcode and install again.

The shared helper `packaging/macos/sign-identity.sh` owns the ad-hoc
default and the appex identity split. `install-dev.sh` is its first
consumer; `stage-appex.sh` and `build-appex.sh` read the same values so
the signed artifact and the pairing check cannot drift.

### Capturing File Provider diagnostics without a profile

To capture local diagnostics from the extension, add `--sign-appex` to a
stable-identity install:

```sh
packaging/macos/install-dev.sh \
  --stable-identity "Apple Development: …" --sign-appex
```

`WYRD_SIGN_APPEX=1` in the environment is equivalent. This signs the appex
with the same identity as the app, CLI and registrar. No provisioning
profile is involved: the default group id is the unprovisioned macOS
form. Apple distinguishes two macOS App Group forms: a
`group.`-prefixed id needs a profile and portal registration, while a
`<team-id>.`-prefixed id needs neither — the system checks that the team
prefix matches the signing team. The default
`JU2RFHWQPH.group.md.enesis.wyrd` is the second form, so a team-signed
appex can resolve its container. The extension then writes detailed
records to `<container>/Diagnostics/records.jsonl` and a user-requested
export can carry them.

This mode may make Finder functional, unlike the plain stable-identity
mode. It is not a production signing path: the free-team identity expires
after seven days, and Developer ID distribution, notarization and
provisioned `group.` ids remain owner-gated work. The installer verifies
that all four Mach-O artifacts carry the requested identity and that no
profile is embedded. Nothing here changes entitlement content or keychain
ACLs. The shell's Finder note keys off profile presence, so it can still
appear in this mode; it is advisory, not a claim that the extension
cannot work.

After the bundle swap the installer stops every running Wyrd
process — supervisors first, then owners, then the app, registrar
and extension — so no process keeps the previous binary mapped.
The per-drive launch agents keep alive only on exit 0, so a SIGTERM'd
supervisor is not restarted by launchd: the installer explicitly
kickstarts each loaded `md.enesis.wyrd.drive-*` job afterwards.
Drives with login-start disabled are not loaded and stay stopped.
This avoids the stale-binary dispatch the diagnostics rollout hit
without leaving the machine without owners. `--no-stop` opts out
when an in-place rebuild is preferred.

The script builds through `packaging/macos/build-bundle.sh`
(link-order pin plus no-Nix-links guard; the wrapper's app-only
default is kept unless `--bundles` is forwarded), verifies
`codesign --verify --deep --strict` on the built and staged copies
before the old app is touched (fail closed), copies with `ditto`
to a staging name and swaps into `/Applications/Wyrd.app` when
writable else `~/Applications/Wyrd.app` (`--dest`/`DEST`
overrides, `--no-open` skips launch, `--` forwards e.g.
`-- --target` to the wrapper), re-verifies the installed copy, and
launches the app. The bundler hook stages the appex plus the
version-matched `wyrd` CLI and its `wyrd-supervise` sibling into
`Contents/Resources` before
the outer sign — the desktop resolves the bundled copy first (the
`MacOS` sibling is the devenv fallback only) and version-checks it
before every lifecycle run — so the UI, owner, registry, and service
work ad-hoc: create/join/leave, status, settings, and managed
service reinstall (the daemon resolves the wrapper as the CLI's
sibling; a bundle without it fails managed operations at
validate). No macFUSE is
needed: the ad-hoc CLI builds vault-only (`WYRD_NO_MOUNT=1`
drops the mount stack, whose macFUSE load dependency a clean Mac
has no copy of); `mount` mode and Finder stay with the
team-signed path below, and the release bundle keeps the full
mount-enabled CLI by leaving the flag unset.

Finder boundary (honest, not a bug): the ad-hoc default logs
`no-group-container` and stays inert — the App Group container only
materializes under a team-signed build with provisioning. Finder can
show "Your device couldn't connect to the server" for a drive whose
owner is running. The shell names the real cause in Settings →
Diagnostics: "Finder integration requires a signed build", with the
signed-install recovery and the fact that a reinstall restarts the
drive agents. The same build fact travels in an explicit diagnostics
export as `finder-state`, distinct from an extension record's
`store-error = temporary` (an unreachable owner socket). With the
signing pair above the app is Finder-capable: the embedded domain
registrar (`domain-registrar/main.swift`) is run by a per-user launch
agent (`md.enesis.wyrd.domain-registrar`, installed by `install-dev.sh`)
as one resident process that reconciles every 5 seconds. A fresh process
per tick would make macOS re-ask the "access data from other apps"
permission on every launch, so the agent keeps one helper alive and
restarts it only if it dies. It needs no running Wyrd.app process.
It polls the App Group's `Sockets/by-drive/` aliases the owner publishes
and reconciles one
`NSFileProviderDomain` per drive, using the 64-char drive id as the
domain identifier the appex expects. The owner publishes the registry
name in a `.label` file beside each alias; the Finder domain uses that
name verbatim. Only a live owner publishes a label, so a locked or
stopped drive has no domain: unlock starts an owner and the domain
appears, lock tears it down and the domain is removed within two poll
ticks. The desktop's lock path uses SIGKILL, so the registrar also
probes the socket endpoint: a stale alias from a killed owner is
ignored. One failed probe is held for a tick — a daemon restart, a full
accept backlog, or the gap between unbinding a stale socket and
rebinding is not proof the drive is gone, and removing a domain can
discard its local materialized state. The domain is removed only after
two consecutive ticks with no live owner; the streak lives in the
resident helper, not in the group container. The helper is
signed with the app's bundle identifier so registration associates
with the containing app; no Cargo dependency or unsafe FFI bridge is
added. A drive appears within the polling interval after its owner binds.
Distribution signing, notarization, and the release rehearsal stay
with the signing owner, never this script.

## Protocol (Stage 0 finding)

The extension implements `NSFileProviderReplicatedExtension`
(one instance per domain, hence per drive). The legacy
`NSFileProviderExtension` class API is unavailable on macOS in
current SDKs — observed while building (an early stub against the
class failed to compile against the macOS SDK).
The report's V1 vocabulary maps as: `startProvidingItem` →
`fetchContents`, with `request: NSFileProviderRequest` threading
through enumeration, lookup, hydration, and mutation. Recorded on
the issue thread (`nostr:nevent1qqsqz3tuhx8vvucgzrdtfc0y49sfs0mc3v7xratvxhg999plxp97e0gpz9mhxue69uhkwunpwdczuap49eehgvak3m0`).

## Sandbox and socket scheme (implemented, registry-owned)

Status: Implemented — the normative transport covers both sockets
(`docs/contracts/owner-interface.md`, Transport): one socket at
`<drive_dir>/owner.sock` (the sandbox boundary, unchanged), plus
the macOS-additive group socket and alias for the File Provider
extension. Production is registry-owned and automatic (container
from the platform convention, drive from the served drive, no env
vars); the env bootstrap (explicit container plus drive) remains
for harness and manual development only.

The extension runs sandboxed inside `fileproviderd`. A sandboxed
process reaches its container, its App Group containers, and
user-intent paths — nothing else. Connecting to a Unix socket
requires filesystem access to the socket node, so the owner socket
at its drive-derived location is unreachable from the extension:
App Group alignment does not make arbitrary paths accessible.

The scheme, therefore:

- On macOS the owner serves each drive's interface socket **also**
  inside the App Group container:
  `~/Library/Group Containers/<group>/Sockets/<fingerprint>.sock`,
  plus a `by-drive/<drive-hex>.sock` symlink alias beside it (see
  the Stage 2 design contract below for the fingerprint, the
  alias, and the dev-only env bootstrap). One socket per drive.
  The daemon
  is unsandboxed and binds there; the extension holds the group
  entitlement, readlinks the alias, and connects to the sibling
  target. In production the daemon derives the container from the
  platform convention (verified for development signing only;
  Developer ID and notarized paths are unverified) and the drive
  from the served drive itself — the daemon half of the
  registry-owned association. The managed owner service enables at
  login (`RunAtLoad`); the full Finder observable (domain
  registered from the registry, clean machine shows drives) stays
  pending the signing-owner verification below, not claimed here.
  `<group>` is the `WYRD_APP_GROUP` value (default
  `JU2RFHWQPH.group.md.enesis.wyrd`; the historical observation
  used the then-current unprefixed id — see Bundle identities above
  for the qualification). Stage 1 probed the container directory form
  with the stub name `wyrd-probe.sock` under a team-signed build
  and recorded the team and build type; the per-drive filename
  form is decided in the Stage 2 design contract below, which
  also live-confirms the daemon's computed bind path against the
  extension-resolved one.
- `socket_path_for` stays the cross-platform default; the group
  container socket is a macOS-additive second listener, never a
  replacement. Linux behavior is unchanged.
- Stale sockets: unlink-before-bind on the drive-directory
  socket; on the group socket a live endpoint fails the bind
  closed instead of being unlinked. Connect failure surfaces
  `Unavailable`, the extension reconnects on demand and never
  assumes a persistent connection.
- `<drive>` is the stable per-drive fingerprint from the Stage 2
  design contract below, coordinated with the multi-drive registry
  — the registry owns the
  drive-to-domain association in production (registration writes
  the entry; the domain resolves from the registry).

This analysis is the Stage 0 confirmation. The live proof comes
in two halves. Stage 1's entry gate, before any adapter logic,
proves the sandbox premise against a stub listener: the sandboxed
extension, running under its real entitlements, connects to a
listening socket at the production group-container path, and the
outcome lands in the Console log. No owner is involved, so owner
restart, stale-socket handling, and distinct multi-drive sockets
stay with Stage 2, beside the real client and the daemon-side
bind. If the Stage 1 probe fails, the transport boundary is
revisited; nothing in Stage 1 depends on it beyond the probe.

## Stage 1 live run (manual, macOS only)

The repo holds the canonical Swift sources; the local throwaway
Xcode project references them (never copies) and provides the
team-signed containing app plus domain registration. Order
matters: the container does not exist until the system creates it,
so the app runs before the listener. Procedure:

1. Build and run the containing app from Xcode with development
   signing on your team, add one domain.
2. Read the resolved socket path from the probe log
   (`md.enesis.wyrd.fileprovider`, category `domain`): the first
   pass logs `unreachable <path>: …`, and `<path>` is the
   resolved location to listen on. If the log says `no group
   container for <group>` instead, the entitlement is
   not effective in the running build (ad-hoc signing, missing
   provisioning) — fix signing and rerun; there is no path to
   listen on yet.
3. Start a stub listener at exactly that path — `mkdir -p`
   only the `Sockets` directory inside the resolved container,
   never the container itself (that would create a decoy the
   extension cannot reach) — then force the extension to
   re-instantiate: quit and relaunch the containing app.
   Reopening the domain alone may not re-run a surviving
   extension process, in which case no second probe line appears.

   ```sh
   p='<path from the probe log>'
   mkdir -p "$(dirname "$p")"
   rm -f "$p"
   python3 - "$p" <<'EOF'
   import socket, sys
   s = socket.socket(socket.AF_UNIX)
   s.bind(sys.argv[1])
   s.listen(1)
   print('listening', sys.argv[1], flush=True)
   s.accept()
   EOF
   ```
4. Open the domain in Finder: `hello.txt` and `notes/` enumerate
   from canned data; opening `hello.txt` hydrates canned bytes.
5. Read the probe outcome in Console: `reachable: …` passes the
   gate. (`listener busy` means the stub exists but cannot take
   the connection — restart the listener, not the theory.)
6. Record the macOS version and signing identity with the result.

Recorded run 2026-11-11 (this procedure, first live pass, with the
then-current unprefixed group id): macOS 15.8.2 (24H232), an Apple
Development identity on a development team (identifiers withheld).
Probe logged `reachable:
…/Group Containers/group.md.enesis.wyrd/Sockets/wyrd-probe.sock`
(persisted, retrievable via `log show`); a later pass with no
listener logged `unreachable …: Connection refused` at `error`.
Finder enumerated `hello.txt` and `notes/` from canned data.
Run notes: the extension launches lazily on first Finder access
of the domain (no probe line until then); if `fileproviderd`
wedges after a failed first launch (observed once as
`dead-on-arrival` followed by no relaunch), remove and re-add the
domain from the containing app to reset the backend binding.

Post-fix hydration observed 2026-11-11 (same machine, identity,
and build line): after `documentSize`, a freshly re-added domain
lists `hello.txt` at 26 bytes and opening it yields the canned
text. Debugging note: Xcode tracks the symlink's mtime, not the
target's, so editing repo sources alone does not trigger a
rebuild — Clean Build Folder first, and re-point the local
project's symlinks after any branch move (an entire evening of
0-byte reads traced to the local project compiling `master`
sources without the fix).

One domain for the spike; per-drive domains and isolation arrive
with Stage 3, under the invariants above.

## Multi-drive invariants (established now)

- One File Provider domain per registered drive, domain identity
  stable and distinct.
- Every item identifier is scoped to its drive. The identifier
  contract is decided (Stage 2 design contract below):
  `wyrd:v1:<drive>:<identity>` for identity-bearing entries,
  `wyrd:legacy:v1:` for rows without owner identity, no persistent
  identity for conflicts. "Drive-scoped" here states the scoping
  invariant only.
- Every socket connection targets the correct drive's owner.
- Removing one domain cannot affect another drive.
- No singleton state in the adapter: a single-drive prototype must
  not become the architecture.

## Stage 2 owner-client seam (decided)

The extension speaks the owner protocol (u32 big-endian length
prefix plus JSON, `docs/contracts/owner-interface.md:44-53`) from
Swift, directly against the normative wire contract, with golden
fixtures shared across Rust and Swift (`crates/wyrd-owner-client/vectors.json`,
decoded by the Harness in `packaging/macos/fileprovider/Harness/`).
No helper binary: that would add an executable, packaging, and
lifecycle for a second owner-adjacent service. The cost is
duplicated framing/JSON code, kept honest by the shared fixtures.
The import allow-list is unchanged — the client needs only
`Foundation` (JSON), `Darwin` (sockets), and the platform
frameworks already allowed — so the boundary rule still pins the
import surface only.

Read-only: lookup, stat, readdir, read. Every operation opens a
fresh connection (reconnect on demand is structural), and every
refusal maps to exactly one of three outcomes — found,
deterministic sync failure, temporary unavailability — never a
collapsed boolean, never a guess.

## Stage 2 design contract (agreed)

- **Identifiers:** `wyrd:v1:<drive>:<identity>` (both 64-char
  lowercase hex), reversible, drive-scoped, validated on parse.
  Cross-drive identifiers are rejected; a zero identity never
  encodes `None`. Legacy and merged-disagreement rows use
  `wyrd:legacy:v1:<hex-path>`: unstable by contract (rename means
  a new item), never written back. Conflicts carry no persistent
  identity and never enumerate.
- **Legacy retirement:** a legacy reference names the identity-less
  entry at its path. Minting an identity retires it: the item,
  contents, enumeration, and create-parent surfaces all 404
  instead of serving the replacement under the old identifier
  (the same rule stable resolution enforces by token match).
  Still-identity-less paths resolve unchanged.
- **Versions and enumeration terminus:** entry versions are
  `wyrd-cv3`: the size in the clear plus a fixed 16-hex digest of
  the `token:content` tail. The raw tail (chunk lists, subtree
  listings) is unbounded and would breach the 128-byte
  item-version bound, so the digest keeps every served version
  within it; the Harness pins the bound including a synthetic
  long tail. `versionFacts` parses `cv1`/`cv2`/`cv3`, normalizing
  a cached `cv2` tail through the same digest so the first
  post-upgrade modify still matches; a cached `cv1` base still
  mismatches once and heals on the next enumeration. Enumerations
  finish with a
  nil page — an anchor-data page fails the fetch persist —
  while the anchor stays live through `currentSyncAnchor`.
- **Anchors:** `wyrd-anchor-v1:g<generation>:r<revision>`,
  translated from the owner's serving coordinate. Enumeration is
  one `readdir` in one coordinate; the returned anchor always
  describes the completed enumeration.
- **Consistency:** identifiers resolve through an
  identifier-to-path map built during enumeration and verified on
  use (the owner's token must agree). A miss re-resolves with a
  bounded walk (4096 entries, mirroring the policy-status
  budget) that classifies explicitly: found, conflicted (fail
  closed), absent, or budget-exceeded (fail closed).
- **Drive fingerprint:** the group socket filename derives from
  `DriveId` via `blake3::derive_key("wyrd-fileprovider-socket-v1",
  drive)` truncated to 64 bits (`wyrd_format::DriveId::fileprovider_socket_fingerprint`,
  golden-pinned). 64 bits — not the decision's 160-bit example —
  because the filename must fit `sun_path` (103 usable bytes on
  macOS) under a real home directory; a handful of drives collide
  with probability ~k²/2⁶⁵, and any collision fails closed at
  bind. The daemon is the only computer of the hash; the
  extension resolves the `by-drive/<drive-hex>.sock` symlink the
  daemon publishes beside the socket (readlinked, then connected
  through the sibling target, which is the only path that must
  fit `sun_path`).
- **Daemon bind:** all-or-nothing — the drive-directory socket and
  the group socket serve from one `OwnerServer` (one
  owner-instance identity, one uid check, one shared pool of at
  most 16 connections across both accept loops). The composer
  entry point binds the group socket for the served drive in
  production (container from the platform convention, drive from
  the served drive — both `wyrd owner` and `wyrd mount`); a live
  endpoint at the group path is never unlinked (`AddrInUse`,
  primary unbound, run refused) while a dead entry reclaims. The
  dev-only env pair (`WYRD_FILEPROVIDER_GROUP_CONTAINER` plus
  `WYRD_FILEPROVIDER_DRIVE_ID`, both or neither; malformed
  refuses, never a silent primary-only fallback) overrides for
  harness and manual development, with the configured drive
  checked against the served drive before anything binds. The
  daemon half is the agreed production design (registration writes
  the entry; the managed service enables at login). The domain half
  is the bundled registrar described above: it derives domains from
  the published aliases, not from a second registry mapping.
- **Errors:** `NotFound` → item-not-found; `Conflicted`,
  `TooLarge`, `Timeout` (after one retry), protocol violations →
  deterministic sync failure; `Unavailable` and transport breaks
  → temporary unavailability. A timeout is never proof an item
  disappeared.
- **Residual window (documented):** the item surface is built
  from the verified node itself, but hydration reads by path
  after verification — a delete-plus-recreate between the verify
  lookup and the read can return the replacement's bytes under
  the old identifier and version. Identity-bound reads (the
  owner answers by token, not by path) close it; planned for
  Stage 3.
- **Symlinks** enumerate under their name but hydration fails
  closed; their presentation is a Stage 3 question.
- **Mutations** stay `cannotSynchronize` until Stage 3, including
  parent capture and submit — tempting through the File Provider
  lifecycle, explicitly out of scope.

## Stage 2 live proof (manual, macOS only)

Observed 2026-11-12 on the development machine (same team
identity line as Stage 1): a fresh `wyrd drive … init` drive served by
`wyrd owner` with the dev bootstrap
(`WYRD_FILEPROVIDER_GROUP_CONTAINER` plus
`WYRD_FILEPROVIDER_DRIVE_ID`, both set).

- The daemon logged the triple it bound:
  `primary=…/owner.sock`,
  `group=…/Group Containers/<group>/Sockets/add3245c6d69f1dd.sock`,
  `link=…/Sockets/by-drive/<drive-hex>.sock`.
- A client through the readlinked alias served `readdir ""`
  (empty fresh drive, generation 0, revision 2), `stat ""`
  (`Dir`, legacy-shaped root with no token), and refused an
  unknown path with `NotFound` — the same shapes the Harness
  checks against the stub.
- Owner restart: stopping and re-running the vault rebound the
  identical alias path (the fingerprint is deterministic) and
  served immediately.
- Shutdown removed all three paths (primary, group socket,
  alias); the stale Stage 1 `wyrd-probe.sock` beside them is
  untouched and unrelated.
- No macFUSE on this machine, so the drive stayed empty: content
  flows through the same tested dispatch (daemon unit tests with
  seeded files, Harness round-trip with content), not through
  this run.

Remaining manual step (in-Finder): repoint the throwaway
containing app at the current `Sources/` (eight new files join
the four Stage 1 symlinks), register the domain with the drive
id as its identifier, and run the Stage 1 Finder procedure
against the live owner — enumerate, hydrate, rename-preserve,
delete/recreate, peer rename, legacy rows, merged disagreement,
and `fileproviderd` recovery.

Ad-hoc `fileproviderd` load check (recorded 2026-11-18, macOS
15.8.3, this issue): an ad-hoc appex does not load, by mechanism,
not by policy. `build-appex.sh` with the default ad-hoc identity
produces a verifying-clean bundle (`codesign --verify --deep
--strict`), identifier `md.enesis.wyrd.fileprovider`, extension
point `com.apple.fileprovider-nonui` — with the development
entitlements passed at sign time but not effective without a
provisioning profile, so the App Group buys no container. `pluginkit -a` accepts the
registration (exit 0) yet the record stays unusable (`!` in
`pluginkit -m -A`) and matches no fileprovider endpoint, so
`fileproviderd` never instantiates it; the system log shows no
contact with the extension id. No containing app exists on the
machine to add a domain, and per the Stage 1 rule an ad-hoc build
has no effective group-container entitlement, so there is no path
to listen on. Team-signed deltas for the distribution run: a
Developer ID identity for Gatekeeper trust, a provisioning
profile carrying the App Group (`WYRD_APP_GROUP`, default
`JU2RFHWQPH.group.md.enesis.wyrd`), a
containing app to register the domain, and notarization — then
the Stage 1 procedure above applies. The notarized
`fileproviderd` load check itself stays with the signing owner.

## Registry-owned switchover (production daemon half)

The dev bootstrap above (explicit container plus drive) stays for
harness and manual development only. The daemon half is
production with no env vars: the owner derives the container from
the platform convention and the drive from the served drive itself
(`OwnerService::bind_for_composer`), so both `wyrd owner` and
`wyrd mount` publish the `by-drive/<drive-hex>.sock` alias a
registry-registered domain resolves through. Registration writes the
registry entry; the managed owner service enables at login
(`RunAtLoad` in the per-drive launchd agent, `WantedBy` in the
systemd user unit — vault and mount share the template). The
container literal is verified for development signing only;
Developer ID and notarized paths are unverified. The
`NSFileProviderDomain` half rides the bundled registrar (see
"Developer install"): it registers a domain for each published
alias, so the full Finder association is a property of the
installed app rather than a manual step. Distribution signing and
the fresh-install denial run remain with the signing owner.

Acceptance (manual, macOS, recorded as release evidence): with the
env vars unset, a registered drive appears in Finder, survives
reboot (service re-enabled at login), and the main window shows it
Available once the owner answers. The containing app must run
before the service on first run (the daemon never creates the
container itself). The reboot-survival drill and
the no-env-vars Finder check are release evidence, owned with the
signing owner (team-signed verify, `spctl`, provisioning profile,
notarized `fileproviderd` load check stay with that owner).

## Stage 3 mutation callbacks (sub-PR 2)

Live create/modify/delete through the owner's single queue with
sub-PR 1 identity preconditions — no parallel commit path. The
conservative capability matrix: token-bearing files accept
write/rename/reparent/delete; token-bearing directories accept
the same (write doubles as adding children); symlinks, legacy
rows, and merged disagreements stay read-only; trash is never
advertised, delete is permanent, trash moves fail closed.

Behavioral rules the callbacks implement: create file as
capture-parent + CreateFile + CommitFile (empty files supported,
`.DS_Store` excluded via `excludedFromSync`); create dir as
`Mkdir`; `AlreadyExists` renders `filenameCollision` for the
system to rename; content changes are whole-file `CommitFile`
against the checked base with a baseVersion check over per-entry
facts (size, token, content digest — mismatch is
`cannotSynchronize`, never overwrite/merge/retry, including
same-size remote edits); renames carry the checked token with
`noReplace`; `StaleParent` recaptures and retries once;
unconfirmed submits reconcile by re-reading before any retry;
folds are never sent; contents past 32 MiB refuse client-side;
symlinks refuse closed. Atomicity is NOT declared (no
`NSExtensionFileProviderAppliesChangesAtomically` key):
create-then-commit exposes an intermediary. Working-set signal:
the replicated extension honors only `.workingSet`, so every
confirmed mutation signals the working set. Cancellation reports
`NSUserCancelledError` pre-submit; a commit that already reached
its terminal answer reports the answer. Remote change
notification stays out by OD-17-C decision A: an open Finder
window needs reopening to show a peer's write.

Harness proof: the stub grew a stateful mutation namespace
(`mut-*` paths plus a scripted one-shot `StaleParent` fixture),
and the driver exercises the full write round-trip —
capabilities, mkdir, create, modify, rename-preserve,
collision, delete, recapture, oversize, legacy refusal,
baseVersion discipline — plus the usual TSan run.

## Stage 3 conflict inspector (sub-PR 3)

Read-only conflict surfacing under the six OD-17-B constraints
(parent-issue thread, final decision): one synthetic report file
per conflicted path, generated from existing owner reads, with no
new owner mutation operation and no new authoritative state in
Swift.

- **Mechanism:** the enumerator turns each conflict row into one
  `ConflictReportItem` (`<name>.wyrd-conflict.txt`, `.plainText`)
  instead of skipping it. The conflicted path itself never
  enumerates as content.
- **Identity:** `wyrd:conflict:v1:<drive>:<hex-path>` — keyed by
  the conflicted logical location plus drive. Never a peer
  identity, never an arbitrary head's token. The identifier names
  the location; invalidation rides the item version (a digest over
  the sorted per-version facts, so any version change moves it)
  and disappearance (a resolved path 404s on lookup and
  hydration).
- **Labels:** snapshot id plus head authoring timestamp
  (threaded from heads through merge to the wire with a
  `0`-default for skew tolerance, see the owner-interface read
  contract), in snapshot-id byte order for display only. The
  report states that no version is authoritative.
- **Capabilities:** `[.allowsReading]` only. Hydration serves the
  generated report text re-rendered live — never a version's
  bytes. No keep-both, no overwrite, no resolution. Delete,
  modify, rename, and create-under-report fail closed in
  `mutationTarget`/`containerPath` before any socket write; a
  submit aimed at the conflicted path itself refuses `Conflicted`
  at the owner, so neither head moves.
- **Gates:** a live two-head drive demo (two replicas, one entry
  edited on both sides while disconnected, reconnected,
  reconciled; the report appears in Finder and disappears after
  the owner-mediated resolution) plus Harness negative tests
  proving inspector operations cannot mutate either head.
  Orphan-state presentation stays deferred; resolution authoring
  stays v0.5.

Harness proof: the stub serves a two-version `split.txt`
conflict with distinct snapshots and timestamps and refuses
`Conflicted` on covered submits against it; the driver checks
enumeration, naming, grammar, round-trip text, invalidation,
version determinism, and the full negative set — plus the usual
TSan run.

## Shipment bundle layout (decided)

The File Provider extension ships embedded in the app bundle:

```text
Wyrd.app/
  Contents/
    MacOS/Wyrd
    MacOS/WyrdDomainRegistrar
    PlugIns/WyrdFileProvider.appex/
      Contents/
        MacOS/WyrdFileProvider
        Info.plist
    Resources/wyrd
    Resources/wyrd-supervise
```

The version-matched `wyrd` CLI rides in `Contents/Resources`
(the fixed co-installation the desktop resolves first via
`bundled_cli_path`; the `MacOS` sibling is the devenv fallback
only, never the bundle contract — on case-insensitive APFS a
`MacOS/wyrd` sibling would collide with the `Wyrd` app binary).
`MacOS/WyrdDomainRegistrar` is the Swift domain-registration helper
built by `domain-registrar/build-registrar.sh` and staged by
`stage-registrar.sh` through the same bundler mapping. It signs as
`md.enesis.wyrd` (the containing app) and calls
`NSFileProviderManager.add`/`remove` against the drive aliases.

Beside it rides `wyrd-supervise`, the supervised-owner wrapper
from `nix/drive/supervise.py` (also the flake's `wyrd-supervise`
package): the daemon resolves it as the CLI's sibling unless
`--supervise-bin` is explicit, so managed bootstrap and
reconfigure fail at validate without it. The staged copy is the
script with a `python3` shebang prepended (a managed service on
the target Mac needs a working `python3`, already required for
the install); it carries no Mach-O linkage, so the no-Nix-links
guard covers it by content (no `/nix/store`), not by `otool`.
`packaging/macos/stage-cli.sh` builds the release CLI with the SDK
link-order pin, stages it at `packaging/macos/stage/wyrd`, and
signs it with the paired identity; `bundle.macOS.files` copies it
before the outer sign, never as a post-sign copy. The desktop
version-checks the bundled CLI (`__build_info`) before every
lifecycle run and refuses skew as `VersionMismatch`, so the bundle
is the matched pair the provisioning adapter requires.

Rejected: beside the app (a standalone appex next to `Wyrd.app`,
or anywhere outside `Contents/PlugIns`). `fileproviderd` loads the
extension through its containing app, domains register from the app,
and nested signing plus notarization assume one versioned bundle.
A sidecar appex is not drag-installable, splits the update into two
replaceable parts, and invites app/extension version skew.

Trade-offs, in the issue's terms:

- **Update atomicity:** whole-bundle replacement. The `.dmg` carries
  one versioned `Wyrd.app`; the bundler hook pins the appex
  `CFBundleVersion` and `CFBundleShortVersionString` to the app
  version so all three move in lockstep (the checked-in plist still
  pins the placeholders; the hook owns the rewrite and fails closed
  on drift). No split
  replacement, no mixed-version support case. Domains persist across
  updates by stable drive-derived domain identity; `fileproviderd`
  re-registers the new appex on first launch.
- **Sandbox sharing:** the App Group is the only shared surface.
  Both app and appex hold the same `WYRD_APP_GROUP` value (team-prefixed
  default), rendered into their entitlements at build time; both
  resolve through `containerURL`, never a hardcoded path. The
  container naming is verified for development signing only;
  Developer ID distribution is unverified and covered by the
  planned `fileproviderd` load check below. The owner server
  publishes `Sockets/<fingerprint>.sock` plus the
  `Sockets/by-drive/<drive-hex>.sock` symlink alias; the daemon is
  the sole computer of the fingerprint and the extension readlinks
  the alias. Group container layout is unchanged from the Stage 2
  contract above.
- **Installer complexity:** one `.app` in the `.dmg`, one drag.
  `build.beforeBundleCommand` runs `packaging/macos/stage-appex.sh`
  plus `packaging/macos/stage-cli.sh`
  (macOS only; clean skips elsewhere), which build the appex and the
  release CLI, pin the appex
  `CFBundleVersion` and `CFBundleShortVersionString` to the
  app version from `tauri.conf.json`, map Tauri's target triple to
  the appex/CLI arch (native per-arch builds only), and stage them at
  `packaging/macos/stage/` (git-ignored, rebuilt every bundle).
  `bundle.macOS.files` copies the staged appex into
  `Contents/PlugIns` and the staged CLI into `Contents/Resources`
  at bundle time; the bundler then signs the
  outer app shallowly — never `--deep`, which would re-sign the
  nested appex without its entitlements — preserving the appex's own
  hardened-runtime signature (the nested-first order under one
  Developer ID). No extra install step, no post-install copy. Hook
  paths assume the CLI runs from `crates/wyrd-desktop` (the same
  convention as `frontendDist`).

Consequences: the production app carries no entitlements file
(decided below); the appex provisioning profile carries the App
Group capability; notarization staples the whole `.dmg`. The
throwaway Xcode app stays throwaway: it never becomes the ship
vehicle.

Follow-on work, scoped not done: the remaining packaging chain —
the notarized `fileproviderd` load check, update
persistence (install old, install new, domains and sockets
survive). The per-section lists below decompose it, not duplicate
it.

## Production entitlements and provisioning (reopened and amended)

The earlier decision was that the app holds no entitlements. The
reasoning stands on its own terms and is preserved here:

- **Previous app decision (superseded):** the shipped desktop stays
  unsandboxed under Developer ID and uses direct paths: the registry
  defaults to `~/Library/Application Support/Wyrd`, owner sockets
  live beside their drive directories, and provisioning spawns the
  co-installed CLI to write `~/Library/LaunchAgents`. Sandboxing the
  app would break discovery, connection, and provisioning. With no
  sandbox and no constrained capabilities there was nothing to
  entitle or provision, and the entitled `containerURL` API was
  thought to belong to the appex alone.
- **Why it was reopened:** that last premise is false for the
  Finder lifecycle. The App Group container is created when an
  entitled, provisioned process calls `containerURL`. The appex has
  the entitlement but only runs once a domain exists; the app had no
  entitlement, so on a fresh machine nothing created the container,
  the owner published no group socket, and no domain could be
  registered by the bundled reconciler. A one-time manual bootstrap
  can hide this in development but cannot be a production step.

### Decisions (this PR)

1. **Wyrd.app joins the configured App Group** (default
   `JU2RFHWQPH.group.md.enesis.wyrd`). It stays unsandboxed;
   the only new key is `com.apple.security.application-groups`. The
   direct-path registry, socket, and provisioning design is unchanged.
2. **The app initializes the App Group container at launch.** The
   bundled registrar calls `containerURL` on its first reconciliation
   tick, which materializes the container before drive creation
   starts the owner. This is normal initialization, not a user step.
3. **The File Provider extension stays independently provisioned**
   with its own App ID (`md.enesis.wyrd.fileprovider`) and unchanged
   sandbox boundary.
4. **The owner remains the only publisher of socket endpoints.** The
   app and registrar never manufacture aliases or impersonate the
   owner. When the container appears after the owner already started,
   the owner's retry path binds the group socket and alias from the
   same `OwnerServer` (no second primary bind); the registrar only
   observes and reconciles domains.
5. **Domain registration is reconciliatory and idempotent.** The
   registrar compares desired domains (published aliases) with
   registered domains, adds missing ones, removes stale ones, and
   treats `NSFileWriteFileExistsError` as a benign race. Repeated
   runs are harmless.
6. **A manual development bootstrap is permitted but is not a
   production dependency.** The acceptance test is a fresh macOS
   user with no container and no domains reaching a working Finder
   drive through ordinary launch and drive creation. That test also
   confirms macOS accepts domain registration from the embedded
   helper signed as the containing app; if it does not, the bridge
   must move into the app process (a Tauri plugin or a narrowly
   scoped FFI allowance) before this shape is called production.

**Security invariant:** the App Group is a **communication boundary**,
not drive storage. It carries only the owner's IPC socket and its
alias. No drive contents, manifests, encryption keys, or secrets go
into the container, and joining the group grants the app no
authority to author drive state — mutation still flows through the
owner's single queue.

- **App** (`packaging/macos/Wyrd.app.entitlements`): only the App
  Group; no sandbox key. Tauri embeds it via
  `bundle.macOS.entitlements`; the domain registrar signs with the
  same entitlements and identifier (`md.enesis.wyrd`).
- **Appex** (`fileprovider/WyrdFileProvider.entitlements`):
  unchanged, sandbox plus the same literal group.

**Provisioning wiring.** Profiles live outside the repository with
the release owner, never in the tree. `PROVISIONING_PROFILE` names
the appex profile; `APP_PROVISIONING_PROFILE` names the app profile
for App ID `md.enesis.wyrd`, embedded at
`Contents/embedded.provisionprofile` before the outer reseal. A real
identity without both profiles refuses before any build. Team
signing pairs `SIGN_IDENTITY` with the same Developer ID Tauri uses
(`bundle.macOS.signingIdentity` / `APPLE_SIGNING_IDENTITY`); signing
stays nested-first, one Developer ID, `--options runtime`.

**Free-team development note.** App Groups on macOS are an
unrestricted entitlement in Apple's docs, and a team-prefixed group
id (`<TEAM_ID>.…`) can be used without registering the group. That
allows a free Personal Team to exercise the real container
lifecycle, which is why the default is now team-prefixed
(`JU2RFHWQPH.group.md.enesis.wyrd`). Personal Team profiles expire
after seven days. `WYRD_APP_GROUP` changes the id in one setting
(the build renders the entitlements, Info.plist and registrar
agent; Rust reads it at runtime) if a different team signs a
build. Distribution still needs Developer ID and notarization.

**Ordering is behavior, not documentation.** Owner-first falls out
of the per-drive launchd agent: `RunAtLoad` starts the owner at
login and `KeepAlive`/`SuccessfulExit` restarts unexpected death
but never the terminal held-drive exit (`render_launchd_plist`,
pinned in the bootstrap tests) — the UI opens whenever and reads
`Unavailable` until the socket appears (the backend's absent-socket
outcome, never a guess). File-Provider-before-UI falls out of
`fileproviderd`: the appex loads on domain access independent of
the UI, and the UI never fronts file bytes (presentation adapters
around the owner, never the product contract).

**Verification.** `codesign --verify --deep --strict` clean on both
targets, plus `spctl -a -vvv -t install` on the app, is the gate for
any team-signed build. The fresh-install denial check — install,
create, see in Finder, add a device, watch `fileproviderd` for
unexpected extension denials on the group container (the
unsandboxed app itself is denial-free by construction) — is manual
on a real identity and stays with the signing owner. What the hook
evidences on this machine: ad-hoc `stage-appex.sh` run — staged
appex version keys equal the app version,
`codesign --verify --deep --strict` clean, no profile embedded by
default — plus a debug `cargo tauri build --bundles app`: the hook
restaged through `beforeBundleCommand`, the appex landed at
`Contents/PlugIns` bit-identical to the staged tree (including its
signature — the preservation proof, not the verify pass), and the
outer app verifies `codesign --verify --deep --strict` clean after a
shallow sign. Staged and bundle artifacts are untracked manual
context, not revision-bound evidence — and the guard below.

Follow-on work, scoped not done: the appex provisioning profile from the
Developer portal with the App Group capability; the notarized
`fileproviderd` load check; the fresh-install denial run.

## Release distinction

- Unsigned/ad-hoc integration: development and manual testing only.
- Distributable macOS integration: Developer ID signing,
  notarization, provisioning, production App Group. A compiled appex
  is not a shipped integration.
- If macOS cannot ship signed, the cut is Linux-only with FUSE —
  explicit, never a macOS target implying support.

## Login items and recovery (decided)

After install, reboot brings both back, owner first: per-drive owner
services start at login (`RunAtLoad` in the launchd agent,
`WantedBy=default.target` in the systemd unit), and the desktop UI
starts once per login from its own login item. The UI never waits on
the manager for ordering — a drive whose owner is not answering yet
reads unavailable, and the owner appearing needs no restart: the view resumes on the
explicit Retry. The full login-item decision (launchd agent vs the
Apple login-item API, no manager ordering) is recorded in
`docs/design/desktop-shell.md`, open-questions item 7.

Install (ad-hoc rehearsal; the real installer owns this step later —
fill the token with the installed location first):

```sh
# macOS: fill @WYRD_APP_PATH@, load once (copies are never hand-edited live).
sed "s|@WYRD_APP_PATH@|$HOME/Applications/Wyrd.app|" \
  packaging/macos/launchd/md.enesis.wyrd.desktop.plist \
  > ~/Library/LaunchAgents/md.enesis.wyrd.desktop.plist
launchctl load -w ~/Library/LaunchAgents/md.enesis.wyrd.desktop.plist
```

```sh
# Linux: fill @WYRD_DESKTOP_BIN@, reload, enable for login.
sed "s|@WYRD_DESKTOP_BIN@|$HOME/.local/bin/wyrd-desktop|" \
  packaging/linux/systemd/wyrd-desktop.service \
  > ~/.config/systemd/user/wyrd-desktop.service
systemctl --user daemon-reload
systemctl --user enable wyrd-desktop.service
```

Non-Nix Linux needs the supervise wrapper too: Nix systems get it
from the drive module, but a hand-installed `wyrd` has no
`wyrd-supervise` sibling, and managed bootstrap/reconfigure fail
at validate without it — the same gate as the macOS bundle above.
Place an executable `wyrd-supervise` beside the `wyrd` binary
itself (the daemon's sibling lookup checks the CLI's own
directory, not `PATH`, so `~/.local/bin` only works when `wyrd`
lives there too): either `nix build .#wyrd-supervise` and copy the
output `bin/wyrd-supervise` next to `wyrd`, or shim the repo
script —

```sh
BIN_DIR="$(dirname "$(realpath "$(command -v wyrd)" 2>/dev/null || command -v wyrd)")"
printf '#!/bin/sh\nexec python3 /path/to/repo/nix/drive/supervise.py "$@"\n' \
  > "$BIN_DIR/wyrd-supervise"
chmod +x "$BIN_DIR/wyrd-supervise"
```

(An explicit `--supervise-bin` on the reconfigure/bootstrap
invocation overrides the sibling lookup wherever the layout
differs.)

Stop, start, disable — per drive on macOS, then the Linux twin:

```sh
launchctl list md.enesis.wyrd.drive-alpha   # loaded? (non-zero: stopped)
launchctl unload ~/Library/LaunchAgents/md.enesis.wyrd.drive-alpha.plist  # stop
launchctl load -w ~/Library/LaunchAgents/md.enesis.wyrd.drive-alpha.plist # start
launchctl unload -w ~/Library/LaunchAgents/md.enesis.wyrd.drive-alpha.plist
rm ~/Library/LaunchAgents/md.enesis.wyrd.drive-alpha.plist                 # disable
```

```sh
systemctl --user is-active wyrd-drive-alpha.service
systemctl --user stop wyrd-drive-alpha.service
systemctl --user restart wyrd-drive-alpha.service
systemctl --user disable --now wyrd-drive-alpha.service  # stop plus no login start
```

The UI login item stops the same way (`launchctl unload [-w]
~/Library/LaunchAgents/md.enesis.wyrd.desktop.plist`,
`systemctl --user disable --now wyrd-desktop.service`); stopping the
UI never touches an owner, and quitting the UI never stops one —
the OS manager is the parent of both (OD-06-E).

Recovery, per unavailable cause (the desktop shows all three, never
one toast): `NotInstalled` (no definition file) reinstalls from the
view's Reinstall, which rewrites the entry and reinstalls, enables,
and restarts; `Stopped` (definition present, unit not loaded) starts
from the view's Start, which enables and starts the existing
definition — a stopped drive is stable, never auto-started;
`Wedged` (manager reports running, socket absent) restarts the
service and reads the drive's service log (`service.log` in the
drive directory on macOS, the user journal on Linux), never the
drive contents; `Unknown` (manager did not answer) retries the
probe, nothing else; `Unsupported` (no manager tool in the build)
starts the owner manually and never ships. Disabling stops without
touching drive data; only an explicit disable is a deliberate stop,
never a crash the manager retries.
