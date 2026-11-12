// Stage 2 live store: enumeration, lookup, and hydration against
// the owner, sharing one path resolver. Every operation opens a
// fresh connection (reconnect on demand is structural), verifies
// identity on use, and maps owner answers to three outcomes:
// found, deterministic sync failure, or temporary unavailability.
// Reads retry once on an owner timeout, then fail closed — a
// timeout is not proof an item disappeared.

import FileProvider
import Foundation
import os

/// The adapter-facing outcome. The call sites translate these to
/// `NSFileProviderError` cases; nothing here names SDK enums, so
/// the mapping table stays in one place.
enum LiveStoreError: Error {
    /// The identifier names nothing: invalid, retired, or deleted.
    case notFound(String)
    /// Deterministic failure: conflicted, too large, undecodable,
    /// budget-exceeded, or a protocol violation. Never retry blind.
    case sync(String)
    /// Temporary: the owner is unreachable or its generation is not
    /// servable yet. The system retries; the adapter holds no state.
    case temporary(String)
}

final class LiveStore {
    let driveHex: String
    var config: SocketConfig
    let resolver: PathResolver
    private let log: Logger

    init(driveHex: String, log: Logger, socketPathOverride: String? = nil) {
        self.driveHex = driveHex
        var config = SocketConfig(driveHex: driveHex)
        config.overridePath = socketPathOverride
        self.config = config
        self.resolver = PathResolver(driveHex: driveHex)
        self.log = log
    }

    // MARK: - Item surface

    /// The root item: the system identifier with an owner-derived
    /// version, so a new generation visibly moves the root.
    func rootItem() throws -> LiveItem {
        let client = try liveClient()
        let attr: OwnerAttr
        let generation: UInt64
        let revision: UInt64
        do {
            (attr, generation, revision) = try client.stat(path: "")
        } catch {
            throw mapError(error, what: "root stat")
        }
        guard attr.kind == "Dir" else {
            throw LiveStoreError.sync("root stat answered kind \(attr.kind)")
        }
        guard let item = LiveItem(
            row: LiveRow(
                name: "Wyrd",
                node: .dir(subtree: "", identity: attr.identity),
                generation: generation,
                revision: revision
            ),
            relativePath: "",
            identifier: .rootContainer,
            parent: .rootContainer
        ) else {
            throw LiveStoreError.sync("root row excluded")
        }
        return item
    }

    /// The item for one identifier: resolve, verify the owner's
    /// token, build. A stale cache re-resolves once with the
    /// bounded walk; a conflict classification fails closed.
    func item(for identifier: NSFileProviderItemIdentifier) throws -> LiveItem {
        if identifier == .rootContainer {
            return try rootItem()
        }
        let client = try liveClient()
        let parsed = WyrdItemIdentifier.parse(identifier.rawValue, driveHex: driveHex)
        switch parsed {
        case .stable(_, let identityHex):
            let (path, parent) = try resolveStable(
                identifier: identifier,
                identityHex: identityHex,
                client: client
            )
            return try itemAt(path: path, parent: parent, client: client)
        case .legacy(let pathHex):
            guard let path = WyrdItemIdentifier.legacyPath(pathHex: pathHex) else {
                throw LiveStoreError.notFound("legacy identifier undecodable")
            }
            let parent = try parentIdentifier(of: path, client: client)
            return try itemAt(path: path, parent: parent, client: client)
        case nil:
            throw LiveStoreError.notFound("identifier not parseable for this drive")
        }
    }

    /// Hydrate one file identifier: resolve exactly like `item(for:)`,
    /// then read. Directories and symlinks fail closed (the former
    /// needs no hydration; the latter's presentation is a Stage 3
    /// question). The owner bounds reads; an owner timeout retries
    /// once on a fresh connection, then fails closed.
    func contents(for identifier: NSFileProviderItemIdentifier) throws -> (
        bytes: Data, item: LiveItem
    ) {
        if identifier == .rootContainer {
            throw LiveStoreError.sync("the root cannot hydrate")
        }
        let client = try liveClient()
        let parsed = WyrdItemIdentifier.parse(identifier.rawValue, driveHex: driveHex)
        let path: String
        let parent: NSFileProviderItemIdentifier
        switch parsed {
        case .stable(_, let identityHex):
            (path, parent) = try resolveStable(
                identifier: identifier,
                identityHex: identityHex,
                client: client
            )
        case .legacy(let pathHex):
            guard let legacyPath = WyrdItemIdentifier.legacyPath(pathHex: pathHex) else {
                throw LiveStoreError.notFound("legacy identifier undecodable")
            }
            path = legacyPath
            parent = try parentIdentifier(of: path, client: client)
        case nil:
            throw LiveStoreError.notFound("identifier not parseable for this drive")
        }
        let item = try itemAt(path: path, parent: parent, client: client)
        guard !item.isDirectory, !item.isSymlink else {
            throw LiveStoreError.sync("only files hydrate")
        }
        let bytes: [UInt8]
        do {
            bytes = try client.read(path: path).bytes
        } catch let error as OwnerClientError {
            if case .refused(let refusal) = error, refusal.code == "Timeout" {
                log.error("owner read timed out once, retrying on a fresh connection")
                do {
                    bytes = try liveClient().read(path: path).bytes
                } catch {
                    throw mapError(error, what: "read retry \(path)")
                }
            } else {
                throw mapError(error, what: "read \(path)")
            }
        }
        return (Data(bytes), item)
    }

