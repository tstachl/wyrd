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

private let callbackTimeout: DispatchTime = .now() + 10

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
/// metadata. Accept only registry-style names; anything else falls
/// back to the id prefix rather than trusting a file in a shared
/// directory. The domain identifier stays the full drive id.
private func displayName(for driveId: String, label: String?) -> String {
    if let label, !label.isEmpty, label.count <= 64,
       label.utf8.allSatisfy({ byte in
           (byte >= 97 && byte <= 122) || (byte >= 48 && byte <= 57) || byte == 45
       }) {
        return label
    }
    return "Wyrd \(driveId.prefix(8))"
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
        let labelURL = aliases.appendingPathComponent("\(id).label")
        let label = try? String(contentsOf: labelURL, encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        drives[id] = displayName(for: id, label: label)
    }
    return drives
}

/// Reconcile domains with the published drive aliases. Partial
/// progress stands: the next invocation retries what is outstanding.
/// - Returns: 0 on success (including "container unavailable", which
///   leaves every domain untouched); 1 for malformed config, 2 for a
///   domain read failure, 3 for an add/remove failure.
func reconcile(_ raw: String) -> (code: Int32, added: Int, removed: Int) {
    guard let decoded = try? JSONDecoder().decode(
        Config.self,
        from: raw.data(using: .utf8) ?? Data()
    ) else {
        log.error("invalid registrar config")
        return (1, 0, 0)
    }
    guard let desired = publishedDrives(group: decoded.group) else {
        // No container means no evidence about drives. Skip the whole
        // reconciliation, removals included.
        return (0, 0, 0)
    }

    let read = Completion<Result<[NSFileProviderDomain], Error>>()
    NSFileProviderManager.getDomainsWithCompletionHandler { domains, error in
        if let error {
            read.resolve(.failure(error))
        } else {
            read.resolve(.success(domains))
        }
    }
    guard let readResult = read.wait(callbackTimeout) else {
        log.error("timed out reading File Provider domains")
        return (2, 0, 0)
    }
    let existing: [NSFileProviderDomain]
    switch readResult {
    case .failure(let error):
        log.error("cannot read File Provider domains: \(String(describing: error), privacy: .public)")
        return (2, 0, 0)
    case .success(let domains):
        existing = domains
    }

    let current = Set(existing.map(\.identifier.rawValue))
    var failed = false
    var added = 0
    var removed = 0
    for (id, name) in desired where !current.contains(id) {
        let domain = NSFileProviderDomain(
            identifier: NSFileProviderDomainIdentifier(id),
            displayName: name
        )
        let add = Completion<Result<Void, Error>>()
        NSFileProviderManager.add(domain) { error in
            add.resolve(error.map { Result.failure($0) } ?? Result.success(()))
        }
        guard let addResult = add.wait(callbackTimeout) else {
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
    for domain in existing where isDriveId(domain.identifier.rawValue)
        && !desired.keys.contains(domain.identifier.rawValue)
    {
        let id = domain.identifier.rawValue
        let remove = Completion<Result<Void, Error>>()
        NSFileProviderManager.remove(domain) { error in
            remove.resolve(error.map { Result.failure($0) } ?? Result.success(()))
        }
        guard let removeResult = remove.wait(callbackTimeout) else {
            log.error("timed out removing stale domain \(id, privacy: .public)")
            failed = true
            continue
        }
        switch removeResult {
        case .failure(let error):
            log.error("cannot remove stale domain \(id, privacy: .public): \(String(describing: error), privacy: .public)")
            failed = true
        case .success:
            removed += 1
            log.notice("removed stale domain \(id, privacy: .public)")
        }
    }
    return (failed ? 3 : 0, added, removed)
}

let config = CommandLine.arguments.dropFirst().first
    ?? #"{"group":"group.md.enesis.wyrd"}"#
let outcome = reconcile(config)
print("added \(outcome.added) removed \(outcome.removed)")
exit(outcome.code)
