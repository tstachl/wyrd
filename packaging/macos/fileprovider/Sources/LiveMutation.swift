// Stage 3 mutation surface: the adapter's write path through the
// owner's single queue. Every mutation resolves through the same
// verified-identity reads as Stage 2, carries the sub-PR 1 identity
// precondition, and reconciles an unconfirmed submit before
// deciding whether retry is safe — a sent request is not a
// committed write. No parallel commit path: the owner remains the
// single authority for identity, ordering, and refusal.
//
// Non-atomic by declaration (no
// `NSExtensionFileProviderAppliesChangesAtomically` key):
// `createFile` commits an empty file before its contents, so the
// intermediary is observable; callers reconcile failures instead
// of assuming all-or-nothing.

import FileProvider
import Foundation

extension LiveStore {
    /// Largest single content image in bytes, mirroring the owner's
    /// 32 MiB submit bound. Checked before any socket opens, so an
    /// oversize write never partially authors.
    static let maxContentBytes = 32 << 20

    // MARK: - Mutation targets

    /// The container's path for a mutation parent. Mirrors the
    /// enumeration resolution (stable tokens verified, conflicts
    /// fail closed) without listing anything.
    func containerPath(for container: NSFileProviderItemIdentifier, client: OwnerClient) throws -> String {
        if container == .rootContainer {
            return ""
        }
        guard let parsed = WyrdItemIdentifier.parse(container.rawValue, driveHex: driveHex) else {
            throw LiveStoreError.notFound("container not parseable for this drive")
        }
        switch parsed {
        case .stable(_, let identityHex):
            let resolved = try stableNode(
                identifier: container, identityHex: identityHex, client: client)
            switch resolved.node {
            case .dir, .mergedDir:
                return resolved.path
            default:
                throw LiveStoreError.notFound("container is not a directory")
            }
        case .legacy(let pathHex):
            guard let path = WyrdItemIdentifier.legacyPath(pathHex: pathHex) else {
                throw LiveStoreError.notFound("container undecodable")
            }
            return path
        }
    }

    /// One stable, writable entry for mutation: the node, its path
    /// and parent, and the entry token the precondition binds.
    /// Legacy rows, symlinks, conflicts, and the root fail closed —
    /// the capabilities matrix advertises them read-only, and this
    /// is the backstop if the system calls anyway.
    func mutationTarget(
        for identifier: NSFileProviderItemIdentifier, client: OwnerClient
    ) throws -> (
        node: OwnerNode, path: String, parent: NSFileProviderItemIdentifier,
        generation: UInt64, revision: UInt64, identityHex: String
    ) {
        if identifier == .rootContainer {
            throw LiveStoreError.sync("the root cannot be mutated")
        }
        guard let parsed = WyrdItemIdentifier.parse(identifier.rawValue, driveHex: driveHex) else {
            throw LiveStoreError.notFound("identifier not parseable for this drive")
        }
        switch parsed {
        case .stable(_, let identityHex):
            let resolved = try stableNode(
                identifier: identifier, identityHex: identityHex, client: client)
            switch resolved.node {
            case .symlink:
                throw LiveStoreError.sync("symlinks are not writable")
            case .conflict:
                throw LiveStoreError.sync("entry is conflicted")
            default:
                break
            }
            guard let token = nodeIdentity(resolved.node) else {
                throw LiveStoreError.sync("entry reports no token")
            }
            return (
                resolved.node, resolved.path, resolved.parent,
                resolved.generation, resolved.revision, token)
        case .legacy:
            throw LiveStoreError.sync("legacy rows are not writable")
        }
    }

    /// Validate a single entry name for mutation. The owner is the
    /// final judge (`Invalid`), but a slash or traversal name never
    /// reaches the socket.
    func validatedName(_ name: String) throws -> String {
        guard !name.isEmpty, !name.contains("/"), name != ".", name != ".." else {
            throw LiveStoreError.sync("invalid entry name")
        }
        return name
    }

    // MARK: - Creates

    /// Create an empty directory and return its item. `AlreadyExists`
    /// survives as `refused` so the caller renders
    /// `filenameCollision`; every other refusal maps as usual.
    func createDirectory(
        parent: NSFileProviderItemIdentifier, name: String
    ) throws -> LiveItem {
        let child = try validatedName(name)
        let client = try liveClient()
        let parentPath = try containerPath(for: parent, client: client)
        let path = parentPath.isEmpty ? child : "\(parentPath)/\(child)"
        do {
            _ = try client.submit(kind: .mkdir(path: path))
        } catch let error as OwnerClientError {
            throw mapMutationError(error, what: "mkdir \(path)")
        }
        let (node, generation, revision) = try lookupNode(path: path, client: client)
        return try buildItem(node: node, path: path, parent: parent, generation: generation, revision: revision)
    }

