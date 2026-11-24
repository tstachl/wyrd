#!/usr/bin/env bash
# tests/harness/test-dev-install.sh — pin the one-command developer
# install: the ad-hoc script builds through the wrapper, stages the
# co-installed CLI the desktop resolves first, verifies the
# signature, installs to an Applications dir, and documents the Finder
# boundary. Static assertions only (the real bundle build and the
# install itself are macOS-only manual evidence).
#
# Pure bash, no host, no nix: run it anywhere with
# `bash tests/harness/test-dev-install.sh`. (Wired into the CI
# shell suites alongside the other stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

INSTALL="$ROOT/packaging/macos/install-dev.sh"
STAGE_CLI="$ROOT/packaging/macos/stage-cli.sh"
CONF="$ROOT/crates/wyrd-desktop/tauri.conf.json"
# Binary resolution moved into the split adapter's process module.
PROVISION="$ROOT/crates/wyrd-desktop/src/provision/process.rs"

# Code lines only, captured once: strip full-line comments before
# matching, so the header prose can never satisfy a pin, and match
# with a here-string. Do not put a quiet grep (`-q`) in a pipeline:
# under `pipefail` it can exit before its producer finishes and
# report 141 even when the match was found.
INSTALL_CODE="$(grep -v '^[[:space:]]*#' "$INSTALL")"
STAGE_CODE="$(grep -v '^[[:space:]]*#' "$STAGE_CLI")"
CONF_CODE="$(cat "$CONF")"
PROVISION_CODE="$(cat "$PROVISION")"
match() { grep -F >/dev/null -- "$1" <<<"$2"; }

# 1. The scripts exist and are executable.
[[ -x "$INSTALL" ]] \
  || die "install-dev.sh missing or not executable"
[[ -x "$STAGE_CLI" ]] \
  || die "stage-cli.sh missing or not executable"
pass "install and stage scripts exist and are executable"

# 2. macOS-only: the .app bundle never ships elsewhere.
match 'uname -s' "$INSTALL_CODE" \
  || die "install: no platform gate"
match 'macOS only' "$INSTALL_CODE" \
  || die "install: no macOS-only refusal"
pass "install script is macOS-only"

# 2b. Frontend dependencies are ensured before the build: a fresh
# checkout without node_modules (or a drifted lockfile/manifest)
# installs from the frozen lockfile instead of failing inside vite.
# The install must precede the build delegation, and all three
# staleness markers must be present — a substring-only pin would
# pass an unconditional install or one placed after the build.
match 'node_modules' "$INSTALL_CODE" \
  || die "install: no node_modules precondition"
match 'bun.lock' "$INSTALL_CODE" \
  || die "install: lockfile is not a staleness marker"
match 'package.json' "$INSTALL_CODE" \
  || die "install: manifest is not a staleness marker"
match 'bun install --frozen-lockfile' "$INSTALL_CODE" \
  || die "install: no frozen frontend install"
INSTALL_LINE="$(grep -n -F -- 'bun install --frozen-lockfile' <<<"$INSTALL_CODE" | cut -d: -f1)"
BUILD_LINE="$(grep -n -F -- '"$HERE/build-bundle.sh"' <<<"$INSTALL_CODE" | cut -d: -f1)"
[ -n "$INSTALL_LINE" ] && [ -n "$BUILD_LINE" ] \
  || die "install: cannot locate install and build lines"
[ "$INSTALL_LINE" -lt "$BUILD_LINE" ] \
  || die "install: frontend install is not before the build"
# Refusals stay cheap: the install must sit after the argument and
# destination guards, so a refused invocation never reaches it.
REFUSE_LINE="$(grep -n -F -- 'must not sit inside the repository' <<<"$INSTALL_CODE" | cut -d: -f1)"
[ -n "$REFUSE_LINE" ] \
  || die "install: cannot locate the destination refusal"
[ "$REFUSE_LINE" -lt "$INSTALL_LINE" ] \
  || die "install: frontend install precedes the destination refusal"
pass "install script ensures frontend dependencies before the build"

# 3. The build delegates to the wrapper: the link-order pin and the
# no-Nix-links guard stay in that one place, never inlined here. The
# hook stages the appex plus the CLI before the outer sign.
match 'build-bundle.sh' "$INSTALL_CODE" \
  || die "install: build is not delegated to build-bundle.sh"
match 'cargo tauri build' "$INSTALL_CODE" \
  && die "install: inline cargo tauri build duplicates the wrapper"
match 'stage-cli.sh' "$CONF_CODE" \
  || die "config: CLI stage is not in beforeBundleCommand"
match 'Resources/wyrd' "$CONF_CODE" \
  || die "config: bundled CLI mapping missing"
match '--bundles app' "$INSTALL_CODE" \
  || die "install: app-only default missing"
pass "install builds through the wrapper with the CLI staged"

