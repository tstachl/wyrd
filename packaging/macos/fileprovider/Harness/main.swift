// Stage 2 Harness: exercises the Swift adapter surface without
// fileproviderd — golden-vector decoding, identifier and anchor
// grammar, and a live store round-trip against the stub owner
// (`stub_owner.py`, started by `run-harness.sh`). Compiled with the
// adapter sources into a CLI binary; never part of the appex.

import Dispatch
import FileProvider
import Foundation
import os

var failures = 0

func check(_ condition: Bool, _ message: String) {
    if !condition {
        print("FAIL: \(message)")
        failures += 1
    }
}

func checkEqual<T: Equatable>(_ left: T, _ right: T, _ message: String) {
    if left != right {
        print("FAIL: \(message) (left \(left), right \(right))")
        failures += 1
    }
}

// MARK: - Golden vectors

guard CommandLine.arguments.count == 3 else {
    print("usage: harness <vectors.json> <socket-path>")
    exit(2)
}
let vectorsURL = URL(fileURLWithPath: CommandLine.arguments[1])
let socketPath = CommandLine.arguments[2]
let vectorsData = try Data(contentsOf: vectorsURL)
let vectors = try JSONSerialization.jsonObject(with: vectorsData) as! [[String: Any]]

func vector(named name: String) -> [String: Any] {
    guard let found = vectors.first(where: { ($0["name"] as? String) == name }) else {
        print("FAIL: vector missing: \(name)")
        failures += 1
        return [:]
    }
    return found["json"] as! [String: Any]
}

func reencode(_ json: [String: Any]) -> Data {
    try! JSONSerialization.data(withJSONObject: json, options: [.sortedKeys])
}

let decoder = JSONDecoder()

// Every request vector decodes; the op names the operation.
for name in ["lookup", "stat", "readdir", "read", "capture-parent", "submit", "submit-create", "submit-commit"] {
    let raw = reencode(vector(named: "\(name)/request"))
    do {
        let request = try decoder.decode(OwnerRequest.self, from: raw)
        checkEqual(request.v, 2, "\(name) request version")
        let kind = String(name.split(separator: "/")[0])
        let expectedOp: String
        switch kind {
        case "submit-create", "submit-commit":
            expectedOp = "submit"
        default:
            expectedOp = kind
        }
        checkEqual(request.op, expectedOp, "\(name) request op")
        checkEqual(request.id, 7, "\(name) request id")
    } catch {
        print("FAIL: \(name)/request decodes: \(error)")
        failures += 1
    }
}

// The lookup request frames byte-for-byte: length prefix plus the
// canonical body both sides agree on.
do {
    let raw = reencode(vector(named: "lookup/request"))
    let request = try decoder.decode(OwnerRequest.self, from: raw)
    let body = try JSONEncoder().encode(request)
    let expectedJSON = "{\"v\":2,\"id\":7,\"op\":\"lookup\",\"params\":{\"path\":\"hello.txt\"}}"
    let bodyJSON = try JSONSerialization.jsonObject(with: body)
    let expectedParsed = try JSONSerialization.jsonObject(with: Data(expectedJSON.utf8))
    check(
        (bodyJSON as? NSDictionary)?.isEqual(to: expectedParsed as! [AnyHashable: Any]) ?? false,
        "lookup request encodes the canonical document")
} catch {
    print("FAIL: lookup framing: \(error)")
    failures += 1
}

