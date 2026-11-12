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
        progress.cancellationHandler = {
            gate.fire {
                completionHandler(nil, [], false, userCancelledError())
            }
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
            if progress.isCancelled {
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
                        name: itemTemplate.filename)
                } else {
                    let data: Data?
                    if let url {
                        do {
                            data = try Data(contentsOf: url)
                        } catch {
                            log.error(
                                "cannot read staged contents: \(String(describing: error))")
                            gate.fire {
                                completionHandler(
                                    nil, [], false,
                                    NSFileProviderError(.cannotSynchronize))
                            }
                            return
                        }
                    } else {
                        // Empty files are supported: no contents
                        // means CreateFile alone, never a fabricated
                        // byte.
                        data = nil
                    }
                    created = try liveStore.createFile(
                        parent: itemTemplate.parentItemIdentifier,
                        name: itemTemplate.filename,
                        contents: data)
                }
                if gate.isDone {
                    // Cancel won the race after the owner committed:
                    // the entry exists; the working-set signal (below)
                    // reconciles the system's view.
                    signalWorkingSet(domain: domain, log: log)
                    return
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
        progress.cancellationHandler = {
            gate.fire {
                completionHandler(nil, [], false, userCancelledError())
            }
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
            if progress.isCancelled {
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
                // The current owner state is the conflict detector:
                // a baseVersion that no longer matches means a
                // remote edit landed first. No overwrite, no merge,
                // no retry on a newer base — fail closed and let the
                // system re-drive the change.
                let current = try liveStore.item(for: item.itemIdentifier)
                if !Self.baseVersionMatches(baseVersion, current: current) {
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
                var result = current
                var resultIdentifier = item.itemIdentifier
                if changedFields.contains(.filename) || changedFields.contains(.parentItemIdentifier) {
                    result = try liveStore.rename(
                        identifier: resultIdentifier,
                        newParent: item.parentItemIdentifier,
                        newName: item.filename)
                    resultIdentifier = result.itemIdentifier
                }
                if changedFields.contains(.contents) {
                    guard let newContents else {
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
                    let data: Data
                    do {
                        data = try Data(contentsOf: newContents)
                    } catch {
                        log.error(
                            "cannot read staged contents: \(String(describing: error))")
                        gate.fire {
                            completionHandler(
                                nil, [], false,
                                NSFileProviderError(.cannotSynchronize))
                        }
                        return
                    }
                    result = try liveStore.writeContent(for: resultIdentifier, data: data)
                }
                if gate.isDone {
                    signalWorkingSet(domain: domain, log: log)
                    return
                }
                let completed = result
                gate.fire {
                    completionHandler(completed, pending, false, nil)
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
        progress.cancellationHandler = {
            gate.fire { completionHandler(userCancelledError()) }
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
            if progress.isCancelled {
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
                try liveStore.delete(identifier: identifier)
                if gate.isDone {
                    signalWorkingSet(domain: domain, log: log)
                    return
                }
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

    private func nsError(for error: LiveStoreError) -> NSFileProviderError {
        switch error {
        case .notFound:
            return NSFileProviderError(.noSuchItem)
        case .sync:
            return NSFileProviderError(.cannotSynchronize)
        case .refused(let code, let message):
            return nsErrorForMutation(
                code: code, message: message, what: "read", log: log)
        case .temporary:
            return NSFileProviderError(.serverUnreachable)
        }
    }

    /// Render a mutation `LiveStoreError` at the write boundary.
    static func mutationError(
        for error: LiveStoreError, what: String, log: Logger
    ) -> NSFileProviderError {
        switch error {
        case .notFound:
            return NSFileProviderError(.noSuchItem)
        case .sync(let message):
            log.error("mutation \(what) failed: \(message, privacy: .public)")
            return NSFileProviderError(.cannotSynchronize)
        case .refused(let code, let message):
            return nsErrorForMutation(
                code: code, message: message, what: what, log: log)
        case .temporary(let message):
            log.error("mutation \(what) unavailable: \(message, privacy: .public)")
            return NSFileProviderError(.serverUnreachable)
        }
    }

    /// Whether the disk's base version still matches the owner's
    /// current item version. A first-sync base (never known in sync)
    /// skips the check — the owner precondition remains
    /// authoritative. Anything else must match byte for byte: the
    /// version moves with every owner coordinate, size, and token
    /// change, so a mismatch is a remote edit, never staleness to
    /// overwrite.
    static func baseVersionMatches(
        _ base: NSFileProviderItemVersion, current: LiveItem
    ) -> Bool {
        let currentVersion = current.itemVersion
        if base.contentVersion == NSFileProviderItemVersion.beforeFirstSyncComponent {
            return true
        }
        return base.contentVersion == currentVersion.contentVersion
    }
}
