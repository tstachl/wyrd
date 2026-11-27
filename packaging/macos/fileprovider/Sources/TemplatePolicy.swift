// Stage 3 boundary policy for `createItem` templates.
//
// macOS sometimes presents a symlink-shaped template for a regular
// cross-provider import: `symlinkTargetPath` names a staging location,
// while `contentType` is the source file's ordinary type and staged
// contents are present. Refusing every template with a target produced
// `FP -2005` for ordinary files (issue `…qqs9f0l25…`).
//
// Wyrd v0 has no symlink authoring (`docs/contracts/write-path.md`), so
// a genuine symlink must still fail closed. The content type is the
// discriminator: only `public.symlink` declares authoring a link.
// A regular type with a stale target is authored from its contents.

import UniformTypeIdentifiers

/// The decision `createItem` makes before it touches the owner. Keeping
/// the whole gate here lets the Harness drive a template-shaped input
/// without a `fileproviderd` template or a live owner socket.
enum TemplateDisposition: Equatable {
    /// No symlink authoring: author a file from its staged contents (or
    /// a directory), even when the template carries a stale target.
    case author
    /// Genuine symlink authoring, or a target with an absent/unknown
    /// type: refuse with `cannotSynchronize` (-2005).
    case refuseSymlinkAuthoring
}

enum TemplatePolicy {
    /// The full template gate used at the top of `createItem`.
    static func disposition(
        contentType: UTType?, symlinkTarget: String?
    ) -> TemplateDisposition {
        guard symlinkTarget != nil else { return .author }
        // Older SDKs expose `public.symlink` by identifier only. An
        // unknown type or an absent content type fails closed to the
        // previous refusal, never opens symlink authoring by guess.
        guard let symlink = UTType("public.symlink"), let contentType else {
            return .refuseSymlinkAuthoring
        }
        return contentType.conforms(to: symlink)
            ? .refuseSymlinkAuthoring : .author
    }
}
