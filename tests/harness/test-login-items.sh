#!/usr/bin/env bash
# tests/harness/test-login-items.sh — pin the UI login launchers: the macOS
# launchd agent template and the Linux systemd user-unit template the
# installer fills, plus the decided no-manager-ordering contract they share.
# Static assertions only (enablement under a real launchd/systemd login
# session is manual rehearsal evidence, recorded on the PR).
#
# Pure bash, no host, no nix: run it anywhere with
# `bash tests/harness/test-login-items.sh`. (Wired into the CI shell suites
# alongside the other stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

PLIST="$ROOT/packaging/macos/launchd/md.enesis.wyrd.desktop.plist"
UNIT="$ROOT/packaging/linux/systemd/wyrd-desktop.service"

# Code lines only, captured once: strip full-line comments and the
# whole XML comment block before matching, so the header prose can
# neither satisfy a pin (a token left only in a comment would still
# pass) nor trip one (a key name mentioned in prose would still
# fail) — and match with a here-string (a `code | grep -q` pipeline
# under pipefail would report 141 once grep exits early and the
# script grows past the pipe buffer).
PLIST_CODE="$(grep -v '^[[:space:]]*#' "$PLIST" | sed '/^[[:space:]]*<!--/,/-->/d')"
UNIT_CODE="$(grep -v '^[[:space:]]*#' "$UNIT")"
match() { grep -F -q -- "$1" <<<"$2"; }
nomatch() { ! grep -F -q -- "$1" <<<"$2"; }
count_is() { [ "$(grep -F -c -- "$1" <<<"$2")" = "$3" ]; }

# 1. The templates exist where the installer consumes them.
[[ -f "$PLIST" ]] \
  || die "macOS login template missing"
[[ -f "$UNIT" ]] \
  || die "Linux login template missing"
pass "login templates exist"

# 2. macOS: a launcher, not a supervisor. RunAtLoad starts the UI once
# per login; no KeepAlive key exists anywhere in the template, so a
# quit stays quit and a crash stays dead until the next login or a
# manual launch — never a respawn the owner-first story did not ask
# for (decided: docs/design/desktop-shell.md open-questions item 7).
match '<key>Label</key>' "$PLIST_CODE" \
  || die "plist: no Label key"
match 'md.enesis.wyrd.desktop' "$PLIST_CODE" \
  || die "plist: label is not the desktop agent"
match '<key>RunAtLoad</key>' "$PLIST_CODE" \
  || die "plist: no RunAtLoad"
nomatch 'KeepAlive' "$PLIST_CODE" \
  || die "plist: KeepAlive would supervise the UI"
pass "macOS template launches once per login, never respawns"

# 3. macOS: the agent opens the installed app bundle by explicit path,
# filled once by the installer — never a bare binary exec, never a
# re-resolved name (reinstall after moving the app).
match '/usr/bin/open' "$PLIST_CODE" \
  || die "plist: launcher is not open"
match '@WYRD_APP_PATH@' "$PLIST_CODE" \
  || die "plist: app-path token missing"
count_is '@WYRD_APP_PATH@' "$PLIST_CODE" 1 \
  || die "plist: app-path token must appear exactly once, in ProgramArguments"
pass "macOS template opens the installed app by filled path"

# 4. macOS: the desktop agent is its own unit — it never names a
# per-drive owner agent (md.enesis.wyrd.<service>), because login
# ordering is a UI behavior (unavailable until the owner answers),
# never a manager dependency.
nomatch 'md.enesis.wyrd.<service>' "$PLIST_CODE" \
  || die "plist: names a per-drive owner agent"
pass "macOS template carries no owner dependency"

# 5. Linux: a launcher with crash backstop. WantedBy starts the UI
# once per login; Restart=on-failure restarts crashes only (a clean
# quit exits 0 and stays quit). No RestartPreventExitStatus: the UI
# has no 78-class terminal state, and none is invented here.
match 'WantedBy=default.target' "$UNIT_CODE" \
  || die "unit: no default.target install"