// Every Stage 2 answer vector decodes to its own shape.
do {
    let stat = try decoder.decode(
        OwnerResponse.self, from: reencode(vector(named: "stat/ok")))
    if case .ok(let ok) = stat.result, case .stat(let attr, let gen, let rev) = ok {
        checkEqual(attr.identity?.count, 64, "stat carries the token")
        checkEqual(gen, 3, "stat generation")
        checkEqual(rev, 9, "stat revision")
    } else {
        print("FAIL: stat/ok shape")
        failures += 1
    }
    let readdir = try decoder.decode(
        OwnerResponse.self, from: reencode(vector(named: "readdir/ok")))
    if case .ok(let ok) = readdir.result, case .readdir(let entries, _, _) = ok {
        checkEqual(entries.count, 6, "readdir row count")
        let kinds = entries.map { entry -> String in
            switch entry.node {
            case .file: return "file"
            case .dir: return "dir"
            case .symlink: return "symlink"
            case .mergedDir: return "merged"
            case .conflict: return "conflict"
            }
        }
        checkEqual(kinds, ["file", "dir", "file", "merged", "merged", "conflict"], "readdir kinds")
        check(entries.contains { if case .file(_, _, _, let id) = $0.node { return id == nil } else { return false } },
            "readdir keeps a legacy row without an identity key")
        check(entries.contains { if case .mergedDir(_, let id) = $0.node { return id == nil } else { return false } },
            "readdir keeps a merged-disagreed row with a null identity")
    } else {
        print("FAIL: readdir/ok shape")
        failures += 1
    }
    let read = try decoder.decode(
        OwnerResponse.self, from: reencode(vector(named: "read/ok")))
    if case .ok(let ok) = read.result, case .read(let hex, let gen, let rev) = ok {
        checkEqual(try HexCodec.decode(hex).count, 5, "read bytes decode")
        checkEqual(gen, 3, "read generation")
        checkEqual(rev, 9, "read revision")
    } else {
        print("FAIL: read/ok shape")
        failures += 1
    }
} catch {
    print("FAIL: answer vectors decode: \(error)")
    failures += 1
}

// Every refusal vector decodes to its own code.
for (shape, code) in [
    ("not-found", "NotFound"), ("too-large", "TooLarge"), ("timeout", "Timeout"),
    ("conflicted", "Conflicted"), ("unavailable", "Unavailable"),
] {
    do {
        let response = try decoder.decode(
            OwnerResponse.self, from: reencode(vector(named: "refused/\(shape)")))
        if case .err(let err) = response.result {
            checkEqual(err.code, code, "refused/\(shape) code")
        } else {
            print("FAIL: refused/\(shape) is a refusal")
            failures += 1
        }
    } catch {
        print("FAIL: refused/\(shape) decodes: \(error)")
        failures += 1
    }
}

// MARK: - Identifiers, anchors, config

let driveHex = String(repeating: "42", count: 32)
let identityHex = String(repeating: "ab12cd34", count: 8)
checkEqual(identityHex.count, 64, "test token width")

let stable = WyrdItemIdentifier.stableID(driveHex: driveHex, identityHex: identityHex)
if case .stable(let drive, let identity) = WyrdItemIdentifier.parse(stable, driveHex: driveHex) {
    checkEqual(drive, driveHex, "stable drive round-trips")
    checkEqual(identity, identityHex.lowercased(), "stable token round-trips")
} else {
    print("FAIL: stable identifier parses")
    failures += 1
}
check(WyrdItemIdentifier.parse(stable, driveHex: String(repeating: "43", count: 32)) == nil,
    "cross-drive identifiers are rejected")
let zeroStable = WyrdItemIdentifier.stableID(driveHex: driveHex, identityHex: String(repeating: "0", count: 64))
check(WyrdItemIdentifier.parse(zeroStable, driveHex: driveHex) == nil,
    "a zero identity never encodes None")
check(WyrdItemIdentifier.parse("wyrd:v1:short", driveHex: driveHex) == nil,
    "malformed stable identifiers are rejected")
check(WyrdItemIdentifier.parse("wyrd:v9:\(driveHex):\(identityHex)", driveHex: driveHex) == nil,
    "unknown identifier versions are rejected")

let legacy = WyrdItemIdentifier.legacyID(path: "notes/nested.txt")
if case .legacy(let pathHex) = WyrdItemIdentifier.parse(legacy, driveHex: driveHex) {
    checkEqual(WyrdItemIdentifier.legacyPath(pathHex: pathHex), "notes/nested.txt", "legacy path round-trips")
} else {
    print("FAIL: legacy identifier parses")
    failures += 1
}

