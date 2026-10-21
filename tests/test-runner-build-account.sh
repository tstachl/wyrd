#!/usr/bin/env bash
# tests/test-runner-build-account.sh — stub pinning the build-account
# policy of nix/microvm/run-microvm.sh: the host names the account the
# nix builds run as (E2E_BUILD_USER), the tree's owner is the
# fallback, a build that would run as root is refused with a message
# naming the setting to change, and the account must be able to build
# the tree it is handed.
#
# The policy is exercised behaviourally, with stat, id and setpriv
# stubbed on PATH, so the wiring is pinned and not only the decision
# functions: a runner that stopped resolving the uid, or that took the
# group from the tree when the host named an account, fails here.
# Pure bash, no host, no nix: run it anywhere with
# `bash tests/test-runner-build-account.sh`. (Wired into CI alongside
# the other harness stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

POLICY="$ROOT/nix/microvm/build-account.sh"
[[ -f "$POLICY" ]] || die "build-account policy missing at $POLICY"
# shellcheck source=/dev/null
source "$POLICY"

# The build account is a fixed name the stub invents, and its home is
# a temporary directory: the stub models the host's passwd entry
# instead of needing a real account with a real home, so it runs the
# same for root and for anyone else.
ACCOUNT="wyrdbuild"

# The stubs. stat reports the tree's owner and group, id resolves the
# one build account they know, setpriv runs the probe it is handed.
STUBS="$TMP/stubs"
mkdir -p "$STUBS"
cat > "$STUBS/stat" <<'STUB'
#!/usr/bin/env bash
# Fake stat: -c %U prints the tree's owner, -c %G its group.
case "${2:-}" in
  %U) printf '%s\n' "$FAKE_TREE_OWNER" ;;
  %G) printf '%s\n' "$FAKE_TREE_GROUP" ;;
  *) echo "fake stat: unsupported args: $*" >&2; exit 2 ;;
esac
STUB
cat > "$STUBS/id" <<'STUB'
#!/usr/bin/env bash
# Fake id: -u, -g and -gn answer for the one build account it knows.
case "${1:-}" in
  -u)
    [[ "${2:-}" == "$FAKE_ACCOUNT" ]] || { echo "id: '$2': no such user" >&2; exit 1; }
    printf '%s\n' "$FAKE_ACCOUNT_UID" ;;
  -g)
    [[ "${2:-}" == "$FAKE_ACCOUNT" ]] || { echo "id: '$2': no such user" >&2; exit 1; }
    printf '%s\n' "$FAKE_ACCOUNT_GID" ;;
  -gn)
    [[ "${2:-}" == "$FAKE_ACCOUNT" ]] || { echo "id: '$2': no such user" >&2; exit 1; }
    printf '%s\n' "$FAKE_ACCOUNT_GROUP" ;;
  *) echo "fake id: unsupported args: $*" >&2; exit 2 ;;
esac
STUB
cat > "$STUBS/setpriv" <<'STUB'
#!/usr/bin/env bash
# Fake setpriv: it runs the command it is handed (the probe's shape is
# then pinned by the probe actually running), and the stub can make it
# fail outright to model an account that cannot open the tree.
[[ "$FAKE_CAN_BUILD" == "yes" ]] || exit 1
while [[ "${1:-}" == -* ]]; do
  case "$1" in
    --reuid | --regid) shift 2 ;;
    --init-groups) shift ;;
    *) break ;;
  esac
