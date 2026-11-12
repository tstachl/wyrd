// Stage 3 conflict inspector: one synthetic read-only report file
// per conflicted path (OD-17-B, presentation-only; DG-3 intact —
// the extension authors nothing).
//
// A conflicted path never enumerates as ordinary content (the
// enumerator excludes conflict rows). Instead each conflict row
// yields exactly one report item: a synthetic identifier keyed by
// (drive, logical location), a derived report filename, read-only
// capabilities, and a version that moves with the conflict's
// versions. Hydration serves only the generated report text —
// never a version's bytes — so opening the report cannot be
// mistaken for opening the file, and no Finder-side action touches
// either head. Labels are snapshot id plus timestamp; peer
// identity never crosses, and the listing order (snapshot id byte
// order) implies no authoritative head.

import FileProvider
import Foundation
import UniformTypeIdentifiers

/// The report filename suffix: derived from the conflicted entry
/// name, clearly synthetic, never an ordinary entry name. A real
/// entry carrying this suffix gets one more appended, so the report
/// never shadows content.
let conflictReportSuffix = ".wyrd-conflict.txt"

/// Derive the report filename for a conflicted entry name.
func conflictReportName(for entryName: String) -> String {
    "\(entryName)\(conflictReportSuffix)"
}

/// Assign report filenames for an ordered run of conflicted entry
/// names: the derived name stands unless a real sibling takes it,
/// in which case the smallest `-N` variant wins. Deterministic in
/// the listing, so enumeration and lookup agree.
func assignReportNames(conflictRows: [String], realNames: Set<String>) -> [String] {
    var taken = realNames
    return conflictRows.map { name in
        let base = conflictReportName(for: name)
        var candidate = base
        var counter = 2
        while taken.contains(candidate) {
            candidate = "\(name).wyrd-conflict-\(counter).txt"
            counter += 1
        }
        taken.insert(candidate)
        return candidate
    }
}

/// The per-version digest input: snapshot, timestamp, kind, and
/// content facts. Any version change — new snapshot, new timestamp,
/// new content — changes the input, which moves the conflict digest
/// and retires the stale report.
func conflictVersionDigest(_ version: OwnerConflictVersion) -> String {
    let kind: String
    let detail: String
    switch version.node {
    case .file(let size, let executable, let chunks, _):
        kind = "file"
        detail = "size=\(size) executable=\(executable) chunks=\(chunks.joined(separator: ","))"
    case .dir(let subtree, _):
        kind = "directory"
        detail = "subtree=\(subtree)"
    case .symlink(let target, _):
        kind = "symlink"
        detail = "target=\(target)"
    case .mergedDir(let subtrees, let identity):
        kind = "merged-directory"
        detail = "subtrees=\(subtrees.flatMap { $0 }.joined(separator: ",")) agreed=\(identity ?? "none")"
    case .conflict(let nested):
        // Nested conflicts never arise (merge flattens per-head
        // resolutions), but the wire can carry them: label, never
        // expand.
        kind = "conflict"
        detail = "nested-versions=\(nested.count)"
    }
    return "\(version.snapshot):\(version.timestamp):\(kind):\(detail)"
}

/// Escape a member-authored value for a line-oriented report:
/// entry names and symlink targets are arbitrary bytes except `/`
/// and NUL, so a newline could forge report structure (a fake
/// `--- Version … ---` line or footer). Backslash and the common
/// controls get short escapes; remaining C0 controls and DEL
/// become `?`. Hex ids (snapshots, chunks, subtrees) are safe by
/// construction and pass through untouched.
func escapeReportValue(_ value: String) -> String {
    var out = ""
    out.reserveCapacity(value.count)
    for scalar in value.unicodeScalars {
        switch scalar.value {
        case 0x5C:
            out.append("\\\\")
        case 0x0A:
            out.append("\\n")
        case 0x0D:
            out.append("\\r")
        case 0x09:
            out.append("\\t")
        case 0x00 ..< 0x20, 0x7F:
            out.append("?")
        default:
            out.append(Character(scalar))
        }
    }
    return out
}

/// A 64-bit FNV-1a digest over UTF-8, hex-rendered. Presentation
/// identity only (version movement), never integrity: no crypto
/// import, no new dependency.
func conflictDigestHex(_ text: String) -> String {    var hash: UInt64 = 0xcbf29ce484222325
    for byte in text.utf8 {
        hash ^= UInt64(byte)
        hash = hash &* 0x100000001b3
    }
    return String(format: "%016llx", hash)
}

/// The canonical conflict digest: per-version facts in snapshot id
/// byte order, joined deterministically. Any version change — new
/// snapshot, new timestamp, new content — moves the digest, which
/// moves the item version, which retires the stale report.
func conflictDigest(versions: [OwnerConflictVersion]) -> String {
    let ordered = versions.sorted { $0.snapshot < $1.snapshot }
    return conflictDigestHex(ordered.map { conflictVersionDigest($0) }.joined(separator: "\n"))
}

/// Render one timestamp: raw milliseconds plus the UTC calendar
/// rendering, or `unknown` for a pre-timestamp payload (missing
/// key decodes to `0`, never a failure).
func conflictTimestampLabel(_ timestamp: UInt64) -> String {
    guard timestamp != 0 else { return "unknown" }
    let date = Date(timeIntervalSince1970: TimeInterval(timestamp) / 1000.0)
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime]
    formatter.timeZone = TimeZone(identifier: "UTC")
    return "\(timestamp) (\(formatter.string(from: date)))"
}

