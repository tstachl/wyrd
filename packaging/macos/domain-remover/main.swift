// Supervised removal of every File Provider domain for this provider.
//
// Separate from the domain registrar on purpose. The registrar is
// add-only: removing one stale domain while other domains still hold
// FPFS state makes fileproviderd fall back to the default domain, which
// this replicated provider cannot use. This helper is invoked by the
// uninstall script only, with an explicit --all flag, so removal is a
// deliberate operation on the whole provider and never a polling result.
//
// It is compiled and signed with the containing app's bundle identifier
// so NSFileProviderManager acts on the same provider as the app.

import FileProvider
import Foundation
import os

private let log = Logger(
    subsystem: "md.enesis.wyrd.domain-remover",
    category: "domains"
)

/// One completion per call. A fresh object prevents a late callback
/// from satisfying the wait for a later operation.
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

private func callbackTimeout() -> DispatchTime { .now() + 15 }

/// An unreadable domain list is never the same as an empty one. Mapping
/// either failure to "no domains" would make the uninstall report a
/// clean reset when it did nothing.
private enum DomainRead {
    case ok([NSFileProviderDomain])
    case failed(String)
}

private func domains() -> DomainRead {
    let done = Completion<DomainRead>()
    NSFileProviderManager.getDomainsWithCompletionHandler { domains, error in
        if let error {
            let detail = String(describing: error)
            log.error("domain read failed: \(detail, privacy: .public)")
            done.resolve(.failed(detail))
        } else {
            done.resolve(.ok(domains))
        }
    }
    return done.wait(callbackTimeout()) ?? .failed("domain read timed out")
}

private func remove(_ domain: NSFileProviderDomain) -> Bool {
    let done = Completion<Bool>()
    NSFileProviderManager.remove(domain) { error in
        if let error {
            log.error("remove failed for \(domain.identifier.rawValue, privacy: .public): \(String(describing: error), privacy: .public)")
            done.resolve(false)
        } else {
            done.resolve(true)
        }
    }
    return done.wait(callbackTimeout()) ?? false
}

let args = Array(CommandLine.arguments.dropFirst())
guard args == ["--all"] else {
    fputs("usage: WyrdDomainRemover --all\n", stderr)
    fputs("error: refusing to remove fewer than every domain of this provider.\n", stderr)
    exit(2)
}

private let read: DomainRead = domains()
switch read {
case .failed(let detail):
    fputs("error: could not enumerate domains: \(detail)\n", stderr)
    exit(1)
case .ok(let found):
    if found.isEmpty {
        print("domains: none registered")
        exit(0)
    }
    print("domains: removing \(found.count) registered domain(s)")
    var failures = 0
    for domain in found {
        print("  \(domain.identifier.rawValue) \(domain.displayName)")
        if !remove(domain) {
            failures += 1
        }
    }
    if failures == 0 {
        print("domains: all removed")
        exit(0)
    }
    fputs("error: \(failures) domain removal(s) failed.\n", stderr)
    exit(1)
}