# 3b. The wrapper reseals the outer app shallowly after the build:
# the bundler's linker-only seal leaves the staged files
# unverified, and only a shallow re-sign (never `--deep`, which
# would re-sign the appex without its entitlements) makes the
# installed copy verify deep-strict; a real identity seals with
# `--timestamp` for notarization.
WRAPPER_CODE="$(grep -v '^[[:space:]]*#' "$ROOT/packaging/macos/build-bundle.sh")"
match 'codesign --force --sign' "$WRAPPER_CODE" \
  || die "wrapper: no post-bundle reseal"
match 'bundle/macos/Wyrd.app' "$WRAPPER_CODE" \
  || die "wrapper: reseal names no bundle path"
match '--entitlements "$APP_ENTITLEMENTS"' "$WRAPPER_CODE" \
  || die "wrapper: reseal drops the app App Group entitlement"
match 'APP_PROVISIONING_PROFILE' "$WRAPPER_CODE" \
  || die "wrapper: app provisioning profile is not embedded"
match '--timestamp' "$WRAPPER_CODE" \
  || die "wrapper: real-identity reseal carries no timestamp"
pass "wrapper reseals the outer app shallowly"

# 4. The desktop resolves the bundled CLI first: the fixed pair the
# acceptance case (create a drive, show it Available) depends on.
match 'bundled_cli_path' "$PROVISION_CODE" \
  || die "provision: bundled CLI resolution missing"
match 'Resources' "$PROVISION_CODE" \
  || die "provision: Resources lookup missing"
match 'Contents/Resources/$BUNDLED' "$INSTALL_CODE" \
  || die "install: bundled CLI presence is not verified"
pass "bundled CLI is staged, mapped, and resolved first"

# 5. Fail closed on signature: the built bundle verifies before the
# install touches the destination, and the staged copy verifies
# before it replaces the old app.
match 'codesign --verify --deep --strict' "$INSTALL_CODE" \
  || die "install: no codesign verification"
[ "$(grep -c -F 'codesign --verify --deep --strict' <<<"$INSTALL_CODE")" -ge 3 ] \
  || die "install: built, staged, and installed copies are not all verified"
pass "install verifies built, staged, and installed copies"

# 6. Installs to an Applications dir: system when writable, the
# user's Applications fallback as its own pattern, DEST/--dest
# override, no sudo. Replace rides a staging name: the old app is
# removed only after the new copy verifies.
match '/Applications/Wyrd.app' "$INSTALL_CODE" \
  || die "install: no /Applications destination"
match '"$HOME/Applications/Wyrd.app"' "$INSTALL_CODE" \
  || die "install: no user Applications fallback"
match '--dest' "$INSTALL_CODE" \
  || die "install: no --dest override"
match 'ditto' "$INSTALL_CODE" \
  || die "install: bundle copy is not ditto"
match '.new' "$INSTALL_CODE" \
  || die "install: no staging-name replace"
pass "install targets an Applications dir with staged replace"

# 6b. The release-dir rule mirrors the wrapper (CARGO_TARGET_DIR,
# triple from --target/CARGO_BUILD_TARGET); the two copies are
# pinned together so the installer inspects what the guard guarded.
for marker in 'CARGO_TARGET_DIR' 'CARGO_BUILD_TARGET' '--target=*)'; do
  match "$marker" "$INSTALL_CODE" \
    || die "install: release-dir rule '$marker' missing"
  match "$marker" "$WRAPPER_CODE" \
    || die "wrapper: release-dir rule '$marker' missing"
done
pass "installer mirrors the wrapper release-dir rule"

# 6c. The destination guard canonicalizes physically (`-P`, so
# symlinked parents resolve) before comparing, and the swap uses
# PID-suffixed staging names, restores only its own `.old`, and
# never deletes another run's work. The refusal block precedes
# the wrapper invocation, so a refused destination costs no
# compile (structural; read top-down).
for marker in 'pwd -P' '.new.$$' '.old.$$' '"$APP"|"$APP"/*'; do
  match "$marker" "$INSTALL_CODE" \
    || die "install: safe-destination marker '$marker' missing"
done
pass "installer canonicalizes and swaps safely"

# 7. Honest Finder boundary in the script output: ad-hoc stays
# inert (no-group-container), team-signed is the separate path.
match 'no-group-container' "$INSTALL_CODE" \
  || die "install: no-group-container boundary missing"
match 'team-signed' "$INSTALL_CODE" \
  || die "install: team-signed path missing"
pass "install states the ad-hoc Finder boundary"

# 7b. A Finder-capable install needs the full signing pair, and both
# native artifacts must be embedded by the bundler, not copied after
# the outer sign. The pairing refusal and post-build checks keep a
# team-signed outer app from shipping an inert appex.
for marker in '--sign-identity' '--provisioning-profile' '--app-provisioning-profile' 'APP_PROVISIONING_PROFILE' 'FINDER_CAPABLE' 'embedded.provisionprofile' 'WyrdDomainRegistrar' 'WyrdFileProvider.appex'; do
  match "$marker" "$INSTALL_CODE" \
    || die "install: Finder-capable marker '$marker' missing"
