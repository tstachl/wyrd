#!/usr/bin/env bash
# Link-check every local Markdown link in the repo's prose docs.
# Fails on links whose target file does not exist, or whose #anchor
# matches no heading in the target file.
#
# Run from the checkout: ./ngit/scripts/tests/test-doc-links.sh
# Exits nonzero on any failure. Pure bash, no network.
set -uo pipefail

fail=0

# Heading text -> GitHub-style anchor slug: lowercase, drop punctuation
# except hyphens/underscores, spaces to hyphens.
slug() {
  printf '%s' "$1" \
    | tr '[:upper:]' '[:lower:]' \
    | sed -e 's/[[:space:]]/-/g' -e 's/[^a-z0-9_.-]//g'
}

check_file() {
  local src="$1" srcdir target anchor full line link
  srcdir="$(dirname "$src")"
  while IFS= read -r line; do
    # Extract ](target) occurrences one at a time.
    while [[ "$line" =~ \]\(([^\)]+)\) ]]; do
      link="${BASH_REMATCH[1]}"
      line="${line#*]($link)}"
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
        if ! grep -E '^#{1,6} ' "$full" \
            | sed -e 's/^#* //' \
            | while IFS= read -r h; do slug "$h"; done \
            | grep -qx "$anchor"; then
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
