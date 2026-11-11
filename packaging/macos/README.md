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
`group.md.enesis.wyrd` granted literally as written (verified in
the Xcode-built artifact; no team prefix). The container
materializes only under a team-signed build with provisioning.
Deployment target macOS 13.0.

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
  `<group>` is `group.md.enesis.wyrd` as granted; Stage 1 probes
  exactly this path form under a team-signed build and records the
  team and build type.
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
team-signed containing app plus domain registration. Procedure:

1. Start a stub listener at the production path:
   `python3 -c` one-liner in `Sources/SocketProbe.swift`.
2. Build and run the containing app from Xcode (team
   `JU2RFHWQPH`, development signing), add one domain.
3. Open the domain in Finder: `hello.txt` and `notes/` enumerate
   from canned data; opening `hello.txt` hydrates canned bytes.
4. Read the probe outcome in Console (`md.enesis.wyrd.fileprovider`,
   category `domain`): `reachable: …` passes the gate.
5. Record the macOS version and signing identity with the result.

One domain for the spike; per-drive domains and isolation arrive
with Stage 3, under the invariants above.

## Multi-drive invariants (established now)

- One File Provider domain per registered drive, domain identity
  stable and distinct.
- Every item identifier is scoped to its drive. The identifier
  contract itself — rename behavior, deletion/recreation,
  persistence across restarts — is the open OD-17-A decision;
  "drive-scoped" here states the scoping invariant only.
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
