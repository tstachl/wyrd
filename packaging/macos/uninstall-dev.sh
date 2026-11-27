#!/bin/sh
# Clean uninstall for the macOS developer install.
#
# Reverses the residue install-dev.sh leaves on a host: running processes,
# launch agents, the app bundle and its plugin registration, App Group
# containers, the application support files, and optionally every File
# Provider domain for this provider.
#
# Nothing is deleted in a dry run. Deletion needs --confirm. Drive data
# and credentials need --delete-data as well, because uninstall normally
# leaves the durable store intact (docs/design/product-gap.md).
#
# Domain removal is a supervised, explicit operation. The registrar is
# add-only and never removes a domain: removing one stale domain while
# other domains still hold File Provider state wedges Finder. This script
# uses a separate signed helper (domain-remover) and only with
# --remove-domains, which removes all domains of this provider at once.
#
# Usage:
#   packaging/macos/uninstall-dev.sh --dry-run [--app PATH] [--group ID]
#   packaging/macos/uninstall-dev.sh --confirm [--remove-domains]
#       [--delete-data] [--backup DIR] [--app PATH] [--group ID]
#
# Environment: WYRD_APP_GROUP selects the effective App Group when
# --group is absent; SIGN_IDENTITY selects the remover signature.

set -eu
set -o pipefail 2>/dev/null || true

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$HERE/app-group.sh"
. "$HERE/sign-identity.sh"

APP_ID=md.enesis.wyrd
APPEX_ID=md.enesis.wyrd.fileprovider
AGENTS_DIR="$HOME/Library/LaunchAgents"
CONTAINERS_DIR="$HOME/Library/Group Containers"
SUPPORT_DIR="$HOME/Library/Application Support/Wyrd"
CLOUD_DIR="$HOME/Library/CloudStorage"

DRY_RUN=1
CONFIRM=0
REMOVE_DOMAINS=0
DELETE_DATA=0
BACKUP=""
APP=""
GROUP=""

usage() {
    cat <<'EOF'
Usage: uninstall-dev.sh [--dry-run|--confirm] [options]

  --dry-run          list residue and delete nothing (default)
  --confirm          perform the deletions listed by the dry run
  --remove-domains   remove every domain of this provider (supervised)
  --delete-data      also delete the drive store, keys and credentials
  --backup DIR       copy the support directory there before --delete-data
  --app PATH         app bundle to remove (default: /Applications/Wyrd.app
                     or ~/Applications/Wyrd.app)
  --group ID         effective App Group id (default: wyrd_app_group)
  -h, --help         show this help

Drive data survives an uninstall unless --delete-data is given.
Domain registration survives unless --remove-domains is given. A reset
for a fresh iCloud Drive test needs both flags.
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1; CONFIRM=0 ;;
        --confirm) CONFIRM=1; DRY_RUN=0 ;;
        --remove-domains) REMOVE_DOMAINS=1 ;;
        --delete-data) DELETE_DATA=1 ;;
        --backup) shift; [ "$#" -gt 0 ] || { echo "error: --backup needs a directory." >&2; exit 2; }; BACKUP=$1 ;;
        --app) shift; [ "$#" -gt 0 ] || { echo "error: --app needs a path." >&2; exit 2; }; APP=$1 ;;
        --group) shift; [ "$#" -gt 0 ] || { echo "error: --group needs an id." >&2; exit 2; }; GROUP=$1 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "error: unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

if [ -n "$GROUP" ]; then
    case "$GROUP" in
        *..*)
            echo "error: --group must not contain parent traversal." >&2
            exit 2
            ;;
    esac
    WYRD_APP_GROUP=$GROUP
    export WYRD_APP_GROUP
fi
EFFECTIVE_GROUP=$(wyrd_app_group)

# Resolve the bundle before domain removal: the helper must be signed by
# the same team as this app, and the app path is needed for that check.
if [ -z "$APP" ]; then
    for candidate in /Applications/Wyrd.app "$HOME/Applications/Wyrd.app"; do
        if [ -e "$candidate" ]; then APP=$candidate; break; fi
    done
fi

