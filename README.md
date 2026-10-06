<p align="center">
  <img src="serbero.jpg" alt="Serbero" width="400">
</p>

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

> **Status.** The notifier is released. Assisted mediation ships **off** and
> can be turned on as a **pilot**, which gathers the facts and hands every
> case to a solver ([Enable mediation](#enable-mediation)). Calibrating its
> judge on labeled conversations, so it can also guide the parties, is the
> remaining work ([`docs/plan.md`](docs/plan.md), tasks T3.8 to T3.10).

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
- [Run with Docker](#run-with-docker)
- [Run with systemd](#run-with-systemd)
- [Back up](#back-up)
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

### Read them in Mostrix

Anyone can send a direct message to a solver, so Mostrix shows only messages
from the senders a solver trusts. Mostrix v0.3.5 or later lists them in the
**SERBERO** pane of each dispute in *Disputes in Progress*. In the solver's
Mostrix `settings.toml`:

```toml
user_mode = "admin"
admin_privkey = "nsec1..."   # the solver's key: its pubkey is in Serbero's [[solvers]]
relays = ["wss://relay.mostro.network", "..."]   # at least one of Serbero's [mostro].relays
trusted_dm_senders = ["npub1..."]   # Serbero's npub (or hex), from its startup log
```

Then restart Mostrix: it reads `trusted_dm_senders` only when it starts,
reconnects, or reloads its keys, and then fetches the last 7 days of
Serbero's messages. If a `settings.toml` sits next to the `mostrix` binary, it
is the one Mostrix reads, instead of `~/.mostrix/settings.toml`.

If the pane stays empty while Serbero logs `notification sent`, check that
the pubkey of `admin_privkey` is exactly the `solver` in Serbero's log line:
Serbero writes to that key only, and Mostrix silently skips messages that
are not addressed to its own key.

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

Each release publishes prebuilt binaries for Linux (x86_64 and aarch64) and
macOS (Apple silicon) on the
[releases page](https://github.com/MostroP2P/serbero/releases), each with a
SHA-256 checksum and the sample config:

```sh
shasum -a 256 -c serbero-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz.sha256
tar -xzf serbero-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz
cd serbero-vX.Y.Z-x86_64-unknown-linux-gnu    # ./serbero, config.sample.toml, deploy/
```

With a Rust toolchain, you can also install it from
[crates.io](https://crates.io/crates/serbero). The binary lands in
`~/.cargo/bin/serbero`; take `config.sample.toml` from this repository:

```sh
cargo install serbero --locked
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
   - `[mostro].relays`: the relays your node publishes to. Serbero also
     sends its messages to solvers there, so each solver's Mostro client must
     read at least one of them;
   - one `[[solvers]]` entry per human solver to notify, with the hex pubkey
     of the key they use in their Mostro client and `read` or `write`
     permission. Do not list Serbero itself: it is registered on Mostro as a
     solver, but it does not message itself, and startup rejects its own key
     here. Each solver then sets up their client to read Serbero's messages
     ([Read them in Mostrix](#read-them-in-mostrix)).

   Optionally, add an `[[observers]]` entry with the hex pubkey of a service
   such as mostro-watchdog, to post mediation progress to your team chat. It
   gets one line per mediation update (for example
   `Dispute <id> · handed off: conflicting_claims`), never what the parties
   wrote, and only while mediation is enabled
   ([`docs/messages.md` §3](docs/messages.md#observers)).

   Every field is described in [`docs/spec.md` §9](docs/spec.md#9-configuration).
   Unknown or misspelled fields are rejected at startup.

2. Give Serbero its own Nostr identity, a 64-character hex private key. The
   key never goes in the config file. Generate it on the server, into a file
   only you can read:

   ```sh
   (umask 077; openssl rand -hex 32 > serbero_private_key)
   export SERBERO_PRIVATE_KEY_FILE=$PWD/serbero_private_key
   ```

   Every secret can be given either as the variable itself
   (`SERBERO_PRIVATE_KEY=<hex>`) or as a file named by the same variable with
   `_FILE` appended, never both. Prefer the file: it keeps the secret out of
   the process environment, out of `docker inspect`, and out of your shell
   history. Serbero warns at startup when a secret file is readable by any
   user. Docker and systemd setups use files by default
   ([Run with Docker](#run-with-docker), [Run with systemd](#run-with-systemd)).

   Solvers receive DMs from this identity. Serbero logs its public key, in hex
   and as an npub, when it starts. Back up the key somewhere safe and offline:
   if it is lost, Serbero gets a new npub and must be registered again.

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
   read -rs TYPESAFE_API_KEY                 # paste it; nothing is echoed
   (umask 077; printf '%s\n' "$TYPESAFE_API_KEY" > typesafe_api_key)
   unset TYPESAFE_API_KEY
   export TYPESAFE_API_KEY_FILE=$PWD/typesafe_api_key
   ```

   Each operator uses and pays for its own account. A turn costs a few
   thousand input tokens. For real disputes, ask TypeSafe for zero data
   retention.
5. **Thresholds for the judge.** Serbero acts on the judge's answers only
   with a `[judge.thresholds."typesafe/jev-<version>"]` table for that exact
   judge and model; without it, mediation stays off. No calibration report on
   a full golden set is published yet, so for now mediation runs as a
   **pilot** with the defaults of
   [`docs/judgments.md` §3](docs/judgments.md#3-from-answers-to-facts), which
   `config.sample.toml` ships commented out:

   ```toml
   [judge.thresholds."typesafe/jev-1.13.0"]
   guide = 0.90
   fact = 0.80
   human_request = 0.80
   fraud = 0.60
   conflict = 0.75
   outside_scope = 0.80
   validated_languages = []
   ```

   With `validated_languages = []`, Serbero talks to both parties, gathers the
   payment facts, notices requests for a human and fraud signals, and hands
   every case to the solvers in `[[solvers]]` with a brief, so the pilot
   needs at least one ([Configure the notifier](#configure-the-notifier)). It
   never tells a party to release or cancel. Keep the list empty during the pilot: a language is added only
   when its golden set passes the evaluation, which is what lets Serbero
   guide the parties to resolve on their own in it
   ([`docs/evaluation.md` §3.1](docs/evaluation.md#31-pilot),
   [`docs/spec.md` §7.7](docs/spec.md#77-languages)). Watch the pilot with
   the reports in [Monitor mediation](#monitor-mediation).
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

## Run with Docker

Each release publishes a multi-arch image (amd64 and arm64) to
`ghcr.io/mostrop2p/serbero`, tagged `X.Y.Z`; `X.Y` and `latest` follow the
newest release of the line and overall. Serbero only
makes outbound connections, so the container needs no published port. The
image runs as an unprivileged user (UID 10001), reads the config from
`/etc/serbero/config.toml`, and keeps the database in the `/data` volume.
[`deploy/compose.yml`](deploy/compose.yml) runs it with a read-only root file
system and no capabilities:

```sh
cd deploy
cp ../config.sample.toml config.toml     # edit it as in "Configure the notifier"
chmod 644 config.toml                    # read by UID 10001; holds no secrets
mkdir -m 700 secrets
(umask 077; openssl rand -hex 32 > secrets/serbero_private_key)
# back up secrets/serbero_private_key now; after the chown only root can read it
sudo chown 10001:10001 secrets/serbero_private_key   # readable by Serbero only
echo SERBERO_VERSION=X.Y.Z > .env        # the release to run, without the v
docker compose up -d
docker compose logs -f serbero
```

This needs Docker Compose 2.24 or later. Secrets are Docker secrets: files in
`deploy/secrets/` (gitignored), mounted
under `/run/secrets` and read through `SERBERO_PRIVATE_KEY_FILE` and
`TYPESAFE_API_KEY_FILE`, so they never show in `docker inspect`. To enable
mediation, put the TypeSafe key in `secrets/typesafe_api_key` the same way and
uncomment its lines in `compose.yml`. Never put a secret in `config.toml`,
`.env` or `compose.yml`; `serbero.env` is only for non-secret settings such
as `SERBERO_LOG`. To inspect the database, run the queries below inside the
container, for example
`docker compose exec serbero sqlite3 serbero.db "SELECT ..."`.

To update, set the new version in `.env` and run `docker compose up -d`
again. Live mediation sessions resume where they were.

Each image carries a signed build provenance, so you can check that it was
built from this repository by its release workflow:

```sh
gh attestation verify oci://ghcr.io/mostrop2p/serbero:X.Y.Z -R MostroP2P/serbero
```

To build the image from a checkout instead, add
[`deploy/compose.build.yml`](deploy/compose.build.yml):

```sh
echo SERBERO_VERSION=dev > .env          # any label for the local image
docker compose -f compose.yml -f compose.build.yml up -d --build
```

## Run with systemd

To run the release binary as a service without Docker, use
[`deploy/serbero.service`](deploy/serbero.service). Its header lists the
install commands. It runs Serbero as a dedicated `serbero` user with a
read-only view of the system and keeps the database in `/var/lib/serbero`.
It needs systemd 247 or later (Debian 12, Ubuntu 22.04, RHEL 9). Secrets are
systemd credentials: root-only files in `/etc/serbero/credentials`
that systemd hands to the service in a private directory, so they never enter
its environment. To keep them encrypted at rest, create them with
`systemd-creds encrypt` and switch the unit to `LoadCredentialEncrypted=`.

```sh
journalctl -u serbero -f                  # logs
cd /var/lib/serbero && sudo -u serbero sqlite3 serbero.db "SELECT ..."
```

Run `sqlite3` as the `serbero` user: as root, it can leave root-owned `-wal`
and `-shm` files that the service then cannot write.

## Back up

Back up the database with SQLite's online backup. It is safe while Serbero
runs; copying the file with `cp` is not, because recent writes may still be in
the `-wal` file.

The database holds the parties' conversations, so keep backups private: set
`umask 077` first, so the copy is readable only by you, and store it where
other users cannot read it.

```sh
umask 077
sqlite3 serbero.db ".backup serbero-$(date +%F).db"
```

Under systemd, run it as the `serbero` user in `/var/lib/serbero`, then move
the copy somewhere private.

Under Docker, run it in the container and copy the result out:

```sh
docker compose exec serbero sqlite3 serbero.db ".backup /data/backup.db"
docker compose cp serbero:/data/backup.db "serbero-$(date +%F).db"
docker compose exec serbero rm /data/backup.db
chmod 600 "serbero-$(date +%F).db"
```

Also keep a copy of Serbero's private key: it is Serbero's identity, and
solvers recognize its messages by it.

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

# Observer notices that failed in the last day (each is retried for a day)
sqlite3 serbero.db "SELECT dispute_id, json_extract(payload_json, '$.subject'),
  json_extract(payload_json, '$.error') FROM events
  WHERE kind = 'observer_failed' AND created_at > unixepoch() - 86400;"
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

Releases use [`cargo-release`](https://github.com/crate-ci/cargo-release)
(`cargo install cargo-release`), configured in [`release.toml`](release.toml),
and run from an up-to-date `main` (it refuses any other branch). You need a
crates.io token with publish rights on `serbero` (`cargo login`).

```sh
git checkout main && git pull
cargo release minor        # dry run: shows every change and step
cargo release minor -x     # or patch / major / an exact X.Y.Z
```

It bumps the version in `Cargo.toml` and `Cargo.lock`, moves the
`[Unreleased]` notes in [`CHANGELOG.md`](CHANGELOG.md) under
`## [X.Y.Z] - <today>`, publishes the crate to crates.io, commits
`chore(release): X.Y.Z`, tags `vX.Y.Z`, and pushes the commit and the tag.

The release workflow checks that the tag matches `Cargo.toml` and that the
changelog has notes for it, runs the tests, builds every platform, and
publishes the release with that section as its notes. It also pushes the
container image to `ghcr.io/mostrop2p/serbero` with build provenance.

The first image push creates the GHCR package as private. Once, after that
first release, an organization admin sets the package's visibility to public
in its settings, so operators can pull it without logging in.

## License

[MIT](LICENSE)
