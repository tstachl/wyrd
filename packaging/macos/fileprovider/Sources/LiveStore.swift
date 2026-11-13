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
    /// The driver observed a recorded cancel before any socket
    /// write and aborted: nothing was sent, so reporting
    /// `NSUserCancelledError` is exact, never a guess about an
    /// in-flight request.
    case cancelled(String)
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
    /// Synthetic report identifiers resolve through their embedded
    /// path: a live conflict rebuilds the report, a resolved path
    /// 404s (the inspector disappears only on state change).
    func item(for identifier: NSFileProviderItemIdentifier) throws -> NSFileProviderItem {
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
            try requireIdentityLess(node)
            return try buildItem(
                node: node,
                path: path,
                parent: parent,
                generation: generation,
                revision: revision
            )
        case .conflict(_, let pathHex):
            return try resolveReport(
                identifier: identifier, pathHex: pathHex, client: client
            ).item
        case nil:
            throw LiveStoreError.notFound("identifier not parseable for this drive")
        }
    }

    /// Hydrate one file identifier: resolve exactly like `item(for:)`,
    /// then read. Directories and symlinks fail closed (the former
    /// needs no hydration; the latter's presentation is a Stage 3
    /// question). The owner bounds reads; an owner timeout retries
    /// once on a fresh connection, then fails closed. Synthetic
    /// report identifiers hydrate the generated report text — never
    /// a version's bytes — re-rendered live so the served bytes
    /// always describe the current conflict.
    func contents(for identifier: NSFileProviderItemIdentifier) throws -> (
        bytes: Data, item: NSFileProviderItem
    ) {
        if identifier == .rootContainer {
            throw LiveStoreError.sync("the root cannot hydrate")
        }
        let client = try liveClient()
        let parsed = WyrdItemIdentifier.parse(identifier.rawValue, driveHex: driveHex)
        switch parsed {
        case .conflict(_, let pathHex):
            let resolved = try resolveReport(
                identifier: identifier, pathHex: pathHex, client: client)
            let bytes = renderConflictReport(
                path: resolved.item.conflictedPath,
                driveHex: driveHex,
                versions: resolved.versions,
                generation: resolved.generation,
                revision: resolved.revision)
            return (bytes, resolved.item)
        case .stable(_, let identityHex):
            let resolved = try stableNode(
                identifier: identifier,
                identityHex: identityHex,
                client: client
            )
            return try fileContents(
                node: resolved.node,
                path: resolved.path,
                parent: resolved.parent,
                generation: resolved.generation,
                revision: resolved.revision,
                client: client)
        case .legacy(let pathHex):
            guard let legacyPath = WyrdItemIdentifier.legacyPath(pathHex: pathHex) else {
                throw LiveStoreError.notFound("legacy identifier undecodable")
            }
            let path = legacyPath
            let parent = try parentIdentifier(of: path, client: client)
            let (node, generation, revision) = try lookupNode(path: path, client: client)
            try requireIdentityLess(node)
            return try fileContents(
                node: node,
                path: path,
                parent: parent,
                generation: generation,
                revision: revision,
                client: client)
        case nil:
            throw LiveStoreError.notFound("identifier not parseable for this drive")
        }
    }

    /// Build the item for a verified file node, then read it. Shared
    /// by the stable and legacy hydration paths; reports never reach
    /// here.
    private func fileContents(
        node: OwnerNode,
        path: String,
        parent: NSFileProviderItemIdentifier,
        generation: UInt64,
        revision: UInt64,
        client: OwnerClient
    ) throws -> (bytes: Data, item: NSFileProviderItem) {
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
    /// `readdir`, one anchor. Conflict rows yield one synthetic
    /// report item each (never ordinary content); siblings enumerate
    /// regardless. Every row records its identifier-to-path mapping
    /// for later lookup, except synthetic reports, which decode
    /// their path from their identifier and always read fresh.
    /// Container resolution is shared with the mutation surface
    /// (`containerPath(for:client:)`), so enumeration and writes
    /// never disagree on what a container names.
    func enumerate(container: NSFileProviderItemIdentifier) throws -> (
        items: [LiveItem], reports: [ConflictReportItem], anchor: WyrdSyncAnchor
    ) {
        let client = try liveClient()
        let containerPath = try containerPath(for: container, client: client)
        let containerId = container
        let entries: [OwnerDirEntry]
        let generation: UInt64
        let revision: UInt64
        do {
            (entries, generation, revision) = try client.readdir(path: containerPath)
        } catch {
            throw mapError(error, what: "readdir \(containerPath)")
        }
        var items: [LiveItem] = []
        var conflictRows: [(name: String, versions: [OwnerConflictVersion])] = []
        for entry in entries {
            if case .conflict(let versions) = entry.node {
                conflictRows.append((entry.name, versions))
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
        // Report names disambiguate against real siblings in
        // readdir order; `item(for:)` replays the same assignment,
        // so the enumerated name and the looked-up name agree.
        let realNames = Set(entries.map { $0.name })
        let reportNames = assignReportNames(conflictRows: conflictRows.map { $0.name }, realNames: realNames)
        var reports: [ConflictReportItem] = []
        for (index, row) in conflictRows.enumerated() {
            let childPath = containerPath.isEmpty ? row.name : "\(containerPath)/\(row.name)"
            let identifier = NSFileProviderItemIdentifier(
                WyrdItemIdentifier.conflictID(driveHex: driveHex, path: childPath))
            reports.append(
                ConflictReportItem(
                    driveHex: driveHex,
                    conflictedPath: childPath,
                    reportName: reportNames[index],
                    versions: row.versions,
                    generation: generation,
                    revision: revision,
                    identifier: identifier,
                    parent: containerId))
        }
        if !conflictRows.isEmpty {
            log.error(
                "enumerated \(containerPath) with \(conflictRows.count) conflict reports"
            )
        }
        return (items, reports, WyrdSyncAnchor(generation: generation, revision: revision))
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

    /// Reject a node that carries owner identity behind a legacy
    /// reference: the reference names the identity-less entry at
    /// its path, so a minted path 404s instead of serving the
    /// replacement under the old identifier — the same rule stable
    /// resolution enforces by token match. Without this, the system
    /// keeps refreshing a retired reference, gets back an item with
    /// a different identifier, and fails the whole fetch as
    /// inconsistent. Still-identity-less nodes pass through
    /// untouched. One helper for the item, contents, and container
    /// surfaces, so no surface can lose the rule again.
    func requireIdentityLess(_ node: OwnerNode) throws {
        if nodeIdentity(node) != nil {
            throw LiveStoreError.notFound("legacy reference retired by identity mint")
        }
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

    /// Resolve one synthetic report identifier through its embedded
    /// path: the path must serve a live conflict, or the report is
    /// gone. A non-conflict node means the conflict resolved (the
    /// report disappears); a missing path 404s. Anything else maps
    /// normally — a transport break stays temporary, never a guess.
    /// The report name replays the enumeration assignment (readdir
    /// the parent, disambiguate in order), so lookup and enumeration
    /// agree on the filename.
    func resolveReport(
        identifier: NSFileProviderItemIdentifier,
        pathHex: String,
        client: OwnerClient
    ) throws -> (
        item: ConflictReportItem, versions: [OwnerConflictVersion],
        generation: UInt64, revision: UInt64
    ) {
        guard let path = WyrdItemIdentifier.conflictPath(pathHex: pathHex) else {
            throw LiveStoreError.notFound("report identifier undecodable")
        }
        let parent = try parentIdentifier(of: path, client: client)
        let (node, generation, revision) = try lookupNode(path: path, client: client)
        guard case .conflict(let versions) = node else {
            // The conflict resolved (or the path never conflicted):
            // the report is gone, exactly the invalidation rule.
            throw LiveStoreError.notFound("no live conflict at \(path)")
        }
        let entryName = path.split(separator: "/").last.map(String.init) ?? path
        let reportName = try assignedReportName(
            entryName: entryName, parentPath: parentPath(of: path), client: client)
        return (
            ConflictReportItem(
                driveHex: driveHex,
                conflictedPath: path,
                reportName: reportName,
                versions: versions,
                generation: generation,
                revision: revision,
                identifier: identifier,
                parent: parent),
            versions, generation, revision)
    }

    /// The report filename for one conflicted entry name, replaying
    /// the enumeration assignment: the parent's real entry names
    /// plus earlier conflict rows in readdir order take precedence.
    /// Both enumeration and lookup run this over the same listing,
    /// so both agree.
    func assignedReportName(
        entryName: String, parentPath: String, client: OwnerClient
    ) throws -> String {
        let siblings: [OwnerDirEntry]
        do {
            siblings = try client.readdir(path: parentPath).entries
        } catch {
            throw mapError(error, what: "readdir \(parentPath)")
        }
        let realNames = Set(siblings.map { $0.name })
        let ordered = siblings.compactMap { entry -> String? in
            if case .conflict = entry.node { return entry.name }
            return nil
        }
        let assigned = assignReportNames(conflictRows: ordered, realNames: realNames)
        guard let index = ordered.firstIndex(of: entryName) else {
            throw LiveStoreError.notFound("no live conflict at \(entryName)")
        }
        return assigned[index]
    }

    /// The parent path of a relative path: `""` for a root child.
    private func parentPath(of path: String) -> String {
        guard let slash = path.lastIndex(of: "/") else { return "" }
        return String(path[..<slash])
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
