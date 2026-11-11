# macOS packaging (`packaging/macos/`)

Platform packaging outside Cargo and Nix: the Swift File Provider
adapter and its build script. Nothing here is a Cargo workspace
member (a member must be a Rust crate); the adapter's boundary is
enforced by the `fileprovider_swift_stays_within_the_owner_boundary`
contract instead of a `MemberPolicy` allow-list — the OD-8
refinement. See the File Provider issue for the OD-17-A/B/C
decisions; identity and conflict-surface decisions are a hard gate
before Stage 2, and Stage 0/1 proceed without them.

## Layout

- `fileprovider/Sources/` — the Swift adapter. One File Provider
  domain per registered drive; item identifiers are drive-scoped.
- `fileprovider/Info.plist` — the appex manifest (`com.apple.fileprovider-nonui`,
  principal class `WyrdFileProvider.FileProviderExtension`).
- `fileprovider/WyrdFileProvider.entitlements` — development
  entitlements (App Group only). Production signing, notarization,
  and the production group container are the license-gated tail.
- `fileprovider/build-appex.sh` — script-driven build: `swiftc`
  against the macOS SDK, manual appex assembly, `codesign`. No
  `.xcodeproj`, no Xcode IDE, no XcodeGen. Verify with
  `packaging/macos/fileprovider/build-appex.sh`; CI cannot run this
  (Linux-only runners), so the contract test is the automated leg
  and the script is human-verified evidence.

Bundle identities: app `md.enesis.wyrd` (from `tauri.conf.json`),
extension `md.enesis.wyrd.fileprovider`, group
`group.md.enesis.wyrd` (dev builds prefix the team identifier).
Deployment target macOS 13.0.

## Protocol (Stage 0 finding)

The extension implements `NSFileProviderReplicatedExtension`
(one instance per domain, hence per drive). The legacy
`NSFileProviderExtension` class API is unavailable on macOS in
current SDKs — the build script proves this by failing against it.
The report's V1 vocabulary maps as: `startProvidingItem` →
`fetchContents`, with `request: NSFileProviderRequest` threading
through enumeration, lookup, hydration, and mutation. Recorded on
the issue thread.

## Sandbox and socket architecture (Stage 0 acceptance)

The extension runs sandboxed inside `fileproviderd`. A sandboxed
process reaches its container, its App Group containers, and
user-intent paths — nothing else. Connecting to a Unix socket
requires filesystem access to the socket node, so the owner socket
at its drive-derived location is unreachable from the extension:
App Group alignment does not make arbitrary paths accessible.

The scheme, therefore:

- On macOS the owner serves each drive's interface socket **also**
  inside the App Group container:
  `~/Library/Group Containers/group.md.enesis.wyrd/Sockets/<drive>.sock`,
  one socket per drive. The daemon is unsandboxed and binds there;
  the extension holds the group entitlement and connects there.
- `socket_path_for` stays the cross-platform default; the group
  container path is a macOS-additive override, never a replacement.
  Linux behavior is unchanged.
- Stale sockets resolve through existing semantics: unlink-before-bind
  on serve, connect failure surfaces `Unavailable`, the extension
  reconnects on demand and never assumes a persistent connection.
- `<drive>` must be a stable per-drive fingerprint, coordinated with
  the multi-drive registry (in progress elsewhere) — Stage 2 detail.

This analysis is the Stage 0 confirmation. The live proof — owner
and extension running under their actual entitlements, socket at the
production path, owner restart and stale-socket handling, distinct
sockets for multiple drives — is Stage 1's entry gate, before any
adapter logic. If the live proof fails, the transport boundary is
revisited; nothing in Stage 1 depends on it beyond the probe.

## Multi-drive invariants (established now)

- One File Provider domain per registered drive, domain identity
  stable and distinct.
- Every item identifier is scoped to its drive.
- Every socket connection targets the correct drive's owner.
- Removing one domain cannot affect another drive.
- No singleton state in the adapter: a single-drive prototype must
  not become the architecture.

## Release distinction

- Unsigned/ad-hoc integration: development and manual testing only.
- Distributable macOS integration: Developer ID signing,
  notarization, provisioning, production App Group. A compiled appex
  is not a shipped integration.
- If macOS cannot ship signed, the cut is Linux-only with FUSE —
  explicit, never a macOS target implying support.
