// Stage 1 stub: an enumerator over the canned model. The root
// lists `hello.txt` and `notes/`; anything else is empty. Anchors
// are trivial — real sync state arrives with the owner client in
// Stage 2.

import FileProvider
import Foundation

final class CannedEnumerator: NSObject, NSFileProviderEnumerator {
    private let container: NSFileProviderItemIdentifier

    init(container: NSFileProviderItemIdentifier) {
        self.container = container
        super.init()
    }

    func enumerateItems(
        for observer: NSFileProviderEnumerationObserver,
        startingAt page: NSFileProviderPage
    ) {
        if container == .rootContainer {
            observer.didEnumerate([CannedItem.hello, CannedItem.notes])
        }
        observer.finishEnumerating(upTo: nil)
    }

    func invalidate() {
        // Nothing held; Stage 2 owns live state.
    }

    func currentSyncAnchor(
        completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void
    ) {
        completionHandler(
            NSFileProviderSyncAnchor("stage1-canned".data(using: .utf8)!)
        )
    }
}