# Refuse to touch paths outside the user's home or /Applications. This
# keeps a bad --app or --group value from turning into a broad deletion.
remove_path() {
    path=$1
    [ -n "$path" ] || return 0
    case "$path" in
        *..*)
            echo "error: refusing path with parent traversal: $path" >&2
            return 1
            ;;
    esac
    # Check the resolved parent, not the typed string: a symlinked
    # directory under HOME can still point outside it.
    parent=$(dirname "$path")
    base=$(basename "$path")
    resolved_parent=""
    if [ -d "$parent" ]; then
        resolved_parent=$(CDPATH= cd -- "$parent" 2>/dev/null && pwd -P) || resolved_parent=""
    fi
    if [ -n "$resolved_parent" ]; then
        resolved="$resolved_parent/$base"
    else
        resolved=$path
    fi
    case "$resolved" in
        "$HOME"/*|/Applications/*) ;;
        *) echo "error: refusing path outside HOME or /Applications: $path" >&2; return 1 ;;
    esac
    if [ ! -e "$path" ] && [ ! -L "$path" ]; then return 0; fi
    if [ "$DRY_RUN" -eq 1 ]; then
        printf 'would remove: %s\n' "$path"
        return 0
    fi
    printf 'remove: %s\n' "$path"
    rm -rf -- "$path"
}

say() {
    if [ "$DRY_RUN" -eq 1 ]; then
        printf 'would %s\n' "$1"
    else
        printf '%s\n' "$1"
    fi
}

if [ "$DRY_RUN" -eq 1 ]; then
    echo "dry run: listing residue; nothing will be deleted"
    echo "app group: $EFFECTIVE_GROUP"
else
    echo "confirm: removing Wyrd residue from this host"
fi

# --- running processes -------------------------------------------------
# Unload the launch agents before stopping processes so KeepAlive cannot
# restart an owner while its files are being removed.
for agent in "$AGENTS_DIR"/md.enesis.wyrd.*.plist; do
    [ -e "$agent" ] || continue
    label=$(basename "$agent" .plist)
    say "unload launch agent $label"
    if [ "$DRY_RUN" -eq 0 ]; then
        launchctl unload -w "$agent" 2>/dev/null || true
        launchctl bootout "gui/$(id -u)" "$agent" 2>/dev/null || true
    fi
done

stop_running() {
    if [ "$DRY_RUN" -eq 0 ] && command -v osascript >/dev/null 2>&1; then
        osascript -e "tell application id \"$APP_ID\" to quit" >/dev/null 2>&1 || true
    fi
    pids=""
    for pattern in '/wyrd-supervise ' '/wyrd owner ' 'Wyrd\.app/Contents/|WyrdDomainRegistrar|WyrdFileProvider\.appex'; do
        matched=$(pgrep -f "$pattern" 2>/dev/null | tr '\n' ' ') || matched=""
        for pid in $matched; do
            [ "$pid" = "$$" ] && continue
            case " $pids " in *" $pid "*) continue ;; esac
            pids="$pids $pid"
            desc=$(ps -p "$pid" -o command= 2>/dev/null | cut -c1-100) || desc=""
            if [ "$DRY_RUN" -eq 1 ]; then
                printf 'would stop pid %s: %s\n' "$pid" "$desc"
                continue
            fi
            echo "  stopping pid $pid: $desc"
            kill -TERM "$pid" 2>/dev/null || true
        done
    done
    if [ "$DRY_RUN" -eq 1 ]; then return 0; fi

    # Only the recorded PIDs are signalled, so a PID recycled after
    # discovery is never caught by the signal loop.
    tick=0
    while [ "$tick" -lt 10 ]; do
        alive=""
        for pid in $pids; do
            kill -0 "$pid" 2>/dev/null && alive=1
        done
        [ -z "$alive" ] && break
        sleep 0.2
        tick=$((tick + 1))
    done
    survivors=""
    for pid in $pids; do
        kill -0 "$pid" 2>/dev/null && survivors="$survivors $pid"
    done
    if [ -n "$survivors" ]; then
        echo "warning: process(es) still running after SIGTERM:$survivors" >&2
    fi
}
stop_running

# --- launch agent files ------------------------------------------------
for agent in "$AGENTS_DIR"/md.enesis.wyrd.*.plist; do
    [ -e "$agent" ] || continue
    remove_path "$agent"
done

# --- File Provider domains ---------------------------------------------
if [ "$REMOVE_DOMAINS" -eq 1 ] && [ -z "$APP" ] && [ "$DRY_RUN" -eq 0 ]; then
    echo "error: no app bundle found; pass --app PATH so the signing team can be checked." >&2
    exit 1
fi
# This is deliberately opt-in. The helper refuses to remove fewer than
# every domain of the provider, so a stale subset can never wedge Finder.
if [ "$REMOVE_DOMAINS" -eq 1 ]; then
    say "remove every File Provider domain for $APPEX_ID"
    if [ "$DRY_RUN" -eq 0 ]; then
        OUT=$(mktemp -d "${TMPDIR:-/tmp}/wyrd-domain-remover.XXXXXX")
        trap 'rm -rf -- "$OUT"' EXIT INT TERM
        "$HERE/domain-remover/build-remover.sh" "$OUT" >/dev/null
        # File Provider domains belong to a signed app identity. A helper
        # from a different team sees no domains and would report a false
        # success. Require a real, equal team on both sides: an ad-hoc
        # app ("not set") cannot own domains this helper can reach.
        APP_TEAM=$(codesign -dv "$APP" 2>&1 | sed -n 's/^TeamIdentifier=//p')
        HELPER_TEAM=$(codesign -dv "$OUT/WyrdDomainRemover" 2>&1 | sed -n 's/^TeamIdentifier=//p')
        if [ -z "$APP_TEAM" ] || [ "$APP_TEAM" = "not set" ] \
           || [ "$APP_TEAM" != "$HELPER_TEAM" ]; then
            echo "error: app team ($APP_TEAM) does not match helper team ($HELPER_TEAM)." >&2
            echo "the app must be team-signed and SIGN_IDENTITY must name that team's identity." >&2
            exit 1
        fi
        "$OUT/WyrdDomainRemover" --all
    fi
else
    echo "note: domains left in place; pass --remove-domains for a full reset"
fi

# --- plugin registration -----------------------------------------------
# Remove only registrations that point into a Wyrd.app bundle. A
# registration from another build is left for the developer, as the
# installer's shadowing warning already documents.
plugins=$(/usr/bin/pluginkit -mAv -i "$APPEX_ID" 2>/dev/null \
    | wyrd_shadowed_plugins /nonexistent/installed.appex \
    | grep -E '/Wyrd\.app/Contents/PlugIns/WyrdFileProvider\.appex$' || true)
for plugin in $plugins; do
    say "unregister plugin at $plugin"
    if [ "$DRY_RUN" -eq 0 ]; then
        /usr/bin/pluginkit -r "$plugin" 2>/dev/null || true
    fi
done

# --- app bundle and interrupted-swap leftovers -------------------------
if [ -n "$APP" ]; then
    remove_path "$APP"
    for leftover in "$APP".new.* "$APP".old.*; do
        [ -e "$leftover" ] || continue
        remove_path "$leftover"
    done
else
    echo "note: no Wyrd app bundle found"
fi

# --- App Group containers and socket aliases ---------------------------
remove_path "$CONTAINERS_DIR/$EFFECTIVE_GROUP"
for container in "$CONTAINERS_DIR/$WYRD_APP_GROUP_SUFFIX" "$CONTAINERS_DIR"/*."$WYRD_APP_GROUP_SUFFIX"; do
    [ -e "$container" ] || continue
    [ "$container" = "$CONTAINERS_DIR/$EFFECTIVE_GROUP" ] && continue
    remove_path "$container"
done

# --- application support -----------------------------------------------
if [ -e "$SUPPORT_DIR" ]; then
    if [ "$DELETE_DATA" -eq 1 ]; then
        if [ -n "$BACKUP" ]; then
            backup_target="$BACKUP/Wyrd-support-$(date +%Y%m%d%H%M%S)"
            say "back up $SUPPORT_DIR to $backup_target"
            if [ "$DRY_RUN" -eq 0 ]; then
                mkdir -p "$BACKUP"
                ditto "$SUPPORT_DIR" "$backup_target"
            fi
        fi
        remove_path "$SUPPORT_DIR"
    else
        for name in drives.json drives.lock settings.json settings.lock; do
            remove_path "$SUPPORT_DIR/$name"
        done
        for stale in "$SUPPORT_DIR"/settings.json.bak-*; do
            [ -e "$stale" ] || continue
            remove_path "$stale"
        done
        echo "note: drive store and keys kept at $SUPPORT_DIR"
    fi
fi

# --- diagnostics exports -----------------------------------------------
for export in "${TMPDIR:-/tmp}"/wyrd-diagnostics-*; do
    [ -e "$export" ] || continue
    say "remove temporary diagnostics export $export"
    if [ "$DRY_RUN" -eq 0 ]; then
        rm -f -- "$export"
    fi
done

# --- Finder CloudStorage mounts ----------------------------------------
# Domain removal normally unmounts these. Only empty directories are
# removed here; a non-empty mount is reported, never deleted blindly.
MOUNTS_LEFT=""
for mount in "$CLOUD_DIR"/Wyrd-*; do
    [ -d "$mount" ] || [ -L "$mount" ] || continue
    if [ "$DRY_RUN" -eq 1 ]; then
        printf 'would inspect mount: %s\n' "$mount"
    elif rmdir "$mount" 2>/dev/null; then
        printf 'removed empty mount: %s\n' "$mount"
    else
        printf 'note: mount not empty, left in place: %s\n' "$mount"
        MOUNTS_LEFT="$MOUNTS_LEFT $mount"
    fi
done

if [ "$DRY_RUN" -eq 1 ]; then
    echo "dry run complete: nothing was deleted"
else
    echo "uninstall complete"
    if [ "$REMOVE_DOMAINS" -eq 0 ]; then
        echo "warning: domains remain registered; this is not a full reset"
    fi
    if [ "$DELETE_DATA" -eq 0 ]; then
        echo "warning: drive data and credentials remain on this host"
    fi
    if [ -n "$MOUNTS_LEFT" ]; then
        echo "warning: Finder mounts remain:$MOUNTS_LEFT (inspect before reinstall)"
    fi
fi
