// Stage 1 stub: a canned backing model behind the File Provider
// item surface. Two children of the working set root — one file,
// one folder — with static versions. No daemon contact; the owner
// socket client (Stage 2) replaces every value here. Imports stay
// within the owner boundary pinned by
// `fileprovider_swift_stays_within_the_owner_boundary`.

import FileProvider
import Foundation
import UniformTypeIdentifiers

/// One canned item: the working set root, `hello.txt`, or `notes/`.
final class CannedItem: NSObject, NSFileProviderItem {
    /// The exact bytes `fetchContents` hydrates for `hello.txt`.
    /// Single source: `documentSize` and the hydrated file both
    /// derive from it, so Finder never materialises a 0-byte
    /// placeholder for content that exists.
    static let helloContents = "hello from the Wyrd spike\n"
    static let root = CannedItem(
        identifier: .rootContainer,
        parent: .rootContainer,
        filename: "Wyrd",
        contentType: .folder,
        documentSize: nil
    )
    static let hello = CannedItem(
        identifier: NSFileProviderItemIdentifier("hello.txt"),
        parent: .rootContainer,
        filename: "hello.txt",
        contentType: .plainText,
        documentSize: helloContents.utf8.count
    )
    static let notes = CannedItem(
        identifier: NSFileProviderItemIdentifier("notes"),
        parent: .rootContainer,
        filename: "notes",
        contentType: .folder,
        documentSize: nil
    )

    let itemIdentifier: NSFileProviderItemIdentifier
    let parentItemIdentifier: NSFileProviderItemIdentifier
    let filename: String
    let contentType: UTType
    /// Byte count for files, nil for folders. Passed per item at
    /// construction so a second canned file cannot silently borrow
    /// hello's size.
    private let size: Int?

    init(
        identifier: NSFileProviderItemIdentifier,
        parent: NSFileProviderItemIdentifier,
        filename: String,
        contentType: UTType,
        documentSize: Int?
    ) {
        self.itemIdentifier = identifier
        self.parentItemIdentifier = parent
        self.filename = filename
        self.contentType = contentType
        self.size = documentSize
        super.init()
    }

    var capabilities: NSFileProviderItemCapabilities {
        // Stage 1 is read-only: writing, renaming, trashing, and
        // deletion arrive with Stage 3 mutation.
        [.allowsReading]
    }

    var itemVersion: NSFileProviderItemVersion {
        NSFileProviderItemVersion(
            contentVersion: "stage1-canned".data(using: .utf8)!,
            metadataVersion: "stage1-canned".data(using: .utf8)!
        )
    }

    var documentSize: NSNumber? {
        // Without this Finder materialises a 0-byte placeholder for
        // hello.txt and the hydrated bytes never visibly arrive.
        guard let size else { return nil }
        return NSNumber(value: size)
    }
}

/// The Stage 1 canned lookup surface, kept for domains without a
/// drive id. Stage 2 serves live identifiers; this path only runs
/// when the domain is unconfigured.
enum CannedLookup {
    static func item(
        for identifier: NSFileProviderItemIdentifier,
        completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void
    ) {
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
    }

    static func fetchContents(
        for itemIdentifier: NSFileProviderItemIdentifier,
        completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
    ) {
        guard itemIdentifier == CannedItem.hello.itemIdentifier else {
            completionHandler(
                nil,
                nil,
                NSError(
                    domain: NSFileProviderErrorDomain,
                    code: NSFileProviderError.noSuchItem.rawValue
                )
            )
            return
        }
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("wyrd-hello-\(UUID().uuidString).txt")
        do {
            try CannedItem.helloContents.write(
                to: url,
                atomically: true,
                encoding: .utf8
            )
            completionHandler(url, CannedItem.hello, nil)
        } catch {
            completionHandler(nil, nil, error)
        }
    }
}
