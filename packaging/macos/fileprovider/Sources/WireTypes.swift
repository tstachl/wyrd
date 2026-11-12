// Stage 2 owner-protocol types: the Swift mirror of the read
// subset of `wyrd-owner-client/src/wire.rs`, pinned against
// `vectors.json` by the Harness. Shapes must match serde exactly:
// externally tagged enums encode as single-key objects
// (`{"ok": {"Lookup": {...}}}`), and absent identity keys decode
// as `nil` (legacy entries) exactly like Rust's `#[serde(default)]`.
// Stage 2 covered lookup, stat, readdir, read. Stage 3 adds the
// mutation vocabulary — capture-parent, submit, and the nine
// mutation kinds with their required identity preconditions —
// spoken live by the mutation callbacks in sub-PR 2.

import Foundation

/// `{"path": "<relative path>"}`, empty string for the drive root.
struct OwnerPathParams: Codable {
    var path: String
}

/// One framed request: version, client-matched id, operation name,
/// and per-operation parameters (`null` where an op takes none).
/// Path-addressed calls carry `{"path": ...}`; `submit` carries
/// `{"kind": ...}`.
struct OwnerRequest: Codable {
    var v: Int
    var id: UInt64
    var op: String
    var params: OwnerParams?
}

/// The per-operation parameters, encoded as the inner document
/// (`{"path": ...}` or `{"kind": ...}`), never wrapped.
enum OwnerParams: Codable {
    case path(OwnerPathParams)
    case submit(OwnerSubmitParams)

    private enum Key: String, CodingKey {
        case kind
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        if container.contains(.kind) {
            self = .submit(try OwnerSubmitParams(from: decoder))
        } else {
            self = .path(try OwnerPathParams(from: decoder))
        }
    }

    func encode(to encoder: Encoder) throws {
        switch self {
        case .path(let params):
            try params.encode(to: encoder)
        case .submit(let params):
            try params.encode(to: encoder)
        }
    }
}

/// Parameters for `submit`: one of the nine mutation kinds.
struct OwnerSubmitParams: Codable {
    var kind: OwnerMutationKind
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

/// The results the adapter consumes. Read results carry the
/// serving generation and durable revision they were served from,
/// so the adapter correlates answers instead of assuming
/// recency. Mutation results are the queue's terminal answers.
enum OwnerOk: Codable {
    case lookup(node: OwnerNode, generation: UInt64, revision: UInt64)
    case stat(attr: OwnerAttr, generation: UInt64, revision: UInt64)
    case readdir(entries: [OwnerDirEntry], generation: UInt64, revision: UInt64)
    case read(bytesHex: String, generation: UInt64, revision: UInt64)
    case parentToken(OwnerParentToken)
    case submitted(OwnerMutationOutcome)

    private enum Key: String, CodingKey {
        case Lookup
        case Stat
        case Readdir
        case Read
        case ParentToken
        case Submitted
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

    private struct ParentTokenBody: Codable {
        var token: OwnerParentToken
    }

