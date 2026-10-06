#!/usr/bin/env sh
# cargo-release pre-release hook (release.toml). Stops a release unless the
# checkout is exactly origin/main. cargo-release only warns when main is
# behind, and a release cut from a stale main tags code that lacks merged
# changes; its push is then rejected, after the version bump was committed.
set -eu

git fetch --quiet origin main
head=$(git rev-parse HEAD)
upstream=$(git rev-parse origin/main)
[ "$head" = "$upstream" ] && exit 0

echo "release stopped: HEAD $head is not origin/main $upstream" >&2
if [ "${DRY_RUN:-true}" = "false" ]; then
    echo "discard the version bump with 'git restore .'," >&2
fi
echo "then run 'git pull --ff-only' and release again" >&2
exit 1
