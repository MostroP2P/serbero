# Serbero

Serbero is an assistant for [Mostro](https://mostro.network) disputes. It
makes sure every dispute reaches your solvers, and it can talk to both parties
of a dispute, find out what happened with the payment, and help them close the
trade themselves. When a case needs a person, it hands it to a solver with a
clear summary.

**Serbero never moves funds and never decides a dispute.** It is registered on
Mostro with read-only permission, so Mostro rejects any fund action it could
send. The parties act from their own Mostro apps, and a human solver always
has the final word.

> **Status.** The notifier is released (v0.1.0). Assisted mediation is
> complete in `main` but ships **off**: it turns on only after its judge has
> been calibrated on labeled conversations, which is the remaining work
> ([`docs/plan.md`](docs/plan.md), tasks T3.8 to T3.10).

## Contents

- [Why Serbero](#why-serbero)
- [How it works](#how-it-works)
- [What the parties see](#what-the-parties-see)
- [What solvers see](#what-solvers-see)
- [Guarantees](#guarantees)
- [Languages](#languages)
- [Install](#install)
- [Configure the notifier](#configure-the-notifier)
- [Enable mediation](#enable-mediation)
- [Run](#run)
- [Inspect](#inspect)
- [Monitor mediation](#monitor-mediation)
- [Documentation](#documentation)
- [Development](#development)

## Why Serbero

Many Mostro disputes do not need a solver at all. The fiat arrived late, the
buyer has not paid yet, or the two parties simply stopped talking. Once the
payment facts are on the table, the parties can usually finish the trade
themselves with the two actions Mostro already allows during a dispute:

- the **seller** can release the bitcoin, or
- **both** parties can agree to a cooperative cancellation.

Serbero gets disputes to that point, and brings in a person only when that is
really needed. That leaves solvers the cases that need them.

## How it works

Serbero does two jobs. The first is always on; the second is optional.

```text
             A dispute is opened on your Mostro node
                              │
                              ▼
   1. NOTIFY ── every solver gets a DM within seconds,
                with reminders until someone takes the dispute
                              │
                              ▼  (mediation enabled)
   2. MEDIATE ─ Serbero takes the dispute as a read-only solver
                and asks each party short questions in their language
                              │
            ┌─────────────────┴───────────────────┐
            ▼                                     ▼
   The facts point to a clear path        Anything else: contested claims,
   ─────────────────────────────          fraud signals, someone asks for a
   • the seller says the fiat arrived:    person, no answers, unclear facts
     the seller may release               ───────────────────────────────────
   • the buyer says they did not pay:     Serbero hands the case to a human
     both may cancel cooperatively        solver with a brief and the full
            │                             conversation
            ▼
   The parties act from their own apps.
   Mostro closes the dispute; Serbero thanks
   them and sends the solvers a final report.
```

Step by step:

1. **Notify.** Serbero watches the dispute events your Mostro node publishes
   and sends every configured solver a direct message for each new dispute. It
   reminds them every `renotify_after` until someone takes it, and tells them
   when it is taken. This works whether or not mediation is enabled.
2. **Take.** With mediation enabled, Serbero takes the new dispute as a
   read-only solver and tells the solvers it is assisting. Any solver with
   write permission can take it over at any moment, and Serbero goes silent
   as soon as one does.
3. **Ask.** Serbero writes to each party on the dispute chat of their Mostro
   app: a short introduction, then one question. It asks the buyer whether
   they sent the payment, and the seller whether it arrived.
4. **Understand.** Each time a party replies, Serbero sends the conversation to
   a *judge*: [Jev](https://docs.typesafe.ai/), a TypeSafe model that answers
   narrow questions ("Does the seller say the payment arrived?", "Is anyone
   asking for a person?") with calibrated probabilities. Jev never writes
   anything and never decides. Serbero's own code decides the next step with a
   fixed decision table, and treats any uncertain answer as unknown.
5. **Guide or hand off.** When the facts are clear, Serbero explains the option
   Mostro already gives the parties, and only to the party who would act,
   after that party stated the fact the option depends on. In every other case
   it hands the dispute to a human solver.
6. **Close.** When Mostro reports the dispute resolved, Serbero thanks the
   parties (if they resolved it themselves) and sends the solvers a short
   final report.

Serbero only ever sends human-written message templates to the parties and
verbatim quotes to the solvers. Nothing it sends is generated text.

## What the parties see

Messages arrive in the dispute chat of the party's Mostro app, in the party's
language. A dispute where the seller confirms the payment looks like this:

> **Serbero → buyer:** Hi, I'm Serbero, an automated assistant helping with
> this dispute. I'll ask you and the other party a few short questions. I
> can't move funds or decide the dispute, and a person can take over whenever
> you ask. Messages in this chat may be monitored and processed by an
> automated service.
>
> Did you send the 50,000 ARS payment for this order? If you did, please tell
> me when and from which account or app.

> **Serbero → seller:** *(the same introduction, then)* Has the 50,000 ARS
> payment for this order arrived in your account?

> **Seller:** yes it's there now, the bank was slow

> **Serbero → seller:** Thanks for confirming. If you have checked that the
> 50,000 ARS is in your account, you can complete the trade yourself by
> releasing it from your Mostro app; that also closes this dispute. If
> anything doesn't add up, tell me here and a person will review it.

> **Serbero → buyer:** The seller reports that your payment arrived, so they
> can now complete the trade from their Mostro app. I'll let you know when the
> dispute is closed; if something goes wrong, tell me here.

The seller releases from the app, and both parties receive: *"The dispute is
now closed. Thank you both for resolving it."*

A party can ask for a person at any point. Every message Serbero can send, in
every language, is listed in [`docs/messages.md`](docs/messages.md).

## What solvers see

Solvers receive short direct messages from Serbero's own Nostr identity. They
read them in Mostro clients such as Mostrix and `mostro-cli`. Every message
starts with `Dispute <id> · …` and carries the dispute id, so a client can
file it under its dispute:

| When | Message |
|---|---|
| A dispute is opened | `Dispute <id> · new`, and who opened it |
| Nobody took it yet | `Dispute <id> · unattended (32 min)`, every `renotify_after` |
| Someone took it | `Dispute <id> · taken`, and whether Serbero or a solver took it |
| Serbero starts assisting | That it is mediating, and that any solver can take over |
| Handoff or guidance | A **brief**, followed by the full **transcript** |
| New party messages after a handoff | An **update** with the new messages |
| The dispute is resolved | A one-line **final report** |

A brief tells the solver what each party claims, how sure Serbero is, and the
exact messages that support each claim:

```text
Dispute <dispute_id> · handed off: conflicting_claims
Topic: payment_not_confirmed (0.91) · rounds: 2 · duration: 14 min
Order: 50000 ARS via Mercado Pago · created 3 h 20 min before the dispute
Languages: buyer en · seller en

Buyer — says sent (0.96), details given (0.88)
  "I sent it at 14:10 from my Mercado Pago account, ref 8841…"  [1 attachment]
Seller — says not received (0.93), checked account (0.90)
  "I checked my bank and there is nothing in pesos"

Signals: conflict 0.87 · fraud 0.08 · human requested: no

Reading of the conversation (advisory, not a verdict):
  evidence that fiat was sent: 3.1 / 4

Transcript (18 messages) follows in the next message.
```

Quotes are always the parties' own words, in the language they wrote them.
Screenshots and files are listed as attachments; Serbero cannot read them.

**Taking over.** To take a dispute from Serbero, take it from your Mostro
client as you normally would (it needs `write` permission). Serbero stops
writing to the parties immediately.

**Feedback.** If a brief got something wrong, reply to it with
`wrong <question>`, for example `wrong seller_receipt`. Serbero records it so
the case can become a test case for the next version of its questions.

Handoffs go to the dispute's assigned human solver if there is one, otherwise
to every `write` solver, otherwise to every solver.

## Guarantees

- **No fund actions.** Serbero is registered with `read` permission. It has no
  code that settles, cancels or releases anything, and Mostro would reject it
  anyway.
- **No verdicts.** Serbero never says who is right. It reports claims, never
  facts, and a person can always take over.
- **Guidance follows the actor's own word.** The seller hears about releasing
  only after the seller says the fiat arrived. A buyer saying "I paid" never
  triggers it.
- **Nothing generated reaches a person.** Parties get human-written templates;
  solvers get rendered facts and verbatim quotes.
- **Privacy.** The judge receives the trade roles, the order amount, currency
  and payment method, and the message text. It never receives public keys,
  event ids or other Nostr metadata. Solvers never see a party's primary
  identity. Logs never contain message text. The first message tells each
  party they are talking to an automated assistant.
- **Mostro works without it.** If Serbero is down, Mostro and your solvers work
  exactly as before. If the judge is down, notifications keep working and live
  mediations go to a person.

## Languages

Serbero talks to each party in that party's own language, and the buyer and
seller may get different ones. English is the default. As soon as a party
writes in another enabled language, Serbero switches to it for that party.

English, Spanish and Portuguese ship today, each as one file under
[`messages/`](messages/), reviewed by a native speaker. A language has two
levels of support:

- **Conversational:** Serbero asks its questions in it, spots requests for a
  person and fraud signals, and hands off with a brief.
- **Validated:** its test set passed for the active judge, so Serbero can also
  guide the parties to release or cancel. Until then, a case that would have
  been guided goes to a person instead.

Adding a language is a new catalog file plus a line of config, never a code
change ([`docs/spec.md` §7.7](docs/spec.md#77-languages)).

## Install

From the next tagged release on, each release publishes prebuilt binaries for
Linux (x86_64 and aarch64) and macOS (Apple silicon) on the
[releases page](https://github.com/MostroP2P/serbero/releases), each with a
SHA-256 checksum and the sample config:

```sh
shasum -a 256 -c serbero-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz.sha256
tar -xzf serbero-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz
cd serbero-vX.Y.Z-x86_64-unknown-linux-gnu    # ./serbero and config.sample.toml
```

To build from source, you need the latest stable Rust toolchain:

```sh
git clone https://github.com/MostroP2P/serbero.git
cd serbero
cargo build --release          # the binary is target/release/serbero
```

The binary is `./serbero` in a release, and `./target/release/serbero` in a
source build.

## Configure the notifier

1. Copy the sample and edit it:

   ```sh
   cp config.sample.toml config.toml
   ```

   At minimum, set:

   - `[mostro].pubkey`: the hex pubkey of your Mostro node;
   - `[mostro].relays`: the relays your node publishes to;
   - one `[[solvers]]` entry per solver to notify, with their hex pubkey and
     `read` or `write` permission.

   Optionally, add an `[[observers]]` entry with the hex pubkey of a service
   such as mostro-watchdog, to post mediation progress to your team chat. It
   gets only the first line of each mediation update (for example
   `Dispute <id> · handed off: conflicting_claims`), never what the parties
   wrote ([`docs/messages.md` §3](docs/messages.md#observers)).

   Every field is described in [`docs/spec.md` §9](docs/spec.md#9-configuration).
   Unknown or misspelled fields are rejected at startup.

2. Give Serbero its own Nostr identity. The private key never goes in the
   file; export it as a 64-character hex key:

   ```sh
   export SERBERO_PRIVATE_KEY=<hex private key>
   ```

   Solvers receive DMs from this identity. Serbero logs its public key, in hex
   and as an npub, when it starts.

That is all the notifier needs. Mediation stays off until you enable it.

## Enable mediation

Mediation is opt-in and needs a few things in place first.

1. **A supported Mostro node.** The node must speak Mostro protocol v2: the
   first `mostrod` release after v0.18.8, or later. Serbero reads the node's
   info at startup and keeps mediation off, with a `mediation off` log line,
   on a node that does not advertise v2. If the relays do not return that
   info in time, it checks again before taking each dispute.
2. **Serbero registered as a read solver.** Your Mostro admin adds Serbero's
   npub (from its startup log) as a solver with `read` permission, with the
   `admin-add-solver` action and the payload `<npub>:read`. Keep at least one
   human solver with `write` permission, so someone can take over.
3. **Relays that store chat messages.** Party messages are `kind 14` events.
   Use relays verified to store them; otherwise, messages sent while Serbero is
   offline are lost. List more than one: public relays often limit events per
   IP, and a Serbero running on the same host as `mostrod` shares that limit
   with it.
4. **Your own judge account.** Create a [TypeSafe](https://typesafe.ai)
   account and export its key:

   ```sh
   export TYPESAFE_API_KEY=<your key>
   ```

   Each operator uses and pays for its own account. A turn costs a few
   thousand input tokens. For real disputes, ask TypeSafe for zero data
   retention.
5. **Calibrated thresholds.** Serbero acts on the judge's answers only with
   thresholds measured for that exact judge and model, in
   `[judge.thresholds."typesafe/jev-<version>"]`. Without them, mediation stays
   off. The values come from a committed calibration report
   ([`docs/evaluation.md`](docs/evaluation.md)); they are not published yet.
6. **Turn it on** in `config.toml`:

   ```toml
   [mediation]
   enabled = true
   default_language = "en"          # language of the first message
   languages = ["en", "es", "pt"]   # languages Serbero may switch to
   ```

   The other `[mediation]` settings have sensible defaults:

   | Setting | Default | Meaning |
   |---|---|---|
   | `quiet_period` | `20s` | Wait after a party's last message, so a burst becomes one turn |
   | `response_timeout` | `30m` | Silence before a reminder, and again before handing off |
   | `max_rounds` | `4` | Rounds of questions per party before handing off |
   | `self_resolution_timeout` | `2h` | Time for the parties to act after being guided |
   | `max_messages_per_turn` | `10` | Sending more than this twice hands off as flooding |
   | `max_message_chars` | `2000` | Longer messages are cut |

At startup, Serbero checks the judge in the background and logs either
`mediation ready` or `mediation off` with the reason. Notification never waits
for it: if the judge check fails, Serbero keeps notifying as usual.

## Run

```sh
./serbero                                         # reads ./config.toml
SERBERO_CONFIG=/etc/serbero/config.toml ./serbero
SERBERO_LOG=serbero=debug ./serbero               # more verbose logs
```

From a source build, use `./target/release/serbero` instead of `./serbero`.

Stop it with Ctrl-C or SIGTERM (as systemd and Docker do); relay connections
are closed cleanly.

On start, Serbero listens for new disputes at once and, in the background,
fetches the disputes your relays already store, keeping only the newest
revision of each. A slow relay never delays a new dispute. The backlog is
synced again every 10 minutes and whenever a relay reconnects. A dispute
Serbero already knows is never announced as new again. A dispute that was opened and then taken or
resolved while Serbero was offline is recorded without notifying anyone. Live
mediation sessions resume where they were.

## Inspect

All state lives in the SQLite file at `[serbero].db_path` (default
`serbero.db`).

```sh
# Disputes and where they are in the lifecycle
sqlite3 serbero.db "SELECT dispute_id, status, lifecycle,
  datetime(first_seen_at, 'unixepoch') FROM disputes ORDER BY first_seen_at DESC LIMIT 20;"

# Disputes still waiting for a solver
sqlite3 serbero.db "SELECT dispute_id, lifecycle, datetime(last_notified_at, 'unixepoch')
  FROM disputes WHERE lifecycle IN ('new', 'notified');"

# Mediation sessions: state, why they were handed off, rounds used
sqlite3 serbero.db "SELECT dispute_id, state, handoff_reason, rounds,
  datetime(opened_at, 'unixepoch') FROM sessions ORDER BY opened_at DESC LIMIT 20;"

# Everything that happened to one dispute
sqlite3 serbero.db "SELECT datetime(created_at, 'unixepoch'), kind, payload_json
  FROM events WHERE dispute_id = '<dispute id>' ORDER BY id;"

# Failed notifications in the last day
sqlite3 serbero.db "SELECT dispute_id, json_extract(payload_json, '$.solver'),
  json_extract(payload_json, '$.error') FROM events
  WHERE kind = 'notification_failed' AND created_at > unixepoch() - 86400;"
```

## Monitor mediation

Two read-only reports summarize mediation
([`docs/evaluation.md` §5](docs/evaluation.md#5-production-monitoring)):

```sh
# The last 7 days: sessions, disputes the parties resolved themselves,
# handoffs by reason, solver feedback, judge requests, latency and tokens
sqlite3 -header -column serbero.db < scripts/weekly-report.sql

# Input tokens and cost per day and judge, for the last 30 days
sqlite3 -header -column serbero.db \
  -cmd ".parameter set :usd_per_million_tokens 0.25" \
  < scripts/cost-report.sql
```

Watch the share of `uncertain` and `round_limit` handoffs and the solver
feedback. When one question keeps getting them, it is time to add test cases
for it and revise it in the next version of the question set.

## Documentation

| Document | What it covers |
|---|---|
| [`docs/spec.md`](docs/spec.md) | The full design: principles, architecture, lifecycle, mediation, data model, configuration, failure handling, privacy |
| [`docs/judgments.md`](docs/judgments.md) | Every question asked to the judge, how answers become facts, and the decision table |
| [`docs/messages.md`](docs/messages.md) | Every message sent to parties (in each language) and to solvers |
| [`docs/evaluation.md`](docs/evaluation.md) | How the questions are tested and the thresholds calibrated |
| [`docs/staging.md`](docs/staging.md) | Running Serbero against a real test Mostro |
| [`docs/plan.md`](docs/plan.md) | The implementation plan and what is left |
| [`AGENTS.md`](AGENTS.md) | Rules for contributors, human or AI |

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests never call a live judge. Changing any judge question needs a new
question-set version and a new evaluation run
([`AGENTS.md`](AGENTS.md#working-with-the-judge-jev)).

### Releasing

1. Move the `[Unreleased]` notes in [`CHANGELOG.md`](CHANGELOG.md) under a
   new `## [X.Y.Z] - YYYY-MM-DD` heading and set `version = "X.Y.Z"` in
   `Cargo.toml`, in one PR.
2. After it merges, tag the merge commit and push the tag:

   ```sh
   git tag -a vX.Y.Z -m "Serbero vX.Y.Z" && git push origin vX.Y.Z
   ```

The release workflow checks that the tag matches `Cargo.toml` and that the
changelog has notes for it, runs the tests, builds every platform, and
publishes the release with that section as its notes.

## License

[MIT](LICENSE)
