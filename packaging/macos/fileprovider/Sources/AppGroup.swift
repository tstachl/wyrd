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
    /// the shell helper; the parity pin lives in the Harness.
    static let defaultIdentifier = "JU2RFHWQPH.group.md.enesis.wyrd"

    static let identifier: String = {
        // The unsandboxed registrar can read the override directly;
        // the sandboxed appex cannot, so its build-rendered Info.plist
        // is the authoritative source there.
        if let fromEnv = ProcessInfo.processInfo.environment["WYRD_APP_GROUP"],
            !fromEnv.isEmpty
        {
            return fromEnv
        }
        let info = Bundle(for: AppGroupBundleFinder.self).infoDictionary
        guard let extensionInfo = info?["NSExtension"] as? [String: Any],
            let value = extensionInfo["NSExtensionFileProviderDocumentGroup"] as? String,
            !value.isEmpty, !value.hasPrefix("@")
        else {
            return defaultIdentifier
        }
        return value
    }()
}
