#!/usr/bin/env bash
# tests/harness/test-desktop-frontend-gate.sh — pin the desktop
# frontend CI gate: the `desktop` workflow's `frontend` job runs the
# `desktop.frontend` command, which fails fast through frozen
# install, unit tests, typecheck, and the vite build, on PRs
# touching the frontend or the shell identity that serves it.
#
# Static assertions only (the guest run itself is CI evidence).
# Pure bash, no host, no nix: run it anywhere with
# `bash tests/harness/test-desktop-frontend-gate.sh`. (Wired into
# the CI shell suites alongside the other stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

YAML="$ROOT/.ngit/nix/workflows/desktop.yaml"
COMMANDS="$ROOT/.ngit/nix/workflows/default.nix"
PACKAGE="$ROOT/apps/desktop/package.json"
DEVENV="$ROOT/devenv.nix"

# Code lines only, captured once: strip full-line comments before
# matching, so the header prose can never satisfy a pin.
YAML_CODE="$(grep -v '^[[:space:]]*#' "$YAML")"
COMMANDS_CODE="$(grep -v '^[[:space:]]*#' "$COMMANDS")"
DEVENV_CODE="$(grep -v '^[[:space:]]*#' "$DEVENV")"
FLAKE_CODE="$(grep -v '^[[:space:]]*#' "$ROOT/flake.nix")"
match() { grep -F -q -- "$1" <<<"$2"; }

# 1. The workflow names the desktop gate with one frontend job on
# the desktop.frontend command, ready-gated like the other PR
# workflows.
match 'name: desktop' "$YAML_CODE" \
  || die "desktop.yaml: workflow name is not desktop"
match 'run: desktop.frontend' "$YAML_CODE" \
  || die "desktop.yaml: frontend job does not run desktop.frontend"
match 'types: [ready_for_review]' "$YAML_CODE" \
  || die "desktop.yaml: PR leg is not ready-gated"
pass "workflow names the desktop gate with its ready-gated frontend job"

# 2. The command group defines desktop.frontend with all four
# steps under fail-fast.
match 'desktop.frontend = ' "$COMMANDS_CODE" \
  || die "default.nix: desktop.frontend command group missing"
for step in 'bun install --frozen-lockfile' 'bun test' 'bun run typecheck' 'bun run build'; do
  match "$step" "$COMMANDS_CODE" \
    || die "default.nix: desktop.frontend step '$step' missing"
done
match 'set -euo pipefail' "$COMMANDS_CODE" \
  || die "default.nix: desktop.frontend is not fail-fast"
pass "command runs frozen install, tests, typecheck, and build fail-fast"

# 3. Trigger paths cover the frontend, the shell identity, the
# gate itself, and the shell-side bun pins — a rename or re-scope
# fails here instead of silently skipping CI.
for path in 'apps/desktop/**' 'crates/wyrd-desktop/tauri.conf.json' 'crates/wyrd-desktop/capabilities/**' 'flake.nix' 'flake.lock' 'devenv.nix' 'devenv.lock' 'devenv.yaml' '.ngit/nix/**'; do
  match "$path" "$YAML_CODE" \
    || die "desktop.yaml: trigger path '$path' missing"
done
pass "trigger paths cover the frontend, the shell identity, the bun pins, and the gate"

# 4. The package scripts the gate calls exist.
for script in '"test":' '"typecheck":' '"build":'; do
  grep -F -q -- "$script" "$PACKAGE" \
    || die "package.json: script $script missing"
done
pass "package scripts test, typecheck, and build exist"

# 5. The suite itself is wired into the native CI shell job.
match 'test-desktop-frontend-gate.sh' "$COMMANDS_CODE" \
  || die "CI: desktop-frontend-gate suite not wired into shell.suites"
pass "CI wires the desktop-frontend-gate suite"

# 6. Shell and CI resolve bun from the same nixpkgs-unstable rev.
# The dev shell pins `unstable.bun`, the flake carries the same rev
# as an input, and the gate commands take bun (and nodejs) from it.
# A bump moves the flake rev and devenv.lock together; the equality
# pin below fails if they drift, before a lockfile-format skew can
# reach CI.
match 'languages.javascript.bun.package = unstable.bun' "$DEVENV_CODE" \
  || die "devenv.nix: shell bun is not pinned to unstable.bun"
match 'pkgsUnstable.bun' "$COMMANDS_CODE" \
  || die "default.nix: gate commands do not take bun from pkgsUnstable"
match 'pkgsUnstable.nodejs' "$COMMANDS_CODE" \
  || die "default.nix: gate commands do not take nodejs from pkgsUnstable"
match 'nixpkgs-unstable.url = "github:NixOS/nixpkgs/' "$FLAKE_CODE" \
  || die "flake.nix: nixpkgs-unstable input missing"
FLAKE_PIN="$(sed -n '/"nixpkgs-unstable"/,/"original"/p' "$ROOT/flake.lock" | grep -o '"rev": "[0-9a-f]*"' | head -n 1)" \
  || die "flake.lock carries no nixpkgs-unstable rev"
DEVENV_PIN="$(sed -n '/"nixpkgs-unstable"/,/"original"/p' "$ROOT/devenv.lock" | grep -o '"rev": "[0-9a-f]*"' | head -n 1)" \
  || die "devenv.lock carries no nixpkgs-unstable rev"
[ -n "$FLAKE_PIN" ] && [ -n "$DEVENV_PIN" ] \
  || die "bun channel pin unreadable: flake $FLAKE_PIN vs devenv $DEVENV_PIN"
[ "$FLAKE_PIN" = "$DEVENV_PIN" ] \
  || die "bun channel drift: flake $FLAKE_PIN vs devenv $DEVENV_PIN — bump both locks together"
pass "shell and CI resolve bun from the same nixpkgs-unstable rev"

# 7. The lockfile stays at the version the unified bun writes. Both
# sides run bun 1.4.x, whose native format is v2; a v1 pin would now
# fail the other way. If this pin fails, regenerate with the pinned
# bun — never by hand-editing the version.
LOCK_VERSION="$(grep -o '"lockfileVersion": [0-9][0-9]*' "$ROOT/apps/desktop/bun.lock" | head -n 1)" \
  || die "bun.lock carries no lockfileVersion"
[ "$LOCK_VERSION" = '"lockfileVersion": 2' ] \
  || die "bun.lock is $LOCK_VERSION: regenerate with the pinned bun (writes v2)"
pass "lockfile stays at the version the unified bun writes"

echo "desktop-frontend-gate: $PASS checks passed"
