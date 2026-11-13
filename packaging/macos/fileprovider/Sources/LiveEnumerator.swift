// Stage 2 enumerator over live owner state, extended by the
// Stage 3 inspector: one `readdir` is one enumeration in one owner
// coordinate, so every page shares the coordinate and the returned
// anchor always describes the completed enumeration. Each conflict
// row yields one synthetic read-only report item (never ordinary
// content); siblings enumerate regardless.

import FileProvider
import Foundation
import os

final class LiveEnumerator: NSObject, NSFileProviderEnumerator {
    private let container: NSFileProviderItemIdentifier
    private let store: LiveStore
    private let log: Logger

    init(container: NSFileProviderItemIdentifier, store: LiveStore, log: Logger) {
        self.container = container
        self.store = store
        self.log = log
        super.init()
    }

    func enumerateItems(
        for observer: NSFileProviderEnumerationObserver,
        startingAt page: NSFileProviderPage
    ) {
        // Single-shot enumeration: the page carries no state because
        // one `readdir` serves the whole container at one coordinate.
        // Reports ride alongside ordinary items; both share the
        // anchor the enumeration served.
        do {
            let (items, reports, _) = try store.enumerate(container: container)
            var combined: [NSFileProviderItem] = []
            combined.reserveCapacity(items.count + reports.count)
            combined.append(contentsOf: items.map { $0 as NSFileProviderItem })
            combined.append(contentsOf: reports.map { $0 as NSFileProviderItem })
            observer.didEnumerate(combined)
            // Full enumeration, no continuation: every page serves
            // the whole container at its current coordinate, so no
            // page state survives the call. (An anchor-data page
            // fails the fetch persist leg-wide with a callback-free
            // 4101; the anchor itself stays live via
            // currentSyncAnchor, which the system consults
            // separately. Canned enumerations likewise finish nil.)
            observer.finishEnumerating(upTo: nil)
        } catch let error as LiveStoreError {
            observer.finishEnumeratingWithError(nsError(for: error))
        } catch {
            log.error("unexpected enumeration failure: \(String(describing: error))")
            observer.finishEnumeratingWithError(NSFileProviderError(.cannotSynchronize))
        }
    }

    func currentSyncAnchor(
        completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void
    ) {
        guard let anchor = store.currentAnchor() else {
            completionHandler(nil)
            return
        }
        completionHandler(NSFileProviderSyncAnchor(Data(anchor.encode().utf8)))
    }

    func invalidate() {
        // No per-enumerator state survives a call: the store owns
        // the resolver, and connections are per-operation.
    }

    private func nsError(for error: LiveStoreError) -> NSFileProviderError {
        switch error {
        case .notFound:
            return NSFileProviderError(.noSuchItem)
        case .sync:
            return NSFileProviderError(.cannotSynchronize)
        case .refused:
            // Mutations never run through the enumerator; a refusal
            // here is a protocol surprise, failed closed.
            return NSFileProviderError(.cannotSynchronize)
        case .cancelled:
            // Enumerations are not cancellable through this table;
            // a cancel here is a protocol surprise, failed closed.
            return NSFileProviderError(.cannotSynchronize)
        case .temporary:
            return NSFileProviderError(.serverUnreachable)
        }
    }
}

/// The working set and trash have no owner rows in a read-only
/// Stage 2 domain: enumerate them empty rather than erroring on
/// every system sync. Item identity for these containers stays
/// not-found — they are not entries.
final class EmptyEnumerator: NSObject, NSFileProviderEnumerator {
    func enumerateItems(
        for observer: NSFileProviderEnumerationObserver,
        startingAt page: NSFileProviderPage
    ) {
        observer.didEnumerate([])
        observer.finishEnumerating(upTo: nil)
    }

    func invalidate() {
        // Nothing held.
    }
}