/// Render the full report text for a conflicted path. Versions list
/// in snapshot id byte order for display only; the footer states
/// the read-only contract in user language.
func renderConflictReport(
    path: String,
    driveHex: String,
    versions: [OwnerConflictVersion],
    generation: UInt64,
    revision: UInt64
) -> Data {
    let ordered = versions.sorted { $0.snapshot < $1.snapshot }
    var lines: [String] = []
    lines.append("Wyrd conflict report")
    lines.append("Path: \(escapeReportValue(path))")
    lines.append("Drive: \(driveHex.lowercased())")
    lines.append("Serving generation: \(generation), revision: \(revision)")
    lines.append("Versions: \(ordered.count)")
    lines.append("")
    for (index, version) in ordered.enumerated() {
        switch version.node {
        case .file(let size, let executable, let chunks, _):
            lines.append("--- Version \(index + 1) of \(ordered.count) ---")
            lines.append("Snapshot: \(version.snapshot)")
            lines.append("Timestamp: \(conflictTimestampLabel(version.timestamp))")
            lines.append("Kind: file")
            lines.append("Size: \(size) bytes")
            lines.append("Executable: \(executable ? "yes" : "no")")
            lines.append("Chunks: \(chunks.count)")
        case .dir(let subtree, _):
            lines.append("--- Version \(index + 1) of \(ordered.count) ---")
            lines.append("Snapshot: \(version.snapshot)")
            lines.append("Timestamp: \(conflictTimestampLabel(version.timestamp))")
            lines.append("Kind: directory")
            lines.append("Subtree: \(subtree)")
        case .symlink(let target, _):
            lines.append("--- Version \(index + 1) of \(ordered.count) ---")
            lines.append("Snapshot: \(version.snapshot)")
            lines.append("Timestamp: \(conflictTimestampLabel(version.timestamp))")
            lines.append("Kind: symlink")
            lines.append("Target: \(escapeReportValue(target))")
        case .mergedDir(let subtrees, let identity):
            lines.append("--- Version \(index + 1) of \(ordered.count) ---")
            lines.append("Snapshot: \(version.snapshot)")
            lines.append("Timestamp: \(conflictTimestampLabel(version.timestamp))")
            lines.append("Kind: merged-directory")
            lines.append("Subtrees: \(subtrees.count)")
            lines.append("Agreed identity: \(identity ?? "none")")
        case .conflict(let nested):
            lines.append("--- Version \(index + 1) of \(ordered.count) ---")
            lines.append("Snapshot: \(version.snapshot)")
            lines.append("Timestamp: \(conflictTimestampLabel(version.timestamp))")
            lines.append("Kind: conflict")
            lines.append("Nested versions: \(nested.count) (not expanded)")
        }
        lines.append("")
    }
    lines.append("--- About this report ---")
    lines.append("This file is read-only. It cannot resolve the conflict:")
    lines.append("editing, renaming, copying, or deleting it changes")
    lines.append("nothing on the drive, and it never touches either version.")
    lines.append("No version above is authoritative; the order follows")
    lines.append("snapshot id byte order for display only.")
    lines.append("Resolve through the Wyrd owner workflow. This report")
    lines.append("disappears once the drive state changes.")
    lines.append("")
    return Data(lines.joined(separator: "\n").utf8)
}

/// A synthetic conflict-report item: read-only, report-only
/// hydration, invalidated by version movement or disappearance.
final class ConflictReportItem: NSObject, NSFileProviderItem {
    let itemIdentifier: NSFileProviderItemIdentifier
    let parentItemIdentifier: NSFileProviderItemIdentifier
    let filename: String
    /// The conflicted owner-relative path this report describes:
    /// the hydration source of truth, re-read live on every fetch.
    let conflictedPath: String
    let contentType: UTType = .plainText
    private let size: UInt64
    private let contentVersionData: Data
    private let metadataVersionData: Data

    init(
        driveHex: String,
        conflictedPath: String,
        reportName: String,
        versions: [OwnerConflictVersion],
        generation: UInt64,
        revision: UInt64,
        identifier: NSFileProviderItemIdentifier,
        parent: NSFileProviderItemIdentifier
    ) {
        self.itemIdentifier = identifier
        self.parentItemIdentifier = parent
        self.filename = reportName
        self.conflictedPath = conflictedPath
        let digest = conflictDigest(versions: versions)
        self.size = UInt64(
            renderConflictReport(
                path: conflictedPath,
                driveHex: driveHex,
                versions: versions,
                generation: generation,
                revision: revision
            ).count)
        let versionCore = "wyrd-conflict-v1:\(generation):\(revision):\(digest)"
        self.contentVersionData = Data(versionCore.utf8)
        self.metadataVersionData = Data("wyrd-conflict-mv1:\(generation):\(revision)".utf8)
        super.init()
    }

    var capabilities: NSFileProviderItemCapabilities {
        // Read-only, unconditionally: no rename, no reparent, no
        // delete, no trash. The mutation surface fails these closed
        // independently; this keeps the UI from offering them.
        [.allowsReading]
    }

    var itemVersion: NSFileProviderItemVersion {
        NSFileProviderItemVersion(
            contentVersion: contentVersionData,
            metadataVersion: metadataVersionData
        )
    }

    var documentSize: NSNumber? {
        NSNumber(value: size)
    }
}
