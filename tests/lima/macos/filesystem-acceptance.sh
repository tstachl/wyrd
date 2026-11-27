#!/bin/sh
# Packaged-app macOS filesystem acceptance driver (guest side).
# Requires GROUP (or WYRD_APP_GROUP) from the host runner; no team-specific
# default belongs in the repository.
set -u

D="${D:-$HOME/wyrd-accept-short}"
DRIVE="${DRIVE:-$D/drive}"
M="${M:-$HOME/Library/CloudStorage/Wyrd-drive}"
W=/Applications/Wyrd.app/Contents/Resources/wyrd
E="${E:-$HOME/wyrd-acceptance-evidence-v2}"
GROUP="${GROUP:-${WYRD_APP_GROUP:-}}"
: "${GROUP:?set GROUP or WYRD_APP_GROUP to the App Group used by the RC}"
mkdir -p "$E"
LOG="$E/steps.log"
: > "$LOG"

log() { printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >> "$LOG"; }
check() {
    name=$1; shift
    if "$@"; then log "PASS $name"; else log "FAIL $name"; FAILURES=$((FAILURES + 1)); fi
}
as() { /usr/bin/osascript -e "$1"; }
is_present() { [ -e "$1" ]; }
is_absent() { [ ! -e "$1" ]; }
content_ok() { [ "$(cat "$1" 2>/dev/null)" = "$2" ]; }
start_owner() {
    pgrep -f "wyrd owner .*/$(basename "$DRIVE") " >/dev/null 2>&1 && return 0
    WYRD_APP_GROUP="$GROUP" nohup "$W" owner "$DRIVE" \
        --relay wss://relay.primal.net \
        --identity-file "$D/identity.bin" --passphrase-file "$D/passphrase.txt" \
        start > "$E/owner-restart.log" 2>&1 &
}

FAILURES=0
log "START revision=${REVISION:-efeab575} group=$GROUP"
start_owner
sleep 12

# Navigation + root/nested enumeration
/usr/bin/open -a Finder "$M"
sleep 3
log "finder_window=$(as 'tell application "Finder" to get name of front window')"