let anchor = WyrdSyncAnchor(generation: 3, revision: 9)
if let decoded = WyrdSyncAnchor.decode(anchor.encode()) {
    checkEqual(decoded, anchor, "anchor round-trips")
} else {
    print("FAIL: anchor parses")
    failures += 1
}
check(WyrdSyncAnchor.decode("wyrd-anchor-v1:g3") == nil, "truncated anchors are rejected")
check(WyrdSyncAnchor.decode("wyrd-anchor-v0:g3:r9") == nil, "unknown anchor versions are rejected")

checkEqual(SocketConfig.domainDrive(driveHex), driveHex, "domain drive parses")
check(SocketConfig.domainDrive("short") == nil, "short domain ids are rejected")
check(SocketConfig.domainDrive(String(repeating: "0", count: 64)) == nil, "zero domain ids are rejected")
check(SocketConfig.domainDrive(driveHex.uppercased()) == driveHex, "domain ids normalize case")

// MARK: - Alias resolution against a scratch container

do {
    let base = FileManager.default.temporaryDirectory
        .appendingPathComponent("wyrd-harness-\(ProcessInfo.processInfo.processIdentifier)")
    let sockets = base.appendingPathComponent("Sockets")
    let byDrive = sockets.appendingPathComponent("by-drive")
    try FileManager.default.createDirectory(at: byDrive, withIntermediateDirectories: true)
    let fingerprint = "399a8bbca022f440.sock"
    let real = sockets.appendingPathComponent(fingerprint)
    // A bound socket proves nothing here; only the path shape
    // matters, so a regular file stands in for the listener.
    FileManager.default.createFile(atPath: real.path, contents: Data())
    // Note: the string-path link creation below is deliberate.
    // `createSymbolicLink(at:withDestinationURL:)` absolutizes a
    // relative destination against the working directory; the
    // daemon publishes the literal relative target, so the Harness
    // must store the literal too.
    try FileManager.default.createSymbolicLink(
        atPath: byDrive.appendingPathComponent("\(driveHex).sock").path,
        withDestinationPath: "../\(fingerprint)")
    let resolved = try SocketConfig.resolve(container: base, driveHex: driveHex)
    checkEqual(resolved, real.path, "the alias resolves to the sibling socket")
    // Negative shapes fail closed, never to a guessed path.
    do {
        _ = try SocketConfig.resolve(container: base, driveHex: String(repeating: "98", count: 32))
        print("FAIL: a missing alias resolves")
        failures += 1
    } catch let error as SocketConfigError {
        if case .aliasMissing = error {
            // Expected.
        } else {
            print("FAIL: a missing alias fails as aliasMissing, got \(error)")
            failures += 1
        }
    } catch {
        print("FAIL: a missing alias fails as aliasMissing, got \(error)")
        failures += 1
    }
    try FileManager.default.removeItem(at: byDrive.appendingPathComponent("\(driveHex).sock"))
    try FileManager.default.createSymbolicLink(
        atPath: byDrive.appendingPathComponent("\(driveHex).sock").path,
        withDestinationPath: "not-a-fingerprint.sock")
    do {
        _ = try SocketConfig.resolve(container: base, driveHex: driveHex)
        print("FAIL: a non-fingerprint target resolves")
        failures += 1
    } catch let error as SocketConfigError {
        if case .badTarget = error {
            // Expected.
        } else {
            print("FAIL: a non-fingerprint target fails as badTarget, got \(error)")
            failures += 1
        }
    } catch {
        print("FAIL: a non-fingerprint target fails as badTarget, got \(error)")
        failures += 1
    }
    try FileManager.default.removeItem(at: byDrive.appendingPathComponent("\(driveHex).sock"))
    try FileManager.default.createSymbolicLink(
        atPath: byDrive.appendingPathComponent("\(driveHex).sock").path,
        withDestinationPath: "../../escape.sock")
    do {
        _ = try SocketConfig.resolve(container: base, driveHex: driveHex)
        print("FAIL: an escaping alias resolves")
        failures += 1
    } catch let error as SocketConfigError {
        if case .aliasEscapes = error {
            // Expected.
        } else {
            print("FAIL: an escaping alias fails as aliasEscapes, got \(error)")
            failures += 1
        }
    } catch {
        print("FAIL: an escaping alias fails as aliasEscapes, got \(error)")
        failures += 1
    }
    try FileManager.default.removeItem(at: base)
} catch {
    print("FAIL: alias resolution: \(error)")
    failures += 1
}

