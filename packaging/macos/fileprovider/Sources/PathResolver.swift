// Stage 2 path resolution: the owner interface is path-addressed
// while File Provider is identifier-addressed, so the adapter keeps
// an identifier-to-path map built during enumeration — presentation
// state, never authority. Entries are verified on use (the owner's
// token must match), and a miss re-resolves with a bounded
// full-tree walk. The walk classifies explicitly: found, conflicted
// (a version carries the token — fail closed, never a synthetic
// deletion), absent, or budget-exceeded. An unavailable child is
// therefore never confused with an absent one at this boundary.

import FileProvider
import Foundation
import os

/// The walk verdict for one stable token.
enum WalkOutcome: Equatable {
    /// The token serves at this path under this parent.
    case found(path: String, parent: NSFileProviderItemIdentifier)
    /// No row carries the token, but a conflict version does: the
    /// entry exists and is conflicted. Fail closed, never 404.
    case conflicted
    /// The token names nothing in this generation.
    case absent
    /// The walk exceeded its entry budget before deciding: fail
    /// closed, never a guess.
    case budgetExceeded
}

/// Identifier-to-path map plus the bounded re-resolution walk.
final class PathResolver {
    /// The traversal budget mirrors the policy-status walk: one
    /// prefix, never the whole drive unbounded.
    static let defaultWalkBudget = 4096

    let driveHex: String
    /// The walk budget, settable for tests; production uses the
    /// default.
    var walkBudget = defaultWalkBudget
    /// File Provider callbacks overlap (enumeration with item and
    /// content requests), so every map access serializes. The walk
    /// itself runs outside the lock — it only reads the owner —
    /// and records through it.
    private let lock = NSLock()
    private var idToPath: [String: (path: String, parent: NSFileProviderItemIdentifier)] = [:]

    init(driveHex: String) {
        self.driveHex = driveHex
    }

    /// Record an enumerated row. Stable rows key by token, legacy
    /// rows by path — both verified on use, so a stale record only
    /// costs one walk, never a wrong item.
    func record(identifier: NSFileProviderItemIdentifier, path: String, parent: NSFileProviderItemIdentifier) {
        lock.withLock {
            idToPath[identifier.rawValue] = (path, parent)
        }
    }

    /// The cached path and parent, if any.
    func cached(identifier: NSFileProviderItemIdentifier) -> (
        path: String, parent: NSFileProviderItemIdentifier
    )? {
        lock.withLock {
            idToPath[identifier.rawValue]
        }
    }

    /// Re-resolve one stable token with a bounded breadth-first walk
    /// from the root at the owner's current state. Every `readdir`
    /// is its own connection; a transport break throws, it never
    /// resolves to a guess.
    func walk(identityHex: String, client: OwnerClient, log: Logger) throws -> WalkOutcome {
        let budget = walkBudget
        let wanted = identityHex.lowercased()
        var queue: [(path: String, identifier: NSFileProviderItemIdentifier)] = [
            ("", .rootContainer)
        ]
        var seen = 0
        var conflictMatch = false
        while !queue.isEmpty {
            let dir = queue.removeFirst()
            let entries: [OwnerDirEntry]
            do {
                entries = try client.readdir(path: dir.path).entries
            } catch {
                // The tree moved under the walk (or the owner went
                // away): fail the resolution, never serve half a tree.
                throw error
            }
            for entry in entries {
                seen += 1
                guard seen <= budget else {
                    log.error("path walk exceeded its budget resolving an identifier")
                    return .budgetExceeded
                }
                let childPath = dir.path.isEmpty ? entry.name : "\(dir.path)/\(entry.name)"
                switch entry.node {
                case .file(_, _, _, let identity),
                    .dir(_, let identity),
                    .symlink(_, let identity):
                    if let identity, identity.lowercased() == wanted {
                        return .found(path: childPath, parent: dir.identifier)
                    }
                    if case .dir = entry.node {
                        queue.append(
                            (childPath, identifierFor(identity: identity, path: childPath))
                        )
                    }
                case .mergedDir(_, let identity):
                    if let identity, identity.lowercased() == wanted {
                        return .found(path: childPath, parent: dir.identifier)
                    }
                    queue.append(
                        (childPath, identifierFor(identity: identity, path: childPath))
                    )
                case .conflict(let versions):
                    for version in versions {
                        if let token = nodeIdentity(version.node),
                            token.lowercased() == wanted
                        {
                            conflictMatch = true
                        }
                    }
                }
            }
        }
        return conflictMatch ? .conflicted : .absent
    }

    private func identifierFor(identity: String?, path: String) -> NSFileProviderItemIdentifier {
        if let identity {
            return NSFileProviderItemIdentifier(
                WyrdItemIdentifier.stableID(driveHex: driveHex, identityHex: identity)
            )
        }
        return NSFileProviderItemIdentifier(WyrdItemIdentifier.legacyID(path: path))
    }
}

/// The owner token a node reports, or `nil` for conflict rows and
/// rows that predate identity.
func nodeIdentity(_ node: OwnerNode) -> String? {
    switch node {
    case .file(_, _, _, let identity):
        return identity
    case .dir(_, let identity):
        return identity
    case .symlink(_, let identity):
        return identity
    case .mergedDir(_, let identity):
        return identity
    case .conflict:
        return nil
    }
}