done
for marker in 'stage-registrar.sh' 'PlugIns/WyrdFileProvider.appex' 'MacOS/WyrdDomainRegistrar'; do
  match "$marker" "$CONF_CODE" \
    || die "config: native artifact mapping '$marker' missing"
done
REGISTRAR_CODE="$(grep -v '^[[:space:]]*#' "$ROOT/packaging/macos/domain-registrar/build-registrar.sh")"
match '--identifier md.enesis.wyrd' "$REGISTRAR_CODE" \
  || die "registrar: helper does not carry the app's signing identifier"
pass "install pairs signing and embeds both native artifacts"

# 7c. Domain reconciliation is a per-user launch agent, not a thread
# inside a running Wyrd.app. One resident helper loops internally every
# 5 seconds: a fresh process per tick makes macOS re-ask the
# "access data from other apps" permission on every launch. KeepAlive
# restarts that one process if it dies; RunAtLoad starts it at login.
# The installer fills the helper path from the installed bundle and
# loads the agent, unloading any previous copy first.
AGENT_TEMPLATE="$ROOT/packaging/macos/launchd/md.enesis.wyrd.domain-registrar.plist"
[[ -f "$AGENT_TEMPLATE" ]] \
  || die "registrar agent: launchd template missing"
# XML comments are prose, not keys: strip them before pinning keys.
AGENT_CODE="$(sed '/<!--/,/-->/d' "$AGENT_TEMPLATE")"
for marker in 'md.enesis.wyrd.domain-registrar' '@WYRD_REGISTRAR_PATH@' '<key>RunAtLoad</key>' '<key>KeepAlive</key>' '<string>--watch</string>'; do
  match "$marker" "$AGENT_CODE" \
    || die "registrar agent: template marker '$marker' missing"
done
if match 'StartInterval' "$AGENT_CODE"; then
  die "registrar agent: StartInterval relaunches the helper and re-prompts"
fi
REGISTRAR_SOURCE="$(cat "$ROOT/packaging/macos/domain-registrar/main.swift")"
match 'while true' "$REGISTRAR_SOURCE" \
  || die "registrar: --watch has no internal reconciliation loop"
# A transient failed socket probe must not drop a Finder domain: the
# resident helper keeps one in-memory absence streak per domain and
# removes only after two consecutive missed ticks. One-shot has no
# history and still removes on the first miss.
for marker in 'removalGraceTicks = 2' 'DomainRemovalGrace' 'observePresent' 'shouldRemove' 'graceEnabled' 'deferring removal of domain'; do
  match "$marker" "$REGISTRAR_SOURCE" \
    || die "registrar: removal grace marker '$marker' missing"
done
for marker in 'REGISTRAR_AGENT_TEMPLATE' 'LaunchAgents' 'launchctl unload -w' 'launchctl load -w'; do
  match "$marker" "$INSTALL_CODE" \
    || die "install: registrar agent marker '$marker' missing"
done
if match 'domains::spawn' "$(cat "$ROOT/crates/wyrd-desktop/src/main.rs")"; then
  die "desktop: registrar still runs inside the app process"
fi
pass "install runs the registrar from a launch agent, not the app"

# 7d. One env-configurable App Group: the installer sources the shared
# helper, validates the signed artifacts against the effective id, and
# renders that id into the launch agent template. Nothing writes a
# checked-in file.
for marker in '. "$HERE/app-group.sh"' 'GROUP=$(wyrd_app_group)' '"$GROUP"*)' '@WYRD_APP_GROUP@' 's|@WYRD_APP_GROUP@|$GROUP|g'; do
  match "$marker" "$INSTALL_CODE" \
    || die "install: App Group marker '$marker' missing"
done
grep -F >/dev/null 'wyrd_render_app_group' "$ROOT/packaging/macos/app-group.sh" \
  || die "helper: no render function"
pass "install renders one WYRD_APP_GROUP into every native artifact"

# 7e. Clean slate: every running Wyrd process is stopped around the
# install so none keeps an old binary mapped. The stop runs after the
# swap and registrar load; supervise precedes owner, and escalation is
# bounded to the PIDs that ignored SIGTERM. --no-stop is the opt-out.
for marker in '--no-stop' 'STOP_RUNNING=1' 'stop_running_wyrd()' 'pgrep -f' "'/wyrd-supervise '" "'/wyrd owner '" 'kill -TERM' 'kill -KILL'; do
  match "$marker" "$INSTALL_CODE" \
    || die "install: clean-slate marker '$marker' missing"
done
STOP_LINE="$(grep -n -F -- 'stop_running_wyrd' <<<"$INSTALL_CODE" | tail -1 | cut -d: -f1)"
LOAD_LINE="$(grep -n -F -- 'launchctl load -w "$REGISTRAR_AGENT"' <<<"$INSTALL_CODE" | cut -d: -f1)"
[ -n "$STOP_LINE" ] && [ -n "$LOAD_LINE" ] && [ "$LOAD_LINE" -lt "$STOP_LINE" ] \
  || die "install: clean-slate stop must follow the registrar reload"