    /// Create a file with `contents` (`nil` means empty, which is
    /// supported) through capture-parent + CreateFile + CommitFile.
    /// The parent token is recaptured once on `StaleParent`; an
    /// unconfirmed `CreateFile` reconciles before any retry, and an
    /// unconfirmed `CommitFile` reconciles against the served size.
    func createFile(
        parent: NSFileProviderItemIdentifier, name: String, contents: Data?
    ) throws -> LiveItem {
        let child = try validatedName(name)
        let bytes = [UInt8](contents ?? Data())
        guard bytes.count <= Self.maxContentBytes else {
            throw LiveStoreError.sync(
                "content exceeds the 32 MiB bound; save a smaller file")
        }
        let client = try liveClient()
        let parentPath = try containerPath(for: parent, client: client)
        let path = parentPath.isEmpty ? child : "\(parentPath)/\(child)"
        let contentHex = bytes.map { String(format: "%02x", $0) }.joined()
        // Step 1: the empty file, with one StaleParent recapture.
        var token = try client.captureParent(path: parentPath)
        var created = false
        for attempt in 0 ... 1 {
            do {
                _ = try client.submit(kind: .createFile(path: path, parent: token))
                created = true
                break
            } catch let error as OwnerClientError {
                if case .refused(let refusal) = error, refusal.code == "StaleParent", attempt == 0 {
                    token = try client.captureParent(path: parentPath)
                    continue
                }
                if case .unconfirmed = error {
                    switch try reconcileCreate(path: path, client: client) {
                    case .committed:
                        created = true
                    case .absent:
                        continue
                    case .superseded(let detail), .unknown(let detail):
                        throw LiveStoreError.temporary("create \(path) unconfirmed: \(detail)")
                    }
                    break
                }
                throw mapMutationError(error, what: "create \(path)")
            }
        }
        guard created else {
            throw LiveStoreError.temporary("create \(path) never reached a verdict")
        }
        // Step 2: contents, if any, against a fresh base and token.
        if !bytes.isEmpty {
            try commitContent(path: path, bytes: bytes, contentHex: contentHex, client: client)
        }
        // The resolver learns the new row through enumeration, but
        // record the identifier now so a follow-up lookup verifies
        // without a walk.
        let (node, generation, revision) = try lookupNode(path: path, client: client)
        guard let token = nodeIdentity(node) else {
            throw LiveStoreError.sync("created entry reports no token")
        }
        let identifier = NSFileProviderItemIdentifier(
            WyrdItemIdentifier.stableID(driveHex: driveHex, identityHex: token))
        resolver.record(identifier: identifier, path: path, parent: parent)
        return try buildItem(node: node, path: path, parent: parent, generation: generation, revision: revision)
    }

    /// Whole-content replacement of one file: lookup the base and
    /// token, then CommitFile. The caller's baseVersion check (the
    /// extension compares item versions) runs before this; a `Stale`
    /// here is a race that fails closed.
    func writeContent(for identifier: NSFileProviderItemIdentifier, data: Data) throws -> LiveItem {
        let bytes = [UInt8](data)
        guard bytes.count <= Self.maxContentBytes else {
            throw LiveStoreError.sync(
                "content exceeds the 32 MiB bound; save a smaller file")
        }
        let contentHex = bytes.map { String(format: "%02x", $0) }.joined()
        let client = try liveClient()
        let target = try mutationTarget(for: identifier, client: client)
        guard case .file = target.node else {
            throw LiveStoreError.sync("only files take content writes")
        }
        try commitContent(path: target.path, bytes: bytes, contentHex: contentHex, client: client)
        let (node, generation, revision) = try lookupNode(path: target.path, client: client)
        return try buildItem(
            node: node, path: target.path, parent: target.parent,
            generation: generation, revision: revision)
    }

