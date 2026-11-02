#!/usr/bin/env bash
# Link-check every local Markdown link in the repo's prose docs.
# Fails on links whose target file does not exist, or whose #anchor
# matches no heading in the target file.
#
# Run from the checkout: .ngit/scripts/tests/test-doc-links.sh
# Exits nonzero on any failure. Bash plus git/grep/sed, no network.
set -uo pipefail

fail=0

# Heading text -> GitHub-style anchor slug: lowercase, drop punctuation
# except hyphens/underscores (dots too, matching GitHub), spaces to hyphens.
slug() {
  printf '%s\n' "$1" \
    | tr '[:upper:]' '[:lower:]' \
    | sed -e 's/[[:space:]]/-/g' -e 's/[^a-z0-9_-]//g'
}

check_file() {
  local src="$1" srcdir anchor full line link rest
  srcdir="$(dirname "$src")"
  while IFS= read -r line; do
    # Extract ](target) occurrences one at a time, cutting past the
    # match literally: $link may contain glob characters (`[`, `*`,
    # `?`), so it must never be used as a pattern — an unmatched
    # strip leaves `line` unchanged and the outer loop spins forever.
    while [[ "$line" =~ \]\(([^\)]+)\) ]]; do
      link="${BASH_REMATCH[1]}"
      rest="${line#*](}"
      rest="${rest:${#link}}"
      line="${rest#\)}"
      case "$link" in
        http://*|https://*|mailto:*|nostr:*|"") continue ;;
      esac
      # Split off any #anchor.
      anchor=""
      case "$link" in
        *"#"*) anchor="${link#*#}"; link="${link%%#*}" ;;
      esac
      # A bare #anchor points at the source file itself.
      if [[ -z "$link" ]]; then
        full="$src"
      else
        # Only .md targets are in scope; anything else (images,
        # directories, source files) is someone else's check.
        case "$link" in *.md) ;; *) continue ;; esac
        full="$(cd "$srcdir" && realpath -m "$link" 2>/dev/null || printf '%s/%s' "$srcdir" "$link")"
      fi
      if [[ ! -f "$full" ]]; then
        echo "BROKEN: $src -> $link (no such file)"
        fail=1
        continue
      fi
      if [[ -n "$anchor" ]]; then
        # Literal match: anchors may contain regex characters.
        if ! grep -E '^#{1,6} ' "$full" \
            | sed -e 's/^#* //' \
            | while IFS= read -r h; do slug "$h"; done \
            | grep -qxF "$anchor"; then
          echo "BROKEN: $src -> $link (no heading #$anchor)"
          fail=1
        fi
      fi
    done
  done < "$src"
}

while IFS= read -r md; do
  # Archived reviews are frozen evidence: their links may point at
  # paths that have since moved. They are exempt by design.
  case "$md" in docs/reviews/archive/*) continue ;; esac
  check_file "$md"
done < <((git ls-files '*.md'; git ls-files --others --exclude-standard '*.md') | grep -v '^vendor/' | sort -u)

if [[ "$fail" -eq 0 ]]; then
  echo "doc links OK"
fi
exit "$fail"
