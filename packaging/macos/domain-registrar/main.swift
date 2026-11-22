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

/// Display name for a domain. The registry has no drive id and the
/// owner protocol carries none, so the stable id prefix is the label;
/// the identifier itself stays the full drive id.
private func displayName(for driveId: String) -> String {
    "Wyrd \(driveId.prefix(8))"
}

/// Drive ids with a published group socket. The app is entitled for
/// the same App Group as the extension, so this supported API both
/// materializes the container on a fresh machine and resolves its
/// real path. Without an effective entitlement (ad-hoc) it returns
/// nil and there is nothing to reconcile — the honest empty result.
private func publishedDriveIds(group: String) -> [String] {
    guard !group.isEmpty, !group.contains("/") else {
        return []
    }
    guard let container = FileManager.default.containerURL(
        forSecurityApplicationGroupIdentifier: group
    ) else {
        log.notice("no resolved App Group container for \(group, privacy: .public)")
        return []
    }
    let aliases = container.appendingPathComponent("Sockets/by-drive")
    let names = (try? FileManager.default.contentsOfDirectory(atPath: aliases.path)) ?? []
    return names.compactMap { name in
        guard name.hasSuffix(".sock") else { return nil }
        let id = String(name.dropLast(".sock".count))
        return isDriveId(id) ? id : nil
    }.sorted()
}

/// Reconcile domains with the published drive aliases. Partial
/// progress stands: the next invocation retries what is outstanding.
/// - Returns: 0 on success; 1 for malformed config, 2 for a domain
///   read failure, 3 for an add/remove failure.
func reconcile(_ raw: String) -> (code: Int32, added: Int, removed: Int) {
    guard let decoded = try? JSONDecoder().decode(
        Config.self,
        from: raw.data(using: .utf8) ?? Data()
    ) else {
        log.error("invalid registrar config")
        return (1, 0, 0)
    }

    let desired = Dictionary(
        uniqueKeysWithValues: publishedDriveIds(group: decoded.group).map {
            ($0, displayName(for: $0))
        }
    )
    let semaphore = DispatchSemaphore(value: 0)
    var existing: [NSFileProviderDomain] = []
    var readError: Error?
    NSFileProviderManager.getDomainsWithCompletionHandler { domains, error in
        existing = domains
        readError = error
        semaphore.signal()
    }
    if semaphore.wait(timeout: .now() + 10) == .timedOut {
        log.error("timed out reading File Provider domains")
        return (2, 0, 0)
    }
    if let readError {
        log.error("cannot read File Provider domains: \(String(describing: readError), privacy: .public)")
        return (2, 0, 0)
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
        var addError: Error?
        NSFileProviderManager.add(domain) { error in
            addError = error
            semaphore.signal()
        }
        guard semaphore.wait(timeout: .now() + 10) != .timedOut else {
            log.error("timed out adding domain \(id, privacy: .public)")
            failed = true
            continue
        }
        if let addError {
            // "already exists" is a benign race with a second caller.
            let ns = addError as NSError
            if ns.domain == NSCocoaErrorDomain && ns.code == NSFileWriteFileExistsError {
                log.notice("domain \(id, privacy: .public) already registered")
            } else {
                log.error("cannot add domain \(id, privacy: .public): \(String(describing: addError), privacy: .public)")
                failed = true
            }
        } else {
            added += 1
            log.notice("registered domain \(id, privacy: .public)")
        }
    }
    for domain in existing where isDriveId(domain.identifier.rawValue)
        && !desired.keys.contains(domain.identifier.rawValue)
    {
        let id = domain.identifier.rawValue
        var removeError: Error?
        NSFileProviderManager.remove(domain) { error in
            removeError = error
            semaphore.signal()
        }
        guard semaphore.wait(timeout: .now() + 10) != .timedOut else {
            log.error("timed out removing stale domain \(id, privacy: .public)")
            failed = true
            continue
        }
        if let removeError {
            log.error("cannot remove stale domain \(id, privacy: .public): \(String(describing: removeError), privacy: .public)")
            failed = true
        } else {
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