match 'Restart=on-failure' "$UNIT_CODE" \
  || die "unit: crash backstop missing"
nomatch 'RestartPreventExitStatus' "$UNIT_CODE" \
  || die "unit: terminal-exit refusal invents UI state"
match '@WYRD_DESKTOP_BIN@' "$UNIT_CODE" \
  || die "unit: binary-path token missing"
count_is '@WYRD_DESKTOP_BIN@' "$UNIT_CODE" 1 \
  || die "unit: binary-path token must appear exactly once, in ExecStart"
pass "Linux template launches per login with crash-only restart"

# 5b. The crash backstop lives at unit scope: StartLimit keys are
# [Unit]-section options and are silently ignored in [Service], so
# the pin checks the section placement, not just the key names (the
# repo's own pattern: nix/drive/nixos.nix sets start limits at unit
# scope, outside serviceConfig).
UNIT_SECTION="$(sed -n '/^\[Unit\]/,/^\[Service\]/p' <<<"$UNIT_CODE")"
for key in 'StartLimitIntervalSec=300' 'StartLimitBurst=3'; do
  match "$key" "$UNIT_SECTION" \
    || die "unit: '$key' is not in [Unit]"
done
SERVICE_SECTION="$(sed -n '/^\[Service\]/,$p' <<<"$UNIT_CODE")"
nomatch 'StartLimit' "$SERVICE_SECTION" \
  || die "unit: StartLimit keys in [Service] are silently ignored"
pass "Linux crash backstop lives at unit scope"

# 6. Linux: no After/Wants on owner units (wyrd-<service>.service) —
# owner units are dynamic per-drive state the login unit must never
# hard-code. network-online may stay; owner units must not.
nomatch 'wyrd-<' "$UNIT_CODE" \
  || die "unit: hard-codes a per-drive owner unit"
nomatch 'After=wyrd-' "$UNIT_CODE" \
  || die "unit: orders after an owner unit"
nomatch 'Wants=wyrd-' "$UNIT_CODE" \
  || die "unit: wants an owner unit"
pass "Linux template carries no owner dependency"

# 7. One app identity across both templates: the macOS agent label
# nests under the desktop bundle id and the Linux unit names the
# desktop binary — the same md.enesis.wyrd / wyrd-desktop pair the
# appex-embedding check covers, never a second id.
match 'md.enesis.wyrd' "$PLIST_CODE" \
  || die "plist: outside the app identity"
match 'wyrd-desktop' "$UNIT_CODE" \
  || die "unit: outside the desktop identity"
pass "both templates share the one app identity"

# 8. The decision is recorded in design-adjacent prose: the shell note
# names the decided login-item shape and rejects SMAppService plus
# manager ordering in the same place.
for marker in 'md.enesis.wyrd.desktop' 'wyrd-desktop.service' 'SMAppService'; do
  grep -Fq "$marker" "$ROOT/docs/design/desktop-shell.md" \
    || die "desktop-shell.md: '$marker' not recorded"
done
pass "design note records the login-item decision"

# 9. The recovery story is written where the installer consumes it:
# the macOS packaging README carries install, stop/start, and the
# per-cause recovery for both platforms.
for marker in 'md.enesis.wyrd.desktop' 'wyrd-desktop.service' 'launchctl load -w' 'systemctl --user'; do
  grep -Fq "$marker" "$ROOT/packaging/macos/README.md" \
    || die "packaging README: '$marker' not recorded"
done
pass "packaging README carries install and recovery"

# 10. The suite itself is wired into the native CI shell job.
grep -Fq 'test-login-items.sh' "$ROOT/.ngit/nix/workflows/default.nix" \
  || die "CI: login-items suite not wired into shell.suites"
pass "CI wires the login-items suite"

echo "login-items: $PASS checks passed"
