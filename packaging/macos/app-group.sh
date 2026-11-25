#!/bin/sh
# One source of truth for the macOS App Group id in shell build
# tooling. Sourced (never executed directly) by build-appex.sh,
# build-bundle.sh, domain-registrar/build-registrar.sh and
# install-dev.sh.
#
# The sandboxed File Provider extension cannot read an arbitrary
# runtime environment variable, so the build scripts bake the
# effective id into the entitlements and Info.plist before signing.
# Rust reads the same WYRD_APP_GROUP at runtime (baked at build time
# by crates/wyrd-settings/build.rs) and Swift reads the rendered
# NSExtensionFileProviderDocumentGroup key; all three fall back to
# this one default when no override is set.
#
# Two knobs, in precedence order:
#   WYRD_APP_GROUP        full id, explicit override (wins)
#   APPLE_TEAM_ID    a developer's 10-character Apple Team ID;
#                         derives <team>.group.md.enesis.wyrd
# With neither set, the built-in team-prefixed default stands. A
# third party only has to set their team id; the group suffix is an
# app invariant shared with the Rust and Swift helpers.

WYRD_APP_GROUP_DEFAULT='JU2RFHWQPH.group.md.enesis.wyrd'
WYRD_APP_GROUP_SUFFIX='group.md.enesis.wyrd'

# Derive the group id from APPLE_TEAM_ID when WYRD_APP_GROUP is
# absent. The team id is the ten-character uppercase alphanumeric
# Apple identifier; anything else is a typo, not a group id, and
# refuses rather than producing a container no process can agree on.
wyrd_group_from_team() {
    team=${APPLE_TEAM_ID:-}
    team=$(printf '%s' "$team" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
    [ -n "$team" ] || {
        printf '%s' "$WYRD_APP_GROUP_DEFAULT"
        return 0
    }
    case "$team" in
        *[!A-Z0-9]*)
            echo "error: APPLE_TEAM_ID has invalid characters: $team" >&2
            return 1
            ;;
    esac
    if [ ${#team} -ne 10 ]; then
        echo "error: APPLE_TEAM_ID must be 10 characters, got ${#team}: $team" >&2
        return 1
    fi
    printf '%s.%s' "$team" "$WYRD_APP_GROUP_SUFFIX"
}

# Print the effective group id. WYRD_APP_GROUP wins; otherwise a
# valid APPLE_TEAM_ID derives the id; otherwise the default.
# Whitespace-only values are the same as unset, matching the Rust
# and Swift accessors' empty-override rules.
wyrd_app_group() {
    group=${WYRD_APP_GROUP:-}
    group=$(printf '%s' "$group" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
    if [ -n "$group" ]; then
        case "$group" in
            *[!A-Za-z0-9._-]*)
                echo "error: WYRD_APP_GROUP has invalid characters: $group" >&2
                return 1
                ;;
        esac
        printf '%s' "$group"
        return 0
    fi
    wyrd_group_from_team
}

# Render a template into a build-time copy: $1 source, $2 token (or
# checked-in literal) to replace, $3 destination. Checked-in files
# are never edited, so the working tree stays clean.
wyrd_render_app_group() {
    group=$(wyrd_app_group) || return 1
    sed "s|$2|$group|g" "$1" > "$3"
}