pass "install stops running Wyrd processes after the swap"

# 7f. The per-drive agents keep alive only on exit 0, so a SIGTERM'd
# supervisor is deliberately not restarted by launchd. The installer
# must kickstart each loaded drive job after stopping it; jobs that are
# not loaded (login-start disabled) are left alone.
for marker in 'launchctl list' "grep '^md\\.enesis\\.wyrd\\.drive-'" 'launchctl kickstart -k' 'gui/$(id -u)/$label' 'RESTART_FAILURES' 'recover: launchctl kickstart -k' 'return 1'; do
  match "$marker" "$INSTALL_CODE" \
    || die "install: drive restart marker '$marker' missing"
done
KILL_LINE="$(grep -n -F -- 'kill -KILL' <<<"$INSTALL_CODE" | tail -1 | cut -d: -f1)"
KICK_LINE="$(grep -n -F -- 'launchctl kickstart -k' <<<"$INSTALL_CODE" | head -1 | cut -d: -f1)"
[ -n "$KILL_LINE" ] && [ -n "$KICK_LINE" ] && [ "$KILL_LINE" -lt "$KICK_LINE" ] \
  || die "install: drive agents must be restarted after the stops"
pass "install restarts loaded drive agents after the clean-slate stop"

# 7g. Narrow stable-identity mode: sign the app, bundled CLI and domain
# registrar with one stable identity so the keychain client survives a
# rebuild, while the appex stays ad-hoc and Finder stays inert. No
# profile is involved, and the mode refuses a profile or a conflicting
# --sign-identity before any build. The post-build checks fail closed if
# a real signature silently falls back to ad-hoc, or if the appex is
# signed/embedded in a way Finder might mistake for capability.
for marker in '--stable-identity' 'STABLE_SIGN_IDENTITY' '. "$HERE/sign-identity.sh"' 'STABLE_MODE=1' 'WYRD_ADHOC_IDENTITY' 'APPEX_SIGN_IDENTITY="$WYRD_ADHOC_IDENTITY"' 'WYRD_ALLOW_ADHOC_APPEX=1' 'FINDER_CAPABLE=0' '--stable-identity takes no provisioning profile' 'assert_stable_signed' 'Signature=adhoc' 'TeamIdentifier=not set' 'Authority=$STABLE_SIGN_IDENTITY' 'different identity than $STABLE_SIGN_IDENTITY' 'appex must stay ad-hoc' '--stable-identity needs the unprovisioned' 'TEAM_PREFIX=${GROUP%%.*}' 'SIGNING_TEAM=' 'stable identity for app, CLI and registrar'; do
  match "$marker" "$INSTALL_CODE" \
    || die "install: stable-identity marker '$marker' missing"
done
# The stable branch must not demand a profile: the refusal belongs to
# --sign-identity only, and the appex identity must be pinned to ad-hoc
# rather than inherited from the stable SIGN_IDENTITY.
STABLE_LINE="$(grep -n -F -- 'if [ -n "$STABLE_SIGN_IDENTITY" ]; then' <<<"$INSTALL_CODE" | cut -d: -f1)"
PROFILE_LINE="$(grep -n -F -- '--sign-identity needs --provisioning-profile' <<<"$INSTALL_CODE" | cut -d: -f1)"
[ -n "$STABLE_LINE" ] && [ -n "$PROFILE_LINE" ] && [ "$STABLE_LINE" -lt "$PROFILE_LINE" ] \
  || die "install: stable branch must precede the --sign-identity profile demand"
# Both flag forms refuse an empty value: a silent fall-through to ad-hoc
# would rebuild the very client identity this mode is meant to stabilise.
[ "$(grep -c -F -- '--stable-identity needs a value.' <<<"$INSTALL_CODE")" -ge 2 ] \
  || die "install: an empty --stable-identity= would silently select ad-hoc"
pass "install offers a profile-free stable-identity dev mode"

# The identity helper owns both defaults and the appex split. Execute it,
# not just grep for it: an env-typed typo here would silently break the
# stable mode without any script failing to parse.
HELPER="$ROOT/packaging/macos/sign-identity.sh"
for marker in "WYRD_ADHOC_IDENTITY='-'" 'wyrd_appex_identity()' 'wyrd_is_real_identity()' 'wyrd_finder_capability()'; do
  grep -F >/dev/null "$marker" "$HELPER" \
    || die "sign-identity.sh: marker '$marker' missing"
