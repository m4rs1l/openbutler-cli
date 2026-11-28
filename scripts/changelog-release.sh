#!/bin/sh
# Moves the Unreleased entries under a version heading. Run this when cutting a
# tag: the release body is exactly what this leaves behind, so an empty
# Unreleased section is refused rather than turned into an empty release.
set -eu

version="${1:-}"
date="${2:-$(date -u +%Y-%m-%d)}"

if [ -z "$version" ]; then
  echo "usage: changelog-release.sh <version> [date]" >&2
  exit 2
fi

changelog=CHANGELOG.md
if [ ! -f "$changelog" ]; then
  echo "no changelog at $changelog" >&2
  exit 1
fi

if ! awk '
  /^## \[Unreleased\]$/ { inside = 1; next }
  inside && /^## \[/ { exit }
  inside && /^- / { found = 1 }
  END { exit found ? 0 : 1 }
' "$changelog"; then
  echo "the Unreleased section has no entries; nothing to release" >&2
  exit 1
fi

tmp="$changelog.tmp.$$"
awk -v version="$version" -v date="$date" '
  { print }
  /^## \[Unreleased\]$/ && !inserted {
    print ""
    print "## [" version "] - " date
    inserted = 1
  }
' "$changelog" > "$tmp"

mv "$tmp" "$changelog"
echo "moved the Unreleased entries under \"## [$version] - $date\""