    // MARK: - Enumeration

    /// Enumerate one container in a single owner coordinate: one
    /// `readdir`, one anchor. Conflict rows are logged and skipped —
    /// siblings enumerate regardless — and every row records its
    /// identifier-to-path mapping for later lookup.
    func enumerate(container: NSFileProviderItemIdentifier) throws -> (
        items: [LiveItem], anchor: WyrdSyncAnchor
    ) {
        let client = try liveClient()
        let containerPath: String
        let containerId = container
        if container == .rootContainer {
            containerPath = ""
        } else {
            guard let parsed = WyrdItemIdentifier.parse(container.rawValue, driveHex: driveHex) else {
                throw LiveStoreError.notFound("container not parseable for this drive")
            }
            switch parsed {
            case .stable(_, let identityHex):
                (containerPath, _) = try resolveStable(
                    identifier: container,
                    identityHex: identityHex,
                    client: client
                )
            case .legacy(let pathHex):
                guard let path = WyrdItemIdentifier.legacyPath(pathHex: pathHex) else {
                    throw LiveStoreError.notFound("container undecodable")
                }
                containerPath = path
            }
        }
        let entries: [OwnerDirEntry]
        let generation: UInt64
        let revision: UInt64
        do {
            (entries, generation, revision) = try client.readdir(path: containerPath)
        } catch {
            throw mapError(error, what: "readdir \(containerPath)")
        }
        var items: [LiveItem] = []
        var skippedConflicts = 0
        for entry in entries {
            if case .conflict = entry.node {
                skippedConflicts += 1
                continue
            }
            let childPath = containerPath.isEmpty ? entry.name : "\(containerPath)/\(entry.name)"
            let identifier = identifierFor(node: entry.node, path: childPath)
            resolver.record(identifier: identifier, path: childPath, parent: containerId)
            if let item = LiveItem(
                row: LiveRow(
                    name: entry.name,
                    node: entry.node,
                    generation: generation,
                    revision: revision
                ),
                relativePath: childPath,
                identifier: identifier,
                parent: containerId
            ) {
                items.append(item)
            }
        }
        if skippedConflicts > 0 {
            log.error(
                "enumerated \(containerPath) with \(skippedConflicts) conflict rows excluded"
            )
        }
        return (items, WyrdSyncAnchor(generation: generation, revision: revision))
    }

    /// The current anchor: the root's serving coordinate. A
    /// transport break yields `nil` (unknown, re-ask later), never a
    /// fabricated coordinate.
    func currentAnchor() -> WyrdSyncAnchor? {
        guard let client = try? liveClient() else { return nil }
        guard let (_, generation, revision) = try? client.stat(path: "") else { return nil }
        return WyrdSyncAnchor(generation: generation, revision: revision)
    }

    // MARK: - Internals

    private func liveClient() throws -> OwnerClient {
        let path: String
        do {
            path = try config.socketPath()
        } catch {
            throw LiveStoreError.temporary("socket unresolvable: \(error)")
        }
        return OwnerClient(socketPath: path)
    }

    private func identifierFor(node: OwnerNode, path: String) -> NSFileProviderItemIdentifier {
        if let identity = nodeIdentity(node) {
            return NSFileProviderItemIdentifier(
                WyrdItemIdentifier.stableID(driveHex: driveHex, identityHex: identity)
            )
        }
        return NSFileProviderItemIdentifier(WyrdItemIdentifier.legacyID(path: path))
    }