done
(
  . "$HELPER"
  unset SIGN_IDENTITY APPEX_SIGN_IDENTITY
  [ "$(wyrd_appex_identity)" = '-' ] || exit 1
  SIGN_IDENTITY='Apple Development: stable'
  [ "$(wyrd_appex_identity)" = 'Apple Development: stable' ] || exit 1
  APPEX_SIGN_IDENTITY='-'
  [ "$(wyrd_appex_identity)" = '-' ] || exit 1
  wyrd_is_real_identity 'Apple Development: stable' || exit 1
  ! wyrd_is_real_identity '-' || exit 1
  ! wyrd_is_real_identity '' || exit 1
  # The capability fact is inferred from the signing inputs so a
  # direct wrapper build cannot inherit a stale marker.
  unset WYRD_FINDER_CAPABILITY APP_PROVISIONING_PROFILE SIGN_IDENTITY APPEX_SIGN_IDENTITY
  [ "$(wyrd_finder_capability)" = 'adhoc' ] || exit 1
  APPEX_SIGN_IDENTITY='Apple Development: stable'
  [ "$(wyrd_finder_capability)" = 'identity' ] || exit 1
  # Without an explicit appex identity, build-appex.sh signs with the
  # shared SIGN_IDENTITY; the capability must follow that same fallback.
  unset APPEX_SIGN_IDENTITY
  SIGN_IDENTITY='Apple Development: stable'
  [ "$(wyrd_finder_capability)" = 'identity' ] || exit 1
  APP_PROVISIONING_PROFILE='/tmp/app.provisionprofile'
  [ "$(wyrd_finder_capability)" = 'profile' ] || exit 1
  WYRD_FINDER_CAPABILITY=adhoc
  [ "$(wyrd_finder_capability)" = 'adhoc' ] || exit 1
  WYRD_FINDER_CAPABILITY=bogus
  if wyrd_finder_capability >/dev/null 2>&1; then exit 1; fi
) || die "sign-identity.sh: appex fallback, identity predicate or capability inference drifted"
pass "signing helper resolves the appex identity and real-identity test"

# 7h. Profile-free identity-signed appex for the local diagnostics
# capture. It is an additive flag on stable-identity mode: the appex
# takes that identity, still with no profile, because the team-prefixed
# group is the unprovisioned macOS form. The refusal for a flag without
# a stable identity must run before any mode is selected.
for marker in '--sign-appex' 'WYRD_SIGN_APPEX' 'APPEX_SIGNED=0' 'APPEX_SIGNED="$WYRD_SIGN_APPEX"' 'APPEX_SIGN_IDENTITY="$SIGN_IDENTITY"' '--sign-appex needs --stable-identity' 'assert_stable_signed "appex" "$APPEX"' 'unless --sign-appex is given' 'stable identity for app, CLI, registrar and appex' 'unprovisioned' 'WYRD_FINDER_CAPABILITY=identity' 'export WYRD_FINDER_CAPABILITY'; do
  match "$marker" "$INSTALL_CODE" \
    || die "install: sign-appex marker '$marker' missing"
done
SIGN_APPEX_REFUSAL_LINE="$(grep -n -F -- '--sign-appex needs --stable-identity' <<<"$INSTALL_CODE" | cut -d: -f1)"
STABLE_BRANCH_LINE="$(grep -n -F -- 'if [ -n "$STABLE_SIGN_IDENTITY" ]; then' <<<"$INSTALL_CODE" | tail -1 | cut -d: -f1)"
[ -n "$SIGN_APPEX_REFUSAL_LINE" ] && [ -n "$STABLE_BRANCH_LINE" ] \
  && [ "$SIGN_APPEX_REFUSAL_LINE" -lt "$STABLE_BRANCH_LINE" ] \
  || die "install: --sign-appex without a stable identity must refuse first"
pass "install signs the appex with the stable identity on request"

# 8. Prerequisites fail with the fix named: Apple tools, cargo
# (devenv), bun (desktop build), python3 (config version pin).
for marker in 'xcode-select' 'cargo is not on PATH' 'bun is not on PATH' 'python3 is required'; do
  match "$marker" "$INSTALL_CODE" \
    || die "install: prerequisite '$marker' missing"
done
pass "install names its prerequisites with fixes"

# 9. The stage script builds the release CLI with the SDK pin,
# signs it with the paired identity, and refuses cross-target
# embeds — mirroring the appex stage, never the outer bundle. The
# CLI signs WITHOUT hardened runtime on purpose: a mount-enabled
# build loads the third-party macFUSE dylib, which a
# runtime-constrained signature rejects in dyld (the appex keeps
# `--options runtime`). The ad-hoc bundle sets WYRD_NO_MOUNT=1 so
# the shipped CLI drops the mount stack entirely (the manifest's
# `--no-default-features` no-FUSE mode): a clean Mac has no macFUSE
# to load, while the release path keeps mount by leaving it unset.
for marker in 'cargo build --release -p wyrd-cli' 'libiconv.tbd' 'codesign' 'TAURI_ENV_TARGET_TRIPLE' 'SDKROOT="$DISCOVERED_SDK"'; do
  match "$marker" "$STAGE_CODE" \
    || die "stage-cli: '$marker' missing"
done
match '--options runtime' "$STAGE_CODE" \
  && die "stage-cli: hardened runtime would reject macFUSE"
match 'WYRD_NO_MOUNT' "$STAGE_CODE" \
  || die "stage-cli: ad-hoc mount drop missing"
match 'WYRD_NO_MOUNT' "$INSTALL_CODE" \
  || die "install: ad-hoc mount drop missing"