    /// Commit `bytes` onto `path` against a fresh lookup base and
    /// token, with one safe retry after an unconfirmed submit.
    func commitContent(path: String, bytes: [UInt8], contentHex: String, client: OwnerClient) throws {
        let (base, token): (OwnerFileIdentity, String) = try {
            let (node, _, _) = try client.lookup(path: path)
            guard case .file(let size, let executable, let chunks, let identity) = node,
                let identity
            else {
                throw LiveStoreError.sync("commit target is not a token-bearing file")
            }
            return (OwnerFileIdentity(size: size, executable: executable, chunks: chunks), identity)
        }()
        for _ in 0 ... 1 {
            do {
                _ = try client.submit(kind: .commitFile(
                    path: path, base: base, executable: false,
                    contentHex: contentHex, expectedIdentity: token))
                return
            } catch let error as OwnerClientError {
                if case .unconfirmed = error {
                    switch try reconcileCommit(path: path, token: token, size: UInt64(bytes.count), client: client) {
                    case .committed:
                        return
                    case .absent:
                        continue
                    case .superseded(let detail):
                        throw LiveStoreError.sync("commit \(path) superseded: \(detail)")
                    case .unknown(let detail):
                        throw LiveStoreError.temporary("commit \(path) unconfirmed: \(detail)")
                    }
                }
                throw mapMutationError(error, what: "commit \(path)")
            }
        }
        throw LiveStoreError.temporary("commit \(path) never reached a verdict")
    }

    // MARK: - Rename and delete

    /// Move `identifier` under `newParent` as `newName`. The source
    /// precondition binds the move; the destination keeps replace
    /// semantics refused with `AlreadyExists` (`noReplace: true`),
    /// so the system renames and re-issues on collision. A trash
    /// destination never reaches here — the extension fails it
    /// closed before the socket.
    func rename(
        identifier: NSFileProviderItemIdentifier,
        newParent: NSFileProviderItemIdentifier,
        newName: String
    ) throws -> LiveItem {
        let child = try validatedName(newName)
        let client = try liveClient()
        let target = try mutationTarget(for: identifier, client: client)
        let destParentPath = try containerPath(for: newParent, client: client)
        let destPath = destParentPath.isEmpty ? child : "\(destParentPath)/\(child)"
        // A no-op rename submits nothing: renaming a path onto
        // itself leaves the queue untouched, matching the FUSE
        // adapter's own short-circuit.
        if destPath == target.path {
            let (node, generation, revision) = try lookupNode(path: target.path, client: client)
            return try buildItem(
                node: node, path: target.path, parent: newParent,
                generation: generation, revision: revision)
        }
        for _ in 0 ... 1 {
            do {
                _ = try client.submit(kind: .rename(
                    from: target.path, to: destPath, noReplace: true,
                    expectedIdentity: target.identityHex))
                break
            } catch let error as OwnerClientError {
                if case .unconfirmed = error {
                    switch try reconcileRename(
                        from: target.path, to: destPath, token: target.identityHex, client: client)
                    {
                    case .committed:
                        break
                    case .absent:
                        continue
                    case .superseded(let detail):
                        throw LiveStoreError.sync("rename superseded: \(detail)")
                    case .unknown(let detail):
                        throw LiveStoreError.temporary("rename unconfirmed: \(detail)")
                    }
                    break
                }
                throw mapMutationError(error, what: "rename \(target.path)")
            }
        }
        let (node, generation, revision) = try lookupNode(path: destPath, client: client)
        guard nodeMatches(node: node, identityHex: target.identityHex) else {
            throw LiveStoreError.sync("renamed entry fails verification")
        }
        let identifier = NSFileProviderItemIdentifier(
            WyrdItemIdentifier.stableID(driveHex: driveHex, identityHex: target.identityHex))
        resolver.record(identifier: identifier, path: destPath, parent: newParent)
        return try buildItem(
            node: node, path: destPath, parent: newParent,
            generation: generation, revision: revision)
    }

    /// Permanently delete one entry. No trash: `allowsTrashing` is
    /// never advertised, so this is the only delete path. A missing
    /// entry reports success (already deleted remotely); a
    /// non-empty directory keeps `DirectoryNotEmpty` as `refused`
    /// for the system to re-create.
    func delete(identifier: NSFileProviderItemIdentifier) throws {
        let client = try liveClient()
        let target: (
            node: OwnerNode, path: String, parent: NSFileProviderItemIdentifier,
            generation: UInt64, revision: UInt64, identityHex: String
        )
        do {
            target = try mutationTarget(for: identifier, client: client)
        } catch let error as LiveStoreError {
            if case .notFound = error {
                return
            }
            throw error
        }
        let kind: OwnerMutationKind
        switch target.node {
        case .file:
            kind = .unlink(path: target.path, expectedIdentity: target.identityHex)
        case .dir, .mergedDir:
            kind = .rmdir(path: target.path, expectedIdentity: target.identityHex)
        case .symlink, .conflict:
            throw LiveStoreError.sync("entry is not deletable")
        }
        for _ in 0 ... 1 {
            do {
                _ = try client.submit(kind: kind)
                return
            } catch let error as OwnerClientError {
                if case .unconfirmed = error {
                    switch try reconcileDelete(
                        path: target.path, token: target.identityHex, client: client)
                    {
                    case .committed:
                        return
                    case .absent:
                        continue
                    case .superseded(let detail):
                        throw LiveStoreError.sync("delete superseded: \(detail)")
                    case .unknown(let detail):
                        throw LiveStoreError.temporary("delete unconfirmed: \(detail)")
                    }
                }
                throw mapMutationError(error, what: "delete \(target.path)")
            }
        }
        throw LiveStoreError.temporary("delete \(target.path) never reached a verdict")
    }

