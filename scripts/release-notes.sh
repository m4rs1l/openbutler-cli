#!/bin/sh
# Writes the release body for one version: the CHANGELOG section for that
# version, exactly as written, and nothing else. A tag whose section is missing
# or empty fails here instead of publishing an empty release.
set -eu

version="${1:-}"
changelog="${2:-CHANGELOG.md}"
out="${3:-notes.md}"

if [ -z "$version" ]; then
  echo "usage: release-notes.sh <version> [changelog] [output]" >&2
  exit 2
fi

if [ ! -f "$changelog" ]; then
  echo "no changelog at $changelog" >&2
  exit 1
fi

# The section ends at the next heading or at the link-reference block that
# closes a Keep a Changelog file; blank lines are held back so the body does
# not end in a run of empty lines.
awk -v version="$version" '
  index($0, "## [" version "]") == 1 { inside = 1; print; next }
  inside && (/^## \[/ || /^\[[^]]*\]: /) { exit }
  inside {
    if ($0 == "") { blank = blank "\n"; next }
    printf "%s", blank
    blank = ""
    print
  }
' "$changelog" > "$out"

if [ ! -s "$out" ]; then
  echo "CHANGELOG has no section for $version; add \"## [$version] - <date>\" before tagging" >&2
  rm -f "$out"
  exit 1
fi

# A heading with no bullets is not a release note.
if ! awk 'NR > 1 && /^- / { found = 1 } END { exit found ? 0 : 1 }' "$out"; then
  echo "the $version section has no entries; a release without notes is not published" >&2
  rm -f "$out"
  exit 1
fi
