// Local diagnostics mode for the File Provider extension (T20 in
// `docs/contracts/trust.md`). This file owns two things and nothing
// else:
// - a read-only reader for the app-group mirror of the application-wide
//   level. The mirror distributes the value written by the settings
//   backend; it is never a second source of truth and is never written
//   here. Any problem fails closed to `default`.
// - a structured event model, a pure line builder and the logger
//   adapter. Emitting a field, marking it `.public` in unified logging,
//   and including it in an export are three separate decisions; every
//   detail carries all three explicitly so the Harness can pin them
//   without capturing a logger.
//
// Default mode adds no new lines at all: the existing safe-summary
// errors remain the default surface, byte-for-byte. The level is read
// at callback boundaries and governs subsequent operations only; work
// already in flight keeps the level it started under. Sinks stay
// bounded by unified-logging retention. No network call, upload or
// protocol field exists here.
//
// Mirror contract for the settings/desktop phases, which write it:
//   <App Group container>/Diagnostics/level.json
// body `{"version":1,"level":"default"|"detailed"}`. Unknown versions,
// unknown levels, malformed JSON and missing files all fail closed to
// `default`. File contents, credentials, keys and protocol identities
// are never read or emitted.

import FileProvider
import Foundation
import os

// MARK: - Level and read-only mirror

enum DiagnosticLevel: String, Equatable {
    case `default`
    case detailed
}

enum DiagnosticConfig {
    static let groupIdentifier = "group.md.enesis.wyrd"
    static let relativePath = "Diagnostics/level.json"

    /// Read at a callback boundary. No container, no file or any
    /// parse problem means `default`: diagnostics are opt-in and the
    /// mirror is a distribution channel, not an authority of its own.
    static func currentLevel() -> DiagnosticLevel {
        guard let container = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: groupIdentifier
        ) else {
            return .default
        }
        let url = container.appendingPathComponent(relativePath)
        guard let data = try? Data(contentsOf: url) else {
            return .default
        }
        return level(from: data)
    }

    /// Pure parse for the Harness. A v1 document with a known level is
    /// accepted; every other shape fails closed. Extra keys are
    /// ignored so an additive writer cannot break this reader.
    static func level(from data: Data) -> DiagnosticLevel {
        guard let object = try? JSONSerialization.jsonObject(with: data),
            let document = object as? [String: Any],
            let version = document["version"] as? Int,
            version == 1,
            let raw = document["level"] as? String,
            let level = DiagnosticLevel(rawValue: raw)
        else {
            return .default
        }
        return level
    }
}

// MARK: - Pure event model

enum DiagnosticOperation: String, Equatable {
    case createItem
    case modifyItem
    case deleteItem
}

/// A validation or mutation stage, not a prose message. Each refusal
/// in the three write callbacks names the stage it reached.
enum DiagnosticStage: String, Equatable {
    case cancelled
    case excluded
    case symlinkTemplate
    case trashRefused
    case stagedContentsRead
    case mutation
    case itemLookup
    case baseVersionMismatch
    case rename
    case contentOnNonFile
    case missingStagedContents
    case contentWrite
    case completed
}

enum DiagnosticVisibility: String, Equatable {
    case `public`
    case `private`
}

/// One structured field with three independent authorizations.
/// Being present in the line's `details` is "emitted"; `visibility`
/// governs unified logging; `exportable` governs the user-requested
/// local bundle. The latter two are set by the T20 allowlist in the
/// builder, never by the call site.
struct DiagnosticDetail: Equatable {
    let key: String
    let value: String
    let visibility: DiagnosticVisibility
    let exportable: Bool
}

struct DiagnosticErrorInfo: Equatable {
    let domain: String
    let code: Int
    /// Owner or system text may embed names. Emitted privately and
    /// never made public; useful context in a deliberate export.
    let detail: String?
}

struct DiagnosticEvent: Equatable {
    let operation: DiagnosticOperation
    let stage: DiagnosticStage
    let succeeded: Bool
    let filename: String?
    let contentType: String?
    let parentIdentifier: String?
    let symlinkTarget: String?
    let options: [String]
    let hadStagedContents: Bool?
    let error: DiagnosticErrorInfo?
    let storeError: String?
}

