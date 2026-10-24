#!/usr/bin/env bash
# nix/microvm/stage-subject.sh — bounded, pinned subject staging.
#
# Sourced, never executed. Pure functions over explicit directory
# arguments: no side effects at source time, no globals read except
# the exclusion lists below, results via stdout/globals documented
# per function. Requires `die` from the sourcer (run-microvm.sh and
# tests/test-stage-subject.sh each define their own): refusals print
# `FAIL: ...` to stderr and return nonzero, so `$( )`-wrapped callers
# observe the refusal without dying.
#
# The exclusion lists are the single source of truth for what never
# enters evaluation (N2): both the pre-copy scan and the tar copy
# below expand these same arrays, so an entry added here applies to
# both — there is no second list to keep in sync.
SUBJECT_EXCLUDE_DIRS=(.git target .direnv .devenv)
SUBJECT_EXCLUDE_GLOBS=('result*')

# Byte and file-count caps (plan step 4). Fixed production values;
# the unit test exercises them with sparse files and many empty
# files rather than overrides, so there is no env knob to get wrong.
SUBJECT_BYTE_CAP=268435456
SUBJECT_FILE_CAP=20000

# subject_build_tar_excludes: print one tar --exclude option per
# line from the exclusion lists. subject_copy collects them into an
# array; the runner builds its copy command from this and nothing
# else.
subject_build_tar_excludes() {
  local _e
  for _e in "${SUBJECT_EXCLUDE_DIRS[@]}"; do
    printf -- '--exclude=%s\n' "$_e"
  done
  for _e in "${SUBJECT_EXCLUDE_GLOBS[@]}"; do
    printf -- '--exclude=%s\n' "$_e"
  done
}

# subject_build_find_prune: print the find prune fragment matching
# the same set tar excludes (component match at any depth, plus
# basename globs). Consumed as an unquoted expansion into find —
# entries are controlled literals with no spaces, so splitting is
# safe and no glob can escape (patterns are quoted at use).
subject_build_find_prune() {
  local _e _first=1
  printf '( '
  for _e in "${SUBJECT_EXCLUDE_DIRS[@]}"; do
    if [[ $_first == 1 ]]; then _first=0; else printf ' -o'; fi
    printf ' -path %s' "*/$_e"
  done
  for _e in "${SUBJECT_EXCLUDE_GLOBS[@]}"; do
    printf ' -o -name %s' "$_e"
  done
  printf ' ) -prune -o'
}

# subject_check_special <src>: refuse fifos, sockets, devices.
# nix cannot hash them and GNU tar would silently drop sockets, so
# either outcome must fail closed here with the offenders named.
subject_check_special() {
  local src="$1" offending total
  # shellcheck disable=SC2046 # intentional splitting of generated find fragment (controlled literals, no spaces)
  offending="$(find "$src" $(subject_build_find_prune) \( -not -type f -not -type d -not -type l -print \) 2>/dev/null | head -6)"
  [[ -z "$offending" ]] || {
    # shellcheck disable=SC2046 # same intentional split as above
    total="$(find "$src" $(subject_build_find_prune) \( -not -type f -not -type d -not -type l -print \) 2>/dev/null | wc -l)"
    die "subject tree has non-file entries (fifos, sockets, devices refused): $total found, first: $(printf '%s' "$offending" | tr '\n' ' ')"
  }
}

# subject_copy <src> <dst>: bounded tar copy honoring the exclusion
# lists. The destination must exist and be empty (caller ensures).
subject_copy() {
  local src="$1" dst="$2" _excludes=()
  while IFS= read -r _line; do _excludes+=("$_line"); done < <(subject_build_tar_excludes)
  tar -C "$src" "${_excludes[@]}" -cf - . | tar -C "$dst" -xf - \
    || die "subject staging copy failed"
}

# subject_measure <staged>: set SUBJECT_BYTES and SUBJECT_FILES
# (globals the runner reports; the test asserts them).
subject_measure() {
  SUBJECT_BYTES="$(du -sb "$1" | cut -f1)"
  SUBJECT_FILES="$(find "$1" -type f | wc -l)"
}

# subject_check_caps <staged> <orig>: enforce the byte and
# file-count caps on the staged copy (exclusions already applied, so
# host scratch cannot blow the caps or poison the pin). Fails closed
# with the measured numbers, never truncated. <orig> names the
# operator's tree in messages; the copy is what gets measured.
subject_check_caps() {
  local staged="$1" orig="$2"
  subject_measure "$staged"
  [[ "$SUBJECT_BYTES" -le "$SUBJECT_BYTE_CAP" ]] \
    || die "subject tree exceeds the byte cap (256 MiB): $SUBJECT_BYTES bytes from $orig"
  [[ "$SUBJECT_FILES" -le "$SUBJECT_FILE_CAP" ]] \
    || die "subject tree exceeds the file-count cap (20000): $SUBJECT_FILES files from $orig"
}

# subject_check_symlinks <staged>: every symlink must resolve inside
# the staged copy and exist. nix records the link rather than the
# target (probed 2026-10-23 — retargeting leaves narHash unchanged),
# so an outside link would bind a stable pin while resolving outside
# the pinned bytes at build time. realpath -m canonicalizes without
# touching the filesystem, so loops cannot hang this check.
subject_check_symlinks() {
  local staged="$1" _link _target
  while IFS= read -r -d '' _link; do
    _target="$(realpath -m "$_link")"
    case "$_target" in
      "$staged"/*) ;;
      *) die "subject tree links outside the staged copy: $_link -> $(readlink "$_link")" ;;
    esac
    [[ -e "$_target" ]] || die "subject tree has a dangling symlink: $_link -> $(readlink "$_link")"
  done < <(find "$staged" -type l -print0)
}

# subject_pin <staged>: print the narHash binding the measured bytes.
# Order matters: hash only after scan, copy, caps, and symlink
# containment all pass, so the pin covers exactly what evaluation
# will see.
subject_pin() {
  nix hash path "$1"
}

# subject_stage <src> <dst>: copy, measure, cap, contain. Sets
# SUBJECT_BYTES and SUBJECT_FILES (globals for the audit line); the
# caller computes the pin from the staged tree and builds the flags.
# Fails closed at the first violation, loudest message first.
subject_stage() {
  local src="$1" dst="$2"
  subject_check_special "$src"
  rm -rf "$dst"
  mkdir -p "$dst"
  subject_copy "$src" "$dst"
  subject_check_caps "$dst" "$src"
  subject_check_symlinks "$dst"
}

# subject_build_flags <staged> <narhash>: fill SUBJECT_FLAGS with the
# nix invocation tail that binds the subject build. The override
# resolves before pure evaluation begins; IFD denial and offline
# match the proven proof configuration (see the --subject header in
# run-microvm.sh). A first subject run on a cold store fails loudly
# naming the missing paths — warm it with a default run first.
# shellcheck disable=SC2034 # SUBJECT_FLAGS is set here, consumed by run-microvm.sh
subject_build_flags() {
  SUBJECT_FLAGS=(--override-input wyrdSrc "path:$1?narHash=$2"
    --option allow-import-from-derivation false --offline)
}
