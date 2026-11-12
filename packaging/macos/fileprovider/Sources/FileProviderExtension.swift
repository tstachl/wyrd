// Stage 2 extension: live owner reads when the domain carries a
// drive id, canned Stage 1 data otherwise. The mode is per-domain
// and explicit in the log — canned is never a silent fallback for a
// misconfigured drive. Mutation stays read-only until Stage 3:
// every write path fails closed with `cannotSynchronize`, never a
// crash. The owner-socket client arrives in Stage 2 (Swift protocol
// client against the normative framing); the import surface stays
// within the owner boundary pinned by
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
        // Read-only until Stage 3: fail closed, never crash. A stray
        // system write (e.g. `.DS_Store`) must surface an error, not
        // a Finder-visible extension crash loop.
        completionHandler(
            nil, [], false, NSFileProviderError(.cannotSynchronize)
        )
        return Progress()
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
        completionHandler(
            nil, [], false, NSFileProviderError(.cannotSynchronize)
        )
        return Progress()
    }

    func deleteItem(
        identifier: NSFileProviderItemIdentifier,
        baseVersion: NSFileProviderItemVersion,
        options: NSFileProviderDeleteItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (Error?) -> Void
    ) -> Progress {
        completionHandler(NSFileProviderError(.cannotSynchronize))
        return Progress()
    }

    private func nsError(for error: LiveStoreError) -> NSFileProviderError {
        switch error {
        case .notFound:
            return NSFileProviderError(.noSuchItem)
        case .sync:
            return NSFileProviderError(.cannotSynchronize)
        case .temporary:
            return NSFileProviderError(.serverUnreachable)
        }
    }
}