struct DiagnosticLine: Equatable {
    let summary: String
    let details: [DiagnosticDetail]
}

// MARK: - Pure line builder

enum DiagnosticLineBuilder {
    /// `nil` in default mode means "emit nothing new"; the caller must
    /// not add a line of its own. The summary is safe summary-level
    /// detail; every sensitive field is a structured detail whose
    /// visibility and exportability the T20 allowlist decides.
    static func buildLine(
        event: DiagnosticEvent,
        level: DiagnosticLevel,
        sessionID: String
    ) -> DiagnosticLine? {
        guard level == .detailed else { return nil }

        let outcome = event.succeeded ? "ok" : "failed"
        var details: [DiagnosticDetail] = [
            DiagnosticDetail(
                key: "session", value: sessionID,
                visibility: .public, exportable: true)
        ]
        if let filename = event.filename {
            // "Allowed": emitted, public, exportable.
            details.append(DiagnosticDetail(
                key: "filename", value: filename,
                visibility: .public, exportable: true))
        }
        if let contentType = event.contentType {
            details.append(DiagnosticDetail(
                key: "content-type", value: contentType,
                visibility: .public, exportable: true))
        }
        if let parent = event.parentIdentifier {
            // "Allowed locally": emitted and exportable, but redacted
            // in the default unified-log view.
            details.append(DiagnosticDetail(
                key: "parent", value: parent,
                visibility: .private, exportable: true))
        }
        if let target = event.symlinkTarget {
            // The decision input for the -2005 investigation.
            // "Allowed locally, subject to explicit policy": the
            // explicit policy is this bug — a symlink-shaped template
            // for a regular file is not diagnosable without its
            // target. Emitted, private, and exportable on request.
            details.append(DiagnosticDetail(
                key: "symlink-target", value: target,
                visibility: .private, exportable: true))
        }
        if !event.options.isEmpty {
            details.append(DiagnosticDetail(
                key: "options", value: event.options.joined(separator: ","),
                visibility: .public, exportable: true))
        }
        if let staged = event.hadStagedContents {
            details.append(DiagnosticDetail(
                key: "staged-contents", value: staged ? "present" : "absent",
                visibility: .public, exportable: true))
        }
        if let store = event.storeError {
            details.append(DiagnosticDetail(
                key: "store-error", value: store,
                visibility: .private, exportable: true))
        }
        if let error = event.error {
            details.append(DiagnosticDetail(
                key: "error-domain", value: error.domain,
                visibility: .public, exportable: true))
            details.append(DiagnosticDetail(
                key: "error-code", value: String(error.code),
                visibility: .public, exportable: true))
            if let detail = error.detail {
                details.append(DiagnosticDetail(
                    key: "error-detail", value: detail,
                    visibility: .private, exportable: true))
            }
        }
        // No content id, identity, membership data, file contents,
        // credentials or keys. T20 notes a provider failure is
        // diagnosable without them; no explicit justification exists
        // in this phase, so those fields are not emitted at all.
        return DiagnosticLine(
            summary: "\(event.operation.rawValue) \(outcome) stage=\(event.stage.rawValue)",
            details: details)
    }
}

// MARK: - Field rendering helpers (pure)

enum DiagnosticFields {
    static func describe(_ error: Error) -> DiagnosticErrorInfo {
        let ns = error as NSError
        return DiagnosticErrorInfo(
            domain: ns.domain, code: ns.code, detail: ns.localizedDescription)
    }

    /// A stable label for the owner-side error: the case and, for a
    /// refusal, the owner's closed code. No message text, so no
    /// filename can leak through this field.
    static func storeLabel(_ error: LiveStoreError) -> String {
        switch error {
        case .notFound: return "notFound"
        case .sync: return "sync"
        case .refused(let code, _): return "refused:\(code)"
        case .cancelled: return "cancelled"
        case .temporary: return "temporary"
        }
    }

    static func createOptionNames(_ options: NSFileProviderCreateItemOptions) -> [String] {
        var names: [String] = []
        let known: NSFileProviderCreateItemOptions = [.mayAlreadyExist, .deletionConflicted]
        if options.contains(.mayAlreadyExist) { names.append("mayAlreadyExist") }
        if options.contains(.deletionConflicted) { names.append("deletionConflicted") }
        appendUnknown(names: &names, value: options.rawValue, known: known.rawValue)
        return names
    }