    /// Resolve a stable token: cache, verify with `lookup`, else one
    /// bounded walk. A verified mismatch (rename, path reuse) walks;
    /// a conflict classification fails closed; anything else 404s.
    private func resolveStable(
        identifier: NSFileProviderItemIdentifier,
        identityHex: String,
        client: OwnerClient
    ) throws -> (path: String, parent: NSFileProviderItemIdentifier) {
        if let cached = resolver.cached(identifier: identifier) {
            if verifyToken(identityHex: identityHex, path: cached.path, client: client) {
                return cached
            }
        }
        switch try resolver.walk(identityHex: identityHex, client: client, log: log) {
        case .found(let path, let parent):
            resolver.record(identifier: identifier, path: path, parent: parent)
            return (path, parent)
        case .conflicted:
            log.error("stable identifier resolves to a conflicted entry, failing closed")
            throw LiveStoreError.sync("entry is conflicted")
        case .absent:
            throw LiveStoreError.notFound("identifier names nothing in this generation")
        case .budgetExceeded:
            throw LiveStoreError.sync("resolution exceeded its budget")
        }
    }

    /// The cached path still names this token: `lookup` must agree.
    /// A conflict at the cached path also fails closed here (the
    /// token's entry exists but is conflicted), while any other
    /// mismatch falls through to the walk.
    private func verifyToken(identityHex: String, path: String, client: OwnerClient) -> Bool {
        guard let (node, _, _) = try? client.lookup(path: path) else {
            return false
        }
        switch node {
        case .conflict:
            return false
        default:
            return nodeIdentity(node)?.lowercased() == identityHex.lowercased()
        }
    }

    /// One item at a known path: `lookup` classifies (conflict fails
    /// closed), the token must agree with a stable identifier, then
    /// build. `nil` from the builder is unreachable here (conflicts
    /// return above), so it fails closed too.
    private func itemAt(
        path: String,
        parent: NSFileProviderItemIdentifier,
        client: OwnerClient
    ) throws -> LiveItem {
        let node: OwnerNode
        let generation: UInt64
        let revision: UInt64
        do {
            (node, generation, revision) = try client.lookup(path: path)
        } catch {
            throw mapError(error, what: "lookup \(path)")
        }
        if case .conflict = node {
            throw LiveStoreError.sync("entry is conflicted")
        }
        let name = path.split(separator: "/").last.map(String.init) ?? path
        let identifier = identifierFor(node: node, path: path)
        guard let item = LiveItem(
            row: LiveRow(name: name, node: node, generation: generation, revision: revision),
            relativePath: path,
            identifier: identifier,
            parent: parent
        ) else {
            throw LiveStoreError.sync("row excluded")
        }
        return item
    }

    /// The parent's identifier for a legacy path: the parent's own
    /// lookup decides stable vs legacy, so a legacy child of a
    /// stable directory still nests correctly.
    private func parentIdentifier(of path: String, client: OwnerClient) throws -> NSFileProviderItemIdentifier {
        guard let slash = path.lastIndex(of: "/") else {
            return .rootContainer
        }
        let parentPath = String(path[..<slash])
        if parentPath.isEmpty {
            return .rootContainer
        }
        let node: OwnerNode
        do {
            (node, _, _) = try client.lookup(path: parentPath)
        } catch {
            throw mapError(error, what: "lookup parent \(parentPath)")
        }
        if case .conflict = node {
            throw LiveStoreError.sync("parent is conflicted")
        }
        return identifierFor(node: node, path: parentPath)
    }

    /// Map every owner/transport answer to the three outcomes. A
    /// timeout that survives its retry is a sync failure, not proof
    /// of absence; an invalid frame is a sync failure with a log,
    /// never a guess.
    func mapError(_ error: Error, what: String) -> LiveStoreError {
        guard let clientError = error as? OwnerClientError else {
            log.error("unexpected error for \(what): \(String(describing: error))")
            return .sync("unexpected failure for \(what)")
        }
        switch clientError {
        case .unavailable(let detail):
            return .temporary("\(what): \(detail)")
        case .unconfirmed(let op, let detail):
            log.error("unconfirmed \(op) for \(what): \(detail)")
            return .temporary("\(what): transport broke mid-request")
        case .refused(let refusal):
            switch refusal.code {
            case "NotFound":
                return .notFound("\(what): \(refusal.message)")
            case "NotADirectory", "NotAFile":
                return .notFound("\(what): \(refusal.message)")
            case "Conflicted":
                return .sync("\(what): \(refusal.message)")
            case "TooLarge":
                log.error("owner refused oversize read for \(what)")
                return .sync("\(what): item exceeds the read bound")
            case "Timeout":
                return .sync("\(what): content did not materialize")
            case "Unavailable":
                return .temporary("\(what): \(refusal.message)")
            default:
                log.error("owner refused \(what) with \(refusal.code): \(refusal.message)")
                return .sync("\(what): \(refusal.code)")
            }
        case .incompatible(let detail):
            log.error("protocol violation for \(what): \(detail)")
            return .sync("\(what): invalid protocol response (\(detail))")
        }
    }
}
