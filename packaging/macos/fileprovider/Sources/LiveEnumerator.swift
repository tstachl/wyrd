// Stage 2 enumerator over live owner state. One `readdir` is one
// enumeration in one owner coordinate, so every page shares the
// coordinate and the returned anchor always describes the completed
// enumeration. Conflict rows are excluded (logged, counted) and
// never break sibling enumeration.

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
        do {
            let (items, anchor) = try store.enumerate(container: container)
            observer.didEnumerate(items)
            observer.finishEnumerating(
                upTo: NSFileProviderPage(rawValue: Data(anchor.encode().utf8))
            )
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
