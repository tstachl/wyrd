// Bounded local record sink for the user-requested diagnostics bundle
// (T20 in `docs/contracts/trust.md`). This file adds no new surface for
// normal operation: it writes nothing unless the File Provider is in
// detailed mode and the builder already produced a line.
//
// Three authorizations stay separate. `DiagnosticLineBuilder` decides
// that a field is emitted; its `visibility` decides unified logging;
// its `exportable` flag decides this sink. A field that is not
// exportable is never written here, even when it was emitted and was
// public in the unified log. The sink applies its own key allowlist as
// a second gate, so an accidental new detail can never widen the
// bundle without an explicit change in this file.
//
// Records are local only. Nothing here opens a network connection,
// and no other process reads this file except the user-requested
// bundle exporter in the desktop shell. The file lives in the private
// `Diagnostics/` leaf of the App Group container; the container itself
// belongs to the signed app and is never created from here.

import Foundation

enum DiagnosticsRecordSinkError: Error {
    case noGroupContainer
}

struct DiagnosticsRecordSink {
    /// The sink writes only these keys. This is an allowlist, not a
    /// denylist: a future detail needs an explicit entry here before
    /// it can reach an exported bundle.
    static let allowedKeys: Set<String> = [
        "session",
        "filename",
        "content-type",
        "parent",
        "symlink-target",
        "options",
        "staged-contents",
        "store-error",
        "error-domain",
        "error-code",
        "error-detail",
    ]

    /// Rotation bound. Two files at most; a hand-edited or oversized
    /// file is rotated away rather than growing without limit.
    static let maxBytes = 512 * 1024
    static let fileName = "records.jsonl"
    static let rotatedFileName = "records.1.jsonl"

    let directory: URL?

    /// Resolve the sink from the App Group entitlement. `nil` is a
    /// clean "no sink" answer for ad-hoc builds: diagnostics still
    /// emit to unified logging, and the bundle simply has no local
    /// File Provider records to collect.
    static func defaultSink() -> DiagnosticsRecordSink {
        let directory = FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: DiagnosticConfig.groupIdentifier)?
            .appendingPathComponent("Diagnostics", isDirectory: true)
        return DiagnosticsRecordSink(directory: directory)
    }

    /// Pure projection from an already-built line to one JSONL record.
    /// Only exportable, allowlisted keys survive. The caller supplies
    /// the timestamp so this stays deterministic for the Harness.
    static func record(
        from line: DiagnosticLine,
        timestamp: String,
        sessionID: String
    ) -> [String: Any] {
        var fields: [String: String] = [:]
        for detail in line.details where detail.exportable && allowedKeys.contains(detail.key) {
            fields[detail.key] = detail.value
        }
        // Correlation uses the extension's ephemeral UUID. No
        // DriveId, device identity, content id or protocol identity
        // has a place in this record.
        fields["session"] = sessionID
        return [
            "version": 1,
            "timestamp": timestamp,
            "summary": line.summary,
            "fields": fields,
        ]
    }

    private static let timestampFormatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()

    /// Append one record. All failures are swallowed: diagnostics
    /// must never change the File Provider callback outcome.
    func append(_ line: DiagnosticLine, sessionID: String, at date: Date = Date()) {
        guard let directory else { return }
        let file = directory.appendingPathComponent(Self.fileName)
        let record = Self.record(
            from: line,
            timestamp: Self.timestampFormatter.string(from: date),
            sessionID: sessionID)
        guard let json = try? JSONSerialization.data(withJSONObject: record) else { return }
        var payload = json
        payload.append(0x0A)

        let manager = FileManager.default
        guard (try? manager.createDirectory(
            at: directory,
            withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700])) != nil
        else { return }

        let existing = (try? Data(contentsOf: file))?.count ?? 0
        if existing > 0 && existing + payload.count > Self.maxBytes {
            let rotated = directory.appendingPathComponent(Self.rotatedFileName)
            try? manager.removeItem(at: rotated)
            try? manager.moveItem(at: file, to: rotated)
        }
        if !manager.fileExists(atPath: file.path) {
            guard manager.createFile(
                atPath: file.path,
                contents: nil,
                attributes: [.posixPermissions: 0o600])
            else { return }
        }
        guard let handle = try? FileHandle(forWritingTo: file) else { return }
        defer { try? handle.close() }
        _ = try? handle.seekToEnd()
        try? handle.write(contentsOf: payload)
    }
}