done
exec "$@"
STUB
chmod +x "$STUBS"/*

# The host's passwd entry for the build account: a home the stub owns.
mkdir -p "$TMP/home"
# The real home resolution, once, before the stub replaces it: an
# account with no passwd entry expands to the literal `~name`, which is
# what the module's comment claims and what the home check relies on.
real_home="$(build_home "wyrdnosuchuser")"
[[ "$real_home" == "~wyrdnosuchuser" ]] || die "an account with no passwd entry did not expand literally: got '$real_home'"
pass "the home resolution leaves an unknown account literal"
build_home() { printf '%s\n' "$TMP/home"; }

FAKE_TREE_OWNER=root
FAKE_TREE_GROUP=root
FAKE_ACCOUNT="$ACCOUNT"
FAKE_ACCOUNT_UID=1000
FAKE_ACCOUNT_GID=1000
FAKE_ACCOUNT_GROUP=staff
FAKE_CAN_BUILD=yes
export FAKE_TREE_OWNER FAKE_TREE_GROUP FAKE_ACCOUNT FAKE_ACCOUNT_UID \
  FAKE_ACCOUNT_GID FAKE_ACCOUNT_GROUP FAKE_CAN_BUILD
export PATH="$STUBS:$PATH"

TREE="$TMP/tree"
mkdir -p "$TREE"
# The tree has an entry: nix copies the tree, so the probe reads what
# is in it.
: >"$TREE/flake.nix"

# resolve <named>: runs the policy against the stubbed host and
# prints the resolved identity, or the refusal. Returns non-zero when
# the policy refused.
resolve() { # <named>
  if resolve_build_identity "$TREE" "$1" 2>"$TMP/err"; then
    printf '%s %s %s\n' "$BUILD_USER" "$BUILD_UID" "$BUILD_GROUP"
  else
    cat "$TMP/err"
    return 1
  fi
}

# refuse <named>: the policy must refuse, and the message is printed.
refuse() { # <named>
  local out
  if out="$(resolve "$1")"; then
    return 1
  fi
  printf '%s' "$out"
}

# The host names the account: it wins over the tree's owner, so a
# root-owned snapshot still builds as the named account.
FAKE_TREE_OWNER=root
FAKE_TREE_GROUP=root
out="$(resolve "$ACCOUNT")" || die "the policy refused: $(cat "$TMP/err")"
[[ "$out" == "$ACCOUNT 1000 staff" ]] || die "named account not used as claimed: got '$out'"
pass "the host's account wins over a root-owned tree"

# A host-named account that also owns the tree takes its own primary
# group, not the tree's — the tree's group may be a foreign one, and a
# build that keeps it keeps half of the privilege the account drops.
FAKE_TREE_OWNER="$ACCOUNT"
FAKE_TREE_GROUP=root
out="$(resolve "$ACCOUNT")" || die "the policy refused: $(cat "$TMP/err")"
[[ "$out" == "$ACCOUNT 1000 staff" ]] || die "named account took the tree's group: got '$out'"
pass "a host-named account runs with its own primary group"

# A host-named account wins over an owner stat cannot resolve: the
# owner is irrelevant once the host names the account.
FAKE_TREE_OWNER=UNKNOWN
FAKE_TREE_GROUP=UNKNOWN
out="$(resolve "$ACCOUNT")" || die "the policy refused: $(cat "$TMP/err")"
[[ "$out" == "$ACCOUNT 1000 staff" ]] || die "named account ignored: got '$out'"
pass "the host's account wins over an unresolved tree owner"

# Without a named account the tree's owner builds, with the tree's
# group: the ordinary checkout is unchanged.
FAKE_TREE_OWNER="$ACCOUNT"
FAKE_TREE_GROUP=wheel
out="$(resolve "")" || die "the policy refused: $(cat "$TMP/err")"
[[ "$out" == "$ACCOUNT 1000 wheel" ]] || die "tree owner not used as before: got '$out'"
pass "the tree's owner remains the fallback, with the tree's group"

# A tree whose owner resolves but whose group does not falls back to
# the account's own group: the tree's group is not an account.
FAKE_TREE_OWNER="$ACCOUNT"
FAKE_TREE_GROUP=UNKNOWN
out="$(resolve "")" || die "the policy refused: $(cat "$TMP/err")"
[[ "$out" == "$ACCOUNT 1000 staff" ]] || die "unresolved tree group not resolved: got '$out'"
pass "an unresolved tree group falls back to the account's own group"

# A root-owned tree with no account named by the host: refused, not
# built as root. The refusal names the setting to change.
FAKE_TREE_OWNER=root
FAKE_TREE_GROUP=root
err="$(refuse "")" || die "a root build was allowed"
[[ "$err" == *"root"* ]] || die "refusal does not say what it refuses: $err"
[[ "$err" == *"E2E_BUILD_USER"* ]] || die "refusal does not name the setting: $err"
pass "a root build with no named account is refused, naming the setting"

# A host that names root has misconfigured the de-escalation: the
# runner must refuse it, not honour it.
err="$(refuse root)" || die "a host naming root was allowed"
[[ "$err" == *"E2E_BUILD_USER"* ]] || die "refusal does not name the setting: $err"
pass "a host naming root is refused, naming the setting"

# The root question is uid-based, not name-based: a second name for
# uid 0 in the host's passwd would pass a name check and still be a
# root build.
FAKE_ACCOUNT_UID=0
err="$(refuse "$ACCOUNT")" || die "uid 0 was allowed"
[[ "$err" == *"uid 0"* ]] || die "refusal does not say what it refuses: $err"
FAKE_ACCOUNT_UID=1000
pass "uid 0 is refused, by uid"

# A name that is not an account is refused: the runner expands the
# account into a home path, so the name must never carry shell
# metacharacters. The literal is deliberate: it is what a compromised
# host configuration would try to inject.
# shellcheck disable=SC2016
err="$(refuse '$(id)')" || die "a malformed account name was allowed"
[[ "$err" == *"E2E_BUILD_USER"* ]] || die "refusal does not name the setting: $err"
pass "a malformed account name is refused, naming the setting"

# An owner stat cannot resolve (it prints the literal UNKNOWN, never a
# numeric id) has no passwd entry and so no home to build under:
# refused, and the way out is named.
FAKE_TREE_OWNER=UNKNOWN
FAKE_TREE_GROUP=UNKNOWN
err="$(refuse "")" || die "an unresolved tree owner was allowed"
[[ "$err" == *"E2E_BUILD_USER"* ]] || die "refusal does not name the setting: $err"
pass "an unresolved tree owner is refused, naming the setting"

# The account must be able to build the tree it is handed: refused by
# name when it cannot, instead of failing minutes into the build.
FAKE_TREE_OWNER=root
FAKE_TREE_GROUP=root
FAKE_CAN_BUILD=no
err="$(refuse "$ACCOUNT")" || die "an account that cannot build the tree was allowed"
[[ "$err" == *"cannot build the source tree"* ]] || die "refusal does not say what it refuses: $err"
[[ "$err" == *"tree it can build"* ]] || die "refusal does not name the host obligation: $err"
FAKE_CAN_BUILD=yes
pass "an account that cannot build the tree is refused, by name"

# The probe's own verdict: a tree with an entry the account cannot
# use is refused by the probe, not by the stub's stand-in. A directory
# with no execute bit is the uid-independent way to model it: the
# probe's traversal test fails for every uid, root included.
mkdir "$TREE/untraversable"
chmod 0644 "$TREE/untraversable"
err="$(refuse "$ACCOUNT")" || die "a tree with an unusable entry was allowed"
[[ "$err" == *"cannot build the source tree"* ]] || die "the probe did not refuse an unusable entry: $err"
rm -rf "$TREE/untraversable"
pass "a tree with an unusable entry is refused, by the probe"

# A symlink is copied as a link: its target is not read, so even a
# link with no target is not a reason to refuse.
ln -s "$TMP/nowhere" "$TREE/dangling"
out="$(resolve "$ACCOUNT")" || die "the policy refused: $(cat "$TMP/err")"
[[ "$out" == "$ACCOUNT 1000 staff" ]] || die "a symlink was refused: got '$out'"
rm -f "$TREE/dangling"
pass "a symlink in the tree is copied, not read"

# The nix client's cache is the account's home: it must be writable by
# that account, and the refusal names the home.
chmod 0500 "$TMP/home"
err="$(refuse "$ACCOUNT")" || die "an account that cannot write its home was allowed"
[[ "$err" == *"cannot write its home"* ]] || die "refusal does not name the home: $err"
chmod 0700 "$TMP/home"
pass "an account that cannot write its home is refused, by name"

# The named account's own group must not be the privileged one: a build
# that keeps gid 0 keeps half of the privilege the account dropped.
FAKE_ACCOUNT_GID=0
err="$(refuse "$ACCOUNT")" || die "gid 0 was allowed"
[[ "$err" == *"gid 0"* ]] || die "refusal does not say what it refuses: $err"
FAKE_ACCOUNT_GID=1000
pass "gid 0 is refused, by gid"

# The runner asks the policy for the identity, and says who the build
# runs as in the run log.
RUNNER="$ROOT/nix/microvm/run-microvm.sh"
[[ -f "$RUNNER" ]] || die "runner missing at $RUNNER"
grep -q 'resolve_build_identity' "$RUNNER" || die "runner does not use the policy"
grep -q 'E2E_BUILD_USER' "$RUNNER" || die "runner does not read E2E_BUILD_USER"
grep -q 'build account:' "$RUNNER" || die "runner does not log the build account"
pass "the runner resolves the identity from the host's E2E_BUILD_USER"

# --teardown builds nothing, so it must not depend on the host naming a
# build account: a host that never named one still has to be able to
# stop its kept run. The resolution sits behind the teardown guard.
# The greps carry `|| true`: under `set -e` a vanished pattern would
# abort the pipeline before the diagnosis below could print.
guard_line="$(grep -n 'TEARDOWN" == 0' "$RUNNER" | head -1 | cut -d: -f1 || true)"
# The pattern quotes the runner's own variable literally.
# shellcheck disable=SC2016
choice_line="$(grep -n 'resolve_build_identity "\$ROOT"' "$RUNNER" | head -1 | cut -d: -f1 || true)"
[[ -n "$guard_line" && -n "$choice_line" ]] || die "the runner does not guard the build identity resolution"
(( choice_line > guard_line )) || die "the build identity is resolved before the teardown guard"
pass "the build identity is resolved only for a run that builds"

echo "runner-build-account: $PASS checks passed"
