// Stage 1 spike: the extension serves the canned model and probes
// the owner-socket path on launch. Note the protocol, not the
// class: the legacy `NSFileProviderExtension` class API is
// unavailable on macOS — the extension implements
// `NSFileProviderReplicatedExtension`, one instance per domain
// (hence per drive). Enumeration, lookup, and hydration serve
// canned data; mutation stays `fatalError` for Stage 3; the
// owner-socket client arrives in Stage 2. Imports stay within the
// owner boundary pinned by
// `fileprovider_swift_stays_within_the_owner_boundary`: platform
// frameworks and the reachability probe only, never store or
// engine.

import FileProvider
import os

final class FileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
    let domain: NSFileProviderDomain
    private let log = Logger(
        subsystem: "md.enesis.wyrd.fileprovider",
        category: "domain"
    )

    required init(domain: NSFileProviderDomain) {
        self.domain = domain
        super.init()
        // Stage 1 entry gate: the sandbox premise, logged where the
        // Console can see it. A stub listener (see SocketProbe) must
        // be waiting; the owner is not involved yet.
        log.info("socket probe: \(SocketProbe.run(), privacy: .public)")
    }

    func invalidate() {
        // Stage 1 owns teardown.
    }

    func enumerator(
        for containerItemIdentifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest
    ) throws -> NSFileProviderEnumerator {
        CannedEnumerator(container: containerItemIdentifier)
    }

    func item(
        for identifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        switch identifier {
        case .rootContainer:
            completionHandler(CannedItem.root, nil)
        case CannedItem.hello.itemIdentifier:
            completionHandler(CannedItem.hello, nil)
        case CannedItem.notes.itemIdentifier:
            completionHandler(CannedItem.notes, nil)
        default:
            completionHandler(
                nil,
                NSError(
                    domain: NSFileProviderErrorDomain,
                    code: NSFileProviderError.noSuchItem.rawValue
                )
            )
        }
        return Progress()
    }

    func fetchContents(
        for itemIdentifier: NSFileProviderItemIdentifier,
        version requestedVersion: NSFileProviderItemVersion?,
        request: NSFileProviderRequest,
        completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        guard itemIdentifier == CannedItem.hello.itemIdentifier else {
            completionHandler(
                nil,
                nil,
                NSError(
                    domain: NSFileProviderErrorDomain,
                    code: NSFileProviderError.noSuchItem.rawValue
                )
            )
            return Progress()
        }
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("wyrd-hello-\(UUID().uuidString).txt")
        do {
            try "hello from the Wyrd spike\n".write(
                to: url,
                atomically: true,
                encoding: .utf8
            )
            completionHandler(url, CannedItem.hello, nil)
        } catch {
            completionHandler(nil, nil, error)
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
        fatalError("Stage 3 owns creation")
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
        fatalError("Stage 3 owns mutation")
    }

    func deleteItem(
        identifier: NSFileProviderItemIdentifier,
        baseVersion: NSFileProviderItemVersion,
        options: NSFileProviderDeleteItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (Error?) -> Void
    ) -> Progress {
        fatalError("Stage 3 owns deletion")
    }
}