// MARK: - Live store against the stub

let log = Logger(subsystem: "md.enesis.wyrd.harness", category: "harness")
let store = LiveStore(driveHex: driveHex, log: log, socketPathOverride: socketPath)

do {
    let (items, storeAnchor) = try store.enumerate(container: .rootContainer)
    checkEqual(storeAnchor, WyrdSyncAnchor(generation: 3, revision: 9), "enumeration anchor is the owner coordinate")
    checkEqual(items.count, 5, "conflict rows are excluded, siblings enumerate")
    check(!items.contains { $0.filename == "split.txt" }, "the conflicted name never surfaces")
    if let hello = items.first(where: { $0.filename == "hello.txt" }) {
        checkEqual(hello.itemIdentifier.rawValue, stable, "hello.txt carries the stable identifier")
        checkEqual(hello.documentSize?.uint64Value, 26, "hello.txt size comes from the owner")
        let lookedUp = try store.item(for: hello.itemIdentifier)
        checkEqual(lookedUp.filename, "hello.txt", "lookup resolves the stable identifier")
        let (bytes, _) = try store.contents(for: hello.itemIdentifier)
        checkEqual(String(data: bytes, encoding: .utf8), "hello", "hydration returns the owner bytes")
    } else {
        print("FAIL: hello.txt enumerates")
        failures += 1
    }
    if items.first(where: { $0.filename == "legacy.bin" }) == nil {
        print("FAIL: legacy.bin enumerates")
        failures += 1
    }
    // The conflicted classification: the stub's conflict version
    // carries a token nothing else reports, so seeking it must fail
    // closed — never 404-as-deletion, never content.
    let ghost = NSFileProviderItemIdentifier(
        WyrdItemIdentifier.stableID(driveHex: driveHex, identityHex: String(repeating: "99", count: 32)))
    do {
        _ = try store.item(for: ghost)
        print("FAIL: conflicted identifiers fail closed")
        failures += 1
    } catch let error as LiveStoreError {
        if case .sync = error {
            // Expected: the entry exists and is conflicted.
        } else {
            print("FAIL: conflicted identifiers fail sync-closed, got \(error)")
            failures += 1
        }
    }
    // Absent identifiers 404.
    let absent = NSFileProviderItemIdentifier(
        WyrdItemIdentifier.stableID(driveHex: driveHex, identityHex: String(repeating: "98", count: 32)))
    do {
        _ = try store.item(for: absent)
        print("FAIL: absent identifiers 404")
        failures += 1
    } catch let error as LiveStoreError {
        if case .notFound = error {
            // Expected.
        } else {
            print("FAIL: absent identifiers 404, got \(error)")
            failures += 1
        }
    }
} catch {
    print("FAIL: live store round-trip: \(error)")
    failures += 1
}

