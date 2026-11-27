// Stage 2 sync anchors: an opaque, versioned serialization of the
// owner snapshot coordinate (serving generation plus durable
// revision). The adapter translates owner-authoritative coordinates
// into anchors; it never invents a versioning scheme, a wall-clock
// timestamp, or a locally incremented counter. Anchors compare by
// equality only — ordering across generations is meaningless — and
// a parse failure means "re-enumerate from scratch", never a guess.

import FileProvider
import Foundation

/// `wyrd-anchor-v1:g<generation>:r<revision>`, both `UInt64`.
struct WyrdSyncAnchor: Equatable {
    var generation: UInt64
    var revision: UInt64

    static let prefix = "wyrd-anchor-v1:g"

    init(generation: UInt64, revision: UInt64) {
        self.generation = generation
        self.revision = revision
    }

    func encode() -> String {
        "\(Self.prefix)\(generation):r\(revision)"
    }

    static func decode(_ raw: String) -> WyrdSyncAnchor? {
        guard raw.hasPrefix(prefix) else { return nil }
        let rest = String(raw.dropFirst(prefix.count))
        let parts = rest.split(separator: ":", omittingEmptySubsequences: false)
        guard parts.count == 2,
            parts[1].hasPrefix("r"),
            let generation = UInt64(parts[0]),
            let revision = UInt64(String(parts[1].dropFirst()))
        else {
            return nil
        }
        return WyrdSyncAnchor(generation: generation, revision: revision)
    }

    /// Decode the framework's opaque anchor through this adapter's
    /// own grammar. A foreign or undecodable anchor is `nil`, never a
    /// guessed coordinate — the caller expires it explicitly.
    init?(syncAnchor: NSFileProviderSyncAnchor) {
        guard let raw = String(data: Data(syncAnchor.rawValue), encoding: .utf8),
            let decoded = WyrdSyncAnchor.decode(raw)
        else {
            return nil
        }
        self = decoded
    }
}
