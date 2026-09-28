# Serbero

Serbero helps the parties of a [Mostro](https://mostro.network) dispute resolve
it themselves, and brings in a human solver when that is needed. It never moves
funds and never decides a dispute.

> **Status:** v0.1 ships the notifier: every dispute on your Mostro node reaches
> your solvers, with reminders until someone takes it. Assisted mediation is in
> development ([`docs/plan.md`](docs/plan.md)).

- Specification: [`docs/`](docs/README.md)
- Implementation plan: [`docs/plan.md`](docs/plan.md)
- Contributor and agent rules: [`AGENTS.md`](AGENTS.md)

## What the notifier does

- Watches the dispute events (`kind 38386`) your Mostro node publishes. Events
  from any other author are ignored.
- Sends every configured solver a DM for each new dispute. DMs are Mostro
  protocol v2 `send-dm` messages, readable in Mostro clients such as Mostrix
  and `mostro-cli`.
- Reminds solvers every `renotify_after` until the dispute is taken, and tells
  them when it is.
- Records every dispute, status change, and notification attempt in SQLite.

Serbero only reads public dispute events and sends DMs to solvers. Mostro works
exactly the same with or without it.

## Install

Serbero needs the latest stable Rust toolchain.

```sh
git clone https://github.com/MostroP2P/serbero.git
cd serbero
cargo build --release
# the binary is target/release/serbero
```

## Configure

1. Copy the sample and edit it:

   ```sh
   cp config.sample.toml config.toml
   ```

   At minimum, set:

   - `[mostro].pubkey`: the hex pubkey of your Mostro node;
   - `[mostro].relays`: the relays your node publishes to;
   - one `[[solvers]]` entry per solver to notify (hex pubkey and `read` or
     `write` permission).

   Every field is described in [`docs/spec.md` §9](docs/spec.md#9-configuration).
   Unknown or misspelled fields are rejected at startup.

2. Give Serbero its own Nostr identity. Its private key never goes in the file;
   export it as a 64-character hex key:

   ```sh
   export SERBERO_PRIVATE_KEY=<hex private key>
   ```

   Solvers see DMs from this identity's pubkey, which Serbero logs at startup.

## Run

```sh
./target/release/serbero                          # reads ./config.toml
SERBERO_CONFIG=/etc/serbero/config.toml ./target/release/serbero
SERBERO_LOG=serbero=debug ./target/release/serbero  # more verbose logs
```

Stop it with Ctrl-C or SIGTERM (as systemd and Docker do); relay connections
are closed cleanly. On start, Serbero listens for new disputes at once and, in
the background, fetches the disputes your relays already store, applying only
the newest revision of each. A slow relay never delays a new dispute. The
backlog is re-synced every 10 minutes and whenever a relay reconnects.
Disputes it already knows are never notified twice, and a dispute that was
opened and taken or resolved while Serbero was offline is recorded without
notifying anyone.

## Inspect

State lives in the SQLite file at `[serbero].db_path` (default `serbero.db`).

```sh
# Disputes and where they are in the lifecycle
sqlite3 serbero.db "SELECT dispute_id, status, lifecycle,
  datetime(first_seen_at, 'unixepoch') FROM disputes ORDER BY first_seen_at DESC LIMIT 20;"

# Disputes still waiting for a solver
sqlite3 serbero.db "SELECT dispute_id, lifecycle, datetime(last_notified_at, 'unixepoch')
  FROM disputes WHERE lifecycle IN ('new', 'notified');"

# Everything that happened to one dispute
sqlite3 serbero.db "SELECT datetime(created_at, 'unixepoch'), kind, payload_json
  FROM events WHERE dispute_id = '<dispute id>' ORDER BY id;"

# Failed notifications in the last day
sqlite3 serbero.db "SELECT dispute_id, json_extract(payload_json, '$.solver'),
  json_extract(payload_json, '$.error') FROM events
  WHERE kind = 'notification_failed' AND created_at > unixepoch() - 86400;"
```

## Monitor mediation

With mediation enabled, two read-only `sqlite3` reports summarize it
([`docs/evaluation.md` §5](docs/evaluation.md#5-production-monitoring)):

```sh
# the last 7 days: sessions, self-resolved disputes, handoffs by reason,
# solver feedback, judge requests, median latency and input tokens
sqlite3 -header -column serbero.db < scripts/weekly-report.sql

# input tokens and cost per day and judge, for the last 30 days
sqlite3 -header -column serbero.db \
  -cmd ".parameter set :usd_per_million_tokens 0.25" \
  < scripts/cost-report.sql
```

Watch the share of `uncertain` and `round_limit` handoffs and the solver
feedback: a rise for one question is the signal to add golden cases and
revise it in the next question-set version. Solvers give feedback by replying
to a brief with `wrong <question>`, for example `wrong seller_receipt`.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## License

[MIT](LICENSE)
