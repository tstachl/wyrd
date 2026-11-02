#!/usr/bin/env bash
# Edge cases for test-doc-links.sh: bracket/star/question filenames and
# dotted anchors must neither hang the checker nor flip its verdict.
# Builds a scratch docs tree inside the checkout (the checker scans
# tracked plus untracked *.md), runs the checker, then removes it.
# No relay, credential, or network.
#
# Run from the checkout: bash .ngit/scripts/tests/test-doc-links-targets.sh
# Exits nonzero on any failure.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

# Hook runners export GIT_DIR into hook processes; drop repo discovery
# so the scratch tree below is an ordinary untracked directory.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR

pass=0
fail=0
ok() { pass=$((pass + 1)); echo "ok: $1"; }
bad() { fail=$((fail + 1)); echo "FAIL: $1"; }

T="$(mktemp -d "$ROOT/repro-links.XXXXXXXX")"
[ -d "$T" ] || { echo "FAIL: scratch dir setup failed" >&2; exit 1; }
trap 'rm -rf "$T"' EXIT

# `timeout` degrades gracefully where absent (macOS has none): without
# it a regressed hang would burn the job timeout instead of failing
# fast here.
if command -v timeout >/dev/null 2>&1; then
  CHECK="timeout 120 bash $SCRIPT_DIR/test-doc-links.sh"
else
  CHECK="bash $SCRIPT_DIR/test-doc-links.sh"
fi

# Phase 1: valid tricky targets must pass (and terminate). Links are
# relative to index.md, which lives beside its targets.
touch "$T/br[ack]et.md" "$T/star*.md" "$T/quest?.md" "$T/sp ace.md"
cat > "$T/other.md" <<'EOF'
# Title

## v1.0 Notes
EOF
cat > "$T/index.md" <<'EOF'
# Index

See [brackets](br[ack]et.md), [star](star*.md), [question](quest?.md),
[space](sp ace.md) and [dotted](other.md#v10-notes).
EOF
if $CHECK >/dev/null 2>&1; then
  ok "bracket/star/question/space targets and dotted anchor pass"
else
  bad "valid tricky targets failed or hung (exit $?)"
fi

# Phase 2: broken tricky targets must still fail (the fix must not
# neuter detection): a missing bracketed file, and a dotted anchor
# (dots are dropped, so #v1.0-notes matches no heading).
cat > "$T/broken.md" <<'EOF'
# Broken

See [missing](missing[x].md) and [bad-anchor](other.md#v1.0-notes).
EOF
if $CHECK >/dev/null 2>&1; then
  bad "broken tricky targets passed (detection neutered)"
else
  ok "missing bracket target and dotted anchor still fail"
fi

echo "---"
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
