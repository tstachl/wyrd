// Stage 3 mutation errors: the owner refusal → File Provider
// error table for write paths. Reads map in `LiveStore.mapError`;
// mutations need finer distinctions (a name collision is not a
// sync failure — the system renames and retries), so the mutation
// surface carries the owner code through `LiveStoreError.refused`
// and this table renders it at the callback boundary. Codes the
// owner never sends on these paths still map closed, never to a
// guess.

import FileProvider
import Foundation
import os

/// Render a mutation refusal at the File Provider boundary.
func nsErrorForMutation(code: String, message: String, what: String, log: Logger) -> NSFileProviderError {
    switch code {
    case "AlreadyExists":
        // The system renames and re-issues the call; never a sync
        // failure.
        return NSFileProviderError(.filenameCollision)
    case "DirectoryNotEmpty":
        // The system re-creates the item from our metadata.
        return NSFileProviderError(.directoryNotEmpty)
    case "NotFound":
        // Parent (create) or item (modify/delete) is gone. For
        // modify/delete the header wants `noSuchItem` so the
        // system drops the disk copy; for create the same code
        // makes the system re-create the parent first.
        return NSFileProviderError(.noSuchItem)
    case "Timeout", "Unavailable":
        // Transient: back off and retry when signalled.
        return NSFileProviderError(.serverUnreachable)
    default:
        // Stale, StaleParent (after its one recapture), StaleEntry,
        // Conflicted, InvalidRename, TooLarge (server backstop —
        // the client pre-checks), Malformed, and every protocol or
        // engine failure: deterministic, never retry blind.
        log.error("mutation \(what) refused \(code): \(message)")
        return NSFileProviderError(.cannotSynchronize)
    }
}

/// The unconfirmed verdict after a transport break mid-mutation:
/// the request may or may not have committed. Never success,
/// never failure — reconcile first, then decide.
enum MutationReconcile {
    /// Re-read proves the write landed; report it, never retry.
    case committed
    /// Re-read proves the write did not land; one retry is safe.
    case absent
    /// Re-read proves someone else moved the entry; fail closed,
    /// never touch the replacement.
    case superseded(String)
    /// Re-read reached no verdict; surface transient and let the
    /// system retry the whole callback later.
    case unknown(String)
}

/// A single-shot completion gate for one provider callback. The
/// progress cancellation handler and the mutation driver race: the
/// first to arrive owns the completion, the loser is silent — so
/// the system never sees a double completion, and a cancel after
/// the owner's terminal answer still reports the answer
/// (post-accept), not a cancellation.
final class SingleCompletion {
    private let lock = NSLock()
    private var done = false

    /// Run `body` unless a completion already fired. Returns
    /// whether this caller owned the completion.
    @discardableResult
    func fire(_ body: () -> Void) -> Bool {
        lock.withLock {
            guard !done else { return false }
            done = true
            body()
            return true
        }
    }

    var isDone: Bool {
        lock.withLock { done }
    }
}

/// The cancellation error the File Provider headers name: an
/// `NSError` in `NSCocoaErrorDomain`, never a File Provider code.
func userCancelledError() -> NSError {
    NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError)
}

/// Signal the working set after a confirmed local mutation. For
/// the replicated extension only `.workingSet` is honored — other
/// container identifiers are ignored by the system — so every
/// mutation signals the working set regardless of which container
/// changed; the parent-vs-item distinction in the Stage 3 spec is
/// about *what* changed, not the signal target. No manager (the
/// Harness, a misconfigured domain) is a logged no-op, never a
/// failure: the completion already told the system what changed.
func signalWorkingSet(domain: NSFileProviderDomain, log: Logger) {
    guard let manager = NSFileProviderManager(for: domain) else {
        log.error("no manager for domain, working set signal skipped")
        return
    }
    manager.signalEnumerator(for: .workingSet) { error in
        if let error {
            log.error("working set signal failed: \(String(describing: error))")
        }
    }
}
