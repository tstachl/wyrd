#!/usr/bin/env bash
# tests/test-stage-subject.sh — stub suite pinning the subject-staging
# contract of nix/microvm/stage-subject.sh: hostile trees are refused
# loudly before any nix evaluation (special files, escaping or
# dangling links, over-cap trees), and clean trees stage with the
# measured bytes, file count, pin, and exact override flags.
#
# GNU userland is required (du -sb, realpath -m, GNU tar excludes):
# the runner only ever executes on the Linux KVM host, so a shell
# without them skips loudly instead of passing vacuously. nix is
# stubbed — this pins the wiring (hash flows into the pin and flags),
# not nix itself; the real `nix hash path` refusal of special files
# is established on Odin, not here.
# Pure bash + coreutils: run it anywhere with
# `bash tests/test-stage-subject.sh`. (Wired into CI shell-tests
# alongside the other harness stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

# GNU gate: du -sb and realpath -m are GNU-only, and the tar
# exclusions are verified against GNU semantics below. Without them
# the refusals cannot be exercised faithfully, so skip loudly.
du -sb / >/dev/null 2>&1 || { echo "SKIP: stage-subject tests need GNU du"; exit 0; }
command -v realpath >/dev/null 2>&1 || { echo "SKIP: stage-subject tests need realpath"; exit 0; }
tar --version 2>/dev/null | grep -qi gnu || { echo "SKIP: stage-subject tests need GNU tar"; exit 0; }

# Stub nix: records its argv, answers `hash path` with a fixed pin.
STUBS="$(mktemp -d)"
TMP="$(mktemp -d)"
trap 'rm -rf "$STUBS" "$TMP"' EXIT
cat > "$STUBS/nix" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$FAKE_NIX_ARGV"
if [[ "${1:-}" == "hash" && "${2:-}" == "path" && -n "${3:-}" ]]; then
  printf '%s\n' "$FAKE_NARHASH"
else
  echo "fake nix: unsupported args: $*" >&2; exit 2
fi
STUB
chmod +x "$STUBS/nix"
FAKE_NARHASH="sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
FAKE_NIX_ARGV="$STUBS/argv"
export PATH="$STUBS:$PATH" FAKE_NARHASH FAKE_NIX_ARGV

LIB="$ROOT/nix/microvm/stage-subject.sh"
[[ -f "$LIB" ]] || die "staging lib missing at $LIB"
# shellcheck source=/dev/null
source "$LIB"

# refuse <desc> <expected-substring> <command...>: the command must
# fail inside its subshell, and stderr must name the reason.
refuse() {
  local desc="$1" want="$2"; shift 2
  local err
  if err="$("$@" 2>&1)"; then
    die "$desc ran clean (expected refusal containing: $want)"
  fi
  [[ "$err" == *"$want"* ]] || die "$desc refusal hid the reason (want: $want; got: $err)"
  pass "$desc"
}

# accept <desc> <command...>: the command must succeed silently.
accept() {
  local desc="$1"; shift
  "$@" >/dev/null 2>&1 || die "$desc failed cleanly (expected pass)"
  pass "$desc"
}

# --- special files fail closed, with the offender named ---
TREE="$TMP/special"; mkdir -p "$TREE/sub"
printf '[workspace]\n' > "$TREE/Cargo.toml"
mkfifo "$TREE/sub/pipe1"
refuse "fifo is refused" "non-file entries" subject_check_special "$TREE"
refuse "fifo names the offender" "pipe1" subject_check_special "$TREE"

# --- exclusions apply to the scan: a fifo under target/ is ignored ---
mkdir -p "$TREE/target"
mkfifo "$TREE/target/pipe2"
accept "excluded fifo does not trip the scan" subject_check_special "$TREE"

# --- escaping symlinks fail closed, naming link and target ---
rm -rf "$TMP/link" && mkdir -p "$TMP/link"
printf 'x\n' > "$TMP/link/file"
ln -s /etc/hostname "$TMP/link/escape"
refuse "escaping symlink is refused" "links outside the staged copy" subject_check_symlinks "$TMP/link"
refuse "refusal names the target" "/etc/hostname" subject_check_symlinks "$TMP/link"

# --- dangling links fail closed ---
rm -rf "$TMP/dangle" && mkdir -p "$TMP/dangle"
ln -s /nonexistent-step5-target "$TMP/dangle/dang"
refuse "dangling symlink is refused" "dangling" subject_check_symlinks "$TMP/dangle"

