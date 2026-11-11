# macOS packaging (`packaging/macos/`)

Status: Proposed. This note holds the adapter layout and the
sandbox/socket scheme under open decision; on any conflict the
normative contracts win — the drive-owner interface
(`docs/contracts/owner-interface.md`) for transport and the File
Provider issue for OD-17-A/B/C and the OD-8 outcome.

Platform packaging outside Cargo and Nix: the Swift File Provider
adapter and its build script. Nothing here is a Cargo workspace
member (a member must be a Rust crate); the adapter's boundary is
enforced by the `fileprovider_swift_stays_within_the_owner_boundary`
contract instead of a `MemberPolicy` allow-list — the OD-8
refinement, proposed on the issue thread and awaiting decision.
See the File Provider issue for the OD-17-A/B/C
decisions (`nostr:nevent1qqsqz3tuhx8vvucgzrdtfc0y49sfs0mc3v7xratvxhg999plxp97e0gpz9mhxue69uhkwunpwdczuap49eehgvak3m0`); identity and conflict-surface decisions are a hard gate
before Stage 2, and Stage 0/1 proceed without them.

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

## Sandbox and socket scheme (Stage 0 proposal)

Status: Proposed — the normative transport is one socket at
`<drive_dir>/owner.sock`, and that path is the sandbox boundary
(`docs/contracts/owner-interface.md:14-19,41`). What follows is the
scheme Stage 1 will probe, not a settled change; if the probe or
the contract review rejects it, this section changes, not the
contract.

The extension runs sandboxed inside `fileproviderd`. A sandboxed
process reaches its container, its App Group containers, and
user-intent paths — nothing else. Connecting to a Unix socket
requires filesystem access to the socket node, so the owner socket
at its drive-derived location is unreachable from the extension:
App Group alignment does not make arbitrary paths accessible.

The proposed scheme, therefore:

- On macOS the owner serves each drive's interface socket **also**
  inside the App Group container:
  `~/Library/Group Containers/<group>/Sockets/<drive>.sock`,
  one socket per drive. The daemon is unsandboxed and binds there;
  the extension holds the group entitlement and connects there.
  `<group>` is the literal `group.md.enesis.wyrd` as observed on
  the development build (see Bundle identities above for the
  qualification). Stage 1 probes the container directory form
  with the stub name `wyrd-probe.sock` under a team-signed build
  and records the team and build type; the per-drive filename
  form arrives with Stage 2, which must also live-confirm the
  daemon's computed bind path against the extension-resolved one.
- `socket_path_for` stays the cross-platform default; the group
  container path is a macOS-additive override proposal, never a
  replacement. Linux behavior is unchanged.
- Stale sockets resolve through existing semantics: unlink-before-bind
  on serve, connect failure surfaces `Unavailable`, the extension
  reconnects on demand and never assumes a persistent connection.
- `<drive>` must be a stable per-drive fingerprint, coordinated with
  the multi-drive registry (in progress elsewhere) — Stage 2 detail.

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
  contract itself — rename behavior, deletion/recreation,
  persistence across restarts — is decided as OD-17-A (drive-scoped
  stable entry identity supplied by the owner); the owner/model
  contract work gates Stage 2. "Drive-scoped" here states the
  scoping invariant only.
- Every socket connection targets the correct drive's owner.
- Removing one domain cannot affect another drive.
- No singleton state in the adapter: a single-drive prototype must
  not become the architecture.

## Stage 2 owner-client seam (intent, undecided)

The extension speaks the owner protocol (u32 big-endian length
prefix plus JSON, `docs/contracts/owner-interface.md:21-30`) from
Swift. Candidate shapes: a Swift protocol client reimplementing the
framing against shared golden fixtures, or a helper binary. The
decision lands with Stage 2, and the import allow-list widens then
to name the chosen module. Until then the boundary rule pins the
import surface only.

## Release distinction

- Unsigned/ad-hoc integration: development and manual testing only.
- Distributable macOS integration: Developer ID signing,
  notarization, provisioning, production App Group. A compiled appex
  is not a shipped integration.
- If macOS cannot ship signed, the cut is Linux-only with FUSE —
  explicit, never a macOS target implying support.
