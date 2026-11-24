#!/bin/sh
# One source of truth for macOS code-signing identities in shell build
# tooling. Sourced (never executed directly) by install-dev.sh,
# stage-appex.sh, fileprovider/build-appex.sh, build-bundle.sh and
# stage-cli.sh.
#
# Normally the outer app and every staged artifact share one identity:
# SIGN_IDENTITY, defaulting to ad-hoc `-`. A narrow developer mode
# deliberately splits them: the app, bundled CLI, supervise wrapper and
# domain registrar take a stable identity so macOS keeps recognising the
# same keychain client across rebuilds, while the File Provider appex
# stays ad-hoc and Finder stays inert. That mode needs no provisioning
# profile. The all-or-nothing `--sign-identity` path is the separate,
# Finder-capable one and is unchanged.
#
# The keychain item ACL is untouched here: items keep default (NULL)
# access, so the non-interactive owner can still read them. Stability
# comes from the signer's identity, not from a per-app trust list.

WYRD_ADHOC_IDENTITY='-'

# Effective appex identity. An explicit APPEX_SIGN_IDENTITY wins (the
# stable-identity mode pins it to ad-hoc); otherwise the shared
# SIGN_IDENTITY applies; unset means ad-hoc. Centralised so no script
# hardcodes a different fallback and drifts from the others.
wyrd_appex_identity() {
    if [ -n "${APPEX_SIGN_IDENTITY:-}" ]; then
        printf '%s' "$APPEX_SIGN_IDENTITY"
    elif [ -n "${SIGN_IDENTITY:-}" ]; then
        printf '%s' "$SIGN_IDENTITY"
    else
        printf '%s' "$WYRD_ADHOC_IDENTITY"
    fi
}

# True (status 0) when $1 is a real signing identity — neither empty nor
# the ad-hoc pseudo-identity. Used where a real signature implies a
# secure timestamp or where a mixed inner/outer pair must be allowed.
wyrd_is_real_identity() {
    [ -n "${1:-}" ] && [ "${1:-}" != "$WYRD_ADHOC_IDENTITY" ]
}

# Paths of registered plugins that are not the installed appex. Pure so
# the harness can feed it a fixture and exercise the real parser: match
# any whitespace-delimited field that is an absolute `.appex` path, so
# `pluginkit` tab and space separators and its trailing summary line do not
# change the result. $1 is the installed appex path.
wyrd_shadowed_plugins() {
    awk -v installed="$1" '
        {
            for (i = 1; i <= NF; i++) {
                if ($i ~ /^\// && $i ~ /\.appex$/ && index($i, installed) != 1)
                    print $i
            }
        }'
}

# Local Finder capability fact staged in the bundle. An explicit
# WYRD_FINDER_CAPABILITY wins; otherwise infer from the signing inputs so
# a direct build-bundle.sh run matches the signature it actually made.
# Invalid explicit values fail closed rather than guessing a capability.
wyrd_finder_capability() {
    if [ -n "${WYRD_FINDER_CAPABILITY:-}" ]; then
        case "$WYRD_FINDER_CAPABILITY" in
            adhoc|identity|profile)
                printf '%s' "$WYRD_FINDER_CAPABILITY"
                return 0
                ;;
            *)
                return 1
                ;;
        esac
    fi
    if [ -n "${APP_PROVISIONING_PROFILE:-}" ] \
        && wyrd_is_real_identity "${SIGN_IDENTITY:-}"; then
        printf 'profile'
    elif wyrd_is_real_identity "$(wyrd_appex_identity)"; then
        # build-appex.sh signs with wyrd_appex_identity(), which falls
        # back to SIGN_IDENTITY. Match that resolution exactly.
        printf 'identity'
    else
        printf 'adhoc'
    fi
}