# --- internal links pass ---
rm -rf "$TMP/oklinks" && mkdir -p "$TMP/oklinks"
printf 'data\n' > "$TMP/oklinks/real"
ln -s real "$TMP/oklinks/alias"
accept "internal symlink passes" subject_check_symlinks "$TMP/oklinks"

# --- byte cap fails closed with a measured number in the right ballpark ---
rm -rf "$TMP/big" && mkdir -p "$TMP/big"
truncate -s 300M "$TMP/big/blob.bin"
if out="$(subject_check_caps "$TMP/big" "$TMP/big" 2>&1)"; then
  die "oversized tree passed the byte cap"
fi
[[ "$out" == *"exceeds the byte cap"* ]] || die "byte-cap refusal hid the reason: $out"
[[ "$out" == *"3145728"* ]] || die "byte-cap refusal hid the measurement: $out"
pass "oversized tree fails the byte cap with a measured number"

# --- file-count cap fails closed with the measured number ---
rm -rf "$TMP/many" && mkdir -p "$TMP/many"
# shellcheck disable=SC2046
touch $(seq -f "$TMP/many/f%g" 1 20001)
if out="$(subject_check_caps "$TMP/many" "$TMP/many" 2>&1)"; then
  die "file-heavy tree passed the file-count cap"
fi
[[ "$out" == *"exceeds the file-count cap"* ]] || die "count-cap refusal hid the reason: $out"
[[ "$out" == *"20001"* ]] || die "count-cap refusal hid the count: $out"
pass "file-heavy tree fails the file-count cap with the count"

# --- happy path: stage, measure, pin, flags ---
rm -rf "$TMP/good" "$TMP/staged" && mkdir -p "$TMP/good"
printf '[workspace]\n[workspace.package]\nversion = "0.0.0-test"\n' > "$TMP/good/Cargo.toml"
printf 'x\n' > "$TMP/good/file"
accept "clean tree passes the scan" subject_check_special "$TMP/good"
subject_stage "$TMP/good" "$TMP/staged" || die "clean tree failed staging"
CONTENT_BYTES="$(find "$TMP/staged" -type f -exec cat {} + | wc -c)"
[[ "$SUBJECT_BYTES" =~ ^[0-9]+$ ]] || die "byte count not a number: $SUBJECT_BYTES"
[[ "$SUBJECT_BYTES" -ge "$CONTENT_BYTES" ]] || die "byte count below content sum: $SUBJECT_BYTES < $CONTENT_BYTES"
[[ "$SUBJECT_FILES" == "2" ]] || die "file count wrong (want 2, got $SUBJECT_FILES)"
pass "clean tree stages with a real measurement ($SUBJECT_BYTES bytes, $SUBJECT_FILES files)"
[[ -f "$TMP/staged/Cargo.toml" && -f "$TMP/staged/file" ]] || die "staged tree lost files"
pass "staged tree carries the files"
PIN="$(subject_pin "$TMP/staged")"
[[ "$PIN" == "$FAKE_NARHASH" ]] || die "pin did not flow from nix hash (got $PIN)"
pass "pin flows from nix hash"
grep -q "hash path" "$FAKE_NIX_ARGV" || die "nix was never asked to hash (wiring broken)"
pass "pin invokes nix hash path"
SUBJECT_FLAGS=()
subject_build_flags "$TMP/staged" "$PIN"
[[ "${SUBJECT_FLAGS[*]}" == "override-input wyrdSrc path:$TMP/staged?narHash=$FAKE_NARHASH --option allow-import-from-derivation false --offline" ]] \
  || die "flags wrong: ${SUBJECT_FLAGS[*]}"
pass "flags bind the pin with IFD-denied and offline"

# --- exclusion lists are defined once and feed both consumers ---
grep -qF 'SUBJECT_EXCLUDE_DIRS=(.git target .direnv .devenv)' "$LIB" \
  || die "dir exclusion list changed shape (update the test deliberately)"
grep -qF "SUBJECT_EXCLUDE_GLOBS=('result*')" "$LIB" \
  || die "glob exclusion list changed shape (update the test deliberately)"
_dir_uses="$(grep -c 'SUBJECT_EXCLUDE_DIRS\[@\]' "$LIB" || true)"
[[ "$_dir_uses" -ge 2 ]] \
  || die "dir exclusions no longer feed both consumers (found $_dir_uses uses)"
_glob_uses="$(grep -c 'SUBJECT_EXCLUDE_GLOBS\[@\]' "$LIB" || true)"
[[ "$_glob_uses" -ge 2 ]] \
  || die "glob exclusions no longer feed both consumers (found $_glob_uses uses)"
pass "exclusion lists are defined once and feed both consumers"

echo "stage-subject: $PASS checks passed"