match '--no-default-features' "$STAGE_CODE" \
  || die "stage-cli: no-FUSE build flag missing"
# N15: the B2 repair is mechanical — with WYRD_NO_MOUNT=1 the
# stage refuses any FUSE mention in the captured linkage — and the
# timestamp is conditional: a real identity seals with it, ad-hoc
# omits it.
match '[Ff][Uu][Ss][Ee]' "$STAGE_CODE" \
  || die "stage-cli: no fuse-free guard for the vault-only CLI"
# One ad-hoc sentinel lives in the shared helper: the wrapper and the
# CLI stage source it and use the predicate/constant instead of spelling
# `-` again. A literal here would let the two defaults drift apart.
match '. "$HERE/sign-identity.sh"' "$WRAPPER_CODE" \
  || die "wrapper: does not source the shared signing helper"
match 'wyrd_is_real_identity "${SIGN_IDENTITY:-}"' "$WRAPPER_CODE" \
  || die "wrapper: reseal does not use the real-identity predicate"
if match '"$SIGN_IDENTITY" != "-"' "$WRAPPER_CODE" \
    || match '${SIGN_IDENTITY:--}' "$WRAPPER_CODE"; then
  die "wrapper: ad-hoc sentinel is duplicated outside the helper"
fi
if match '${SIGN_IDENTITY:--}' "$STAGE_CODE"; then
  die "stage-cli: ad-hoc sentinel is duplicated outside the helper"
fi
match '. "$HERE/sign-identity.sh"' "$STAGE_CODE" \
  || die "stage-cli: does not source the shared signing helper"
match 'SIGN_IDENTITY=${SIGN_IDENTITY:-$WYRD_ADHOC_IDENTITY}' "$STAGE_CODE" \
  || die "stage-cli: ad-hoc fallback is not the helper constant"
match 'SDKROOT="$DISCOVERED_SDK"' "$WRAPPER_CODE" \
  || die "wrapper: discovered SDK is not exported as SDKROOT"
match 'wyrd_is_real_identity "${SIGN_IDENTITY:-}"' "$WRAPPER_CODE" \
  || die "wrapper: timestamp is not conditioned on a real identity"
match '--timestamp' "$WRAPPER_CODE" \
  || die "wrapper: real-identity reseal carries no timestamp"
pass "stage script builds and signs the release CLI"

# 9b. Linkage is covered on both sides of the stage boundary:
# the guard owns the release-dir-relative bundled copy, and the
# stage script checks its own staged copy before signing (the
# staged path is $ROOT-relative, so the guard must never name it
# or an empty release tree would pass).
CHECK_CODE="$(grep -v '^[[:space:]]*#' "$ROOT/packaging/macos/check-no-nix-links.sh")"
match 'Resources/wyrd' "$CHECK_CODE" \
  || die "check: bundled CLI leg missing"
match 'stage/wyrd' "$CHECK_CODE" \
  && die "check: staged path would defeat fail-closed"
match 'otool -L' "$STAGE_CODE" \
  || die "stage-cli: no staged-CLI linkage check"
match 'LINKED=$(otool' "$STAGE_CODE" \
  || die "stage-cli: linkage is not captured without a pipe"
match '/nix/store' "$STAGE_CODE" \
  || die "stage-cli: linkage check names no Nix store"
pass "linkage is covered on both sides of the stage boundary"

# 10. The decision is recorded in hook-adjacent prose: the packaging
# README names the command, the bundled CLI, the boundary, and the
# team-signed pointer together.
for marker in 'install-dev.sh' 'Resources/wyrd' 'no-group-container' 'team-signed' 'SDKROOT' '--sign-appex' 'unprovisioned'; do
  grep -F -- "$marker" "$ROOT/packaging/macos/README.md" >/dev/null \
    || die "README: '$marker' not recorded"
done
pass "README records the command, the CLI, and the Finder boundary"

# 11. The suite itself is wired into the native CI shell job.
grep -F >/dev/null 'test-dev-install.sh' "$ROOT/.ngit/nix/workflows/default.nix" \
  || die "CI: dev-install suite not wired into shell.suites"
pass "CI wires the dev-install suite"

# 12. The supervised-owner wrapper ships as the CLI's sibling: the
# daemon resolves `wyrd-supervise` beside the running `wyrd`, so a
# bundle without it fails every managed operation at validate.
# The stage script owns the staged copy (shebang + script source,
# executable bit, content check instead of otool — a script has no
# Mach-O), the bundler maps it, and the installer refuses a bundle
# missing it or carrying it non-executable.
for marker in 'wyrd-supervise' 'nix/drive/supervise.py' 'chmod +x'; do
  match "$marker" "$STAGE_CODE" \
    || die "stage-cli: supervise marker '$marker' missing"
done
match "Resources/wyrd-supervise" "$CONF_CODE" \
  || die "config: bundled supervise mapping missing"
match "Resources/wyrd-finder-capability.txt" "$CONF_CODE" \
  || die "config: Finder capability marker is not bundled"
