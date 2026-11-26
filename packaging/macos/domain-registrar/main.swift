// Domain registrar for the macOS developer install.
//
// A small signed helper invoked by Wyrd.app, never by fileproviderd.
// macOS registers a File Provider domain only when the containing app
// asks for it; this helper is embedded in Contents/MacOS and signed
// with the app's bundle identifier so the registration associates
// with Wyrd.app. It reconciles NSFileProviderManager's domains
// against the per-drive aliases the owner publishes in the App Group
// container.
//
// The alias name is the authoritative drive id (`SocketConfig` in the
// appex parses the same 64-char lowercase hex from the domain
// identifier). No second mapping table, no owner-protocol change, and
// no extra Cargo edge for the desktop crate.

import Darwin
import FileProvider
import Foundation
import os

private let log = Logger(
    subsystem: "md.enesis.wyrd.domain-registrar",
    category: "domains"
)

private struct Config: Decodable {
    let group: String
}

/// One completion per API call. Reusing a single semaphore across
/// operations lets a late callback from a timed-out call satisfy the
/// next wait and report a phantom result.
private final class Completion<T> {
    private let semaphore = DispatchSemaphore(value: 0)
    private var value: T?
    private let lock = NSLock()

    func resolve(_ result: T) {
        lock.lock()
        value = result
        lock.unlock()
        semaphore.signal()
    }

    func wait(_ timeout: DispatchTime) -> T? {
        guard semaphore.wait(timeout: timeout) == .success else { return nil }
        lock.lock()
        let result = value
        lock.unlock()
        return result
    }
}

/// A fresh deadline per wait: a shared absolute deadline would make
/// later add/remove calls time out after a slow domain read.
private func callbackTimeout() -> DispatchTime { .now() + 10 }

/// Whether `value` is a Wyrd drive id: 64 lowercase hex characters,
/// never the all-zero placeholder the appex also refuses.
private func isDriveId(_ value: String) -> Bool {
    guard value.count == 64, value.utf8.allSatisfy({ byte in
        (byte >= 48 && byte <= 57) || (byte >= 97 && byte <= 102)
    }) else {
        return false
    }
    return value != String(repeating: "0", count: 64)
}

/// The owner publishes the registry name beside the alias as control
/// metadata. Only a live owner publishes a label, so a stale alias (a
/// locked or stopped drive) never becomes a Finder domain. Accept
/// only registry-style names; anything else is ignored rather than
/// trusted from a shared directory. The domain identifier stays the
/// full drive id.
private func displayName(for label: String) -> String? {
    guard !label.isEmpty, label.count <= 64,
          label.utf8.allSatisfy({ byte in
              (byte >= 97 && byte <= 122) || (byte >= 48 && byte <= 57) || byte == 45
          })
    else {
        return nil
    }
    return label
}

/// A Unix socket file can survive a killed owner: the desktop's lock
/// path uses SIGKILL, so shutdown cleanup cannot run. Probe the
/// endpoint instead of trusting the file — a stale alias is not a
/// live drive and must not hold a Finder domain.
private func socketResponds(at path: String) -> Bool {
    let fd = socket(AF_UNIX, SOCK_STREAM, 0)
    if fd < 0 { return false }
    defer { close(fd) }
    var addr = sockaddr_un()
    addr.sun_family = sa_family_t(AF_UNIX)
    let pathBytes = Array(path.utf8)
    guard pathBytes.count < MemoryLayout<sockaddr_un>.size - 2 else { return false }
    withUnsafeMutablePointer(to: &addr.sun_path) { dest in
        dest.withMemoryRebound(to: CChar.self, capacity: pathBytes.count + 1) { chars in
            for (index, byte) in pathBytes.enumerated() {
                chars[index] = CChar(bitPattern: byte)
            }
            chars[pathBytes.count] = 0
        }
    }
    let length = socklen_t(MemoryLayout<sockaddr_un>.offset(of: \.sun_path)! + pathBytes.count + 1)
    addr.sun_len = UInt8(length)
    return withUnsafePointer(to: &addr) { pointer in
        pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockaddrPtr in
            connect(fd, sockaddrPtr, length) == 0
        }
    }
}

/// Resolve the alias to the socket it targets, inside `Sockets/`.
private func resolvedAliasPath(_ alias: URL, in container: URL) -> String? {
    guard let target = try? FileManager.default.destinationOfSymbolicLink(atPath: alias.path) else {
        return nil
    }
    let fileName: String
    if target.hasPrefix("../") {
        fileName = String(target.dropFirst(3))
    } else {
        fileName = target
    }
    guard !fileName.contains("/"), !fileName.isEmpty else { return nil }
    return container.appendingPathComponent("Sockets").appendingPathComponent(fileName).path
}

