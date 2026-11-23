#!/bin/sh
# One source of truth for the macOS App Group id in shell build
# tooling. Sourced (never executed directly) by build-appex.sh,
# build-bundle.sh, domain-registrar/build-registrar.sh and
# install-dev.sh.
#
# The sandboxed File Provider extension cannot read an arbitrary
# runtime environment variable, so the build scripts bake the
# effective id into the entitlements and Info.plist before signing.
# Rust reads the same WYRD_APP_GROUP at runtime and Swift reads the
# rendered NSExtensionFileProviderDocumentGroup key; all three fall
# back to this one default when the variable is unset.

WYRD_APP_GROUP_DEFAULT='JU2RFHWQPH.group.md.enesis.wyrd'

# Print the effective group id. An empty or whitespace-only override
# means the default; anything outside the App Group charset is a
# misconfiguration and refuses rather than producing an unusable
# signed bundle.
wyrd_app_group() {
    group=${WYRD_APP_GROUP:-$WYRD_APP_GROUP_DEFAULT}
    case "$group" in
        *[!A-Za-z0-9._-]*)
            echo "error: WYRD_APP_GROUP has invalid characters: $group" >&2
            return 1
            ;;
    esac
    printf '%s' "$group"
}

# Render a template into a build-time copy: $1 source, $2 token (or
# checked-in literal) to replace, $3 destination. Checked-in files
# are never edited, so the working tree stays clean.
wyrd_render_app_group() {
    group=$(wyrd_app_group) || return 1
    sed "s|$2|$group|g" "$1" > "$3"
}