    // MARK: - Reconciliation

    /// Re-read after an unconfirmed create: the entry serves the
    /// new path exactly when `CreateFile` landed.
    func reconcileCreate(path: String, client: OwnerClient) throws -> MutationReconcile {
        do {
            let (node, _, _) = try client.lookup(path: path)
            if nodeIdentity(node) != nil {
                return .committed
            }
            return .unknown("created path serves no token")
        } catch let error as OwnerClientError {
            if case .refused(let refusal) = error, refusal.code == "NotFound" {
                return .absent
            }
            return .unknown("re-read failed: \(error)")
        }
    }

    /// Re-read after an unconfirmed commit: the same token with the
    /// new size proves the write; the same token with the old size
    /// proves it did not land; a different token means someone
    /// replaced the entry — fail closed, never write over it.
    func reconcileCommit(
        path: String, token: String, size: UInt64, client: OwnerClient
    ) throws -> MutationReconcile {
        do {
            let (node, _, _) = try client.lookup(path: path)
            guard case .file(let served, _, _, let identity) = node else {
                return .superseded("commit target is no longer a file")
            }
            guard identity?.lowercased() == token.lowercased() else {
                return .superseded("entry changed under the commit")
            }
            return served == size ? .committed : .absent
        } catch let error as OwnerClientError {
            if case .refused(let refusal) = error, refusal.code == "NotFound" {
                return .superseded("commit target is gone")
            }
            return .unknown("re-read failed: \(error)")
        }
    }

    /// Re-read after an unconfirmed rename: the token at the
    /// destination proves the move; the token still at the source
    /// proves it did not land.
    func reconcileRename(
        from: String, to: String, token: String, client: OwnerClient
    ) throws -> MutationReconcile {
        if let dest = try? client.lookup(path: to),
            nodeMatches(node: dest.node, identityHex: token)
        {
            return .committed
        }
        do {
            let (node, _, _) = try client.lookup(path: from)
            if nodeMatches(node: node, identityHex: token) {
                return .absent
            }
            return .superseded("source entry changed under the rename")
        } catch let error as OwnerClientError {
            if case .refused(let refusal) = error, refusal.code == "NotFound" {
                return .unknown("neither source nor destination serves the entry")
            }
            return .unknown("re-read failed: \(error)")
        }
    }

    /// Re-read after an unconfirmed delete: absence proves it; the
    /// same token proves it did not land; a different token means a
    /// replacement arrived — fail closed, never delete it.
    func reconcileDelete(path: String, token: String, client: OwnerClient) throws -> MutationReconcile {
        do {
            let (node, _, _) = try client.lookup(path: path)
            if nodeMatches(node: node, identityHex: token) {
                return .absent
            }
            return .superseded("a replacement arrived under the delete")
        } catch let error as OwnerClientError {
            if case .refused(let refusal) = error, refusal.code == "NotFound" {
                return .committed
            }
            return .unknown("re-read failed: \(error)")
        }
    }

    // MARK: - Mutation error mapping

    /// Map a mutation submit failure. Collisions and non-empty
    /// directories survive as `refused` so the callback renders
    /// the system's own resolution; everything else reuses the
    /// read table (missing → notFound, transient → temporary,
    /// deterministic → sync).
    func mapMutationError(_ error: Error, what: String) -> LiveStoreError {
        guard let clientError = error as? OwnerClientError else {
            log.error("unexpected mutation error for \(what): \(String(describing: error))")
            return .sync("unexpected failure for \(what)")
        }
        if case .refused(let refusal) = clientError,
            refusal.code == "AlreadyExists" || refusal.code == "DirectoryNotEmpty"
        {
            return .refused(code: refusal.code, message: refusal.message)
        }
        return mapError(error, what: what)
    }
}
