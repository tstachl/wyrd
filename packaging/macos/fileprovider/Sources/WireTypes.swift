// Stage 2 owner-protocol types: the Swift mirror of the read
// subset of `wyrd-owner-client/src/wire.rs`, pinned against
// `vectors.json` by the Harness. Shapes must match serde exactly:
// externally tagged enums encode as single-key objects
// (`{"ok": {"Lookup": {...}}}`), and absent identity keys decode
// as `nil` (legacy entries) exactly like Rust's `#[serde(default)]`.
// Read-only: lookup, stat, readdir, read. Nothing here names a
// mutation operation.

import Foundation

/// `{"path": "<relative path>"}`, empty string for the drive root.
struct OwnerPathParams: Codable {
    var path: String
}

/// One framed request: version, client-matched id, operation name,
/// and path parameters (`null` where an op takes none — unused in
/// Stage 2, kept so the shape matches the wire).
struct OwnerRequest: Codable {
    var v: Int
    var id: UInt64
    var op: String
    var params: OwnerPathParams?
}

/// One framed response: the version, the echoed request id, and
/// exactly one of a result or a refusal.
struct OwnerResponse: Codable {
    var v: Int
    var id: UInt64
    var result: OwnerResult
}

/// `{"ok": {...}}` or `{"err": {...}}`, exactly one key.
enum OwnerResult: Codable {
    case ok(OwnerOk)
    case err(OwnerError)

    private enum Key: String, CodingKey {
        case ok
        case err
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        if container.contains(.ok) {
            self = .ok(try container.decode(OwnerOk.self, forKey: .ok))
        } else if container.contains(.err) {
            self = .err(try container.decode(OwnerError.self, forKey: .err))
        } else {
            throw DecodingError.dataCorrupted(
                DecodingError.Context(
                    codingPath: decoder.codingPath,
                    debugDescription: "result names neither ok nor err"
                )
            )
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .ok(let ok):
            try container.encode(ok, forKey: .ok)
        case .err(let err):
            try container.encode(err, forKey: .err)
        }
    }
}

/// The read results Stage 2 consumes. Each carries the serving
/// generation and durable revision it was served from, so the
/// adapter correlates answers instead of assuming recency.
enum OwnerOk: Codable {
    case lookup(node: OwnerNode, generation: UInt64, revision: UInt64)
    case stat(attr: OwnerAttr, generation: UInt64, revision: UInt64)
    case readdir(entries: [OwnerDirEntry], generation: UInt64, revision: UInt64)
    case read(bytesHex: String, generation: UInt64, revision: UInt64)

    private enum Key: String, CodingKey {
        case Lookup
        case Stat
        case Readdir
        case Read
    }

    private struct LookupBody: Codable {
        var node: OwnerNode
        var generation: UInt64
        var revision: UInt64
    }

    private struct StatBody: Codable {
        var attr: OwnerAttr
        var generation: UInt64
        var revision: UInt64
    }

    private struct ReaddirBody: Codable {
        var entries: [OwnerDirEntry]
        var generation: UInt64
        var revision: UInt64
    }

    private struct ReadBody: Codable {
        var bytes_hex: String
        var generation: UInt64
        var revision: UInt64
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        if container.contains(.Lookup) {
            let body = try container.decode(LookupBody.self, forKey: .Lookup)
            self = .lookup(node: body.node, generation: body.generation, revision: body.revision)
        } else if container.contains(.Stat) {
            let body = try container.decode(StatBody.self, forKey: .Stat)
            self = .stat(attr: body.attr, generation: body.generation, revision: body.revision)
        } else if container.contains(.Readdir) {
            let body = try container.decode(ReaddirBody.self, forKey: .Readdir)
            self = .readdir(entries: body.entries, generation: body.generation, revision: body.revision)
        } else if container.contains(.Read) {
            let body = try container.decode(ReadBody.self, forKey: .Read)
            self = .read(bytesHex: body.bytes_hex, generation: body.generation, revision: body.revision)
        } else {
            throw DecodingError.dataCorrupted(
                DecodingError.Context(
                    codingPath: decoder.codingPath,
                    debugDescription: "ok names no known read result"
                )
            )
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .lookup(let node, let generation, let revision):
            try container.encode(
                LookupBody(node: node, generation: generation, revision: revision),
                forKey: .Lookup
            )
        case .stat(let attr, let generation, let revision):
            try container.encode(
                StatBody(attr: attr, generation: generation, revision: revision),
                forKey: .Stat
            )
        case .readdir(let entries, let generation, let revision):
            try container.encode(
                ReaddirBody(entries: entries, generation: generation, revision: revision),
                forKey: .Readdir
            )
        case .read(let bytesHex, let generation, let revision):
            try container.encode(
                ReadBody(bytes_hex: bytesHex, generation: generation, revision: revision),
                forKey: .Read
            )
        }
    }
}