// The owner error table maps without collapsing distinct refusals.
check({
    if case .notFound = store.mapError(OwnerClientError.refused(OwnerError(code: "NotFound", message: "x")), what: "t") { return true }
    return false
}(), "NotFound maps to notFound")
check({
    if case .sync = store.mapError(OwnerClientError.refused(OwnerError(code: "Conflicted", message: "x")), what: "t") { return true }
    return false
}(), "Conflicted maps to sync")
check({
    if case .sync = store.mapError(OwnerClientError.refused(OwnerError(code: "TooLarge", message: "x")), what: "t") { return true }
    return false
}(), "TooLarge maps to sync")
check({
    if case .sync = store.mapError(OwnerClientError.refused(OwnerError(code: "Timeout", message: "x")), what: "t") { return true }
    return false
}(), "Timeout maps to sync")
check({
    if case .temporary = store.mapError(OwnerClientError.refused(OwnerError(code: "Unavailable", message: "x")), what: "t") { return true }
    return false
}(), "Unavailable maps to temporary")
check({
    if case .temporary = store.mapError(OwnerClientError.unavailable("down"), what: "t") { return true }
    return false
}(), "transport breaks map to temporary")
check({
    if case .sync = store.mapError(OwnerClientError.incompatible("bad"), what: "t") { return true }
    return false
}(), "protocol violations map to sync")

// MARK: - Resolver concurrency and budget

// File Provider callbacks overlap: enumeration, item, and content
// requests share one resolver. Hammer it concurrently — under
// Thread Sanitizer this fails on an unserialized map.
do {
    let group = DispatchGroup()
    let queue = DispatchQueue.global(qos: .utility)
    let lock = NSLock()
    var concurrentFailures = 0
    for i in 0 ..< 20 {
        queue.async(group: group) {
            do {
                if i % 3 == 0 {
                    _ = try store.enumerate(container: .rootContainer)
                } else if i % 3 == 1 {
                    _ = try store.item(for: NSFileProviderItemIdentifier(stable))
                } else {
                    _ = try store.contents(for: NSFileProviderItemIdentifier(stable))
                }
            } catch {
                lock.withLock { concurrentFailures += 1 }
                print("FAIL: concurrent store use: \(error)")
            }
        }
    }
    group.wait()
    checkEqual(concurrentFailures, 0, "concurrent enumerate/item/hydrate stay consistent")
}

// A tiny budget forces the budget-exceeded branch: fail closed,
// never a guess.
do {
    let budgeted = LiveStore(
        driveHex: driveHex, log: log, socketPathOverride: socketPath)
    budgeted.resolver.walkBudget = 2
    let overBudget = NSFileProviderItemIdentifier(
        WyrdItemIdentifier.stableID(driveHex: driveHex, identityHex: String(repeating: "98", count: 32)))
    do {
        _ = try budgeted.item(for: overBudget)
        print("FAIL: an over-budget walk resolves")
        failures += 1
    } catch let error as LiveStoreError {
        if case .sync = error {
            // Expected: the walk gave up loudly.
        } else {
            print("FAIL: an over-budget walk fails sync-closed, got \(error)")
            failures += 1
        }
    }
}

// MARK: - Mutation wire proof (no UI path sends these yet)

do {
    let client = OwnerClient(socketPath: socketPath)
    // capture-parent answers the drive-root token the stub mints.
    let token = try client.captureParent(path: "")
    check(token.instance == 9 && token.counter == 3, "capture-parent answers the stub token")
    // A commit carrying hello.txt's token commits.
    let committed = try client.submit(kind: .commitFile(
        path: "hello.txt",
        base: OwnerFileIdentity(size: 26, executable: false, chunks: ["aa"]),
        executable: false,
        contentHex: "68656c6c6f",
        expectedIdentity: String(repeating: "ab12cd34", count: 8)
    ))
    if case .committed(let identity) = committed {
        checkEqual(identity.size, 5, "the committed size comes from the content")
    } else {
        print("FAIL: a matching precondition commits: \(committed)")
        failures += 1
    }
    // A wrong token refuses as StaleEntry, naming both sides.
    do {
        _ = try client.submit(kind: .unlink(
            path: "hello.txt",
            expectedIdentity: String(repeating: "99", count: 32)
        ))
        print("FAIL: a wrong token commits")
        failures += 1
    } catch let error as OwnerClientError {
        if case .refused(let refusal) = error, refusal.code == "StaleEntry" {
            check(
                refusal.message.contains(String(repeating: "99", count: 4)),
                "the refusal names the expected token")
        } else {
            print("FAIL: a wrong token refuses StaleEntry: \(error)")
            failures += 1
        }
    }
    // A precondition-less covered kind never leaves the client:
    // the type system requires the token, so raw framing proves
    // the server side refuses a missing one as Malformed — and a
    // v1-framed request as UnknownVersion.
    do {
        let malformed = try rawRoundTrip(
            socketPath: socketPath,
            body: "{\"v\":2,\"id\":99,\"op\":\"submit\",\"params\":{\"kind\":{\"Unlink\":{\"path\":\"hello.txt\"}}}}")
        checkEqual(malformed.code, "Malformed", "a missing precondition refuses Malformed")
        let old = try rawRoundTrip(
            socketPath: socketPath,
            body: "{\"v\":1,\"id\":98,\"op\":\"lookup\",\"params\":{\"path\":\"hello.txt\"}}")
        checkEqual(old.code, "UnknownVersion", "a v1 frame refuses UnknownVersion")
        checkEqual(old.version, 2, "the refusal carries the serving version")
    } catch {
        print("FAIL: raw wire checks: \(error)")
        failures += 1
    }
} catch {
    print("FAIL: mutation wire proof: \(error)")
    failures += 1
}

