// Stage 3 extension: live owner reads when the domain carries a
// drive id, canned Stage 1 data otherwise. The mode is per-domain
// and explicit in the log — canned is never a silent fallback for a
// misconfigured drive. Mutations run live through the owner's
// single queue with identity preconditions (create, modify,
// delete); every write path reconciles an unconfirmed submit
// before deciding on retry, and fails closed with
// `cannotSynchronize`, never a crash. Atomicity is NOT declared
// (no `NSExtensionFileProviderAppliesChangesAtomically` key):
// create-then-commit exposes an intermediary, documented in
// `LiveMutation.swift`. The owner-socket client arrives in Stage 2
// (Swift protocol client against the normative framing); the import
// surface stays within the owner boundary pinned by
// `fileprovider_swift_stays_within_the_owner_boundary`.

import FileProvider
import os

final class FileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
    let domain: NSFileProviderDomain
    private let log = Logger(
        subsystem: "md.enesis.wyrd.fileprovider",
        category: "domain"
    )
    private let liveStore: LiveStore?

    required init(domain: NSFileProviderDomain) {
        self.domain = domain
        if let driveHex = SocketConfig.domainDrive(domain.identifier.rawValue) {
            let store = LiveStore(driveHex: driveHex, log: log)
            self.liveStore = store
            super.init()
            log.notice("live drive \(driveHex, privacy: .public)")
            probeLive(store: store)
        } else {
            self.liveStore = nil
            super.init()
            log.notice("canned mode: domain carries no drive id")
            probeCanned()
        }
    }

    private func probeLive(store: LiveStore) {
        let log = self.log
        let config = store.config
        DispatchQueue.global(qos: .utility).async {
            do {
                let path = try config.socketPath()
                let outcome = SocketProbe.run(path: path)
                if outcome.isReachable {
                    log.notice("\(outcome.message, privacy: .public)")
                } else {
                    log.error("\(outcome.message, privacy: .public)")
                }
            } catch {
                log.error("socket unresolvable: \(String(describing: error))")
            }
        }
    }

    private func probeCanned() {
        let log = self.log
        DispatchQueue.global(qos: .utility).async {
            guard let stub = SocketProbe.legacyStubPath() else {
                log.error("no group container for \(SocketProbe.groupIdentifier)")
                return
            }
            let outcome = SocketProbe.run(path: stub)
            if outcome.isReachable {
                log.notice("\(outcome.message, privacy: .public)")
            } else {
                log.error("\(outcome.message, privacy: .public)")
            }
        }
    }

    func invalidate() {
        // The store holds no persistent connection: every operation
        // opens its own, so invalidation owns no teardown.
    }

    func enumerator(
        for containerItemIdentifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest
    ) throws -> NSFileProviderEnumerator {
        if containerItemIdentifier == .workingSet
            || containerItemIdentifier == .trashContainer
        {
            return EmptyEnumerator()
        }
        if let liveStore {
            return LiveEnumerator(
                container: containerItemIdentifier,
                store: liveStore,
                log: log
            )
        }
        return CannedEnumerator(container: containerItemIdentifier)
    }

    func item(
        for identifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        guard let liveStore else {
            CannedLookup.item(for: identifier, completionHandler: completionHandler)
            return Progress()
        }
        do {
            completionHandler(try liveStore.item(for: identifier), nil)
        } catch let error as LiveStoreError {
            completionHandler(nil, nsError(for: error))
        } catch {
            log.error("unexpected item failure: \(String(describing: error))")
            completionHandler(nil, NSFileProviderError(.cannotSynchronize))
        }
        return Progress()
    }

    func fetchContents(
        for itemIdentifier: NSFileProviderItemIdentifier,
        version requestedVersion: NSFileProviderItemVersion?,
        request: NSFileProviderRequest,
        completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        guard let liveStore else {
            CannedLookup.fetchContents(
                for: itemIdentifier,
                completionHandler: completionHandler
            )
            return Progress()
        }
        do {
            let (bytes, item) = try liveStore.contents(for: itemIdentifier)
            let url = FileManager.default.temporaryDirectory
                .appendingPathComponent("wyrd-\(UUID().uuidString).bin")
            try bytes.write(to: url)
            completionHandler(url, item, nil)
        } catch let error as LiveStoreError {
            completionHandler(nil, nil, nsError(for: error))
        } catch {
            log.error("unexpected hydration failure: \(String(describing: error))")
            completionHandler(nil, nil, NSFileProviderError(.cannotSynchronize))
        }
        return Progress()
    }

    func createItem(
        basedOn itemTemplate: NSFileProviderItem,
        fields: NSFileProviderItemFields,
        contents url: URL?,
        options: NSFileProviderCreateItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (
            NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?
        ) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)
        let gate = SingleCompletion()
        // Record-only cancellation: the driver checks before every
        // socket write, so a cancel wins exactly when nothing was
        // sent; a terminal answer always owns the completion, even
        // if a cancel arrived mid-flight. The one exception is the
        // create-then-commit intermediary: a cancel between
        // CreateFile and its CommitFile aborts with the empty file
        // left behind (documented at the commit call site).
        let cancel = MutationCancel()
        progress.cancellationHandler = {
            cancel.request()
        }
        guard let liveStore else {
            gate.fire {
                completionHandler(nil, [], false, NSFileProviderError(.cannotSynchronize))
            }
            return progress
        }
        let domain = self.domain
        let log = self.log
        DispatchQueue.global(qos: .userInitiated).async {
            if cancel.isRequested {
                gate.fire {
                    completionHandler(nil, [], false, userCancelledError())
                }
                return
            }
            do {
                // `.DS_Store` is excluded where the platform
                // supports it (macOS 13+, our floor) instead of
                // authoring a junk entry; anything else the owner
                // refuses fails per item, never the extension.
                if itemTemplate.filename == ".DS_Store" {
                    gate.fire {
                        completionHandler(
                            nil, [], false,
                            NSFileProviderError(.excludedFromSync))
                    }
                    return
                }
                // Symlink authoring does not exist: refuse closed.
                if itemTemplate.symlinkTargetPath != nil {
                    log.error("symlink creation refused (no symlink authoring)")
                    gate.fire {
                        completionHandler(
                            nil, [], false,
                            NSFileProviderError(.cannotSynchronize))
                    }
                    return
                }
                // No trash namespace: creating directly under trash
                // is a semantic mismatch (Wyrd has no trash or
                // recovery model), never a synthesized entry.
                if itemTemplate.parentItemIdentifier == .trashContainer {
                    log.error("creation under trash refused (no trash model)")
                    gate.fire {
                        completionHandler(
                            nil, [], false,
                            NSFileProviderError(.cannotSynchronize))
                    }
                    return
                }
                // `MayAlreadyExist` (replay/import) and
                // `DeletionConflicted` (local edit vs disk delete)
                // take the normal path: the owner's
                // AlreadyExists/NotFound refusals are authoritative,
                // never an adapter-side merge.
                let created: LiveItem
                if itemTemplate.contentType == .folder {
                    created = try liveStore.createDirectory(
                        parent: itemTemplate.parentItemIdentifier,
                        name: itemTemplate.filename,
                        cancel: cancel)
                } else {
                    // Empty files are supported: no contents means
                    // CreateFile alone, never a fabricated byte. The
                    // staged size is checked before loading, so an
                    // oversize file refuses without materializing.
                    let data = try Self.stagedOptionalContents(at: url, log: log)
                    created = try liveStore.createFile(
                        parent: itemTemplate.parentItemIdentifier,
                        name: itemTemplate.filename,
                        contents: data,
                        cancel: cancel)
                }
                gate.fire {
                    completionHandler(created, [], false, nil)
                    signalWorkingSet(domain: domain, log: log)
                }
            } catch let error as LiveStoreError {
                gate.fire {
                    completionHandler(
                        nil, [], false,
                        Self.mutationError(for: error, what: "create", log: log))
                }
            } catch {
                log.error("unexpected create failure: \(String(describing: error))")
                gate.fire {
                    completionHandler(
                        nil, [], false, NSFileProviderError(.cannotSynchronize))
                }
            }
        }
        return progress
    }

    func modifyItem(
        _ item: NSFileProviderItem,
        baseVersion: NSFileProviderItemVersion,
        changedFields: NSFileProviderItemFields,
        contents newContents: URL?,
        options: NSFileProviderModifyItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (
            NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?
        ) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)
        let gate = SingleCompletion()
        let cancel = MutationCancel()
        progress.cancellationHandler = {
            cancel.request()
        }
        guard let liveStore else {
            gate.fire {
                completionHandler(nil, [], false, NSFileProviderError(.cannotSynchronize))
            }
            return progress
        }
        let domain = self.domain
        let log = self.log
        DispatchQueue.global(qos: .userInitiated).async {
            if cancel.isRequested {
                gate.fire {
                    completionHandler(nil, [], false, userCancelledError())
                }
                return
            }
            do {
                // Trash has no namespace here: reparenting into
                // trash is permanent-delete semantics the owner does
                // not have, so it fails closed instead of retiring
                // an identity into a recovery model that cannot
                // restore it.
                if changedFields.contains(.parentItemIdentifier),
                    item.parentItemIdentifier == .trashContainer
                {
                    log.error("trash move refused (no trash model)")
                    gate.fire {
                        completionHandler(
                            nil, [], false,
                            NSFileProviderError(.cannotSynchronize))
                    }
                    return
                }
                // Read once: the item, base, and token share one
                // lookup, and the commit below sends that same base
                // and token — never a re-read one. A remote edit
                // landing between the check and the submit therefore
                // meets the owner's refusal (Stale/StaleEntry)
                // instead of being overwritten. No merge, no retry
                // on a newer base: fail closed and let the system
                // re-drive the change.
                let checked = try liveStore.mutationItem(for: item.itemIdentifier)
                if !Self.baseVersionMatches(baseVersion, current: checked.item) {
                    log.error("base version mismatch, refusing without overwrite")
                    gate.fire {
                        completionHandler(
                            nil, [], false,
                            NSFileProviderError(.cannotSynchronize))
                    }
                    return
                }
                // Whole-content replacement only, plus rename/move.
                // Fields the owner has no representation for (dates
                // and the like) are reported back as still pending
                // rather than claimed or silently dropped.
                let supported: NSFileProviderItemFields = [.filename, .parentItemIdentifier, .contents]
                let pending = changedFields.subtracting(supported)
                var result = checked.item
                var resultPath = checked.path
                var resultParent = checked.parent
                if changedFields.contains(.filename) || changedFields.contains(.parentItemIdentifier) {
                    result = try liveStore.rename(
                        identifier: item.itemIdentifier,
                        newParent: item.parentItemIdentifier,
                        newName: item.filename,
                        expectedToken: checked.token,
                        cancel: cancel)
                    resultPath = result.relativePath
                    resultParent = result.parentItemIdentifier
                }
                if changedFields.contains(.contents) {
                    guard let base = checked.base else {
                        // Only files take content writes; the
                        // mutation surface already refused symlinks
                        // and legacy rows, so this is a directory
                        // with a contents flag — fail closed.
                        log.error("content change on a non-file refused")
                        gate.fire {
                            completionHandler(
                                nil, [], false,
                                NSFileProviderError(.cannotSynchronize))
                        }
                        return
                    }
                    guard let stagedURL = newContents else {
                        // A content change with no staged bytes is
                        // not fabricable: fail the item closed.
                        log.error("content change without staged contents refused")
                        gate.fire {
                            completionHandler(
                                nil, [], false,
                                NSFileProviderError(.cannotSynchronize))
                        }
                        return
                    }
                    let data = try Self.stagedContents(at: stagedURL, log: log)
                    result = try liveStore.writeContent(
                        path: resultPath, parent: resultParent, data: data,
                        base: base, token: checked.token, cancel: cancel)
                }
                gate.fire {
                    completionHandler(result, pending, false, nil)
                    signalWorkingSet(domain: domain, log: log)
                }
            } catch let error as LiveStoreError {
                gate.fire {
                    completionHandler(
                        nil, [], false,
                        Self.mutationError(for: error, what: "modify", log: log))
                }
            } catch {
                log.error("unexpected modify failure: \(String(describing: error))")
                gate.fire {
                    completionHandler(
                        nil, [], false, NSFileProviderError(.cannotSynchronize))
                }
            }
        }
        return progress
    }

    func deleteItem(
        identifier: NSFileProviderItemIdentifier,
        baseVersion: NSFileProviderItemVersion,
        options: NSFileProviderDeleteItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)
        let gate = SingleCompletion()
        let cancel = MutationCancel()
        progress.cancellationHandler = {
            cancel.request()
        }
        guard let liveStore else {
            gate.fire {
                completionHandler(NSFileProviderError(.cannotSynchronize))
            }
            return progress
        }
        let domain = self.domain
        let log = self.log
        DispatchQueue.global(qos: .userInitiated).async {
            if cancel.isRequested {
                gate.fire { completionHandler(userCancelledError()) }
                return
            }
            do {
                // Permanent delete: `allowsTrashing` is never
                // advertised, so this is the only delete path. An
                // already-absent entry reports success (deleted
                // remotely); the token precondition keeps a
                // same-path replacement safe. No baseVersion check:
                // a content change on the same entry does not
                // invalidate the user's delete of that entry.
                try liveStore.delete(identifier: identifier, cancel: cancel)
                gate.fire {
                    completionHandler(nil)
                    signalWorkingSet(domain: domain, log: log)
                }
            } catch let error as LiveStoreError {
                if case .notFound = error {
                    gate.fire { completionHandler(nil) }
                    return
                }
                gate.fire {
                    completionHandler(
                        Self.mutationError(for: error, what: "delete", log: log))
                }
            } catch {
                log.error("unexpected delete failure: \(String(describing: error))")
                gate.fire {
                    completionHandler(NSFileProviderError(.cannotSynchronize))
                }
            }
        }
        return progress
    }

    private func nsError(for error: LiveStoreError) -> Error {
        switch error {
        case .notFound:
            return NSFileProviderError(.noSuchItem)
        case .sync:
            return NSFileProviderError(.cannotSynchronize)
        case .refused(let code, let message):
            return nsErrorForMutation(
                code: code, message: message, what: "read", log: log)
        case .cancelled:
            return userCancelledError()
        case .temporary:
            return NSFileProviderError(.serverUnreachable)
        }
    }

    /// Render a mutation `LiveStoreError` at the write boundary.
    /// `cancelled` renders the Cocoa cancel error (an `NSError`,
    /// not a File Provider code — the header names the code, and
    /// the domain matters), so the return is the `Error`
    /// existential, not the File Provider error type.
    static func mutationError(
        for error: LiveStoreError, what: String, log: Logger
    ) -> Error {
        switch error {
        case .notFound:
            return NSFileProviderError(.noSuchItem)
        case .sync(let message):
            log.error("mutation \(what) failed: \(message, privacy: .public)")
            return NSFileProviderError(.cannotSynchronize)
        case .refused(let code, let message):
            return nsErrorForMutation(
                code: code, message: message, what: what, log: log)
        case .cancelled:
            return userCancelledError()
        case .temporary(let message):
            log.error("mutation \(what) unavailable: \(message, privacy: .public)")
            return NSFileProviderError(.serverUnreachable)
        }
    }

    /// Read staged file contents for a mutation. The staged size is
    /// checked before loading, so an oversize file refuses without
    /// materializing tens of megabytes.
    static func stagedContents(at url: URL, log: Logger) throws -> Data {
        do {
            let values = try url.resourceValues(forKeys: [.fileSizeKey])
            if let size = values.fileSize, UInt64(size) > UInt64(LiveStore.maxContentBytes) {
                throw LiveStoreError.sync(
                    "content exceeds the 32 MiB bound; save a smaller file")
            }
            return try Data(contentsOf: url)
        } catch let error as LiveStoreError {
            throw error
        } catch {
            log.error("cannot read staged contents: \(String(describing: error))")
            throw LiveStoreError.sync("cannot read staged contents")
        }
    }

    /// Read optional staged contents: a `nil` URL means empty, never
    /// a fabricated byte.
    static func stagedOptionalContents(at url: URL?, log: Logger) throws -> Data? {
        guard let url else { return nil }
        return try stagedContents(at: url, log: log)
    }

    /// Whether the disk's base version still describes the owner's
    /// current item. A first-sync base (never known in sync) skips
    /// the check — the owner precondition remains authoritative.
    /// Otherwise the per-entry facts (size plus the token and
    /// content digest carried in the version) must match. The
    /// serving generation and revision are deliberately excluded:
    /// they move with every commit anywhere, so comparing them
    /// would fail closed on unrelated writes. A same-size remote
    /// edit changes the digest, so it fails here; a race between
    /// this check and the submit — same facts at check time, moved
    /// after — meets the owner's content-base refusal instead.
    /// Report versions (`wyrd-conflict-v1:`) never match an entry
    /// version: reports are read-only, so a modify built on one
    /// fails closed without overwriting.
    static func baseVersionMatches(
        _ base: NSFileProviderItemVersion, current: NSFileProviderItem
    ) -> Bool {
        if base.contentVersion == NSFileProviderItemVersion.beforeFirstSyncComponent {
            return true
        }
        guard let disk = versionFacts(base.contentVersion),
            let currentVersion = current.itemVersion,
            let live = versionFacts(currentVersion.contentVersion)
        else {
            return false
        }
        return disk == live
    }

    /// Split a `wyrd-cv2` (or legacy `wyrd-cv1`) content version into
    /// the per-entry facts: the size and everything after it (token
    /// plus content digest, or the legacy tag). The generation and
    /// revision prefix is dropped by construction. Anything else —
    /// a foreign format, a truncated version — parses to nothing
    /// and fails the match, never the parse.
    static func versionFacts(_ contentVersion: Data) -> (size: String, rest: String)? {
        guard let raw = String(data: contentVersion, encoding: .utf8) else {
            return nil
        }
        for prefix in ["wyrd-cv2:", "wyrd-cv1:"] as [String] {
            if raw.hasPrefix(prefix) {
                let parts = raw.dropFirst(prefix.count).split(
                    separator: ":", maxSplits: 3, omittingEmptySubsequences: false
                ).map(String.init)
                guard parts.count == 4 else { return nil }
                return (parts[2], parts[3])
            }
        }
        return nil
    }
}