/// A defined refusal: a machine-readable code plus a human message.
/// Transport failures never produce one of these — a refusal is a
/// terminal answer to a parsed request.
struct OwnerError: Codable, Error {
    var code: String
    var message: String
}

/// The entry attribute: kind, size, executable bit, and the stable
/// token as lowercase hex — or `nil` for pre-identity entries and
/// synthetic states.
struct OwnerAttr: Codable {
    var kind: String
    var size: UInt64
    var executable: Bool
    var identity: String?
}

/// One directory row: the entry name and its node.
struct OwnerDirEntry: Codable {
    var name: String
    var node: OwnerNode
}

/// A namespace node. `mergedDir` reports the agreed token when every
/// merged head carries the identical token, else `nil`.
/// `conflict` versions carry their head's token each.
enum OwnerNode: Codable {
    case file(size: UInt64, executable: Bool, chunks: [String], identity: String?)
    case dir(subtree: String, identity: String?)
    case symlink(target: String, identity: String?)
    case mergedDir(subtrees: [[String]], identity: String?)
    case conflict(versions: [OwnerConflictVersion])

    private enum Key: String, CodingKey {
        case File
        case Dir
        case Symlink
        case MergedDir
        case Conflict
    }

    private struct FileBody: Codable {
        var size: UInt64
        var executable: Bool
        var chunks: [String]
        var identity: String?
    }

    private struct DirBody: Codable {
        var subtree: String
        var identity: String?
    }

    private struct SymlinkBody: Codable {
        var target: String
        var identity: String?
    }

    private struct MergedDirBody: Codable {
        var subtrees: [[String]]
        var identity: String?
    }

    private struct ConflictBody: Codable {
        var versions: [OwnerConflictVersion]
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        if container.contains(.File) {
            let body = try container.decode(FileBody.self, forKey: .File)
            self = .file(
                size: body.size,
                executable: body.executable,
                chunks: body.chunks,
                identity: body.identity
            )
        } else if container.contains(.Dir) {
            let body = try container.decode(DirBody.self, forKey: .Dir)
            self = .dir(subtree: body.subtree, identity: body.identity)
        } else if container.contains(.Symlink) {
            let body = try container.decode(SymlinkBody.self, forKey: .Symlink)
            self = .symlink(target: body.target, identity: body.identity)
        } else if container.contains(.MergedDir) {
            let body = try container.decode(MergedDirBody.self, forKey: .MergedDir)
            self = .mergedDir(subtrees: body.subtrees, identity: body.identity)
        } else if container.contains(.Conflict) {
            let body = try container.decode(ConflictBody.self, forKey: .Conflict)
            self = .conflict(versions: body.versions)
        } else {
            throw DecodingError.dataCorrupted(
                DecodingError.Context(
                    codingPath: decoder.codingPath,
                    debugDescription: "node names no known variant"
                )
            )
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .file(let size, let executable, let chunks, let identity):
            try container.encode(
                FileBody(size: size, executable: executable, chunks: chunks, identity: identity),
                forKey: .File
            )
        case .dir(let subtree, let identity):
            try container.encode(DirBody(subtree: subtree, identity: identity), forKey: .Dir)
        case .symlink(let target, let identity):
            try container.encode(
                SymlinkBody(target: target, identity: identity),
                forKey: .Symlink
            )
        case .mergedDir(let subtrees, let identity):
            try container.encode(
                MergedDirBody(subtrees: subtrees, identity: identity),
                forKey: .MergedDir
            )
        case .conflict(let versions):
            try container.encode(ConflictBody(versions: versions), forKey: .Conflict)
        }
    }
}

/// One conflict version: the head snapshot and its node.
struct OwnerConflictVersion: Codable {
    var snapshot: String
    var node: OwnerNode
}

/// Decode lowercase hex to bytes. Rejects odd lengths and any
/// character outside `0-9a-f` — validated per character, never via
/// a numeric parser that tolerates a leading `+` or uppercase.
enum HexCodec {
    static func decode(_ hex: String) throws -> [UInt8] {
        guard hex.count % 2 == 0 else {
            throw HexError.oddLength
        }
        func nibble(_ character: Character) throws -> UInt8 {
            switch character {
            case "0" ... "9":
                return character.asciiValue! - Character("0").asciiValue!
            case "a" ... "f":
                return 10 + character.asciiValue! - Character("a").asciiValue!
            default:
                throw HexError.badDigit(String(character))
            }
        }
        var bytes: [UInt8] = []
        bytes.reserveCapacity(hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let high = hex[index]
            let low = hex[hex.index(after: index)]
            bytes.append(try nibble(high) << 4 | nibble(low))
            index = hex.index(index, offsetBy: 2)
        }
        return bytes
    }

    enum HexError: Error {
        case oddLength
        case badDigit(String)
    }
}
