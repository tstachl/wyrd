# macOS packaging (`packaging/macos/`)

Status: Implemented behind an explicit dev bootstrap. This note
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

Bundle identities: app `md.enesis.wyrd` (from `tauri.conf.json`),
extension `md.enesis.wyrd.fileprovider`, group
`group.md.enesis.wyrd`. Observed 2026-11-11 under development
signing on macOS 15.8.2: the system materialised the container at
the literal group id,
`~/Library/Group Containers/group.md.enesis.wyrd/`, with no team
prefix — confirmed by the probe log and `fileproviderd`'s own
dump. Treat that as this build's resolved path, not a platform
law: it is verified for development signing only, and other
signing types (Developer ID, notarised distribution) are
unverified. (Prefixed directories on this machine, e.g.
`W5364U7YZB.group.io.tailscale.ipn.macsys`, read most plausibly
as groups whose ids literally contain the team prefix — an old
naming convention — but that reading is inferred, not verified.)
Accordingly: the extension resolves through `containerURL` and
never hardcodes, and Stage 2 must confirm the daemon-side bind
lands where the extension resolves before the scheme settles —
an unsandboxed daemon without the group entitlement cannot call
`containerURL`, so whatever rule it uses to compute the directory
needs the same live confirmation this build gave the extension
side. The container materializes only under a team-signed build
with provisioning. Deployment target macOS 13.0.

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

## Sandbox and socket scheme (implemented, dev bootstrap)

Status: Implemented — the normative transport covers both sockets
(`docs/contracts/owner-interface.md`, Transport): one socket at
`<drive_dir>/owner.sock` (the sandbox boundary, unchanged), plus
the macOS-additive group socket and alias for the File Provider
extension. The dev bootstrap (explicit container plus drive) is
the only configuration; the registry owns the production
association.

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
  alias, and the dev bootstrap). One socket per drive. The daemon
  is unsandboxed and binds there; the extension holds the group
  entitlement, readlinks the alias, and connects to the sibling
  target.
  `<group>` is the literal `group.md.enesis.wyrd` as observed on
  the development build (see Bundle identities above for the
  qualification). Stage 1 probed the container directory form
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
  (in progress elsewhere) — the registry owns the
  drive-to-domain association in production.

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

1. Build and run the containing app from Xcode (team
   `JU2RFHWQPH`, development signing), add one domain.
2. Read the resolved socket path from the probe log
   (`md.enesis.wyrd.fileprovider`, category `domain`): the first
   pass logs `unreachable <path>: …`, and `<path>` is the
   resolved location to listen on. If the log says `no group
   container for group.md.enesis.wyrd` instead, the entitlement is
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

Recorded run 2026-11-11 (this procedure, first live pass): macOS
15.8.2 (24H232), `Apple Development: Thomas Stachl (XZQVCF66WX)`
team `JU2RFHWQPH`, development signing. Probe logged `reachable:
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
  entry point checks the configured drive against the served
  drive before anything binds (both `wyrd vault` and `wyrd
  mount`); a mismatch refuses the run. A live endpoint at the
  group path is never unlinked (`AddrInUse`, primary unbound,
  run refused); a dead entry reclaims. Dev bootstrap only, via
  `WYRD_FILEPROVIDER_GROUP_CONTAINER` plus
  `WYRD_FILEPROVIDER_DRIVE_ID` (both or neither; malformed
  refuses, never a silent primary-only fallback). The registry
  owns this association in production.
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
identity line as Stage 1): a fresh `wyrd init` drive served by
`wyrd vault` with the dev bootstrap
(`WYRD_FILEPROVIDER_GROUP_CONTAINER` plus
`WYRD_FILEPROVIDER_DRIVE_ID`, both set).

- The daemon logged the triple it bound:
  `primary=…/owner.sock`,
  `group=…/Group Containers/group.md.enesis.wyrd/Sockets/add3245c6d69f1dd.sock`,
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

## Release distinction

- Unsigned/ad-hoc integration: development and manual testing only.
- Distributable macOS integration: Developer ID signing,
  notarization, provisioning, production App Group. A compiled appex
  is not a shipped integration.
- If macOS cannot ship signed, the cut is Linux-only with FUSE —
  explicit, never a macOS target implying support.
