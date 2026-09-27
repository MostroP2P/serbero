#!/usr/bin/env bash
# Runs Serbero's staging test against an Ortsom regtest stack.
#
#   ortsom stack up            # in the Ortsom checkout, first
#   scripts/staging-ortsom.sh [path/to/ortsom]   # default: ../ortsom
#
# Reads the stack's identities from Ortsom's local files and passes them to
# the test through the environment. Secrets are never printed.
set -euo pipefail

ORTSOM_DIR="$(cd "${1:-$(dirname "$0")/../../ortsom}" && pwd)"
local_toml="$ORTSOM_DIR/ortsom.regtest.local.toml"
settings="$ORTSOM_DIR/ci/regtest/config/settings.toml"
for f in "$local_toml" "$settings"; do
  [[ -r "$f" ]] || { echo "missing $f: run 'ortsom stack up' in $ORTSOM_DIR" >&2; exit 1; }
done

# Value of `key = "..."` inside `[section]` of a TOML file.
toml_value() {
  awk -v section="[$2]" -v key="$3" '
    /^\[/ { in_section = ($0 == section) }
    in_section && $1 == key { sub(/^[^=]*= */, ""); gsub(/["'"'"']/, ""); print; exit }
  ' "$1"
}

export SERBERO_STAGING_RELAY="ws://127.0.0.1:7000"
export SERBERO_STAGING_MOSTRO_PUBKEY="$(toml_value "$local_toml" mostro pubkey)"
export SERBERO_STAGING_SOLVER_NSEC="$(toml_value "$local_toml" solver nsec)"
export SERBERO_STAGING_DAEMON_NSEC="$(toml_value "$settings" nostr nsec_privkey)"
# Where Ortsom runs its scenario: the stack's checkout by default, or another
# worktree of it (ORTSOM_RUN_DIR) that shares the same stack configuration.
RUN_DIR="${ORTSOM_RUN_DIR:-$ORTSOM_DIR}"
export SERBERO_STAGING_ORTSOM_DIR="$RUN_DIR"
export SERBERO_STAGING_ORTSOM_BIN="${ORTSOM_BIN:-$RUN_DIR/target/release/ortsom}"
# dispute_by_buyer (default) or dispute_answers_external_solver, where both
# parties reply to Serbero on the dispute chat.
export SERBERO_STAGING_SCENARIO="${SERBERO_STAGING_SCENARIO:-dispute_by_buyer}"

for v in SERBERO_STAGING_MOSTRO_PUBKEY SERBERO_STAGING_SOLVER_NSEC SERBERO_STAGING_DAEMON_NSEC; do
  [[ -n "${!v}" ]] || { echo "could not read $v from the Ortsom stack files" >&2; exit 1; }
done

cd "$(dirname "$0")/.."
cargo test --test staging_ortsom -- --ignored --nocapture
