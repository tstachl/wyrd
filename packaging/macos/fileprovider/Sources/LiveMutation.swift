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

    /// The item, base, and token for one writable entry in a
    /// single lookup: the extension's check and its commit share
    /// this precondition, so a remote edit between them meets the
    /// owner's refusal instead of being overwritten. `base` is set
    /// for files (the commit base) and `nil` otherwise.
    func mutationItem(for identifier: NSFileProviderItemIdentifier) throws -> (
        item: LiveItem, base: OwnerFileIdentity?, token: String, path: String,
        parent: NSFileProviderItemIdentifier
    ) {
        let client = try liveClient()
        let target = try mutationTarget(for: identifier, client: client)
        let item = try buildItem(
            node: target.node, path: target.path, parent: target.parent,
            generation: target.generation, revision: target.revision)
        var base: OwnerFileIdentity? = nil
        if case .file(let size, let executable, let chunks, _) = target.node {
            base = OwnerFileIdentity(size: size, executable: executable, chunks: chunks)
        }
        return (item, base, target.identityHex, target.path, target.parent)
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

    /// Abort when the recorded cancel predates a socket write.
    /// Checked before every submit, never after a terminal answer:
    /// pre-submit aborts report `NSUserCancelledError` with nothing
    /// sent; post-submit answers own the completion.
    func checkCancel(_ cancel: MutationCancel?, what: String) throws {
        if cancel?.isRequested == true {
            throw LiveStoreError.cancelled(what)
        }
    }

    /// Create an empty directory and return its item. `AlreadyExists`
    /// survives as `refused` so the caller renders
    /// `filenameCollision`; every other refusal maps as usual.
    func createDirectory(
        parent: NSFileProviderItemIdentifier, name: String,
        cancel: MutationCancel? = nil
    ) throws -> LiveItem {
        let child = try validatedName(name)
        try checkCancel(cancel, what: "mkdir \(child)")
        let client = try liveClient()
        let parentPath = try containerPath(for: parent, client: client)
        let path = parentPath.isEmpty ? child : "\(parentPath)/\(child)"
        try checkCancel(cancel, what: "mkdir \(path)")
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
    /// unconfirmed `CreateFile` reconciles before any retry (absence
    /// retries, ambiguity reports temporary — a serving entry never
    /// proves our create landed), and an unconfirmed `CommitFile`
    /// reconciles against the full pre-submit base.
    func createFile(
        parent: NSFileProviderItemIdentifier, name: String, contents: Data?,
        cancel: MutationCancel? = nil
    ) throws -> LiveItem {
        let child = try validatedName(name)
        try checkCancel(cancel, what: "create \(child)")
        let bytes = [UInt8](contents ?? Data())
        guard bytes.count <= Self.maxContentBytes else {
            throw LiveStoreError.sync(
                "content exceeds the 32 MiB bound; save a smaller file")
        }
        let client = try liveClient()
        let parentPath = try containerPath(for: parent, client: client)
        let path = parentPath.isEmpty ? child : "\(parentPath)/\(child)"
        let contentHex = HexCodec.encode(bytes)
        // Step 1: the empty file, with one StaleParent recapture.
        // Every owner call on this path maps transport breaks to
        // temporary, never to a deterministic failure.
        var parentToken: OwnerParentToken
        do {
            parentToken = try client.captureParent(path: parentPath)
        } catch let error as OwnerClientError {
            throw mapMutationError(error, what: "capture-parent \(parentPath)")
        }
        var created = false
        for attempt in 0 ... 1 {
            try checkCancel(cancel, what: "create \(path)")
            do {
                _ = try client.submit(kind: .createFile(path: path, parent: parentToken))
                created = true
                break
            } catch let error as OwnerClientError {
                if case .refused(let refusal) = error, refusal.code == "StaleParent", attempt == 0 {
                    do {
                        parentToken = try client.captureParent(path: parentPath)
                    } catch let recapture as OwnerClientError {
                        throw mapMutationError(recapture, what: "capture-parent \(parentPath)")
                    }
                    continue
                }
                if case .unconfirmed = error {
                    switch try reconcileCreate(path: path, client: client) {
                    case .absent:
                        continue
                    case .committed, .superseded, .unknown:
                        throw LiveStoreError.temporary(
                            "create \(path) unconfirmed; retry the callback")
                    }
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
    func writeContent(
        for identifier: NSFileProviderItemIdentifier, data: Data,
        cancel: MutationCancel? = nil
    ) throws -> LiveItem {
        guard [UInt8](data).count <= Self.maxContentBytes else {
            throw LiveStoreError.sync(
                "content exceeds the 32 MiB bound; save a smaller file")
        }
        let checked = try mutationItem(for: identifier)
        guard let base = checked.base else {
            throw LiveStoreError.sync("only files take content writes")
        }
        return try writeContent(
            path: checked.path, parent: checked.parent, data: data,
            base: base, token: checked.token, cancel: cancel)
    }

    /// Whole-content replacement against a caller-checked base and
    /// token: the version check and the commit share one
    /// precondition, so a remote edit between check and submit meets
    /// the owner's refusal instead of being overwritten. `path` is
    /// the entry's path as checked (post-rename when the caller
    /// moved it first); no re-resolution runs between check and
    /// submit.
    func writeContent(
        path: String,
        parent: NSFileProviderItemIdentifier,
        data: Data,
        base: OwnerFileIdentity,
        token: String,
        cancel: MutationCancel? = nil
    ) throws -> LiveItem {
        let bytes = [UInt8](data)
        guard bytes.count <= Self.maxContentBytes else {
            throw LiveStoreError.sync(
                "content exceeds the 32 MiB bound; save a smaller file")
        }
        let contentHex = HexCodec.encode(bytes)
        let client = try liveClient()
        try commitContent(
            path: path, bytes: bytes, contentHex: contentHex,
            base: base, token: token, client: client, cancel: cancel)
        let (node, generation, revision) = try lookupNode(path: path, client: client)
        guard nodeMatches(node: node, identityHex: token) else {
            throw LiveStoreError.sync("committed entry fails verification")
        }
        return try buildItem(
            node: node, path: path, parent: parent,
            generation: generation, revision: revision)
    }

    /// The fresh content base and entry token for `path`. Read once
    /// by callers that bind a check to the commit they send.
    func lookupBase(path: String, client: OwnerClient) throws -> (OwnerFileIdentity, String) {
        do {
            let (node, _, _) = try client.lookup(path: path)
            guard case .file(let size, let executable, let chunks, let identity) = node,
                let identity
            else {
                throw LiveStoreError.sync("commit target is not a token-bearing file")
            }
            return (OwnerFileIdentity(size: size, executable: executable, chunks: chunks), identity)
        } catch let error as OwnerClientError {
            throw mapMutationError(error, what: "lookup base \(path)")
        }
    }

    /// Commit `bytes` onto `path` against an explicit base and token
    /// with one safe retry after an unconfirmed submit. The base and
    /// token come from the caller's check (see `lookupBase`), never
    /// from a second lookup that could launder a newer base.
    func commitContent(
        path: String, bytes: [UInt8], contentHex: String,
        base: OwnerFileIdentity, token: String, client: OwnerClient,
        cancel: MutationCancel? = nil
    ) throws {
        for _ in 0 ... 1 {
            try checkCancel(cancel, what: "commit \(path)")
            do {
                _ = try client.submit(kind: .commitFile(
                    path: path, base: base, executable: false,
                    contentHex: contentHex, expectedIdentity: token))
                return
            } catch let error as OwnerClientError {
                if case .unconfirmed = error {
                    switch try reconcileCommit(
                        path: path, preBase: base, token: token, client: client)
                    {
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

    /// Commit `bytes` onto `path` against a fresh lookup base and
    /// token, with one safe retry after an unconfirmed submit.
    /// For callers with no prior check (creates); checked callers
    /// use the explicit-base variant above.
    func commitContent(
        path: String, bytes: [UInt8], contentHex: String, client: OwnerClient,
        cancel: MutationCancel? = nil
    ) throws {
        let (base, token) = try lookupBase(path: path, client: client)
        try commitContent(
            path: path, bytes: bytes, contentHex: contentHex,
            base: base, token: token, client: client, cancel: cancel)
    }

    // MARK: - Rename and delete

    /// Move `identifier` under `newParent` as `newName`. The source
    /// precondition binds the move; the destination keeps replace
    /// semantics refused with `AlreadyExists` (`noReplace: true`),
    /// so the system renames and re-issues on collision. A trash
    /// destination never reaches here — the extension fails it
    /// closed before the socket. `expectedToken`, when set, is the
    /// checked entry the move must land on: the submit carries it
    /// instead of a re-read token, so a delete-plus-recreate
    /// between the caller's check and the submit refuses instead of
    /// moving the replacement.
    func rename(
        identifier: NSFileProviderItemIdentifier,
        newParent: NSFileProviderItemIdentifier,
        newName: String,
        expectedToken: String? = nil,
        cancel: MutationCancel? = nil
    ) throws -> LiveItem {
        let child = try validatedName(newName)
        try checkCancel(cancel, what: "rename \(child)")
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
        let bound = expectedToken ?? target.identityHex
        for _ in 0 ... 1 {
            try checkCancel(cancel, what: "rename \(target.path)")
            do {
                _ = try client.submit(kind: .rename(
                    from: target.path, to: destPath, noReplace: true,
                    expectedIdentity: bound))
                break
            } catch let error as OwnerClientError {
                if case .unconfirmed = error {
                    switch try reconcileRename(
                        from: target.path, to: destPath, token: bound, client: client)
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
        guard nodeMatches(node: node, identityHex: bound) else {
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
    func delete(
        identifier: NSFileProviderItemIdentifier,
        cancel: MutationCancel? = nil
    ) throws {
        try checkCancel(cancel, what: "delete")
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
            try checkCancel(cancel, what: "delete \(target.path)")
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

    /// Re-read after an unconfirmed create: absence proves the
    /// create did not land and one retry is safe. A serving entry
    /// proves nothing about *which* create landed — ours or a
    /// concurrent replacement's — so it reads as unknown, never
    /// committed: the system retries the whole callback and the
    /// owner's `AlreadyExists` refusal sorts out the duplicate.
    func reconcileCreate(path: String, client: OwnerClient) throws -> MutationReconcile {
        do {
            _ = try client.lookup(path: path)
            return .unknown("an entry serves the path; ownership unproven")
        } catch let error as OwnerClientError {
            if case .refused(let refusal) = error, refusal.code == "NotFound" {
                return .absent
            }
            return .unknown("re-read failed: \(error)")
        }
    }

    /// Re-read after an unconfirmed commit: the same entry still
    /// carrying the pre-submit base proves the write did not land
    /// (one retry is safe); a missing entry or a changed entry
    /// proves someone else moved it (fail closed, never write over
    /// a replacement); anything else — including a same-size edit,
    /// which is indistinguishable from our own write — is unknown,
    /// never committed. Size alone is not the content identity; the
    /// full base (size, executable bit, chunks) is compared, and
    /// ambiguity fails to temporary, never to success.
    func reconcileCommit(
        path: String, preBase: OwnerFileIdentity, token: String, client: OwnerClient
    ) throws -> MutationReconcile {
        do {
            let (node, _, _) = try client.lookup(path: path)
            guard case .file(let served, let executable, let chunks, let identity) = node else {
                return .superseded("commit target is no longer a file")
            }
            guard identity?.lowercased() == token.lowercased() else {
                return .superseded("entry changed under the commit")
            }
            if served == preBase.size, executable == preBase.executable, chunks == preBase.chunks {
                return .absent
            }
            return .unknown("the entry moved under the commit; ownership unproven")
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
    /// the system's own resolution; transport-shaped refusals
    /// (`Timeout`, `Unavailable`) stay temporary — the read table
    /// would call a mutation `Timeout` deterministic, which is the
    /// wrong lane for a write that may still land. Everything else
    /// reuses the read table.
    func mapMutationError(_ error: Error, what: String) -> LiveStoreError {
        guard let clientError = error as? OwnerClientError else {
            log.error("unexpected mutation error for \(what): \(String(describing: error))")
            return .sync("unexpected failure for \(what)")
        }
        if case .refused(let refusal) = clientError {
            switch refusal.code {
            case "AlreadyExists", "DirectoryNotEmpty":
                return .refused(code: refusal.code, message: refusal.message)
            case "Timeout", "Unavailable":
                return .temporary("\(what): \(refusal.message)")
            default:
                break
            }
        }
        return mapError(error, what: what)
    }
}
