// Stage 2 item identifiers: versioned, reversible, drive-scoped.
// A stable entry maps to `wyrd:v1:<drive>:<identity>` where both
// components are lowercase hex (64 chars each: 32-byte DriveId,
// 256-bit EntryIdentity), validated on parse. No path, content id,
// storage id, or mutable metadata participates — the identifier
// names the entry, and the entry's path stays mutable metadata.
// Reversible does not mean interpretable: Finder treats the string
// as opaque; only the adapter parses it, to reject cross-drive
// identifiers and to resolve legacy entries without a lookup table.
//
// The adapter may construct presentation identifiers. It never
// manufactures EntryIdentity: legacy and merged-disagreement rows
// use the separate `wyrd:legacy:v1:` namespace, which is explicitly
// unstable (rename means a new identifier) and never written back.

import Foundation

/// The parsed form of a Stage 2 item identifier.
enum WyrdItemIdentifier: Equatable {
    /// An identity-bearing entry: both components validated.
    case stable(driveHex: String, identityHex: String)
    /// A row without owner identity (legacy entry or merged
    /// disagreement): the current relative path, hex-encoded.
    /// Unstable by contract — never an EntryIdentity. The reference
    /// resolves only while the path stays identity-less: minting an
    /// identity retires it (a rename moves the path, a delete ends
    /// it, a recreate mints — and minting ends the legacy name), so
    /// a replacement never serves under the old identifier.
    case legacy(pathHex: String)
    /// A synthetic conflict report for a conflicted logical
    /// location: the drive plus the conflicted relative path,
    /// hex-encoded. Deterministic per (drive, path) — never a peer
    /// identity, never an arbitrary head's EntryIdentity. The
    /// identifier names the location, not the conflict's content:
    /// invalidation rides the item version (which moves with the
    /// conflict's versions) and disappearance (resolution 404s).
    case conflict(driveHex: String, pathHex: String)

    /// The identifier prefix for identity-bearing entries.
    static let stablePrefix = "wyrd:v1:"
    /// The identifier prefix for rows without owner identity.
    static let legacyPrefix = "wyrd:legacy:v1:"
    /// The identifier prefix for synthetic conflict reports.
    static let conflictPrefix = "wyrd:conflict:v1:"

    /// Parse a raw identifier string for the configured drive.
    /// Cross-drive identifiers fail: `nil` means "not ours", never
    /// a fallback. `nil` also covers malformed strings and a `None`
    /// encoded as a zero identity — that encoding is forbidden.
    static func parse(_ raw: String, driveHex: String) -> WyrdItemIdentifier? {
        if raw.hasPrefix(stablePrefix) {
            let rest = String(raw.dropFirst(stablePrefix.count))
            let parts = rest.split(separator: ":", omittingEmptySubsequences: false)
            guard parts.count == 2 else { return nil }
            let drive = String(parts[0])
            let identity = String(parts[1])
            guard drive == driveHex.lowercased(),
                isLowerHex(drive, count: 64),
                isLowerHex(identity, count: 64),
                identity != String(repeating: "0", count: 64)
            else {
                return nil
            }
            return .stable(driveHex: drive, identityHex: identity)
        }
        if raw.hasPrefix(legacyPrefix) {
            let pathHex = String(raw.dropFirst(legacyPrefix.count))
            guard !pathHex.isEmpty,
                isLowerHex(pathHex, count: nil),
                (try? HexCodec.decode(pathHex)) != nil
            else {
                return nil
            }
            return .legacy(pathHex: pathHex)
        }
        if raw.hasPrefix(conflictPrefix) {
            let rest = String(raw.dropFirst(conflictPrefix.count))
            let parts = rest.split(separator: ":", omittingEmptySubsequences: false)
            guard parts.count == 2 else { return nil }
            let drive = String(parts[0])
            let pathHex = String(parts[1])
            guard drive == driveHex.lowercased(),
                isLowerHex(drive, count: 64)
            else {
                return nil
            }
            guard conflictPath(pathHex: pathHex) != nil else {
                return nil
            }
            return .conflict(driveHex: drive, pathHex: pathHex)
        }
        return nil
    }

    /// Render a stable identifier for an identity-bearing node.
    static func stableID(driveHex: String, identityHex: String) -> String {
        "\(stablePrefix)\(driveHex.lowercased()):\(identityHex.lowercased())"
    }

    /// Render a legacy identifier for a relative path. The path is
    /// hex-encoded UTF-8, so any valid component survives the
    /// round-trip without escaping rules.
    static func legacyID(path: String) -> String {
        let hex = path.utf8.map { String(format: "%02x", $0) }.joined()
        return "\(legacyPrefix)\(hex)"
    }

    /// Decode a legacy identifier back to its relative path.
    static func legacyPath(pathHex: String) -> String? {
        guard let bytes = try? HexCodec.decode(pathHex) else { return nil }
        let data = Data(bytes)
        guard let path = String(data: data, encoding: .utf8),
            !path.isEmpty,
            !path.contains("\0")
        else {
            return nil
        }
        return path
    }

    /// Render a synthetic conflict-report identifier for a conflicted
    /// relative path. The path is hex-encoded UTF-8, like the legacy
    /// form, but the namespace is separate and drive-scoped: a report
    /// is keyed by (drive, logical location), never by content.
    static func conflictID(driveHex: String, path: String) -> String {
        let hex = path.utf8.map { String(format: "%02x", $0) }.joined()
        return "\(conflictPrefix)\(driveHex.lowercased()):\(hex)"
    }

    /// Decode a conflict identifier back to its conflicted relative
    /// path. Same grammar as the legacy form; a separate function so
    /// the two namespaces never share a decoder.
    static func conflictPath(pathHex: String) -> String? {
        guard let bytes = try? HexCodec.decode(pathHex) else { return nil }
        let data = Data(bytes)
        guard let path = String(data: data, encoding: .utf8),
            !path.isEmpty,
            !path.contains("\0")
        else {
            return nil
        }
        return path
    }

    private static func isLowerHex(_ text: String, count: Int?) -> Bool {
        if let count, text.count != count {
            return false
        }
        guard !text.isEmpty else { return false }
        return text.allSatisfy { $0.isHexDigit && !$0.isUppercase }
    }
}
