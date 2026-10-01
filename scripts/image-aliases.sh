#!/usr/bin/env sh
# Prints the container image tags a release may publish, one per line, given
# its version and every git tag of the repository on stdin:
#
#   git ls-remote --tags --refs origin | sed 's|.*refs/tags/||' \
#     | scripts/image-aliases.sh 1.2.3
#
# The version itself is always printed. The moving aliases `X.Y` and `latest`
# are printed only when the version is the newest stable release of its line,
# or of all lines. Release runs are independent, so an older run that finishes
# after a newer one must not move those aliases back to its image; deciding
# from git tags, which exist from the moment they are pushed, prevents that.
set -eu

version=${1:?usage: image-aliases.sh <version> < tags}
stable='^[0-9]+\.[0-9]+\.[0-9]+$'

if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; then
    echo "not a version: $version" >&2
    exit 1
fi
echo "$version"
# A pre-release never moves an alias.
printf '%s\n' "$version" | grep -Eq "$stable" || exit 0

releases=$(sed -n 's/^v//p' | grep -E "$stable" || true)
newest() { printf '%s\n%s\n' "$version" "$1" | grep -v '^$' | sort -V | tail -n 1; }

line=${version%.*}
line_releases=$(printf '%s\n' "$releases" | grep -E "^$(printf '%s' "$line" | sed 's/\./\\./g')\." || true)
[ "$(newest "$line_releases")" = "$version" ] && echo "$line"
[ "$(newest "$releases")" = "$version" ] && echo latest
exit 0
