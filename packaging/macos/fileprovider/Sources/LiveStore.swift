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
    /// budget-exceeded, stale precondition, or a protocol violation.
    /// Never retry blind.
    case sync(String)
    /// A mutation refusal the system resolves itself: a name
    /// collision (the system renames and re-issues) or a
    /// non-empty directory (the system re-creates from metadata).
    /// Carried separately so the callback renders the system's
    /// own resolution instead of a sync failure.
    case refused(code: String, message: String)
    /// Temporary: the owner is unreachable, its generation is not
    /// servable yet, or an unconfirmed write reached no verdict.
    /// The system retries; the adapter holds no state.
    case temporary(String)
}

final class LiveStore {
    let driveHex: String
    var config: SocketConfig
    let resolver: PathResolver
    let log: Logger

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
            let resolved = try stableNode(
                identifier: identifier,
                identityHex: identityHex,
                client: client
            )
            return try buildItem(
                node: resolved.node,
                path: resolved.path,
                parent: resolved.parent,
                generation: resolved.generation,
                revision: resolved.revision
            )
        case .legacy(let pathHex):
            guard let path = WyrdItemIdentifier.legacyPath(pathHex: pathHex) else {
                throw LiveStoreError.notFound("legacy identifier undecodable")
            }
            let parent = try parentIdentifier(of: path, client: client)
            let (node, generation, revision) = try lookupNode(path: path, client: client)
            return try buildItem(
                node: node,
                path: path,
                parent: parent,
                generation: generation,
                revision: revision
            )
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
        let node: OwnerNode
        let path: String
        let parent: NSFileProviderItemIdentifier
        let generation: UInt64
        let revision: UInt64
        switch parsed {
        case .stable(_, let identityHex):
            let resolved = try stableNode(
                identifier: identifier,
                identityHex: identityHex,
                client: client
            )
            (node, path, parent, generation, revision) = (
                resolved.node, resolved.path, resolved.parent,
                resolved.generation, resolved.revision
            )
        case .legacy(let pathHex):
            guard let legacyPath = WyrdItemIdentifier.legacyPath(pathHex: pathHex) else {
                throw LiveStoreError.notFound("legacy identifier undecodable")
            }
            path = legacyPath
            parent = try parentIdentifier(of: path, client: client)
            (node, generation, revision) = try lookupNode(path: path, client: client)
        case nil:
            throw LiveStoreError.notFound("identifier not parseable for this drive")
        }
        let item = try buildItem(
            node: node,
            path: path,
            parent: parent,
            generation: generation,
            revision: revision
        )
        guard !item.isSymlinkRow, !item.isDirectory else {
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
                let resolved = try stableNode(
                    identifier: container,
                    identityHex: identityHex,
                    client: client
                )
                switch resolved.node {
                case .dir, .mergedDir:
                    containerPath = resolved.path
                default:
                    throw LiveStoreError.notFound("container is not a directory")
                }
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

    func liveClient() throws -> OwnerClient {
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

    /// Resolve one stable token to its serving node in a single
    /// `lookup` on the fast path: the cached path verifies by
    /// agreeing with the requested token, so a rename,
    /// delete-plus-recreate, or path reuse never returns a
    /// replacement *item* under the old identifier. A mismatch falls
    /// through to one bounded walk; a conflict classification fails
    /// closed; anything else 404s. Transport breaks propagate, never
    /// trigger a walk.
    ///
    /// Residual window (documented, not closed): hydration reads by
    /// path after this verification, so a delete-plus-recreate
    /// between the verify `lookup` and the `read` can return the
    /// replacement's bytes under the old identifier and version.
    /// Closing it needs identity-bound reads (the owner answers by
    /// token, not by path) — Stage 3 work. The item surface above
    /// carries no such window: the item is built from the verified
    /// node itself.
    func stableNode(
        identifier: NSFileProviderItemIdentifier,
        identityHex: String,
        client: OwnerClient
    ) throws -> (
        node: OwnerNode, path: String, parent: NSFileProviderItemIdentifier,
        generation: UInt64, revision: UInt64
    ) {
        if let cached = resolver.cached(identifier: identifier) {
            do {
                let (node, generation, revision) = try client.lookup(path: cached.path)
                if nodeMatches(node: node, identityHex: identityHex) {
                    return (node, cached.path, cached.parent, generation, revision)
                }
                // Verified mismatch: the cache is stale. The walk
                // below re-resolves (or classifies the conflict).
            } catch let error as OwnerClientError {
                switch error {
                case .refused(let refusal) where refusal.code == "NotFound":
                    break // Deleted path: the walk decides.
                case .refused(let refusal) where refusal.code == "Conflicted":
                    log.error("cached path is conflicted, failing closed")
                    throw LiveStoreError.sync("entry is conflicted")
                default:
                    throw mapError(error, what: "lookup \(cached.path)")
                }
            }
        }
        switch try resolver.walk(identityHex: identityHex, client: client, log: log) {
        case .found(let path, let parent):
            resolver.record(identifier: identifier, path: path, parent: parent)
            let (node, generation, revision) = try lookupNode(path: path, client: client)
            guard nodeMatches(node: node, identityHex: identityHex) else {
                throw LiveStoreError.notFound("identifier moved during resolution")
            }
            return (node, path, parent, generation, revision)
        case .conflicted:
            log.error("stable identifier resolves to a conflicted entry, failing closed")
            throw LiveStoreError.sync("entry is conflicted")
        case .absent:
            throw LiveStoreError.notFound("identifier names nothing in this generation")
        case .budgetExceeded:
            throw LiveStoreError.sync("resolution exceeded its budget")
        }
    }

    /// A looked-up node carries the requested token: files, dirs,
    /// and symlinks by their entry token, merged dirs by the agreed
    /// token. Conflict rows never match — the walk classifies them.
    func nodeMatches(node: OwnerNode, identityHex: String) -> Bool {
        nodeIdentity(node)?.lowercased() == identityHex.lowercased()
    }

    /// One `lookup` with error mapping. Conflict rows return as
    /// nodes (the caller classifies); every refusal maps.
    func lookupNode(path: String, client: OwnerClient) throws -> (
        node: OwnerNode, generation: UInt64, revision: UInt64
    ) {
        do {
            return try client.lookup(path: path)
        } catch {
            throw mapError(error, what: "lookup \(path)")
        }
    }

    /// Build the item for a classified node. Conflict rows and
    /// excluded rows fail closed — every caller classifies first,
    /// and this is the backstop if one does not.
    func buildItem(
        node: OwnerNode,
        path: String,
        parent: NSFileProviderItemIdentifier,
        generation: UInt64,
        revision: UInt64
    ) throws -> LiveItem {
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
