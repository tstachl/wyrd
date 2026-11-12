// Stage 3 live item: an `NSFileProviderItem` translated from an
// owner answer, never invented. Identity comes from the owner's
// token (stable) or the enumerated path (legacy); versions derive
// from the owner coordinate, so a rename, edit, or restart moves
// the item's version exactly when the owner state moves. Writable
// exactly where the owner accepts a precondition-bound write:
// token-bearing files and directories. Symlinks, legacy rows,
// merged-disagreement rows, and conflicts stay read-only; trash
// is never advertised anywhere.

import FileProvider
import Foundation
import UniformTypeIdentifiers

/// One enumerated row with its owner coordinate.
struct LiveRow {
    var name: String
    var node: OwnerNode
    var generation: UInt64
    var revision: UInt64
}

/// A File Provider item over a live owner row.
final class LiveItem: NSObject, NSFileProviderItem {
    let itemIdentifier: NSFileProviderItemIdentifier
    let parentItemIdentifier: NSFileProviderItemIdentifier
    let filename: String
    /// The owner-relative path this item was built from: the rename
    /// source of truth a modify-then-write flow carries forward.
    let relativePath: String
    let contentType: UTType
    let isDirectory: Bool
    /// Presentation-internal symlink marker: File Provider has no
    /// symlink kind, so symlinks present as ordinary files whose
    /// hydration fails closed. Not the system's `isSymbolicLink` —
    /// nothing here claims that property.
    let isSymlinkRow: Bool
    private let size: UInt64?
    /// Whether the owner accepts precondition-bound writes to this
    /// row: token-bearing files and directories only. Symlinks,
    /// legacy rows, and merged-disagreement rows are read-only —
    /// the mutation surface fails them closed, and this keeps the
    /// UI from offering what the owner refuses.
    private let isWritableRow: Bool
    private let contentVersionData: Data
    private let metadataVersionData: Data

    /// Translate one row at its enumerated relative path. Returns
    /// `nil` for rows Stage 2 excludes: conflict rows never surface
    /// as ordinary content (the enumerator logs them and keeps
    /// serving siblings).
    init?(
        row: LiveRow,
        relativePath: String,
        identifier: NSFileProviderItemIdentifier,
        parent: NSFileProviderItemIdentifier
    ) {
        let identityTag: String
        var fileSize: UInt64?
        var directory = false
        var symlink = false
        let writable: Bool
        switch row.node {
        case .file(let size, _, _, let identity):
            fileSize = size
            writable = identity != nil
            identityTag = Self.identityTag(identity: identity, relativePath: relativePath)
        case .dir(_, let identity):
            directory = true
            writable = identity != nil
            identityTag = Self.identityTag(identity: identity, relativePath: relativePath)
        case .symlink(_, let identity):
            // Presented under the entry's name with a data type;
            // hydration fails closed (symlink presentation is a
            // Stage 3 question, and fabricated content would lie).
            // Never writable: the owner has no symlink authoring.
            symlink = true
            writable = false
            identityTag = Self.identityTag(identity: identity, relativePath: relativePath)
        case .mergedDir(_, let identity):
            directory = true
            writable = identity != nil
            identityTag = Self.identityTag(identity: identity, relativePath: relativePath)
        case .conflict:
            return nil
        }
        self.itemIdentifier = identifier
        self.parentItemIdentifier = parent
        self.filename = row.name
        self.relativePath = relativePath
        self.isDirectory = directory
        self.isSymlinkRow = symlink
        if directory {
            self.contentType = .folder
        } else if let ext = row.name.split(separator: ".").last.map(String.init),
            let mapped = UTType(filenameExtension: ext)
        {
            self.contentType = mapped
        } else {
            self.contentType = .data
        }
        self.size = fileSize
        self.isWritableRow = writable
        // Identity is content identity's opposite: the token is
        // stable across content changes, so the version must move
        // with the owner coordinate (and size, which the owner
        // reports per answer) — never with a local counter.
        let versionCore =
            "wyrd-cv1:\(row.generation):\(row.revision):\(fileSize.map(String.init) ?? "-"):\(identityTag)"
        self.contentVersionData = Data(versionCore.utf8)
        self.metadataVersionData = Data("wyrd-mv1:\(row.generation):\(row.revision)".utf8)
        super.init()
    }

    /// The version-stable tag for the row's identity: the owner's
    /// token when reported, else the legacy path marker. Legacy
    /// entries are unstable by contract (rename means a new item),
    /// so the path is exactly the right tag.
    private static func identityTag(identity: String?, relativePath: String) -> String {
        if let identity {
            return "i:\(identity.lowercased())"
        }
        return "l:\(relativePath)"
    }

    var capabilities: NSFileProviderItemCapabilities {
        // The root accepts children but is never renamed or
        // deleted. Token-bearing files and directories accept the
        // full conservative set (write covers adding subitems for
        // directories); symlinks, legacy rows, and
        // merged-disagreement rows stay read-only. Trash is never
        // advertised: delete is permanent, trash moves fail closed.
        if itemIdentifier == .rootContainer {
            return [.allowsReading, .allowsWriting]
        }
        guard isWritableRow else {
            return [.allowsReading]
        }
        return [
            .allowsReading, .allowsWriting, .allowsRenaming,
            .allowsReparenting, .allowsDeleting,
        ]
    }

    var itemVersion: NSFileProviderItemVersion {
        NSFileProviderItemVersion(
            contentVersion: contentVersionData,
            metadataVersion: metadataVersionData
        )
    }

    var documentSize: NSNumber? {
        guard let size, !isDirectory, !isSymlinkRow else { return nil }
        return NSNumber(value: size)
    }
}