# The wrapper writes the marker on every build path; an absent value
# means ad-hoc, so a stale marker from another install mode cannot leak.
match 'finder-capability.txt' "$WRAPPER_CODE" \
  || die "wrapper: Finder capability marker is not staged"
match 'CAPABILITY=$(wyrd_finder_capability)' "$WRAPPER_CODE" \
  || die "wrapper: Finder capability does not use the shared inference"
for marker in 'for BUNDLED in wyrd wyrd-supervise' 'Contents/Resources/$BUNDLED' 'is not executable'; do
  match "$marker" "$INSTALL_CODE" \
    || die "install: supervise guard '$marker' missing"
done
grep -F >/dev/null 'wyrd-supervise' "$ROOT/packaging/macos/README.md" \
  || die "README: wyrd-supervise not recorded"
pass "supervise wrapper is staged, mapped, guarded, and recorded"

# 13. Finder binds a domain to the appex bundle id, not to a path. A
# stale plugin with the same id can serve the domains with a different
# group id, so socket paths disagree for a reason unrelated to path
# derivation. The installer must report it and the README must say so.
for marker in '/usr/bin/pluginkit -mAv -i md.enesis.wyrd.fileprovider' 'SHADOWED=' '/usr/bin/pluginkit -r <path>'; do
  match "$marker" "$INSTALL_CODE" \
    || die "install: plugin shadowing guard '$marker' missing"
done
for marker in 'pluginkit -mAv' 'most recently' 'derivation defect'; do
  grep -F -- "$marker" "$ROOT/packaging/macos/README.md" >/dev/null \
    || die "README: plugin shadowing note '$marker' missing"
done
# The parser is a pure shared function: feed it real-shaped fixtures with
# both tab and space separators, a stale plugin, the installed plugin,
# and pluginkit's trailing summary line. Format drift must not hide a
# stale path or invent one.
match 'wyrd_shadowed_plugins()' "$(cat "$ROOT/packaging/macos/sign-identity.sh")" \
  || die "sign-identity.sh: plugin path parser missing"
match 'wyrd_shadowed_plugins "$INSTALLED_APPEX"' "$INSTALL_CODE" \
  || die "install: shadowing guard does not use the shared parser"
. "$ROOT/packaging/macos/sign-identity.sh"
INSTALLED_APPEX='/Applications/Wyrd.app/Contents/PlugIns/WyrdFileProvider.appex'
STALE_APPEX='/Users/thomas/Library/Developer/Xcode/DerivedData/wyrd/Build/Products/Debug/wyrd.app/Contents/PlugIns/fileprovider.appex'
for SEP in "$(printf '\t')" ' '; do
  FIXTURE=$(printf '!    md.enesis.wyrd.fileprovider(0.4.0)%sUUID%s2026-11-24%s%s\n!    md.enesis.wyrd.fileprovider(1.0)%sUUID2%s2026-11-22%s%s\n (1 plug-in)\n' \
    "$SEP" "$SEP" "$SEP" "$INSTALLED_APPEX" \
    "$SEP" "$SEP" "$SEP" "$STALE_APPEX")
  RESULT=$(printf '%s' "$FIXTURE" | wyrd_shadowed_plugins "$INSTALLED_APPEX")
  match "$STALE_APPEX" "$RESULT" \
    || die "parser: stale plugin path missed with separator '$SEP'"
  match "$INSTALLED_APPEX" "$RESULT" \
    && die "parser: installed plugin reported as shadowing"
  match 'plug-in' "$RESULT" \
    && die "parser: pluginkit summary line reported as a plugin"
done
pass "stale plugin shadowing is detected and documented"

# 14. The appex passes the configured group id to containerURL and the
# owner joins the same accessor into Group Containers. Neither strips a
# team prefix, so both name the same directory.
SWIFT_SOCKET="$(cat "$ROOT/packaging/macos/fileprovider/Sources/SocketConfig.swift")"
RUST_SOCKET="$(cat "$ROOT/crates/wyrd-daemon/src/owner/server/socket.rs")"
for marker in 'forSecurityApplicationGroupIdentifier: Self.groupIdentifier' 'Sockets'; do
  match "$marker" "$SWIFT_SOCKET" \
    || die "SocketConfig.swift: container derivation marker '$marker' missing"
done
for marker in '.join(app_group_identifier())' 'Library/Group Containers'; do
  match "$marker" "$RUST_SOCKET" \
    || die "socket.rs: container derivation marker '$marker' missing"
done
pass "appex and owner derive the container from one group id"

# 15. SecretSpec (devenv keyring) signs the dev install without putting
# values in the repo or on the command line. The manifest declares four
# stable names, devenv exports them, and the installer maps them onto the
# script-local signing variables that every build hook reads.
SPEC_MANIFEST="$(cat "$ROOT/secretspec.toml")"
DEVENV_YAML="$(cat "$ROOT/devenv.yaml")"
DEVENV_NIX="$(cat "$ROOT/devenv.nix")"
for marker in APPLE_IDENTITY APPLE_SIGN_APEX APPLE_PROVISIONING_PROFILE APPLE_APP_PROVISIONING_PROFILE; do
  match "$marker" "$SPEC_MANIFEST" \
    || die "secretspec.toml: '$marker' is not declared"
  match "env.$marker" "$DEVENV_NIX" \
    || die "devenv.nix: '$marker' is not exported"
  match "$marker" "$INSTALL_CODE" \
    || die "install-dev.sh: '$marker' is not consumed"
