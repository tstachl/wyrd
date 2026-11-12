// Stage 2 sync anchors: an opaque, versioned serialization of the
// owner snapshot coordinate (serving generation plus durable
// revision). The adapter translates owner-authoritative coordinates
// into anchors; it never invents a versioning scheme, a wall-clock
// timestamp, or a locally incremented counter. Anchors compare by
// equality only — ordering across generations is meaningless — and
// a parse failure means "re-enumerate from scratch", never a guess.

import Foundation

/// `wyrd-anchor-v1:g<generation>:r<revision>`, both `UInt64`.
struct WyrdSyncAnchor: Equatable {
    var generation: UInt64
    var revision: UInt64

    static let prefix = "wyrd-anchor-v1:g"

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
}
