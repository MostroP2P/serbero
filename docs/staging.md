# Staging

Serbero's staging environment is an [Ortsom](https://github.com/MostroP2P/ortsom)
regtest stack: a real `mostrod` built from Mostro's `main`, a local relay,
bitcoind and LND nodes with channels, and two trading clients (Alice and
Bob). Ortsom's dispute scenarios open real disputes, and its stack registers
a `write` solver, which plays the human who takes over from Serbero.

## Set up from scratch

Requirements: Docker, git, and the latest stable Rust toolchain.

```sh
git clone https://github.com/MostroP2P/ortsom.git
cd ortsom
cargo build --release
./target/release/ortsom stack up    # first run compiles mostrod; later runs take ~2 min
./target/release/ortsom doctor      # every line must read PASS
```

`stack up` mints the daemon's identity and a `write` solver, and writes them
to `ortsom.regtest.local.toml` and `ci/regtest/config/settings.toml`. The
stack's relay is `ws://127.0.0.1:7000` and stores `kind 14` events, as the
Mostro chat requires (`docs/spec.md` §5.1). `doctor` confirms protocol v2.

Only one regtest stack runs per machine. If another checkout already runs
one, `stack down` it there first, or use that one.

## Run

From the Serbero checkout, with Ortsom next to it:

```sh
scripts/staging-ortsom.sh            # or: scripts/staging-ortsom.sh /path/to/ortsom
```

The script reads the stack's identities from Ortsom's files (never printing
them) and runs `tests/staging_ortsom.rs`, which the default `cargo test`
skips. It runs Ortsom's `dispute_by_buyer` by default. To also exercise
party replies, run Ortsom's `dispute_answers_external_solver`, which Ortsom
skips unless its local config declares how long to wait for an external
solver:

```sh
# in the Ortsom checkout that will run the scenario, gitignored:
printf '\n[daemon_limits]\nexternal_solver_wait_secs = 60\n' >> ortsom.regtest.local.toml

SERBERO_STAGING_SCENARIO=dispute_answers_external_solver scripts/staging-ortsom.sh
# ORTSOM_RUN_DIR=/path/to/another/ortsom/worktree runs the scenario from there
```

In that scenario the seller opens the dispute, and both parties answer each
message from the external solver with `ortsom-ack: <received text>`.

## What it checks

1. A fresh Serbero identity is registered on the daemon as a `read` solver.
2. `ortsom run dispute_by_buyer` opens a real dispute; the Ortsom scenario
   must pass.
3. Serbero detects the dispute and notifies the solver.
4. Serbero takes the dispute and receives `SolverDisputeInfo` (buyer and
   seller trade keys, amount, payment method).
5. Serbero reads the order's `f` and `published_at`.
6. Serbero writes to the buyer and the seller on the dispute chat.
7. With `dispute_answers_external_solver`: both parties' replies arrive,
   pass the protocol's inbound validation, and are stored.
8. Ortsom's teardown has its `write` solver take the dispute over and cancel
   it: Serbero supersedes its session and records the resolution.

## Checklist (T2.8)

| Step | Result |
|---|---|
| Take | Verified by the staging test |
| Send | Verified by the staging test (the relay accepts the message) |
| Receive | Verified with Ortsom's `dispute_answers_external_solver`: both parties' replies received, validated and stored |
| Human takeover | Verified by the staging test |

First run: 2026-09-27, `mostrod` 0.18.8 (`main` @ `1e793aa`), all checks pass,
with both `dispute_by_buyer` and `dispute_answers_external_solver`.