    static func modifyOptionNames(_ options: NSFileProviderModifyItemOptions) -> [String] {
        var names: [String] = []
        let known: NSFileProviderModifyItemOptions = [.mayAlreadyExist]
        if options.contains(.mayAlreadyExist) { names.append("mayAlreadyExist") }
        appendUnknown(names: &names, value: options.rawValue, known: known.rawValue)
        return names
    }

    static func deleteOptionNames(_ options: NSFileProviderDeleteItemOptions) -> [String] {
        var names: [String] = []
        let known: NSFileProviderDeleteItemOptions = [.recursive]
        if options.contains(.recursive) { names.append("recursive") }
        appendUnknown(names: &names, value: options.rawValue, known: known.rawValue)
        return names
    }

    /// Changed fields are decision inputs too: which attribute Finder
    /// actually asked to write. The three the adapter handles are
    /// named; any other bit is kept as a hex tag, never guessed.
    static func fieldNames(_ fields: NSFileProviderItemFields) -> [String] {
        var names: [String] = []
        let known: NSFileProviderItemFields = [
            .contents, .filename, .parentItemIdentifier
        ]
        if fields.contains(.contents) { names.append("contents") }
        if fields.contains(.filename) { names.append("filename") }
        if fields.contains(.parentItemIdentifier) { names.append("parentItemIdentifier") }
        appendUnknown(names: &names, value: fields.rawValue, known: known.rawValue)
        return names
    }

    private static func appendUnknown(
        names: inout [String], value: UInt, known: UInt
    ) {
        let extra = value & ~known
        if extra != 0 {
            names.append("other:0x\(String(extra, radix: 16))")
        }
    }
}

// MARK: - Logger adapter

/// Builds an event from the callback inputs and emits the line. Kept
/// separate from the builder so the builder stays pure and testable.
struct DiagnosticRecorder {
    let log: Logger
    let sessionID: String
    /// Optional local sink for the user-requested bundle. `nil` in
    /// fixtures and ad-hoc builds: unified logging still works, and
    /// no local record file is created.
    let sink: DiagnosticsRecordSink?

    init(log: Logger, sessionID: String, sink: DiagnosticsRecordSink? = nil) {
        self.log = log
        self.sessionID = sessionID
        self.sink = sink
    }

    func record(
        _ level: DiagnosticLevel,
        _ operation: DiagnosticOperation,
        _ stage: DiagnosticStage,
        item: NSFileProviderItem? = nil,
        parent: NSFileProviderItemIdentifier? = nil,
        options: [String] = [],
        changedFields: NSFileProviderItemFields = [],
        stagedContents: Bool? = nil,
        symlinkTarget: String? = nil,
        error: Error? = nil,
        storeError: LiveStoreError? = nil,
        succeeded: Bool = false
    ) {
        guard level == .detailed else { return }
        var allOptions = options
        allOptions.append(contentsOf: DiagnosticFields.fieldNames(changedFields))
        let event = DiagnosticEvent(
            operation: operation,
            stage: stage,
            succeeded: succeeded,
            filename: item?.filename,
            contentType: item?.contentType?.identifier,
            parentIdentifier: (parent ?? item?.parentItemIdentifier)?.rawValue,
            symlinkTarget: symlinkTarget,
            options: allOptions,
            hadStagedContents: stagedContents,
            error: error.map(DiagnosticFields.describe),
            storeError: storeError.map(DiagnosticFields.storeLabel))
        guard let line = DiagnosticLineBuilder.buildLine(
            event: event, level: level, sessionID: sessionID
        ) else {
            return
        }
        sink?.append(line, sessionID: sessionID)
        log.notice("\(line.summary, privacy: .public)")
        for detail in line.details {
            switch detail.visibility {
            case .public:
                log.notice("  \(detail.key)=\(detail.value, privacy: .public)")
            case .private:
                log.notice("  \(detail.key)=\(detail.value)")
            }
        }
    }
}