    private struct SubmittedBody: Codable {
        var outcome: OwnerMutationOutcome
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
        } else if container.contains(.ParentToken) {
            let body = try container.decode(ParentTokenBody.self, forKey: .ParentToken)
            self = .parentToken(body.token)
        } else if container.contains(.Submitted) {
            let body = try container.decode(SubmittedBody.self, forKey: .Submitted)
            self = .submitted(body.outcome)
        } else {
            throw DecodingError.dataCorrupted(
                DecodingError.Context(
                    codingPath: decoder.codingPath,
                    debugDescription: "ok names no known result"
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
        case .parentToken(let token):
            try container.encode(ParentTokenBody(token: token), forKey: .ParentToken)
        case .submitted(let outcome):
            try container.encode(SubmittedBody(outcome: outcome), forKey: .Submitted)
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

    /// Encode bytes as lowercase hex, preallocated. The write path
    /// stages up to 32 MiB, so per-byte `String(format:)` allocation
    /// is not the shape — one pass into a reserved buffer.
    static func encode(_ bytes: [UInt8]) -> String {
        let digits = Array("0123456789abcdef".utf8)
        var out = ""
        out.reserveCapacity(bytes.count * 2)
        for byte in bytes {
            out.append(Character(UnicodeScalar(digits[Int(byte >> 4)])))
            out.append(Character(UnicodeScalar(digits[Int(byte & 0x0F)])))
        }
        return out
    }
}

// MARK: - Stage 3 mutation types

// The wire form of `WireParentToken`: the queue counter bound to
// the owner instance that minted it. Never invented client-side,
// never reused across an owner restart — a token from a dead
// instance refuses as `StaleParent`.
struct OwnerParentToken: Codable {
    var instance: UInt64
    var counter: UInt64
}

// The wire form of `WireFileIdentity`: the exact content base a
// commit or guarded setattr sends against, and the exact identity
// a successful outcome reports.
struct OwnerFileIdentity: Codable {
    var size: UInt64
    var executable: Bool
    var chunks: [String]
}

// The wire form of `WireMutationKind`: all nine operations the
// owner forwards verbatim to the single queue. `expected_identity`
// is the caller's entry precondition (lowercase hex, required
// since wire v2): the owner validates it at execution inside the
// serialized queue and refuses a mismatch instead of landing on
// the wrong entry. Creations address absent paths and carry no
// entry precondition; their parent token covers the parent
// instead. `Fold` is mirrored for wire completeness but never
// sent by the adapter.
indirect enum OwnerMutationKind: Codable {
    case mkdir(path: String)
    case createFile(path: String, parent: OwnerParentToken)
    case commitFile(
        path: String,
        base: OwnerFileIdentity,
        executable: Bool,
        contentHex: String,
        expectedIdentity: String
    )
    case appendFile(path: String, contentHex: String, expectedIdentity: String)
    case unlink(path: String, expectedIdentity: String)
    case rmdir(path: String, expectedIdentity: String)
    case rename(from: String, to: String, noReplace: Bool, expectedIdentity: String)
    case setAttrs(
        path: String,
        size: UInt64?,
        executable: Bool?,
        base: OwnerFileIdentity?,
        expectedIdentity: String
    )
    case fold(members: [OwnerFoldMember])

    private enum Key: String, CodingKey {
        case Mkdir
        case CreateFile
        case CommitFile
        case AppendFile
        case Unlink
        case Rmdir
        case Rename
        case SetAttrs
        case Fold
    }

    private struct MkdirBody: Codable {
        var path: String
    }

    private struct CreateFileBody: Codable {
        var path: String
        var parent: OwnerParentToken
    }

    private struct CommitFileBody: Codable {
        var path: String
        var base: OwnerFileIdentity
        var executable: Bool
        var content_hex: String
        var expected_identity: String
    }

    private struct AppendFileBody: Codable {
        var path: String
        var content_hex: String
        var expected_identity: String
    }

    private struct UnlinkBody: Codable {
        var path: String
        var expected_identity: String
    }

    private struct RmdirBody: Codable {
        var path: String
        var expected_identity: String
    }

    private struct RenameBody: Codable {
        var from: String
        var to: String
        var no_replace: Bool
        var expected_identity: String
    }

    private struct SetAttrsBody: Codable {
        var path: String
        var size: UInt64?
        var executable: Bool?
        var base: OwnerFileIdentity?
        var expected_identity: String
    }

    private struct FoldBody: Codable {
        var members: [OwnerFoldMember]
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        if container.contains(.Mkdir) {
            let body = try container.decode(MkdirBody.self, forKey: .Mkdir)
            self = .mkdir(path: body.path)
        } else if container.contains(.CreateFile) {
            let body = try container.decode(CreateFileBody.self, forKey: .CreateFile)
            self = .createFile(path: body.path, parent: body.parent)
        } else if container.contains(.CommitFile) {
            let body = try container.decode(CommitFileBody.self, forKey: .CommitFile)
            self = .commitFile(
                path: body.path,
                base: body.base,
                executable: body.executable,
                contentHex: body.content_hex,
                expectedIdentity: body.expected_identity
            )
        } else if container.contains(.AppendFile) {
            let body = try container.decode(AppendFileBody.self, forKey: .AppendFile)
            self = .appendFile(
                path: body.path,
                contentHex: body.content_hex,
                expectedIdentity: body.expected_identity
            )
        } else if container.contains(.Unlink) {
            let body = try container.decode(UnlinkBody.self, forKey: .Unlink)
            self = .unlink(path: body.path, expectedIdentity: body.expected_identity)
        } else if container.contains(.Rmdir) {
            let body = try container.decode(RmdirBody.self, forKey: .Rmdir)
            self = .rmdir(path: body.path, expectedIdentity: body.expected_identity)
        } else if container.contains(.Rename) {
            let body = try container.decode(RenameBody.self, forKey: .Rename)
            self = .rename(
                from: body.from,
                to: body.to,
                noReplace: body.no_replace,
                expectedIdentity: body.expected_identity
            )
        } else if container.contains(.SetAttrs) {
            let body = try container.decode(SetAttrsBody.self, forKey: .SetAttrs)
            self = .setAttrs(
                path: body.path,
                size: body.size,
                executable: body.executable,
                base: body.base,
                expectedIdentity: body.expected_identity
            )
        } else if container.contains(.Fold) {
            let body = try container.decode(FoldBody.self, forKey: .Fold)
            self = .fold(members: body.members)
        } else {
            throw DecodingError.dataCorrupted(
                DecodingError.Context(
                    codingPath: decoder.codingPath,
                    debugDescription: "mutation kind names no known operation"
                )
            )
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .mkdir(let path):
            try container.encode(MkdirBody(path: path), forKey: .Mkdir)
        case .createFile(let path, let parent):
            try container.encode(CreateFileBody(path: path, parent: parent), forKey: .CreateFile)
        case .commitFile(let path, let base, let executable, let contentHex, let expectedIdentity):
            try container.encode(
                CommitFileBody(
                    path: path,
                    base: base,
                    executable: executable,
                    content_hex: contentHex,
                    expected_identity: expectedIdentity
                ),
                forKey: .CommitFile
            )
        case .appendFile(let path, let contentHex, let expectedIdentity):
            try container.encode(
                AppendFileBody(path: path, content_hex: contentHex, expected_identity: expectedIdentity),
                forKey: .AppendFile
            )
        case .unlink(let path, let expectedIdentity):
            try container.encode(
                UnlinkBody(path: path, expected_identity: expectedIdentity),
                forKey: .Unlink
            )
        case .rmdir(let path, let expectedIdentity):
            try container.encode(
                RmdirBody(path: path, expected_identity: expectedIdentity),
                forKey: .Rmdir
            )
        case .rename(let from, let to, let noReplace, let expectedIdentity):
            try container.encode(
                RenameBody(
                    from: from,
                    to: to,
                    no_replace: noReplace,
                    expected_identity: expectedIdentity
                ),
                forKey: .Rename
            )
        case .setAttrs(let path, let size, let executable, let base, let expectedIdentity):
            try container.encode(
                SetAttrsBody(
                    path: path,
                    size: size,
                    executable: executable,
                    base: base,
                    expected_identity: expectedIdentity
                ),
                forKey: .SetAttrs
            )
        case .fold(let members):
            try container.encode(FoldBody(members: members), forKey: .Fold)
        }
    }
}

// One folded member: a non-`Fold` operation plus whether it is the
// commit-forcing event. Nesting a fold inside a fold refuses at
// the wire.
struct OwnerFoldMember: Codable {
    var kind: OwnerMutationKind
    var forcer: Bool
}

// The wire form of `WireMutationOutcome`: the terminal answer to a
// submitted mutation. The adapter decodes the non-fold outcomes;
// a fold outcome is a protocol violation here because the adapter
// never submits folds.
enum OwnerMutationOutcome: Codable {
    case done
    case created(OwnerFileIdentity)
    case committed(OwnerFileIdentity)

    init(from decoder: Decoder) throws {
        if let single = try? decoder.singleValueContainer(),
            let tag = try? single.decode(String.self), tag == "Done"
        {
            self = .done
            return
        }
        let container = try decoder.container(keyedBy: OutcomeKey.self)
        if container.contains(.Created) {
            self = .created(try container.decode(OwnerFileIdentity.self, forKey: .Created))
        } else if container.contains(.Committed) {
            self = .committed(try container.decode(OwnerFileIdentity.self, forKey: .Committed))
        } else {
            throw DecodingError.dataCorrupted(
                DecodingError.Context(
                    codingPath: decoder.codingPath,
                    debugDescription: "mutation outcome names no known result"
                )
            )
        }
    }

    func encode(to encoder: Encoder) throws {
        switch self {
        case .done:
            var single = encoder.singleValueContainer()
            try single.encode("Done")
        case .created(let identity):
            var container = encoder.container(keyedBy: OutcomeKey.self)
            try container.encode(identity, forKey: .Created)
        case .committed(let identity):
            var container = encoder.container(keyedBy: OutcomeKey.self)
            try container.encode(identity, forKey: .Committed)
        }
    }

    private enum OutcomeKey: String, CodingKey {
        case Created
        case Committed
    }
}
