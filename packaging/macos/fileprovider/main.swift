// Entry point for the file provider extension.
//
// fileproviderd loads the principal class named by Info.plist from this
// executable and drives it on its own; main only keeps the process
// alive. Building as an executable (rather than a Mach-O bundle) is
// also what lets codesign embed the App Group entitlement — bundles
// silently drop it, which leaves the extension unable to resolve its
// group container.

import Foundation

RunLoop.current.run()
