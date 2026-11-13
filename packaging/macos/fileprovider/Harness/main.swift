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
        if let conflictRow = entries.first(where: { if case .conflict = $0.node { return true } else { return false } }) {
            if case .conflict(let versions) = conflictRow.node {
                checkEqual(versions.count, 2, "the conflict row carries two head versions")
                checkEqual(
                    versions.map { $0.snapshot }, ["11", "22"],
                    "versions label by snapshot id")
                checkEqual(
                    versions.map { $0.timestamp }, [1720000000000, 1720000060000],
                    "versions label by head timestamp")
            } else {
                print("FAIL: conflict row decodes versions")
                failures += 1
            }
        } else {
            print("FAIL: readdir keeps a conflict row")
            failures += 1
        }
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
    let (items, reports, storeAnchor) = try store.enumerate(container: .rootContainer)
    checkEqual(storeAnchor, WyrdSyncAnchor(generation: 3, revision: 9), "enumeration anchor is the owner coordinate")
    checkEqual(items.count, 5, "conflict rows are excluded, siblings enumerate")
    check(!items.contains { $0.filename == "split.txt" }, "the conflicted name never surfaces")
    checkEqual(reports.count, 1, "one conflict row yields one report")
    if let report = reports.first {
        checkEqual(report.filename, "split.txt.wyrd-conflict.txt", "the report name derives from the conflicted name")
        checkEqual(report.capabilities, [.allowsReading], "reports are read-only")
        check(
            report.itemIdentifier.rawValue.hasPrefix(WyrdItemIdentifier.conflictPrefix),
            "reports carry the synthetic conflict namespace")
        checkEqual(
            report.itemIdentifier.rawValue,
            WyrdItemIdentifier.conflictID(driveHex: driveHex, path: "split.txt"),
            "the report identifier is the deterministic (drive, path) key")
    } else {
        print("FAIL: conflict report enumerates")
        failures += 1
    }
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

// MARK: - Enumerator terminus (nil page, live anchor)

/// A recording `NSFileProviderEnumerationObserver`: the Harness
/// drives the real enumerator and pins the nil-page finish the
/// Finder leg depends on.
final class RecordingEnumerationObserver: NSObject, NSFileProviderEnumerationObserver {
    var enumerated: [NSFileProviderItem] = []
    var finished = false
    var finishedPageIsNil = false
    var finishError: Error?
    func didEnumerate(_ updatedItems: [NSFileProviderItem]) {
        enumerated += updatedItems
    }
    func finishEnumerating(upTo page: NSFileProviderPage?) {
        finished = true
        finishedPageIsNil = page == nil
    }
    func finishEnumeratingWithError(_ error: Error) {
        finished = true
        finishError = error
    }
}

