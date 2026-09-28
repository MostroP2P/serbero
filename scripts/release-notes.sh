#!/usr/bin/env sh
# Prints the CHANGELOG.md section of one release, for its GitHub release
# notes. Fails when the section is missing or empty, so a tag cannot be
# published without notes.
#
#   scripts/release-notes.sh v1.0.0
set -eu

tag=${1:?usage: release-notes.sh <tag>}
version=${tag#v}
changelog=$(dirname "$0")/../CHANGELOG.md

notes=$(awk -v version="$version" '
    /^## \[/ { in_section = index($0, "## [" version "]") == 1; next }
    in_section { print }
' "$changelog" | sed -e '/./,$!d')

if [ -z "$(printf '%s' "$notes" | tr -d '[:space:]')" ]; then
    echo "CHANGELOG.md has no notes for $version" >&2
    exit 1
fi
printf '%s\n' "$notes"