/// Drive id to Finder display name for the published aliases, or
/// `nil` when the container cannot be resolved. `nil` is deliberately
/// distinct from an empty list: an ad-hoc build or a failed
/// entitlement must never be read as "no drives", because the removal
/// half would then delete every domain a team-signed install had
/// registered.
private func publishedDrives(group: String) -> [String: String]? {
    guard !group.isEmpty, !group.contains("/") else {
        return nil
    }
    guard let container = FileManager.default.containerURL(
        forSecurityApplicationGroupIdentifier: group
    ) else {
        log.notice("no resolved App Group container for \(group, privacy: .public)")
        return nil
    }
    let aliases = container.appendingPathComponent("Sockets/by-drive")
    let names = (try? FileManager.default.contentsOfDirectory(atPath: aliases.path)) ?? []
    var drives: [String: String] = [:]
    for name in names where name.hasSuffix(".sock") {
        let id = String(name.dropLast(".sock".count))
        guard isDriveId(id) else { continue }
        guard let socketPath = resolvedAliasPath(aliases.appendingPathComponent(name), in: container),
              socketResponds(at: socketPath)
        else {
            log.notice("skipping dead owner for \(id, privacy: .public)")
            continue
        }
        let labelURL = aliases.appendingPathComponent("\(id).label")
        guard let label = try? String(contentsOf: labelURL, encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines),
              let name = displayName(for: label)
        else {
            // No label means no live owner published this alias.
            log.notice("skipping unlabelled alias \(id, privacy: .public)")
            continue
        }
        drives[id] = name
    }
    return drives
}

// The registrar never removes domains. On this File Provider shape,
// `NSFileProviderManager.remove` makes fileproviderd tear down FPFS
// data for the provider, then fall back to the default domain — which
// this replicated provider cannot use — and leaves the remaining
// domains disconnected with no CloudStorage mount. It does not
// recover. A domain whose owner dies therefore stays registered; the
// extension already reports it as unavailable, and Finder shows an
// empty folder instead of a permanently wedged one. Domain removal is
// a deliberate, manually supervised operation, not a polling result.

/// Reconcile domains with the published drive aliases. Partial
/// progress stands: the next invocation retries what is outstanding.
/// Domains are only added: a drive that stops publishing keeps its
/// domain, because removing one wedges FPFS.
/// - Returns: 0 on success (including "container unavailable", which
///   leaves every domain untouched); 1 for malformed config, 2 for a
///   domain read failure, 3 for an add failure.
func reconcile(_ raw: String) -> (code: Int32, added: Int) {
    guard let decoded = try? JSONDecoder().decode(
        Config.self,
        from: raw.data(using: .utf8) ?? Data()
    ) else {
        log.error("invalid registrar config")
        return (1, 0)
    }
    guard let desired = publishedDrives(group: decoded.group) else {
        // No container means no evidence about drives, so add nothing.
        return (0, 0)
    }

    let read = Completion<Result<[NSFileProviderDomain], Error>>()
    NSFileProviderManager.getDomainsWithCompletionHandler { domains, error in
        if let error {
            read.resolve(.failure(error))
        } else {
            read.resolve(.success(domains))
        }
    }
    guard let readResult = read.wait(callbackTimeout()) else {
        log.error("timed out reading File Provider domains")
        return (2, 0)
    }
    let existing: [NSFileProviderDomain]
    switch readResult {
    case .failure(let error):
        log.error("cannot read File Provider domains: \(String(describing: error), privacy: .public)")
        return (2, 0)
    case .success(let domains):
        existing = domains
    }

    let current = Set(existing.map(\.identifier.rawValue))
    var failed = false
    var added = 0
    for (id, name) in desired where !current.contains(id) {
        let domain = NSFileProviderDomain(
            identifier: NSFileProviderDomainIdentifier(id),
            displayName: name
        )
        let add = Completion<Result<Void, Error>>()
        NSFileProviderManager.add(domain) { error in
            add.resolve(error.map { Result.failure($0) } ?? Result.success(()))
        }
        guard let addResult = add.wait(callbackTimeout()) else {
            log.error("timed out adding domain \(id, privacy: .public)")
            failed = true
            continue
        }
        switch addResult {
        case .failure(let error):
            // "already exists" is a benign race with a second caller.
            let ns = error as NSError
            if ns.domain == NSCocoaErrorDomain && ns.code == NSFileWriteFileExistsError {
                log.notice("domain \(id, privacy: .public) already registered")
            } else {
                log.error("cannot add domain \(id, privacy: .public): \(String(describing: error), privacy: .public)")
                failed = true
            }
        case .success:
            added += 1
            log.notice("registered domain \(id, privacy: .public)")
        }
    }
    return (failed ? 3 : 0, added)
}

// One resident process shares one reconcile pass across ticks. The
// launch agent uses `--watch`: calling the File Provider domain API
// from a fresh process every tick makes macOS re-ask the "access data
// from other apps" permission on every launch.
let args = Array(CommandLine.arguments.dropFirst())
let watching = args.first == "--watch"
let config = (watching ? args.dropFirst().first : args.first)
    // The launch agent passes the group rendered from WYRD_APP_GROUP;
    // this fallback serves one-shot manual runs through the same
    // helper (env override, then the built-in default).
    ?? #"{"group":"\#(AppGroup.identifier)"}"#

if watching {
    while true {
        let outcome = reconcile(config)
        print("added \(outcome.added)")
        fflush(stdout)
        sleep(5)
    }
}

let outcome = reconcile(config)
print("added \(outcome.added)")
exit(outcome.code)
