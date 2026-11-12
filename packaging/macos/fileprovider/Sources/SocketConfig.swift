// Stage 2 socket configuration: resolve the per-drive owner socket
// through the `by-drive` alias, never by hashing. The alias
// `<container>/Sockets/by-drive/<drive-hex>.sock` is a symlink the
// daemon publishes beside the fingerprint-named socket it bound; the
// extension readlinks the alias and connects to the sibling target.
// Two consequences are deliberate:
// - the extension never computes the fingerprint — one
//   implementation (the daemon), so the derivation cannot drift;
// - the alias path itself may exceed `sun_path`, which is fine:
//   only the path passed to `connect()` must fit, and that is the
//   short sibling target. `readlink` has no such ceiling.
// The domain carries the drive id (64 lowercase hex chars). A
// production registry may qualify the domain identifier later; the
// strict 64-hex parse then extends additively, never silently.

import Foundation

/// How the extension reaches its drive's owner.
struct SocketConfig {
    /// The App Group granted literally as written — the system does
    /// not team-prefix group ids, so no substitution happens here.
    static let groupIdentifier = "group.md.enesis.wyrd"

    /// The configured drive, lowercase hex, from the domain.
    var driveHex: String

    /// Harness-only socket override: when set, `socketPath()`
    /// returns it without touching the App Group container. The
    /// appex never sets this; the Harness uses it to exercise the
    /// live store against a stub owner in a CLI binary with no
    /// entitlements.
    var overridePath: String?

    /// Parse the File Provider domain identifier for this drive.
    /// Stage 2 dev bootstrap: the identifier is exactly the drive
    /// id. Anything else is unconfigured, never a guessed drive.
    static func domainDrive(_ identifier: String) -> String? {
        let drive = identifier.lowercased()
        guard drive.count == 64,
            drive.allSatisfy({ $0.isHexDigit && !$0.isUppercase }),
            drive != String(repeating: "0", count: 64)
        else {
            return nil
        }
        return drive
    }

    /// Resolve the owner socket path: `containerURL` plus the
    /// by-drive alias, readlinked to its sibling target. The target
    /// filename must be a 16-hex-char fingerprint socket; the alias
    /// target must stay inside `Sockets/` (a `..` that escapes is a
    /// misconfiguration, never followed).
    func socketPath() throws -> String {
        if let overridePath {
            return overridePath
        }
        guard let container = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: Self.groupIdentifier
        ) else {
            throw SocketConfigError.noGroupContainer(Self.groupIdentifier)
        }
        return try Self.resolve(container: container, driveHex: driveHex)
    }

    /// Resolve against an explicit container URL (the Harness
    /// exercises this with a scratch container; the appex passes the
    /// entitled `containerURL`).
    static func resolve(container: URL, driveHex: String) throws -> String {
        let alias = container
            .appendingPathComponent("Sockets")
            .appendingPathComponent("by-drive")
            .appendingPathComponent("\(driveHex).sock")
        let target: String
        do {
            target = try FileManager.default.destinationOfSymbolicLink(atPath: alias.path)
        } catch {
            throw SocketConfigError.aliasMissing(alias.path)
        }
        guard !target.contains("/") || target.hasPrefix("../") else {
            throw SocketConfigError.aliasEscapes(target)
        }
        let fileName: String
        if target.hasPrefix("../") {
            // One level: the alias lives in `by-drive/` beside the
            // socket. Anything deeper still escapes.
            fileName = String(target.dropFirst(3))
            guard !fileName.contains("/") else {
                throw SocketConfigError.aliasEscapes(target)
            }
        } else {
            fileName = target
        }
        guard isFingerprintSocket(fileName) else {
            throw SocketConfigError.badTarget(target)
        }
        let resolved = container
            .appendingPathComponent("Sockets")
            .appendingPathComponent(fileName)
            .path
        guard resolved.utf8.count < 104 else {
            throw SocketConfigError.pathTooLong(resolved)
        }
        return resolved
    }

    private static func isFingerprintSocket(_ fileName: String) -> Bool {
        guard fileName.hasSuffix(".sock") else { return false }
        let stem = String(fileName.dropLast(".sock".count))
        return stem.count == 16
            && stem.allSatisfy({ $0.isHexDigit && !$0.isUppercase })
    }
}

/// Why resolution failed. All cases are configuration or transport,
/// never a synthesized deletion: the caller reports temporary
/// unavailability and retries on the next operation.
enum SocketConfigError: Error {
    case noGroupContainer(String)
    case aliasMissing(String)
    case aliasEscapes(String)
    case badTarget(String)
    case pathTooLong(String)
}