done
for marker in 'enable: true' 'provider: keyring' 'profile: default'; do
  match "$marker" "$DEVENV_YAML" \
    || die "devenv.yaml: SecretSpec setting '$marker' missing"
done
match 'config.secretspec.secrets.APPLE_IDENTITY' "$DEVENV_NIX" \
  || die "devenv.nix: secrets are not read from the SecretSpec integration"
# Explicit flags and script-local variables still win over SecretSpec:
# the mapping is a default, never an override.
match 'APPLE_SIGNING_IDENTITY:-$APPLE_IDENTITY' "$INSTALL_CODE" \
  || die "install: profile-backed identity mapping missing"
match 'STABLE_SIGN_IDENTITY:-$APPLE_IDENTITY' "$INSTALL_CODE" \
  || die "install: profile-free identity mapping missing"
match 'PROVISIONING_PROFILE:-${APPLE_PROVISIONING_PROFILE:-}}' "$INSTALL_CODE" \
  || die "install: appex profile mapping missing"
match 'APP_PROVISIONING_PROFILE:-${APPLE_APP_PROVISIONING_PROFILE:-}}' "$INSTALL_CODE" \
  || die "install: app profile mapping missing"
# Static strings cannot catch an unbound variable or a wrong precedence,
# so execute the real mapping and flag-parse block with controlled inputs.
# It runs under `sh -eu`, matching the installer's own shell settings.
MAPPING=$(sed -n '/^SIGN_IDENTITY=/,/^# Signing posture:/p' "$INSTALL" | sed '$d')
[ -n "$MAPPING" ] || die "install: cannot extract the signing mapping block"
MAPFILE=$(mktemp)
printf '%s\n' "$MAPPING" \
  'printf "%s|%s|%s|%s\n" "$STABLE_SIGN_IDENTITY" "$WYRD_SIGN_APPEX" "$SIGN_IDENTITY" "$PROVISIONING_PROFILE"' \
  > "$MAPFILE"
UNSET_SIGNING='-u SIGN_IDENTITY -u APPLE_SIGNING_IDENTITY -u STABLE_SIGN_IDENTITY -u WYRD_SIGN_APPEX -u PROVISIONING_PROFILE -u APP_PROVISIONING_PROFILE'
R=$(env $UNSET_SIGNING APPLE_IDENTITY='Profile Identity' APPLE_SIGN_APEX=1 \
  /bin/sh -eu "$MAPFILE") || { rm -f "$MAPFILE"; die "mapping: profile-free path failed"; }
[ "$R" = 'Profile Identity|1||' ] \
  || { rm -f "$MAPFILE"; die "mapping: profile-free result '$R'"; }
R=$(env $UNSET_SIGNING APPLE_IDENTITY='Profile Identity' APPLE_SIGN_APEX=1 \
  APPLE_PROVISIONING_PROFILE=/tmp/a.provisionprofile \
  APPLE_APP_PROVISIONING_PROFILE=/tmp/b.provisionprofile \
  /bin/sh -eu "$MAPFILE") || { rm -f "$MAPFILE"; die "mapping: profile-backed path failed"; }
[ "$R" = '|0|Profile Identity|/tmp/a.provisionprofile' ] \
  || { rm -f "$MAPFILE"; die "mapping: profile-backed result '$R'"; }
R=$(env $UNSET_SIGNING APPLE_IDENTITY='Profile Identity' APPLE_SIGN_APEX=1 \
  APPLE_PROVISIONING_PROFILE=/tmp/a.provisionprofile \
  APPLE_APP_PROVISIONING_PROFILE=/tmp/b.provisionprofile \
  /bin/sh -eu "$MAPFILE" --stable-identity 'Explicit Identity') \
  || { rm -f "$MAPFILE"; die "mapping: explicit flag path failed"; }
[ "$R" = 'Explicit Identity|1||' ] \
  || { rm -f "$MAPFILE"; die "mapping: explicit flag result '$R'"; }
rm -f "$MAPFILE"
match 'require_reason = false' "$SPEC_MANIFEST" \
  || die "secretspec.toml: reason policy would break non-interactive devenv shell"
for marker in 'APPLE_IDENTITY' 'APPLE_SIGN_APEX' 'APPLE_PROVISIONING_PROFILE' 'APPLE_APP_PROVISIONING_PROFILE'; do
  grep -F -- "$marker" "$ROOT/packaging/macos/README.md" >/dev/null \
    || die "README: SecretSpec variable '$marker' not documented"
done
pass "SecretSpec signing inputs are declared, exported and consumed"

echo "dev-install: $PASS checks passed"