/// One raw framed exchange against the stub, bypassing
/// `OwnerClient`: length-prefixed JSON out, length-prefixed JSON
/// back, decoded just far enough to name the refusal code.
func rawRoundTrip(socketPath: String, body: String) throws -> (code: String, version: Int) {
    let fd = socket(AF_UNIX, SOCK_STREAM, 0)
    guard fd >= 0 else { throw HarnessError.socket("socket() failed") }
    defer { close(fd) }
    var addr = sockaddr_un()
    memset(&addr, 0, MemoryLayout<sockaddr_un>.size)
    addr.sun_family = sa_family_t(AF_UNIX)
    let connected: Int32 = socketPath.withCString { cString in
        withUnsafeMutableBytes(of: &addr.sun_path) { raw in
            let dest = raw.baseAddress!.assumingMemoryBound(to: CChar.self)
            strncpy(dest, cString, 103)
        }
        return withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sock in
                connect(fd, sock, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
    }
    guard connected == 0 else { throw HarnessError.socket("connect refused") }
    let bodyData = Data(body.utf8)
    let length = UInt32(bodyData.count).bigEndian
    withUnsafeBytes(of: length) { prefix in
        _ = write(fd, prefix.baseAddress!, prefix.count)
    }
    _ = bodyData.withUnsafeBytes { pointer in
        write(fd, pointer.baseAddress!, bodyData.count)
    }
    var prefix = [UInt8](repeating: 0, count: 4)
    try prefix.withUnsafeMutableBytes { raw in
        var done = 0
        while done < 4 {
            let chunk = read(fd, raw.baseAddress!.advanced(by: done), 4 - done)
            guard chunk > 0 else { throw HarnessError.socket("short length") }
            done += chunk
        }
    }
    let lengthValue = (UInt32(prefix[0]) << 24) | (UInt32(prefix[1]) << 16)
        | (UInt32(prefix[2]) << 8) | UInt32(prefix[3])
    var payload = [UInt8](repeating: 0, count: Int(lengthValue))
    let total = payload.count
    try payload.withUnsafeMutableBytes { raw in
        var done = 0
        while done < total {
            let chunk = read(fd, raw.baseAddress!.advanced(by: done), total - done)
            guard chunk > 0 else { throw HarnessError.socket("short body") }
            done += chunk
        }
    }
    let json = try JSONSerialization.jsonObject(with: Data(payload)) as! [String: Any]
    let version = json["v"] as! Int
    let result = json["result"] as! [String: Any]
    let err = result["err"] as! [String: Any]
    return (code: err["code"] as! String, version: version)
}

enum HarnessError: Error {
    case socket(String)
}

if failures == 0 {
    print("harness: all checks passed")
} else {
    print("harness: \(failures) checks failed")
    exit(1)
}
