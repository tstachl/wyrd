#!/usr/bin/env bash
# tests/harness/test-passphrase-stdin.sh — pin the per-session unlock
# transport: `--passphrase-stdin` exists on the CLI, reads one bounded
# line from stdin, fails closed, never persists, and never travels
# argv/env/temp/log. Static assertions only (live unlock + no-file
# boot is manual rehearsal evidence on the PR).
#
# Pure bash, no host, no nix: run it anywhere with
# `bash tests/harness/test-passphrase-stdin.sh`. (Wired into the CI
# shell suites alongside the other stubs.)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
pass() { PASS=$((PASS + 1)); echo "  PASS: $1"; }
die() { echo "  FAIL: $1" >&2; exit 1; }

ARGS="$ROOT/crates/wyrd-cli/src/args.rs"
CREDS="$ROOT/crates/wyrd-cli/src/credentials.rs"
IDENTITY_COMMIT="$ROOT/crates/wyrd-cli/src/commands/identity.rs"
SHELL_NOTE="$ROOT/docs/design/desktop-shell.md"

# Code lines only, captured once: strip full-line comments before
# matching, so prose can neither satisfy a pin nor trip one — and
# match with a here-string (a `code | grep -q` pipeline under
# pipefail would report 141 once grep exits early and the script
# grows past the pipe buffer).
ARGS_ALL="$(cat "$ARGS")"
ARGS_CODE="$(grep -v '^[[:space:]]*//' "$ARGS")"
CREDS_CODE="$(grep -v '^[[:space:]]*//' "$CREDS")"
match() { grep -F -q -- "$1" <<<"$2"; }
nomatch() { ! grep -F -q -- "$1" <<<"$2"; }

# 1. The flag exists on the shared Credentials surface, conflicts
# with the file flag, and every credential-bearing command inherits
# it (flattened once, not per-command).
match 'passphrase_stdin' "$ARGS_CODE" \
  || die "args: no passphrase_stdin surface"
match 'conflicts_with' "$ARGS_CODE" \
  || die "args: stdin/file pair not mutually exclusive"
pass "CLI offers one stdin unlock surface exclusive with the file"

# 2. The stdin reader is bounded at the same 4096 the file reader
# enforces — a hostile pipe cannot exhaust memory.
match '4096' "$CREDS_CODE" \
  || die "credentials: no 4096 bound on the stdin read"
pass "stdin read is bounded like the file read"

# 3. Empty pipes fail closed before anything opens: EOF and a bare
# newline both refuse as usage, never as an empty passphrase the
# owner would try.
match 'no passphrase on stdin' "$CREDS_CODE" \
  || die "credentials: empty stdin does not fail closed"
pass "empty stdin fails closed"

# 4. One trailing newline is line discipline, not passphrase — the
# same stripping both surfaces apply, so file and stdin open the
# same keystore.
match 'strip_suffix' "$CREDS_CODE" \
  || die "credentials: no trailing-newline discipline on stdin"
pass "stdin strips one trailing newline like the file"

# 5. The ambiguous pair refuses even where clap never parses: a
# programmatic Credentials with both surfaces fails instead of
# silently preferring one.
match 'conflicts with --passphrase-file' "$CREDS_CODE" \
  || die "credentials: ambiguous pair not refused in code"
pass "stdin plus file refuses in code, not just in clap"

# 6. Secrets stay out of argv: the commit precedent (stdin pipe,
# never argv) is the only pattern, and the new reader names its
# pipe `<stdin>` in errors, never the bytes.
match 'never persists' "$ARGS_ALL" \
  || die "args: stdin help does not state nothing is persisted"
match '<stdin>' "$CREDS_CODE" \
  || die "credentials: stdin errors do not name the pipe"
for banned in 'passphrase.as_str()' 'passphrase.as_bytes()' 'println!("{}", passphrase'; do
  nomatch "$banned" "$CREDS_CODE" \
    || die "credentials: secret bytes near output: $banned"
done
pass "secrets stay out of argv and logs"

# 7. Identity still comes from its file in stdin mode: the flag
# help says the identity secret keeps its file, so stdin never
# becomes a whole-credential pipe.
match 'identity secret still comes from its file' "$ARGS_ALL" \
  || die "args: identity-file requirement in stdin mode not stated"
pass "stdin mode keeps the identity file"

# 8. The decision is recorded: R1/R2 have a repo home in the shell
# note, and the onboarding mandate is posture-scoped.
for marker in 'R1' 'R2' '--passphrase-stdin'; do
  grep -Fq -- "$marker" "$SHELL_NOTE" \
    || die "desktop-shell.md: '$marker' not recorded"
done
pass "design note defines R1/R2 and names the transport"

echo "passphrase-stdin: $PASS checks passed"
