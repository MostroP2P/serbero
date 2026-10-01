#!/usr/bin/env sh
# Tests scripts/image-aliases.sh. Run: scripts/image-aliases.test.sh
set -eu

script=$(dirname "$0")/image-aliases.sh
failures=0

# check <name> <version> <expected aliases> <tags...>
check() {
    name=$1 version=$2 expected=$3
    shift 3
    actual=$(printf '%s\n' "$@" | "$script" "$version" | tr '\n' ' ' | sed 's/ $//')
    if [ "$actual" = "$expected" ]; then
        echo "ok   $name"
    else
        echo "FAIL $name: expected '$expected', got '$actual'"
        failures=$((failures + 1))
    fi
}

check "newest release gets every alias" \
    1.2.4 "1.2.4 1.2 latest" v1.2.3 v1.2.4
check "older run finishing last keeps only its own version" \
    1.2.3 "1.2.3" v1.2.3 v1.2.4
check "patch of an older line keeps its line alias, not latest" \
    1.1.5 "1.1.5 1.1" v1.1.4 v1.1.5 v1.2.0
check "versions compare numerically, not as text" \
    0.10.0 "0.10.0 0.10 latest" v0.9.0 v0.10.0
check "pre-release tags never win an alias" \
    1.2.4 "1.2.4 1.2 latest" v1.2.4 v1.3.0-rc.1
check "a pre-release gets only its own version" \
    1.3.0-rc.1 "1.3.0-rc.1" v1.2.4 v1.3.0-rc.1
check "non-version tags are ignored" \
    0.2.0 "0.2.0 0.2 latest" v0.1.0 v0.2.0 vnext docs-v9.9.9

if "$script" "not-a-version" < /dev/null > /dev/null 2>&1; then
    echo "FAIL rejects an invalid version"
    failures=$((failures + 1))
else
    echo "ok   rejects an invalid version"
fi

[ "$failures" -eq 0 ]
