#!/usr/bin/env sh
# Tests scripts/release-check.sh. Run: scripts/release-check.test.sh
set -eu

script=$(cd "$(dirname "$0")" && pwd)/release-check.sh
failures=0
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The user's Git config (signing, hooks, default branch) must not change the
# outcome.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.com
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.com

commit() { git -C "$1" commit --quiet --allow-empty -m "$2"; }

git init --quiet --bare -b main "$work/origin.git"
git clone --quiet "$work/origin.git" "$work/seed" 2> /dev/null
commit "$work/seed" "first"
git -C "$work/seed" push --quiet origin main

# check <name> <expected: pass|fail> <clone>
check() {
    name=$1 expected=$2 dir=$3
    # A fail counts only when the script stopped for this reason, not because
    # it could not run.
    if (cd "$dir" && DRY_RUN=false "$script" > /dev/null 2> "$work/stderr"); then
        actual=pass
    elif grep -q "is not origin/main" "$work/stderr"; then
        actual=fail
    else
        actual="error: $(cat "$work/stderr")"
    fi
    if [ "$actual" = "$expected" ]; then
        echo "ok   $name"
    else
        echo "FAIL $name: expected $expected, got $actual"
        failures=$((failures + 1))
    fi
}

git clone --quiet "$work/origin.git" "$work/current"
check "a main equal to origin/main may be released" pass "$work/current"

git clone --quiet "$work/origin.git" "$work/stale"
commit "$work/seed" "merged elsewhere"
git -C "$work/seed" push --quiet origin main
check "a main behind origin/main is stopped, even before fetching" \
    fail "$work/stale"

git clone --quiet "$work/origin.git" "$work/ahead"
commit "$work/ahead" "not pushed"
check "a main with unpushed commits is stopped" fail "$work/ahead"

[ "$failures" -eq 0 ]
