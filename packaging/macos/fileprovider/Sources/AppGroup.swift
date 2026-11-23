// One source of truth for the App Group id inside the extension.
//
// `build-appex.sh` renders WYRD_APP_GROUP into the appex Info.plist
// (`NSExtensionFileProviderDocumentGroup`) before signing, so the
// Apple-facing grant and every Swift consumer read the same value.
// Anything missing or unrendered falls closed to the default: the
// Harness compiles these sources into an executable with no appex
// Info.plist and must stay deterministic.

import Foundation

private final class AppGroupBundleFinder {}

enum AppGroup {
    /// The built-in default. Keep in step with the Rust accessor and
    /// the shell helper; the Harness pins the cross-language agreement.
    static let defaultIdentifier = "JU2RFHWQPH.group.md.enesis.wyrd"
    static let environmentKey = "WYRD_APP_GROUP"

    /// Pure resolution so the Harness can exercise the override and
    /// the fail-closed fallbacks without a signed appex.
    static func identifier(from environment: [String: String], info: [String: Any]?) -> String {
        // The unsandboxed registrar can read the override directly;
        // the sandboxed appex cannot, so its build-rendered Info.plist
        // is the authoritative source there.
        if let fromEnv = environment[environmentKey], !fromEnv.isEmpty {
            return fromEnv
        }
        guard let extensionInfo = info?["NSExtension"] as? [String: Any],
            let value = extensionInfo["NSExtensionFileProviderDocumentGroup"] as? String,
            !value.isEmpty, !value.hasPrefix("@")
        else {
            return defaultIdentifier
        }
        return value
    }

    static let identifier: String = {
        let info = Bundle(for: AppGroupBundleFinder.self).infoDictionary
        return identifier(from: ProcessInfo.processInfo.environment, info: info)
    }()
}