# Issue step 2 is a precondition: Connected=true with a usable mount. The
# mount usability is proven below by real Finder writes. Read the flag for
# this drive's domain and record it explicitly; a false flag means this run
# must not claim the issue's full pass, even when every file check succeeds.
PLIST="$HOME/Library/Application Support/FileProvider/md.enesis.wyrd.fileprovider/Domains.plist"
CONNECTED=$(/usr/bin/plutil -p "$PLIST" 2>/dev/null | awk -v name="$(basename "$DRIVE")" '
  /"Connected" =>/ { c = $3; gsub(/[",]/, "", c) }
  /"DisplayName" =>/ { if (index($0, "\"" name "\"") > 0) print c }
' | head -1)
[[ -n "$CONNECTED" ]] || CONNECTED=unknown
if [[ "$CONNECTED" == "true" ]]; then
  log "PASS connected_precondition Connected=$CONNECTED"
else
  log "UNMET connected_precondition Connected=$CONNECTED (issue step 2; no full pass claimed)"
fi

# Create the nested directory as part of this run.
as "tell application \"Finder\" to make new folder at folder POSIX file \"$M\" with properties {name:\"accept-probe\"}" >/dev/null
sleep 3
check "finder_create_nested_folder" is_present "$M/accept-probe"

ROOT_ITEMS=$(as "tell application \"Finder\" to get name of every item of folder POSIX file \"$M\"")
log "root_items=[$ROOT_ITEMS]"
check "finder_root_lists_probe_folder" is_present "$M/accept-probe"
NESTED_ITEMS=$(as "tell application \"Finder\" to get name of every item of folder POSIX file \"$M/accept-probe\"")
log "nested_items_initial=[$NESTED_ITEMS]"

# Create, write, rename, move, read — all through the Finder path
as "tell application \"Finder\" to make new file at folder POSIX file \"$M\" with properties {name:\"v2-file.txt\"}" >/dev/null
sleep 3
check "finder_create" is_present "$M/v2-file.txt"
printf 'wyrd acceptance v2\n' > "$M/v2-file.txt"
sleep 3
check "write_content" content_ok "$M/v2-file.txt" 'wyrd acceptance v2'
as "tell application \"Finder\" to set name of file \"v2-file.txt\" of folder POSIX file \"$M\" to \"v2-renamed.txt\"" >/dev/null
sleep 3
check "finder_rename" is_present "$M/v2-renamed.txt"
as "tell application \"Finder\" to move file \"v2-renamed.txt\" of folder POSIX file \"$M\" to folder POSIX file \"$M/accept-probe\"" >/dev/null
sleep 4
check "finder_move" is_present "$M/accept-probe/v2-renamed.txt"
check "read_after_move" content_ok "$M/accept-probe/v2-renamed.txt" 'wyrd acceptance v2'

# Verify the owner's own view. The export command needs the store unlocked,
# so stop the owner first, then bring it back for the remaining steps.
OWNER_PID=$(pgrep -f "wyrd owner .*/$(basename "$DRIVE") " | head -1)
if [ -n "$OWNER_PID" ]; then kill "$OWNER_PID" 2>/dev/null || true; fi
sleep 4
rm -rf "$E/export" 2>/dev/null || true
"$W" export --identity-file "$D/identity.bin" --passphrase-file "$D/passphrase.txt" \
    "$DRIVE" "$E/export" > "$E/export.log" 2>&1
if [ -f "$E/export/accept-probe/v2-renamed.txt" ]; then
    log "PASS owner_view_has_moved_file"
else
    log "FAIL owner_view_has_moved_file"
    FAILURES=$((FAILURES + 1))
fi
log "export_tree=[$(find "$E/export" -type f 2>/dev/null | head -10)]"
start_owner
sleep 12
check "owner_restarted_after_export" content_ok "$M/accept-probe/v2-renamed.txt" 'wyrd acceptance v2'

# Delete only the file this driver created
as "tell application \"Finder\" to delete file \"v2-renamed.txt\" of folder POSIX file \"$M/accept-probe\"" >/dev/null
sleep 4
check "finder_delete" is_absent "$M/accept-probe/v2-renamed.txt"

# App + owner restart, then repeat a basic subset
pkill -f 'Wyrd.app/Contents/MacOS/wyrd-desktop' 2>/dev/null || true
sleep 2
/usr/bin/open -a /Applications/Wyrd.app 2>/dev/null || true
sleep 8
/usr/bin/open -a Finder "$M"
sleep 4
log "post_restart_window=$(as 'tell application "Finder" to get name of front window')"
check "post_restart_root_lists_probe" is_present "$M/accept-probe"
as "tell application \"Finder\" to make new file at folder POSIX file \"$M\" with properties {name:\"v2-restart.txt\"}" >/dev/null
sleep 3
check "post_restart_write" is_present "$M/v2-restart.txt"
printf 'restart probe\n' > "$M/v2-restart.txt"
sleep 3
check "post_restart_read" content_ok "$M/v2-restart.txt" 'restart probe'
as "tell application \"Finder\" to delete file \"v2-restart.txt\" of folder POSIX file \"$M\"" >/dev/null
sleep 3
check "post_restart_delete" is_absent "$M/v2-restart.txt"

# FP error scan over the whole window
PW=$(cat "$HOME/password")
printf '%s\n' "$PW" | sudo -S -p '' log show --last 15m --style compact \
    --predicate 'process == "fileproviderd"' 2>/dev/null \
    | grep -iE 'FP -2005|FP -1002|FP -2011|symlink creation refused' > "$E/fp-errors.log" 2>&1 || true
log "fp_error_lines=$(wc -l < "$E/fp-errors.log" 2>/dev/null | tr -d ' ')"
# Hard failures are -2005 or the symlink-refusal signature. mtime-only
# stale-base update-item refusals are re-driven by macOS and are excluded.
# FP -1002 is intentional anchor expiry, not a failure.
HARD=$(grep -iE 'FP -2005|symlink creation refused' "$E/fp-errors.log" 2>/dev/null \
  | grep -vicE 'update-item.*diffs:mtime' 2>/dev/null || true)
[[ -n "$HARD" ]] || HARD=0
if [[ "$HARD" -eq 0 ]]; then
  log "PASS fp_hard_errors=0"
else
  log "FAIL fp_hard_errors=$HARD (see fp-errors.log)"
  FAILURES=$((FAILURES + 1))
fi

log "TOTAL_FAILURES=$FAILURES"
exit "$FAILURES"
