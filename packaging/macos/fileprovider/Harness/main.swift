// Stage 2 Harness: exercises the Swift adapter surface without
// fileproviderd — golden-vector decoding, identifier and anchor
// grammar, and a live store round-trip against the stub owner
// (`stub_owner.py`, started by `run-harness.sh`). Compiled with the
// adapter sources into a CLI binary; never part of the appex.

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
for name in ["lookup", "stat", "readdir", "read"] {
    let raw = reencode(vector(named: "\(name)/request"))
    do {
        let request = try decoder.decode(OwnerRequest.self, from: raw)
        checkEqual(request.v, 1, "\(name) request version")
        checkEqual(request.op, name, "\(name) request op")
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
    let expectedJSON = "{\"v\":1,\"id\":7,\"op\":\"lookup\",\"params\":{\"path\":\"hello.txt\"}}"
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

if failures == 0 {
    print("harness: all checks passed")
} else {
    print("harness: \(failures) checks failed")
    exit(1)
}