do {
    // The enumerator finishes with a nil page: an anchor-data page
    // fails the fetch persist leg-wide, so no continuation state
    // survives the call. The anchor itself stays live through
    // `currentSyncAnchor`, which the system consults separately.
    let observer = RecordingEnumerationObserver()
    let enumerator = LiveEnumerator(container: .rootContainer, store: store, log: log)
    enumerator.enumerateItems(for: observer, startingAt: NSFileProviderPage(Data()))
    check(observer.finishError == nil, "enumeration finishes without error")
    check(observer.finished, "enumeration finishes")
    check(observer.finishedPageIsNil, "enumeration finishes with a nil page")
    check(!observer.enumerated.isEmpty, "enumeration serves rows before finishing")
    // The anchor half goes through the enumerator's own method, not
    // the store's: a regression in `currentSyncAnchor` must fail here.
    let anchorSemaphore = DispatchSemaphore(value: 0)
    var liveAnchor: NSFileProviderSyncAnchor?
    enumerator.currentSyncAnchor { anchor in
        liveAnchor = anchor
        anchorSemaphore.signal()
    }
    check(
        anchorSemaphore.wait(timeout: .now() + 10) == .success,
        "the anchor callback answers")
    check(liveAnchor != nil, "the anchor stays live through currentSyncAnchor")
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

/// One framed debug exchange against the stub, bypassing
/// `OwnerClient` (test-only ops never enter production code):
/// length-prefixed JSON out, length-prefixed JSON back, failing
/// closed unless the stub answers `ok`.
func debugStub(socketPath: String, op: String, params: [String: Any]) throws {
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
    let frame: [String: Any] = ["v": 2, "id": 996, "op": op, "params": params]
    let bodyData = try JSONSerialization.data(withJSONObject: frame)
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
    let result = json["result"] as! [String: Any]
    guard result["ok"] != nil else {
        throw HarnessError.socket("debug op refused: \(result)")
    }
}

enum HarnessError: Error {
    case socket(String)
}

// MARK: - Mutation callbacks (Stage 3 sub-PR 2)

do {
    // Capability matrix: token-bearing files and directories are
    // writable without trash; symlinks, legacy rows, and
    // disagreements stay read-only.
    func capabilitiesCase(_ item: LiveItem?) -> NSFileProviderItemCapabilities? {
        item?.capabilities
    }
    let fileRow = LiveRow(
        name: "w.txt",
        node: .file(size: 1, executable: false, chunks: [], identity: identityHex),
        generation: 3, revision: 9)
    let fileItem = try store.buildItem(
        node: fileRow.node, path: "w.txt", parent: .rootContainer,
        generation: 3, revision: 9)
    let fileCaps = capabilitiesCase(fileItem) ?? []
    check(fileCaps.contains(.allowsWriting), "files advertise writing")
    check(fileCaps.contains(.allowsRenaming), "files advertise renaming")
    check(fileCaps.contains(.allowsDeleting), "files advertise deleting")
    check(!fileCaps.contains(.allowsTrashing), "files never advertise trash")
    let dirItem = try store.buildItem(
        node: .dir(subtree: "", identity: identityHex), path: "d",
        parent: .rootContainer, generation: 3, revision: 9)
    let dirCaps = capabilitiesCase(dirItem) ?? []
    check(dirCaps.contains(.allowsWriting), "dirs advertise adding children")
    check(!dirCaps.contains(.allowsTrashing), "dirs never advertise trash")
    let linkItem = try store.buildItem(
        node: .symlink(target: "w.txt", identity: identityHex), path: "l",
        parent: .rootContainer, generation: 3, revision: 9)
    checkEqual(
        capabilitiesCase(linkItem) ?? [], [.allowsReading], "symlinks stay read-only")
    let legacyItem = try store.buildItem(
        node: .file(size: 1, executable: false, chunks: [], identity: nil),
        path: "old.bin", parent: .rootContainer, generation: 3, revision: 9)
    checkEqual(
        capabilitiesCase(legacyItem) ?? [], [.allowsReading], "legacy rows stay read-only")
    _ = fileCaps
} catch {
    print("FAIL: capability matrix: \(error)")
    failures += 1
}

do {
    // Version bound: fileproviderd rejects item versions whose
    // content or metadata data exceeds 128 bytes, failing the whole
    // enumeration. Every served row — items and reports — stays
    // within the bound, no matter how long the underlying tail
    // (chunk lists, subtree listings) grows.
    let (boundItems, boundReports, _) = try store.enumerate(container: .rootContainer)
    for item in boundItems {
        check(
            item.itemVersion.contentVersion.count <= 128,
            "item \(item.filename) content version fits 128 bytes")
        check(
            item.itemVersion.metadataVersion.count <= 128,
            "item \(item.filename) metadata version fits 128 bytes")
    }
    for report in boundReports {
        check(
            report.itemVersion.contentVersion.count <= 128,
            "report \(report.filename) content version fits 128 bytes")
        check(
            report.itemVersion.metadataVersion.count <= 128,
            "report \(report.filename) metadata version fits 128 bytes")
    }
    // Synthetic long tail: thousands of chunks would breach the
    // bound as a raw tail (over 160 KB), so a revert to raw tails
    // fails here while the digest stays fixed width.
    let longChunks = (0..<5000).map { String(format: "%032x", $0) }
    let longRawTail = "\(identityHex.lowercased()):\(longChunks.joined(separator: ","))"
    check(longRawTail.count > 128, "the long tail would breach the bound raw")
    let longItem = try store.buildItem(
        node: .file(size: 12345, executable: false, chunks: longChunks, identity: identityHex),
        path: "long-tail.bin", parent: .rootContainer, generation: 3, revision: 9)
    check(
        longItem.itemVersion.contentVersion.count <= 128,
        "a long-tail item version fits 128 bytes")
    check(
        longItem.itemVersion.metadataVersion.count <= 128,
        "a long-tail metadata version fits 128 bytes")
} catch {
    print("FAIL: version bound: \(error)")
    failures += 1
}

do {
    // End-to-end write path against the stateful stub: mkdir,
    // create-with-contents, modify, rename, delete.
    let dir = try store.createDirectory(parent: .rootContainer, name: "mut-dir")
    check(dir.isDirectory, "created dir serves as a directory")
    // A duplicate create refuses as AlreadyExists — the callback
    // renders filenameCollision, never a sync failure.
    do {
        _ = try store.createDirectory(parent: .rootContainer, name: "mut-dir")
        print("FAIL: duplicate mkdir commits")
        failures += 1
    } catch let error as LiveStoreError {
        if case .refused(let code, _) = error {
            checkEqual(code, "AlreadyExists", "duplicate mkdir refuses collision")
            checkEqual(
                nsErrorForMutation(code: code, message: "", what: "t", log: log).code,
                NSFileProviderError.filenameCollision,
                "collision renders filenameCollision")
        } else {
            print("FAIL: duplicate mkdir refuses collision, got \(error)")
            failures += 1
        }
    }
    let dirID = dir.itemIdentifier
    let file = try store.createFile(
        parent: dirID, name: "mut-file.txt", contents: Data("mut-hello".utf8))
    checkEqual(file.documentSize?.uint64Value, 9, "created file serves its bytes")
    let (hydrated, _) = try store.contents(for: file.itemIdentifier)
    checkEqual(
        String(data: hydrated, encoding: .utf8), "mut-hello",
        "created contents read back")
    // Modify: size changes and the version moves.
    let before = file.itemVersion
    let modified = try store.writeContent(
        for: file.itemIdentifier, data: Data("mut-hello!!".utf8))
    checkEqual(modified.documentSize?.uint64Value, 11, "modified size comes from the owner")
    check(
        modified.itemVersion.contentVersion != before.contentVersion,
        "modify moves the item version")
    // Rename preserves the entry: the stable identifier is
    // unchanged and resolves to the new name.
    let moved = try store.rename(
        identifier: file.itemIdentifier, newParent: dirID, newName: "mut-renamed.txt")
    checkEqual(moved.itemIdentifier, file.itemIdentifier, "rename preserves the token")
    let resolved = try store.item(for: file.itemIdentifier)
    checkEqual(resolved.filename, "mut-renamed.txt", "token resolves to the new name")
    // A no-op rename submits nothing and still resolves.
    let noop = try store.rename(
        identifier: file.itemIdentifier, newParent: dirID, newName: "mut-renamed.txt")
    checkEqual(noop.filename, "mut-renamed.txt", "no-op rename resolves")
    // Rename onto an existing name collides.
    let other = try store.createFile(
        parent: dirID, name: "mut-other.txt", contents: Data("x".utf8))
    _ = other
    do {
        _ = try store.rename(
            identifier: file.itemIdentifier, newParent: dirID, newName: "mut-other.txt")
        print("FAIL: colliding rename commits")
        failures += 1
    } catch let error as LiveStoreError {
        if case .refused(let code, _) = error {
            checkEqual(code, "AlreadyExists", "colliding rename refuses collision")
        } else {
            print("FAIL: colliding rename refuses collision, got \(error)")
            failures += 1
        }
    }
    // Delete removes; a second delete reports success (already gone).
    try store.delete(identifier: other.itemIdentifier)
    do {
        _ = try store.item(for: other.itemIdentifier)
        print("FAIL: deleted entry still serves")
        failures += 1
    } catch let error as LiveStoreError {
        if case .notFound = error {} else {
            print("FAIL: deleted entry 404s, got \(error)")
            failures += 1
        }
    }
    try store.delete(identifier: other.itemIdentifier)
    // Empty files are supported: no contents, just CreateFile.
    let empty = try store.createFile(parent: dirID, name: "mut-empty.txt", contents: nil)
    checkEqual(empty.documentSize?.uint64Value, 0, "empty files commit")
    // Non-empty directories keep DirectoryNotEmpty.
    do {
        try store.delete(identifier: dirID)
        print("FAIL: non-empty rmdir commits")
        failures += 1
    } catch let error as LiveStoreError {
        if case .refused(let code, _) = error {
            checkEqual(code, "DirectoryNotEmpty", "non-empty dir refuses")
            checkEqual(
                nsErrorForMutation(code: code, message: "", what: "t", log: log).code,
                NSFileProviderError.directoryNotEmpty,
                "non-empty renders directoryNotEmpty")
        } else {
            print("FAIL: non-empty dir refuses, got \(error)")
            failures += 1
        }
    }
    // Legacy rows are not writable, even for delete.
    let legacyID = NSFileProviderItemIdentifier(
        WyrdItemIdentifier.legacyID(path: "legacy.bin"))
    do {
        try store.delete(identifier: legacyID)
        print("FAIL: legacy delete commits")
        failures += 1
    } catch let error as LiveStoreError {
        if case .sync = error {} else {
            print("FAIL: legacy delete fails closed, got \(error)")
            failures += 1
        }
    }
} catch {
    print("FAIL: mutation round-trip: \(error)")
    failures += 1
}

do {
    // One StaleParent recaptures and retries: the stub scripts the
    // first create under stale-dir to refuse, the retry commits.
    // The fixture is hidden from readdir (so the Stage 2 counts
    // hold) and recorded directly — the verify-lookup still checks
    // the token on use.
    let staleParentID = NSFileProviderItemIdentifier(
        WyrdItemIdentifier.stableID(driveHex: driveHex, identityHex: String(repeating: "ed", count: 32)))
    store.resolver.record(
        identifier: staleParentID, path: "stale-dir", parent: .rootContainer)
    // The recorded mapping verifies against the fixture's lookup.
    let created = try store.createFile(
        parent: staleParentID, name: "mut-stale.txt", contents: Data("s".utf8))
    checkEqual(created.filename, "mut-stale.txt", "recaptured create commits")
    // Oversize writes never reach the socket: the client refuses
    // before any submit.
    let huge = Data(repeating: 0, count: LiveStore.maxContentBytes + 1)
    do {
        _ = try store.createFile(parent: .rootContainer, name: "mut-huge.bin", contents: huge)
        print("FAIL: oversize create submits")
        failures += 1
    } catch let error as LiveStoreError {
        if case .sync(let message) = error {
            check(message.contains("32 MiB"), "oversize refusal names the bound")
        } else {
            print("FAIL: oversize create fails closed, got \(error)")
            failures += 1
        }
    }
} catch {
    print("FAIL: stale-parent recapture: \(error)")
    failures += 1
}

do {
    // Base-version discipline is per entry, not per drive: size,
    // token, and content digest must match; the serving generation
    // and revision move with every commit anywhere, so they are
    // excluded (an unrelated write must not invalidate this item).
    // First-sync skips — the owner precondition stays authoritative.
    let current = try store.item(for: NSFileProviderItemIdentifier(
        WyrdItemIdentifier.stableID(driveHex: driveHex, identityHex: identityHex)))
    guard let currentVersion = current.itemVersion ?? nil else {
        print("FAIL: current item carries a version")
        failures += 1
        throw HarnessError.socket("no version")
    }
    check(
        FileProviderExtension.baseVersionMatches(
            currentVersion, current: current),
        "matching base version passes")
    let facts = FileProviderExtension.versionFacts(currentVersion.contentVersion)
    check(facts != nil, "our versions parse into per-entry facts")
    if let facts {
        let unrelatedWrite = NSFileProviderItemVersion(
            contentVersion: Data("wyrd-cv3:99:99:\(facts.size):\(facts.rest)".utf8),
            metadataVersion: currentVersion.metadataVersion)
        check(
            FileProviderExtension.baseVersionMatches(unrelatedWrite, current: current),
            "a newer coordinate with the same entry facts still matches")
        let changedEntry = NSFileProviderItemVersion(
            contentVersion: Data("wyrd-cv3:3:9:12345:\(facts.rest)".utf8),
            metadataVersion: currentVersion.metadataVersion)
        check(
            !FileProviderExtension.baseVersionMatches(changedEntry, current: current),
            "a changed size fails")
        // Upgrade continuity: the `cv2` tail is the exact
        // `token:content` string `cv3` digests, so a cached `cv2`
        // base still matches after the upgrade instead of failing
        // once and healing. `hello.txt` serves chunks `["aa"]`
        // under the test token, so its raw tail is known.
        let tokenTag = identityHex.lowercased()
        let helloRawRest = "\(tokenTag):aa"
        checkEqual(
            conflictDigestHex(helloRawRest), facts.rest,
            "the current digest covers the token-plus-chunk tail")
        let upgradedFacts = FileProviderExtension.versionFacts(
            Data("wyrd-cv2:3:9:\(facts.size):\(helloRawRest)".utf8))
        check(
            upgradedFacts?.size == facts.size && upgradedFacts?.rest == facts.rest,
            "a cached cv2 tail normalizes to the cv3 facts")
        let upgradedBase = NSFileProviderItemVersion(
            contentVersion: Data("wyrd-cv2:99:99:\(facts.size):\(helloRawRest)".utf8),
            metadataVersion: currentVersion.metadataVersion)
        check(
            FileProviderExtension.baseVersionMatches(upgradedBase, current: current),
            "a cached cv2 base still matches after the cv3 upgrade")
        let staleRawBase = NSFileProviderItemVersion(
            contentVersion: Data("wyrd-cv2:3:9:\(facts.size):\(tokenTag):bb".utf8),
            metadataVersion: currentVersion.metadataVersion)
        check(
            !FileProviderExtension.baseVersionMatches(staleRawBase, current: current),
            "a cv2 base with different content still fails")
        // Legacy cv1 versions still parse (transitional cached
        // versions fail closed on shape, never on parse).
        let legacy = NSFileProviderItemVersion(
            contentVersion: Data("wyrd-cv1:3:9:\(facts.size):\(facts.rest)".utf8),
            metadataVersion: currentVersion.metadataVersion)
        check(
            FileProviderExtension.versionFacts(legacy.contentVersion) != nil,
            "legacy versions still parse")
        // Current cv3 versions round-trip: same facts, same match;
        // a same-size digest swap fails.
        let rebuilt = NSFileProviderItemVersion(
            contentVersion: Data("wyrd-cv3:3:9:\(facts.size):\(facts.rest)".utf8),
            metadataVersion: currentVersion.metadataVersion)
        let reparsed = FileProviderExtension.versionFacts(rebuilt.contentVersion)
        check(
            reparsed?.size == facts.size && reparsed?.rest == facts.rest,
            "current versions round-trip into the same facts")
        check(
            FileProviderExtension.baseVersionMatches(rebuilt, current: current),
            "a rebuilt current version still matches")
        let swappedDigest = NSFileProviderItemVersion(
            contentVersion: Data("wyrd-cv3:3:9:\(facts.size):deadbeefdeadbeef".utf8),
            metadataVersion: currentVersion.metadataVersion)
        check(
            !FileProviderExtension.baseVersionMatches(swappedDigest, current: current),
            "a swapped content digest fails")
    }
    let stale = NSFileProviderItemVersion(
        contentVersion: Data("wyrd-cv1:0:0:0:i:dead".utf8),
        metadataVersion: currentVersion.metadataVersion)
    check(
        !FileProviderExtension.baseVersionMatches(stale, current: current),
        "a foreign entry fails")
    check(
        FileProviderExtension.versionFacts(Data("not-a-version".utf8)) == nil,
        "a foreign version string parses to nothing")
    let first = NSFileProviderItemVersion(
        contentVersion: NSFileProviderItemVersion.beforeFirstSyncComponent,
        metadataVersion: currentVersion.metadataVersion)
    check(
        FileProviderExtension.baseVersionMatches(first, current: current),
        "first-sync base skips the check")
    // Single-completion gate: exactly one owner.
    let gate = SingleCompletion()
    check(gate.fire {}, "first completion owns the gate")
    check(!gate.fire {}, "second completion is silent")
    check(gate.isDone, "gate reports done")
    // Working-set signal without a manager never throws: the
    // Harness has no fileproviderd.
    let domain = NSFileProviderDomain(
        identifier: NSFileProviderDomainIdentifier(driveHex), displayName: "Wyrd")
    signalWorkingSet(domain: domain, log: log)
} catch {
    print("FAIL: version and completion checks: \(error)")
    failures += 1
}

do {
    // Cancellation aborts before the first socket write and never
    // invents an answer: a pre-cancelled mutation throws
    // `.cancelled` without touching the transport.
    let cancelled = MutationCancel()
    cancelled.request()
    check(cancelled.isRequested, "cancel records")
    do {
        _ = try store.createDirectory(
            parent: .rootContainer, name: "mut-cancelled", cancel: cancelled)
        print("FAIL: cancelled create submits")
        failures += 1
    } catch let error as LiveStoreError {
        if case .cancelled = error {} else {
            print("FAIL: cancelled create aborts, got \(error)")
            failures += 1
        }
    }
    // The mutation error table renders cancel as the Cocoa code the
    // header names — an NSError, not a File Provider code.
    if case .cancelled = LiveStoreError.cancelled("t") {
        let rendered = FileProviderExtension.mutationError(
            for: .cancelled("t"), what: "t", log: log)
        check(
            (rendered as NSError).domain == NSCocoaErrorDomain
                && (rendered as NSError).code == NSUserCancelledError,
            "cancel renders NSUserCancelledError")
    }
    // Transport-shaped mutation refusals stay temporary, never
    // deterministic: the read table would call a mutation Timeout
    // sync, which is the wrong lane for a write that may land.
    check({
        if case .temporary = store.mapMutationError(
            OwnerClientError.refused(OwnerError(code: "Timeout", message: "x")),
            what: "t") { return true }
        return false
    }(), "mutation Timeout maps to temporary")
} catch {
    print("FAIL: cancellation checks: \(error)")
    failures += 1
}

do {
    // Unconfirmed submits reconcile before any retry. The stub
    // drops the first connection per (kind, path): `drop-before`
    // closes before processing (nothing landed — retry converges),
    // `drop-after` processes, then closes without answering (the
    // write landed — report temporary, never claim success).
    _ = try store.createDirectory(parent: .rootContainer, name: "drop-before")
    _ = try store.createDirectory(parent: .rootContainer, name: "drop-after")
    let beforeID = try store.enumerate(container: .rootContainer).items
        .first(where: { $0.filename == "drop-before" })!.itemIdentifier
    let dropped = try store.createFile(
        parent: beforeID, name: "f.txt", contents: Data("dropped".utf8))
    let (redropped, _) = try store.contents(for: dropped.itemIdentifier)
    checkEqual(
        String(data: redropped, encoding: .utf8), "dropped",
        "a dropped-before create retries and converges")
    // Same-size content is no proof: the commit that landed without
    // an answer reports temporary, and the bytes are already there.
    let afterID = try store.enumerate(container: .rootContainer).items
        .first(where: { $0.filename == "drop-after" })!.itemIdentifier
    do {
        _ = try store.createFile(
            parent: afterID, name: "g.txt", contents: Data("v1".utf8))
        print("FAIL: dropped-after create claims success")
        failures += 1
    } catch let error as LiveStoreError {
        if case .temporary = error {} else {
            print("FAIL: dropped-after create reports temporary, got \(error)")
            failures += 1
        }
    }
    // The entry exists exactly once: the retry meets AlreadyExists,
    // never a silent duplicate.
    do {
        _ = try store.createFile(
            parent: afterID, name: "g.txt", contents: Data("v1".utf8))
        print("FAIL: duplicate create commits")
        failures += 1
    } catch let error as LiveStoreError {
        if case .refused(let code, _) = error {
            checkEqual(code, "AlreadyExists", "the landed write blocks a duplicate")
        } else {
            print("FAIL: duplicate create collides, got \(error)")
            failures += 1
        }
    }
} catch {
    print("FAIL: unconfirmed reconciliation: \(error)")
    failures += 1
}

do {
    // The full content base is checked, not just the size: a
    // same-size edit with different chunks is still stale, and a
    // tampered chunk list is stale too.
    let dirID = try store.enumerate(container: .rootContainer).items
        .first(where: { $0.filename == "mut-dir" })!.itemIdentifier
    _ = try store.createFile(
        parent: dirID, name: "mut-base.txt", contents: Data("1234".utf8))
    let client = OwnerClient(socketPath: socketPath)
    let (node, _, _) = try client.lookup(path: "mut-dir/mut-base.txt")
    guard case .file(_, _, _, let identity?) = node else {
        print("FAIL: base fixture serves a token-bearing file")
        failures += 1
        throw HarnessError.socket("no token")
    }
    _ = try store.writeContent(
        for: try store.item(for: NSFileProviderItemIdentifier(
            WyrdItemIdentifier.stableID(driveHex: driveHex, identityHex: identity))).itemIdentifier,
        data: Data("12345678".utf8))
    func submitCommit(base: OwnerFileIdentity) throws -> OwnerMutationOutcome {
        try client.submit(kind: .commitFile(
            path: "mut-dir/mut-base.txt", base: base, executable: false,
            contentHex: "31323334", expectedIdentity: identity))
    }
    do {
        _ = try submitCommit(base: OwnerFileIdentity(
            size: 4, executable: false, chunks: ["aa"]))
        print("FAIL: stale base commits")
        failures += 1
    } catch let error as OwnerClientError {
        if case .refused(let refusal) = error {
            checkEqual(refusal.code, "Stale", "a stale base refuses")
        } else {
            print("FAIL: stale base refuses Stale, got \(error)")
            failures += 1
        }
    }
    do {
        _ = try submitCommit(base: OwnerFileIdentity(
            size: 8, executable: false, chunks: ["bb"]))
        print("FAIL: tampered chunks commit")
        failures += 1
    } catch let error as OwnerClientError {
        if case .refused(let refusal) = error {
            checkEqual(refusal.code, "Stale", "tampered chunks refuse")
        } else {
            print("FAIL: tampered chunks refuse Stale, got \(error)")
            failures += 1
        }
    }
} catch {
    print("FAIL: content-base checks: \(error)")
    failures += 1
}

do {
    // The Finding-1 scenario at adapter level: a same-size remote
    // edit lands after the system observed the item. The stored
    // base version no longer matches — the digest moved — so a
    // modify built on it fails closed without overwriting.
    let dirID = try store.enumerate(container: .rootContainer).items
        .first(where: { $0.filename == "mut-dir" })!.itemIdentifier
    let observed = try store.createFile(
        parent: dirID, name: "mut-ss.txt", contents: Data("aaaa".utf8))
    let before = try store.item(for: observed.itemIdentifier)
    // A peer writes the same size with different bytes, through a
    // fresh base — exactly what the owner's queue accepts.
    let peer = OwnerClient(socketPath: socketPath)
    let (peerNode, _, _) = try peer.lookup(path: "mut-dir/mut-ss.txt")
    guard case .file(let peerSize, _, let peerChunks, let peerToken?) = peerNode else {
        print("FAIL: same-size fixture serves a token-bearing file")
        failures += 1
        throw HarnessError.socket("no token")
    }
    _ = try peer.submit(kind: .commitFile(
        path: "mut-dir/mut-ss.txt",
        base: OwnerFileIdentity(size: peerSize, executable: false, chunks: peerChunks),
        executable: false, contentHex: "62626262", expectedIdentity: peerToken))
    let now = try store.item(for: observed.itemIdentifier)
    guard let beforeVersion = before.itemVersion ?? nil,
        let nowItem = now as? LiveItem
    else {
        print("FAIL: same-size items carry versions")
        failures += 1
        throw HarnessError.socket("no version")
    }
    check(
        !FileProviderExtension.baseVersionMatches(beforeVersion, current: nowItem),
        "a same-size remote edit invalidates the stored base")
    let (kept, _) = try store.contents(for: observed.itemIdentifier)
    checkEqual(
        String(data: kept, encoding: .utf8), "bbbb",
        "the peer bytes stand; nothing overwrote them")
} catch {
    print("FAIL: same-size remote edit: \(error)")
    failures += 1
}

do {
    // The ambiguity branch of commit reconciliation: a commit that
    // landed without an answer reports temporary, and the bytes are
    // already there — never a false success, never a silent retry.
    // Each (kind, path) drops once, so create and commit drop
    // independently. The fixture dirs already exist from the
    // unconfirmed-reconciliation block above.
    let afterID = try store.enumerate(container: .rootContainer).items
        .first(where: { $0.filename == "drop-after" })!.itemIdentifier
    do {
        _ = try store.createFile(parent: afterID, name: "k.txt", contents: nil)
        print("FAIL: dropped-after create claims success")
        failures += 1
    } catch let error as LiveStoreError {
        if case .temporary = error {} else {
            print("FAIL: dropped-after create reports temporary, got \(error)")
            failures += 1
        }
    }
    // The entry exists exactly once: the retry meets AlreadyExists,
    // never a silent duplicate.
    do {
        _ = try store.createFile(parent: afterID, name: "k.txt", contents: nil)
        print("FAIL: duplicate create commits")
        failures += 1
    } catch let error as LiveStoreError {
        if case .refused(let code, _) = error {
            checkEqual(code, "AlreadyExists", "the landed create blocks a duplicate")
        } else {
            print("FAIL: duplicate create collides, got \(error)")
            failures += 1
        }
    }
    let kid = try store.enumerate(container: afterID).items
        .first(where: { $0.filename == "k.txt" })!.itemIdentifier
    do {
        _ = try store.writeContent(for: kid, data: Data("kk".utf8))
        print("FAIL: dropped-after commit claims success")
        failures += 1
    } catch let error as LiveStoreError {
        if case .temporary = error {} else {
            print("FAIL: dropped-after commit reports temporary, got \(error)")
            failures += 1
        }
    }
    let (kb, _) = try store.contents(for: kid)
    checkEqual(
        String(data: kb, encoding: .utf8), "kk",
        "the dropped commit landed exactly once")
    // A second write converges normally on the fresh base.
    let reconverged = try store.writeContent(for: kid, data: Data("kkk".utf8))
    checkEqual(
        reconverged.documentSize?.uint64Value, 3, "retry converges on the fresh base")
} catch {
    print("FAIL: drop-after commit: \(error)")
    failures += 1
}

do {
    // Post-mutation enumeration serves what the callbacks wrote:
    // the working set re-observes the new state on next read.
    let (items, reports, _) = try store.enumerate(container: .rootContainer)
    check(items.contains { $0.filename == "mut-dir" }, "created dir enumerates")
    checkEqual(reports.count, 1, "the conflict report survives mutations elsewhere")
    let dirID = items.first(where: { $0.filename == "mut-dir" })!.itemIdentifier
    let (children, _, _) = try store.enumerate(container: dirID)
    let names = Set(children.map { $0.filename })
    for expected in ["mut-renamed.txt", "mut-empty.txt", "mut-base.txt"] {
        check(names.contains(expected), "\(expected) enumerates after mutation")
    }
    check(!names.contains("mut-file.txt"), "the renamed source name stays gone")
    check(!names.contains("mut-other.txt"), "the deleted entry stays gone")
} catch {
    print("FAIL: post-mutation enumeration: \(error)")
    failures += 1
}

// MARK: - Conflict inspector (Stage 3 sub-PR 3)

// Synthetic namespace grammar: deterministic (drive, path),
// drive-scoped, never a peer identity or a head's token.
do {
    let reportID = WyrdItemIdentifier.conflictID(driveHex: driveHex, path: "split.txt")
    guard case .conflict(let drive, let pathHex) = WyrdItemIdentifier.parse(reportID, driveHex: driveHex) else {
        print("FAIL: report identifier parses")
        failures += 1
        throw HarnessError.socket("report identifier unparseable")
    }
    checkEqual(drive, driveHex, "report identifier is drive-scoped")
    checkEqual(
        WyrdItemIdentifier.conflictPath(pathHex: pathHex), "split.txt",
        "report identifier decodes to the conflicted path")
    check(
        WyrdItemIdentifier.parse(reportID, driveHex: String(repeating: "43", count: 32)) == nil,
        "cross-drive report identifiers are rejected")
    check(
        WyrdItemIdentifier.parse(
            "\(WyrdItemIdentifier.conflictPrefix)\(driveHex):zzzz", driveHex: driveHex) == nil,
        "malformed report identifiers are rejected")
    check(
        WyrdItemIdentifier.parse(
            "\(WyrdItemIdentifier.conflictPrefix)\(driveHex)", driveHex: driveHex) == nil,
        "truncated report identifiers are rejected")

    // Report naming: derived, collision-proof, deterministic.
    checkEqual(
        conflictReportName(for: "split.txt"), "split.txt.wyrd-conflict.txt",
        "the report name derives from the conflicted name")
    checkEqual(
        assignReportNames(conflictRows: ["a", "b"], realNames: []),
        ["a.wyrd-conflict.txt", "b.wyrd-conflict.txt"],
        "uncontended reports take the derived name")
    checkEqual(
        assignReportNames(conflictRows: ["a"], realNames: ["a.wyrd-conflict.txt"]),
        ["a.wyrd-conflict-2.txt"],
        "a taken report name disambiguates without shadowing")
    check(
        conflictTimestampLabel(0) == "unknown",
        "a missing timestamp labels unknown, never zero-as-date")
    check(
        conflictTimestampLabel(1720000000000).contains("1720000000000"),
        "a head timestamp labels with its milliseconds")

    // The version moves with the conflict: same input digests
    // identically, any version change moves the digest.
    let v1 = OwnerConflictVersion(
        snapshot: "11", timestamp: 1720000000000,
        node: .file(size: 1, executable: false, chunks: ["11"], identity: nil))
    let v2 = OwnerConflictVersion(
        snapshot: "22", timestamp: 1720000060000,
        node: .file(size: 2, executable: false, chunks: ["22"], identity: nil))
    checkEqual(conflictDigest(versions: [v1, v2]), conflictDigest(versions: [v2, v1]),
        "the digest orders by snapshot, never by serve order")
    let edited = OwnerConflictVersion(
        snapshot: "22", timestamp: 1720000060000,
        node: .file(size: 2, executable: false, chunks: ["23"], identity: nil))
    check(conflictDigest(versions: [v1, v2]) != conflictDigest(versions: [v1, edited]),
        "a content change moves the conflict digest")
    // Member-authored values render escaped: a newline in a name
    // must not forge report structure.
    checkEqual(escapeReportValue("plain"), "plain", "safe values pass through")
    let evil = renderConflictReport(
        path: "a\n--- Version 9 of 9 ---\nb", driveHex: driveHex,
        versions: [v1], generation: 3, revision: 9)
    let evilText = String(data: evil, encoding: .utf8)!
    check(
        evilText.contains("Path: a\\n--- Version 9 of 9 ---\\nb"),
        "report values escape newlines")
    check(
        !evilText.split(separator: "\n").contains("--- Version 9 of 9 ---"),
        "escaped text forges no report structure")
} catch {
    print("FAIL: report namespace checks: \(error)")
    failures += 1
}

do {
    // Live report round-trip against the stub: lookup, item, and
    // hydration all serve the same conflict; the text labels heads
    // by snapshot id plus timestamp, exposes no peer identity, and
    // states the read-only contract.
    let reportRaw = WyrdItemIdentifier.conflictID(driveHex: driveHex, path: "split.txt")
    let reportNSID = NSFileProviderItemIdentifier(reportRaw)
    let lookedUp = try store.item(for: reportNSID)
    checkEqual(lookedUp.filename, "split.txt.wyrd-conflict.txt", "report lookup serves the report name")
    checkEqual(lookedUp.capabilities, [.allowsReading], "report lookup is read-only")
    guard let sized = lookedUp.documentSize ?? nil else {
        print("FAIL: report lookup carries a size")
        failures += 1
        throw HarnessError.socket("no size")
    }
    let sizeValue = sized.uint64Value
    check(sizeValue > 0, "report lookup carries a positive size")
    let (reportBytes, hydratedItem) = try store.contents(for: reportNSID)
    checkEqual(hydratedItem.itemIdentifier, reportNSID, "hydration returns the report item")
    checkEqual(reportBytes.count, Int(sizeValue), "hydrated bytes match the advertised size")
    guard let text = String(data: reportBytes, encoding: .utf8) else {
        print("FAIL: report bytes decode as text")
        failures += 1
        throw HarnessError.socket("no text")
    }
    for expected in [
        "Wyrd conflict report", "Path: split.txt", "Snapshot: 11", "Snapshot: 22",
        "1720000000000", "1720000060000", "read-only",
        "No version above is authoritative", "disappears once the drive state changes",
    ] {
        check(text.contains(expected), "the report states \(expected)")
    }
    if let first = text.range(of: "Snapshot: 11"),
        let second = text.range(of: "Snapshot: 22") {
        check(
            first.lowerBound < second.lowerBound,
            "versions list in snapshot id order")
    } else {
        print("FAIL: versions list in snapshot id order (labels missing)")
        failures += 1
    }
    check(!text.contains("peer-"), "the report exposes no peer identity")
    // Two enumerations agree: same conflict, same version.
    let (_, again, _) = try store.enumerate(container: .rootContainer)
    checkEqual(
        again.first?.itemVersion.contentVersion, reportsFirstContentVersion(),
        "re-enumeration serves the same report version")
} catch {
    print("FAIL: report round-trip: \(error)")
    failures += 1
}

// Helper: re-enumerate and return the report version. Compared
// against the enumeration above: the stub is static, so the
// deterministic digest must serve byte-identical versions.
func reportsFirstContentVersion() -> Data? {
    do {
        let (_, again, _) = try store.enumerate(container: .rootContainer)
        return again.first?.itemVersion.contentVersion
    } catch {
        print("FAIL: report re-enumeration: \(error)")
        failures += 1
        return nil
    }
}

do {
    // Nested conflicts replay the enumeration assignment through
    // the parent listing: the stub serves `notes/deep.txt` as a
    // conflict beside a real `deep.txt.wyrd-conflict.txt` file, so
    // the report disambiguates and lookup agrees with enumeration.
    let notesID = NSFileProviderItemIdentifier(
        WyrdItemIdentifier.stableID(
            driveHex: driveHex,
            identityHex: "cd34ab12cd34ab12cd34ab12cd34ab12cd34ab12cd34ab12cd34ab12cd34ab12"))
    let (noteItems, noteReports, _) = try store.enumerate(container: notesID)
    checkEqual(noteItems.count, 1, "the nested container lists its real file")
    checkEqual(
        noteItems.first?.filename, "deep.txt.wyrd-conflict.txt",
        "the real sibling keeps its name")
    checkEqual(noteReports.count, 1, "the nested conflict yields one report")
    guard let nested = noteReports.first else {
        print("FAIL: nested report enumerates")
        failures += 1
        throw HarnessError.socket("no nested report")
    }
    checkEqual(
        nested.filename, "deep.txt.wyrd-conflict-2.txt",
        "the nested report disambiguates against the real sibling")
    let relinked = try store.item(for: nested.itemIdentifier)
    checkEqual(
        relinked.filename, "deep.txt.wyrd-conflict-2.txt",
        "nested lookup replays the assigned name")
    let (nestedBytes, _) = try store.contents(for: nested.itemIdentifier)
    let nestedText = String(data: nestedBytes, encoding: .utf8)!
    check(
        nestedText.contains("Path: notes/deep.txt"),
        "the nested report names the full conflicted path")
    check(
        nestedText.contains("Snapshot: 33") && nestedText.contains("Snapshot: 44"),
        "the nested report labels both heads")
} catch {
    print("FAIL: nested conflict: \(error)")
    failures += 1
}

do {
    // Invalidation: a synthetic id over a non-conflicted path 404s
    // (the conflict resolved), as does one over a missing path.
    // The inspector disappears only on authoritative state change.
    for path in ["hello.txt", "no-such.txt"] {
        let raw = WyrdItemIdentifier.conflictID(driveHex: driveHex, path: path)
        do {
            _ = try store.item(for: NSFileProviderItemIdentifier(raw))
            print("FAIL: report for \(path) serves without a conflict")
            failures += 1
        } catch let error as LiveStoreError {
            if case .notFound = error {} else {
                print("FAIL: report for \(path) 404s, got \(error)")
                failures += 1
            }
        }
        do {
            _ = try store.contents(for: NSFileProviderItemIdentifier(raw))
            print("FAIL: hydration for \(path) serves without a conflict")
            failures += 1
        } catch let error as LiveStoreError {
            if case .notFound = error {} else {
                print("FAIL: hydration for \(path) 404s, got \(error)")
                failures += 1
            }
        }
    }
} catch {
    print("FAIL: report invalidation: \(error)")
    failures += 1
}

do {
    // Negative non-mutation: no inspector operation writes. Delete,
    // content write, and rename on a report fail closed as sync —
    // never a refusal the system resolves, never a submit — and a
    // submit aimed at the conflicted path itself refuses Conflicted
    // (the stub mirrors the owner), so neither head moves.
    let reportRaw = WyrdItemIdentifier.conflictID(driveHex: driveHex, path: "split.txt")
    let reportNSID = NSFileProviderItemIdentifier(reportRaw)
    do {
        try store.delete(identifier: reportNSID)
        print("FAIL: report delete commits")
        failures += 1
    } catch let error as LiveStoreError {
        if case .sync = error {} else {
            print("FAIL: report delete fails closed, got \(error)")
            failures += 1
        }
    }
    do {
        _ = try store.writeContent(for: reportNSID, data: Data("x".utf8))
        print("FAIL: report write commits")
        failures += 1
    } catch let error as LiveStoreError {
        if case .sync = error {} else {
            print("FAIL: report write fails closed, got \(error)")
            failures += 1
        }
    }
    do {
        _ = try store.rename(
            identifier: reportNSID, newParent: .rootContainer, newName: "moved.txt")
        print("FAIL: report rename commits")
        failures += 1
    } catch let error as LiveStoreError {
        if case .sync = error {} else {
            print("FAIL: report rename fails closed, got \(error)")
            failures += 1
        }
    }
    do {
        _ = try store.createFile(
            parent: reportNSID, name: "nested.txt", contents: Data("x".utf8))
        print("FAIL: create under a report commits")
        failures += 1
    } catch let error as LiveStoreError {
        if case .notFound = error {} else {
            print("FAIL: create under a report 404s, got \(error)")
            failures += 1
        }
    }
    // A covered write aimed at the conflicted path refuses
    // Conflicted however well-formed the precondition is.
    do {
        _ = try OwnerClient(socketPath: socketPath).submit(kind: .commitFile(
            path: "split.txt",
            base: OwnerFileIdentity(size: 1, executable: false, chunks: ["11"]),
            executable: false,
            contentHex: "78",
            expectedIdentity: String(repeating: "99", count: 32)))
        print("FAIL: submit against a conflicted path commits")
        failures += 1
    } catch let error as OwnerClientError {
        if case .refused(let refusal) = error {
            checkEqual(refusal.code, "Conflicted", "conflicted submits refuse Conflicted")
        } else {
            print("FAIL: conflicted submit refuses Conflicted, got \(error)")
            failures += 1
        }
    }
} catch {
    print("FAIL: negative non-mutation: \(error)")
    failures += 1
}

do {
    // Legacy references retire when an identity is minted: the same
    // rule stable resolution enforces by token match. A legacy id
    // over a path that now serves a token-bearing entry 404s on both
    // lookup and hydration instead of returning the replacement
    // under the old identifier; a still-identity-less path keeps
    // resolving through its legacy reference.
    let dirID = try store.enumerate(container: .rootContainer).items
        .first(where: { $0.filename == "mut-dir" })!.itemIdentifier
    _ = try store.createFile(
        parent: dirID, name: "mut-legacy.txt", contents: Data("m".utf8))
    let retired = NSFileProviderItemIdentifier(
        WyrdItemIdentifier.legacyID(path: "mut-dir/mut-legacy.txt"))
    for op in ["item", "contents"] {
        do {
            if op == "item" { _ = try store.item(for: retired) } else {
                _ = try store.contents(for: retired)
            }
            print("FAIL: minted path serves under a legacy reference (\(op))")
            failures += 1
        } catch let error as LiveStoreError {
            if case .notFound = error {} else {
                print("FAIL: minted legacy reference 404s (\(op)), got \(error)")
                failures += 1
            }
        }
    }
    let control = try store.item(
        for: NSFileProviderItemIdentifier(WyrdItemIdentifier.legacyID(path: "legacy.bin")))
    checkEqual(
        control.filename, "legacy.bin",
        "an identity-less path keeps its legacy reference")
    // Resolve-before-mint at one path: the stub seeds an
    // identity-less file, the legacy reference resolves while it
    // stays identity-less, and the same reference 404s once the
    // stub mints — the transition, not just the outcomes.
    try debugStub(
        socketPath: socketPath, op: "debug-seed-legacy",
        params: ["path": "mut-dir/mut-trans.txt", "content_hex": "7472616e73"])
    let transLegacy = NSFileProviderItemIdentifier(
        WyrdItemIdentifier.legacyID(path: "mut-dir/mut-trans.txt"))
    let transBefore = try store.item(for: transLegacy)
    checkEqual(
        transBefore.filename, "mut-trans.txt",
        "a seeded identity-less path resolves through its legacy reference")
    try debugStub(
        socketPath: socketPath, op: "debug-mint",
        params: ["path": "mut-dir/mut-trans.txt"])
    do {
        _ = try store.item(for: transLegacy)
        print("FAIL: minted transition path serves under the old legacy reference")
        failures += 1
    } catch let error as LiveStoreError {
        if case .notFound = error {} else {
            print("FAIL: minted transition legacy reference 404s, got \(error)")
            failures += 1
        }
    }
    // Retired directories: enumerating through a legacy directory
    // reference and creating under one both 404 once the path is
    // minted — the container surface enforces the same rule as the
    // item and contents surfaces.
    let rdir = try store.createDirectory(parent: .rootContainer, name: "mut-rdir")
    _ = try store.createFile(
        parent: rdir.itemIdentifier, name: "kid.txt", contents: Data("k".utf8))
    let retiredDir = NSFileProviderItemIdentifier(
        WyrdItemIdentifier.legacyID(path: "mut-rdir"))
    do {
        _ = try store.enumerate(container: retiredDir)
        print("FAIL: minted directory enumerates under a legacy reference")
        failures += 1
    } catch let error as LiveStoreError {
        if case .notFound = error {} else {
            print("FAIL: minted directory enumerate 404s, got \(error)")
            failures += 1
        }
    }
    do {
        _ = try store.createFile(
            parent: retiredDir, name: "sneak.txt", contents: Data("s".utf8))
        print("FAIL: minted directory accepts a create under a legacy reference")
        failures += 1
    } catch let error as LiveStoreError {
        if case .notFound = error {} else {
            print("FAIL: minted directory create-parent 404s, got \(error)")
            failures += 1
        }
    }
    // Delete-plus-recreate lifecycle: deleting the entry 404s the
    // legacy reference (the path is gone), and recreating mints a
    // new identity, so the old reference stays retired — a
    // replacement never serves under it.
    let legacyKid = try store.enumerate(container: dirID).items
        .first(where: { $0.filename == "mut-legacy.txt" })!.itemIdentifier
    try store.delete(identifier: legacyKid)
    do {
        _ = try store.item(for: retired)
        print("FAIL: deleted path serves under a legacy reference")
        failures += 1
    } catch let error as LiveStoreError {
        if case .notFound = error {} else {
            print("FAIL: deleted legacy reference 404s, got \(error)")
            failures += 1
        }
    }
    _ = try store.createFile(
        parent: dirID, name: "mut-legacy.txt", contents: Data("m2".utf8))
    do {
        _ = try store.item(for: retired)
        print("FAIL: recreated path serves under the old legacy reference")
        failures += 1
    } catch let error as LiveStoreError {
        if case .notFound = error {} else {
            print("FAIL: recreated legacy reference stays retired, got \(error)")
            failures += 1
        }
    }
} catch {
    print("FAIL: legacy retirement: \(error)")
    failures += 1
}

if failures == 0 {
    print("harness: all checks passed")
} else {
    print("harness: \(failures) checks failed")
    exit(1)
}
