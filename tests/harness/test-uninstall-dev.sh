#!/usr/bin/env bash
# tests/harness/test-uninstall-dev.sh — pin the safety shape of the macOS
# uninstall script and its domain-removal helper. Static assertions only;
# a live full reset is VM evidence, not a CI gate.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

UNINSTALL="$ROOT/packaging/macos/uninstall-dev.sh"
REMOVER="$ROOT/packaging/macos/domain-remover/main.swift"
BUILDER="$ROOT/packaging/macos/domain-remover/build-remover.sh"
REGISTRAR="$ROOT/packaging/macos/domain-registrar/main.swift"
README="$ROOT/packaging/macos/README.md"
WORKFLOW="$ROOT/.ngit/nix/workflows/default.nix"

match() { grep -F >/dev/null -- "$1" <<<"$2"; }
nomatch() { ! grep -F >/dev/null -- "$1" <<<"$2"; }
code() { grep -v -e '^[[:space:]]*#' -e '^[[:space:]]*//' "$1"; }

for file in "$UNINSTALL" "$REMOVER" "$BUILDER"; do
    [[ -f "$file" ]] || die "missing file: $file"
done
pass "uninstall and helper files exist"

bash -n "$UNINSTALL" || die "uninstall script has a syntax error"
bash -n "$BUILDER" || die "remover build script has a syntax error"
pass "shell syntax is valid"

UNINSTALL_CODE="$(code "$UNINSTALL")"
REMOVER_CODE="$(code "$REMOVER")"
REGISTRAR_CODE="$(code "$REGISTRAR")"

match 'DRY_RUN=1' "$UNINSTALL_CODE" \
  || die "the default must be a dry run"
match 'CONFIRM=0' "$UNINSTALL_CODE" \
  || die "confirmation must start disabled"
match '--confirm) CONFIRM=1; DRY_RUN=0' "$UNINSTALL_CODE" \
  || die "only --confirm may enable deletion"
match 'refusing path outside HOME or /Applications' "$UNINSTALL_CODE" \
  || die "path guard is missing"
match 'resolved_parent' "$UNINSTALL_CODE" \
  || die "path guard must check the resolved parent"
match '*..*' "$UNINSTALL_CODE" \
  || die "path guard must reject parent traversal"
match 'if [ "$DRY_RUN" -eq 1 ]' "$UNINSTALL_CODE" \
  || die "removal must honour the dry-run gate"
pass "dry run and confirmation gates are present"

match '--delete-data' "$UNINSTALL_CODE" \
  || die "drive data needs its own flag"
match 'DELETE_DATA=0' "$UNINSTALL_CODE" \
  || die "drive data must be kept by default"
match 'if [ "$DELETE_DATA" -eq 1 ]; then' "$UNINSTALL_CODE" \
  || die "support directory deletion is not gated"
match 'say "unload launch agent' "$UNINSTALL_CODE" \
  || die "launch agents must be listed before unloading"
match 'would stop pid' "$UNINSTALL_CODE" \
  || die "a dry run must list the processes it would stop"
match 'wyrd_shadowed_plugins' "$UNINSTALL_CODE" \
  || die "plugin parsing must use the shared tolerant parser"
match 'MOUNTS_LEFT' "$UNINSTALL_CODE" \
  || die "leftover mounts must be named in the final summary"
pass "drive data, agents, processes and mounts are reported"

match '--remove-domains' "$UNINSTALL_CODE" \
  || die "domain removal must be opt-in"
match 'REMOVE_DOMAINS=0' "$UNINSTALL_CODE" \
  || die "domains must be kept by default"
match 'domains left in place' "$UNINSTALL_CODE" \
  || die "a partial uninstall must say so"
match '"$OUT/WyrdDomainRemover" --all' "$UNINSTALL_CODE" \
  || die "the uninstall must call the helper with --all"
match 'TeamIdentifier' "$UNINSTALL_CODE" \
  || die "helper and app signing teams must be checked"
match 'APP_TEAM" = "not set' "$UNINSTALL_CODE" \
  || die "an ad-hoc app must be refused for domain removal"
match "trap 'rm -rf" "$UNINSTALL_CODE" \
  || die "the temporary helper directory must be cleaned on failure"
match 'could not enumerate domains' "$REMOVER_CODE" \
  || die "a failed domain read must not report success"
pass "domain removal is supervised and fails closed"

match 'NSFileProviderManager.remove' "$REMOVER_CODE" \
  || die "the remover helper does not remove domains"
match 'args == ["--all"]' "$REMOVER_CODE" \
  || die "the helper must refuse a partial removal"
match 'refusing to remove fewer than every domain' "$REMOVER_CODE" \
  || die "the helper must explain the all-or-nothing rule"
nomatch 'NSFileProviderManager.remove' "$REGISTRAR_CODE" \
  || die "the registrar must stay add-only"
pass "removal lives in the helper, not the registrar"

match 'uninstall-dev.sh' "$(cat "$README")" \
  || die "the README must document the uninstall"
match 'tests/harness/test-uninstall-dev.sh' "$(cat "$WORKFLOW")" \
  || die "this test must be wired into CI"
pass "documentation and CI wiring are present"

echo "test-uninstall-dev: $PASS checks passed"
