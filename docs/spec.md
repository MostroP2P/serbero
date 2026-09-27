# Serbero Specification

Status: draft v1 · Target: Rust (edition 2024), single crate

## 1. Purpose

Many Mostro disputes do not need a solver at all. The fiat arrived late, the
buyer has not paid yet, or the parties simply stopped talking. Once the facts
are on the table, the parties can usually close the trade themselves with the
actions Mostro already gives them during a dispute: the seller can release the
bitcoin, and both parties can agree to a cooperative cancellation.

Serbero exists to get disputes there, and to bring in a human only when needed.

1. **Assist the parties** (opt-in). Serbero takes the dispute as a read-only
   solver, talks to both parties in their own language, establishes the payment
   facts, and, when those facts point to a clear path, explains the options
   Mostro offers so the parties can resolve the dispute themselves.
2. **Escalate when needed.** When the claims contradict each other, something
   looks like fraud, a party asks for a person, or self-resolution stalls,
   Serbero hands the case to a human solver with a brief: what each party
   claims, how sure Serbero is, and the exact messages that support each claim.
3. **Notify.** Every new dispute reaches every configured solver within seconds,
   and keeps reminding them until someone takes it, so no dispute waits
   unnoticed whether or not Serbero is assisting.

Serbero never moves funds. The parties act from their own Mostro apps; Serbero
only tells them what their options are.

## 2. Principles

| # | Principle | What it means in this design |
|---|---|---|
| P1 | **Fund isolation** | Serbero is registered on Mostro with `read` permission only. Mostro rejects any settle or cancel it could ever send. Safety is enforced by the protocol, not by software discipline. |
| P2 | **Parties first, human when needed** | The goal of every session is for the parties to resolve the dispute themselves. Anything contested, suspicious, or stalled goes to a human, who always has the final word. Serbero never states an outcome. |
| P3 | **Guidance follows the actor's own word** | Serbero describes a fund-related option only to the party who would take it, and only after that party has stated the fact the option depends on. The seller hears about releasing only after the seller says the fiat arrived; the buyer's claim never triggers it. |
| P4 | **The user's language** | Parties are addressed in their own language: English by default, Spanish or Portuguese as soon as Serbero detects it. Everything else (code, docs, Jev questions, solver messages) is English. |
| P5 | **Code owns the workflow** | Transport, state, timers, retries, and decisions are ordinary Rust. Jev answers narrow questions; it never chooses what Serbero does. |
| P6 | **Nothing generated reaches a person** | Every message a party or solver receives is a human-written template or a verbatim quote. Jev output is numbers and labels only. |
| P7 | **Calibrated uncertainty** | Decisions use Jev's probabilities against explicit, configurable thresholds. Uncertain facts are treated as unknown and trigger a question, never a guess. |
| P8 | **Auditability** | Every Jev request's answers, every decision, and every message are stored with the question-set version that produced them. |
| P9 | **Privacy by default** | Jev receives roles and message text, never pubkeys. Solvers receive trade roles, never primary identities. Parties are addressed only through trade-scoped shared keys. |
| P10 | **Graceful degradation** | Mostro works without Serbero. Serbero's notifications work without Jev. Mediation failures always fall back to a human. |
| P11 | **Small and legible** | One crate, few modules, few tables. A new contributor should read the whole mediation path in an afternoon. |
| P12 | **Portable judges** | Jev is the v1 judge, but Serbero depends on the capability (typed, calibrated answers), not on a vendor. The provider is chosen in config; nothing outside its adapter knows which one it is ([§5.2](#52-judge-providers)). |

## 3. Why Jev

Mediation here is a sequence of small semantic judgments, repeated every time a
party replies:

- Has the buyer said they sent the payment? Did they give concrete details?
- Has the seller said the payment arrived, or that it did not?
- Do their accounts contradict each other? Is anything suspicious?
- Is someone asking for a human, asking which language we speak, or confused?

Jev is built for exactly this shape: text in, typed and calibrated answers out.
It is the judge Serbero v1 uses; the design keeps the provider replaceable
([§5.2](#52-judge-providers)).

- **One call per turn.** All questions are independent and run in parallel
  inside a single request. A full evaluation of a dispute conversation costs a
  few thousand input tokens, about $0.0001 at list price, and returns in well
  under a second.
- **Typed answers.** A `choice` returns one of the options Serbero defined; a
  `noul` returns a probability. There is nothing to parse, repair, or sanitize.
- **Real probabilities.** Thresholds mean something and can be tuned on labeled
  data. A 0.55 on "seller confirms receipt" is treated as "not yet known", so
  Serbero asks the seller again instead of acting on a coin flip.
- **Selection instead of generation.** For the solver brief, Jev picks which
  message best supports each fact. Serbero quotes that message verbatim, so a
  brief can never contain a claim nobody made.
- **Every language, one set of questions.** Questions are written once, in
  English. Parties may write in Spanish, Portuguese, or anything else; Jev reads
  the text and the templates answer in the party's language.

### What Jev can do for Serbero

- Classify each party's latest message (substantive answer, greeting, "do you
  speak Spanish?", "I don't understand", "what should I do?").
- Judge claims across the whole conversation (payment sent / not sent, received
  / not received, concrete details given, account checked).
- Detect contradictions, fraud signals, and explicit requests for a human.
- Identify each party's language.
- Classify what the dispute is about (payment not received, wrong amount, wrong
  account, unresponsive counterpart, technical problem, other).
- Select the messages that best support each fact, for verbatim quoting.
- Rate, for the solver's eyes only, how strongly the conversation indicates that
  the fiat was sent.

### What Jev does not do (by design, or by nature)

- **It does not write messages.** Serbero's words are templates. Serbero cannot
  improvise a question for an unusual case; unusual cases go to a human, which
  is the correct outcome for them anyway.
- **It does not translate.** Each template exists in each supported language.
  Adding a language means adding a human translation.
- **It does not see images.** Screenshots or receipts sent as attachments are
  recorded as "attachment sent" and forwarded to the solver as a reference.
  Jev cannot read them.
- **It does not know the truth.** Jev judges what the parties *say*. It cannot
  check a bank account. The brief reports claims, never facts.
- **It does not decide.** No Jev answer maps to a fund action. Serbero cannot
  move funds, and the only guidance that mentions one is gated on the acting
  party's own statement at a high threshold. The worst plausible Jev error causes a
  wrong question or an early handoff to a human.
- **It is an external service.** Conversation text leaves the host. See §11.
- **English questions work best.** Party text in other languages must be
  validated (see [evaluation.md](evaluation.md)) before a language is enabled.

## 4. Scope

### In scope (v1)

- Detection of Mostro disputes (`kind 38386`), deduplication, persistence.
- Solver notifications as NIP-44 direct messages (`kind 14`): new dispute,
  reminder, taken, mediation handoff, final report.
- Opt-in mediation: take the dispute, chat with both parties through
  trade-scoped shared keys, evaluate each turn with Jev, send templated
  messages, guide the parties through a self-resolution path, hand off to a
  human with a brief.
- Restart-safe operation: all state in SQLite, keys re-derived at startup.
- An evaluation harness for the question set.

### Out of scope

- Any fund-moving or dispute-closing action.
- Reading images, files, or voice notes.
- Free-form conversation, small talk, or answering general questions.
- More than one Mostro instance per process.
- A web UI. Operators inspect state with `sqlite3` and logs.

## 5. Architecture

```text
               kind 38386 dispute events
  ┌────────┐ ─────────────────────────────▶ ┌──────────────────────────────┐
  │ Mostro │                                │           Serbero            │
  │        │ ◀── admin-take-dispute ─────── │                              │
  │        │ ───── SolverDisputeInfo ─────▶ │  notifier   (always on)      │
  └────────┘                                │  mediator   (opt-in)         │
                                            │   ├ session state machine    │
  ┌─────────────┐   dispute chat (kind 14)  │   ├ judge ──▶ provider (Jev) │
  │ Buyer/Seller│ ◀───────────────────────▶ │   ├ policy (decision table)  │
  └─────────────┘                           │   └ templates / brief        │
  ┌─────────────┐   NIP-44 DMs (kind 14)    │  store (SQLite)              │
  │  Solvers    │ ◀──────────────────────── │                              │
  └─────────────┘                           └──────────────────────────────┘
```

### Modules

| Module | Responsibility |
|---|---|
| `config` | Load `config.toml`, apply env overrides, validate at startup. |
| `store` | SQLite connection, migrations, typed queries. No ORM. |
| `nostr` | Relay client, subscriptions, NIP-44 direct messages to solvers. |
| `mostro` | Everything Mostro-specific, via `mostro-core`: transport detection, take-dispute exchange, dispute events, dispute chat keys and envelopes, inbound validation. |
| `notifier` | Dispute lifecycle, solver notifications, reminder timer, final report. |
| `mediation` | Session state machine, per-turn evaluation loop, timers. |
| `judge` | Provider-neutral `Judge` trait, question set, typed answers; one adapter per provider (`typesafe` for Jev), retries. |
| `policy` | Pure function: (session facts, answers, config) → next action. |
| `messages` | Template catalog (en / es / pt), rendering, solver brief. |

`policy` and `messages` are pure and hold most of the product logic, so most
tests need neither relays nor a judge. `judge` sits behind a small trait with a
recorded-answers implementation for tests (see [evaluation.md](evaluation.md)).

### Dependencies

`tokio`, `nostr-sdk` (nip44), `mostro-core` (transport, dispute and chat helpers), `rusqlite` (bundled),
`reqwest` (rustls, json), `serde`, `serde_json`, `toml`, `tracing`,
`tracing-subscriber`, `thiserror`. Nothing else without a written reason.

### 5.1 Mostro protocol

Serbero follows the [Mostro protocol](https://mostro.network/protocol/) and
uses `mostro-core` for every wire format, so it never re-implements Mostro
cryptography. The facts below are the ones Serbero depends on.

**Supported Mostro versions.** Serbero targets the first `mostrod` release
after v0.18.8 and later ones: protocol v2 only, and order and dispute events
carrying `published_at` (`mostrod` #1000 for orders, #1001 for disputes).
Older nodes are not supported.

**Transport to the daemon.** Serbero speaks only Mostro protocol v2: NIP-44
direct messages (`kind 14`, message `version: 2`, NIP-40 `expiration` tag),
built and parsed with `mostro-core`, honouring the node's proof-of-work tags.
At startup Serbero checks the node's instance-info event (`kind 38385`) and
refuses to enable mediation if the node does not advertise
`protocol_version = 2`; notification keeps working, since it only reads public
dispute events.

**Dispute events.** `kind 38386`, authored by the Mostro node, addressable by
`d` = dispute id. Tags: `s` (status), `initiator` (`buyer` | `seller`),
`published_at` (when the dispute was opened, in Unix seconds; identical in
every revision; `mostrod` #1001 renamed it from the former `created_at` tag,
matching order events), `y` (platform name), `z = dispute`. Revisions are
ordered by the event's own `created_at`, which Mostro bumps on every status
change; `published_at` is never used for ordering. There is
no tag naming the solver, so Serbero filters by **author** (the configured
Mostro pubkey) and learns who took a dispute only from its own actions.

| Status | Meaning |
|---|---|
| `initiated` | Open, waiting for a solver. |
| `in-progress` | A solver took it. A new revision is published on every take, including a takeover. |
| `settled` / `seller-refunded` | Resolved by a solver (buyer paid / seller refunded). |
| `released` / `cooperatively-canceled` | Resolved by the parties themselves (same outcomes). |

**Order events.** `kind 38383`, authored by the Mostro node, addressable by
`d` = order id. Serbero reads two tags from the order behind a dispute:

| Tag | Meaning |
|---|---|
| `f` | Fiat currency code (ISO 4217). |
| `published_at` | When the order was created, in Unix seconds (NIP-69; named as in NIP-23). The same value in every revision of the order. |

Supported nodes always publish both tags. An order event missing either one
does not follow the protocol: Serbero logs it and treats the order facts as
unknown. `published_at` is never taken from the event's own `created_at`,
which is the time of the latest revision. Dispute events use the same
`published_at` tag for the dispute's open time (see above). A `created_at`
tag on either kind comes from a node older than Serbero supports and is
ignored.

**Taking a dispute.** Serbero sends `admin-take-dispute`; Mostro answers
`admin-took-dispute` with `Payload::Dispute(id, SolverDisputeInfo)` (order id,
trade pubkeys of both parties, `fiat_amount`, `payment_method`, reputation
info), tells both parties the solver's pubkey, and publishes an `in-progress`
revision. Any registered solver may take an `initiated` dispute. A solver with
`write` permission may take over a dispute held by a `read` solver such as
Serbero; nobody else can. Serbero therefore treats any `in-progress` revision
newer than its own take as a takeover (§6).

**Dispute chat.** Each party talks to the solver on its own channel:

```text
shared = ECDH(serbero_key, party_trade_pubkey)
K_conv = HKDF-SHA256(shared, "mostro:chat:conv:v1")   // encryption; p tag = pub(K_conv)
K_sign = HKDF-SHA256(shared, "mostro:chat:sign:v1")   // outer signer; author = pub(K_sign)
```

A message is a `kind 1` inner event signed by the sender's own key (Serbero's
key outbound, the party's trade key inbound), NIP-44 encrypted under `K_conv`,
inside a `kind 14` event signed with `K_sign`, with exactly one `p` tag equal
to `pub(K_conv)` and a real `created_at`. Serbero derives the keys and wraps
and unwraps with `mostro-core::chat` (`derive_chat_keys`, `wrap_chat_message`,
`unwrap_chat_message`, `chat_filter`).

Inbound validation follows the protocol's client security requirements, in
their cheapest-first order: author, `p` tag, clock-skew bound, size, outer-id
LRU, per-conversation rate limit, outer signature, decryption, inner
signature, inner signer (that party's trade key only), inner kind 1, durable
inner-id dedup, and inner/outer timestamp agreement. The subscription is by
author (`authors = [pub(K_sign)]`) with a persisted `since` cursor that is
never advanced past Serbero's own clock.

**Relays.** Signed `kind 14` events are outside what NIPs guarantee relays
store. Operators must configure relays verified to store and serve them.

**Not used in v1: trade-chat disclosure.** A party may voluntarily disclose
the `K_conv` of the buyer–seller trade chat to the solver, giving read-only
access to that conversation. It would be strong evidence for the judge (what
was said before the dispute), but the protocol defines no message to deliver
it yet. It is noted as a future extension.

### 5.2 Judge providers

Jev is the first model of its kind: a System One model that answers typed
questions with calibrated probabilities instead of generating text. Serbero v1
uses it, but Serbero depends on that **capability**, not on the vendor. When
other models of this kind exist, switching is a configuration change.

**Provider-neutral contract.** Everything outside `src/judge/providers/` uses
Serbero's own types:

```rust
pub enum Question {
    Noul   { instructions: Value, criteria: Option<NoulCriteria> },
    Choice { instructions: Value, options: Vec<(String, Option<Value>)> },
    Score  { instructions: Value, levels: Vec<Value> },
}

pub enum Answer {
    Noul   { p_yes: f64 },
    Choice { probabilities: BTreeMap<String, f64> },
    Score  { probabilities: Vec<f64> },   // one per level
}

pub trait Judge: Send + Sync {
    /// Stable identifier, e.g. "typesafe/jev-1.13.0", stored with every evaluation.
    fn id(&self) -> &str;
    fn capabilities(&self) -> Capabilities;
    async fn evaluate(&self, state: &Value, questions: &QuestionSet) -> Result<Answers, JudgeError>;
    async fn health_check(&self) -> Result<(), JudgeError>;
}
```

- The question set ([judgments.md](judgments.md)) is written once, in these
  types. Each provider adapter translates it to its wire format and translates
  answers back. No provider-specific type crosses the adapter boundary.
- Serbero derives everything it needs from probabilities: the winning option,
  the score value, and confidence (computed by Serbero with one formula for
  every provider). Policy never depends on a vendor's own definition of
  confidence.
- `JudgeError` is provider-neutral: `Unavailable` (retryable: overload, rate
  limit, network, timeout), `Unauthorized`, `InvalidRequest`, `Malformed`. Each
  adapter maps its status codes onto these.
- `Capabilities` declares what the provider supports: question types, maximum
  options per choice, maximum score levels, context size. At startup Serbero
  validates the question set against them and refuses to enable mediation if a
  question cannot be expressed.

**Adapters.**

| `provider` | Status | Notes |
|---|---|---|
| `typesafe` | v1 | Jev via `POST {api_base}/v1/systemone`. |
| `recorded` | v1 | Replays stored answers. Used by tests and for dry runs. |
| *new vendor* | future | One file in `src/judge/providers/` implementing `Judge`; nothing else changes. |

**Switching provider** means editing `[judge]` in the config (`provider`,
`model`, `api_base`, `api_key_env`) and restarting. Because calibration differs
between models, thresholds are stored per provider
(`[judge.thresholds."<provider>/<model>"]`), and a provider or model is enabled
only after it passes the golden set. Production configs pin a concrete model
version rather than an alias such as `jev-latest`, so the calibrated thresholds
always match the model that answers ([evaluation.md](evaluation.md)). Every
evaluation row records the judge id, so results from different providers are
never mixed up.


## 6. Dispute lifecycle (notifier)

```text
new ──notify──▶ notified ──(s=in-progress)──▶ taken ──(terminal status)──▶ resolved
                   │ ▲
                   └─┘ reminder every `renotify_after`
```

1. **Detect.** Subscribe to `kind 38386` authored by the configured Mostro
   pubkey with `z = dispute` ([§5.1](#51-mostro-protocol)). Insert by
   `dispute_id` with `ON CONFLICT DO NOTHING`; a duplicate is a no-op. Only the
   newest revision of each dispute is applied, ordered by the event's own
   `created_at` (the `published_at` tag is the dispute's open time and stays
   the same across revisions).
2. **Notify.** Send the "new dispute" DM to every configured solver. Record each
   attempt as an event. Move to `notified` if at least one send succeeded.
3. **Remind.** A timer re-sends to all solvers for disputes still `notified`
   after `renotify_after`.
4. **Taken.** On `s = in-progress`, mark the dispute `taken` and notify all
   solvers. Dispute events do not name the solver: if Serbero has just taken the
   dispute itself, the mediator owns it (`assigned_solver` = Serbero);
   otherwise the solver is recorded as unknown. An `in-progress` revision newer
   than Serbero's own take means a `write` solver took it over: the session
   ends as `superseded` and Serbero stops writing to the parties immediately.
5. **Resolved.** On a terminal dispute status, close any open session and, if
   mediation took part, send the final report. Terminal statuses (as published
   by Mostro, kebab-case) are `settled` and `seller-refunded`, set by a solver,
   and `released` and `cooperatively-canceled`, set when the parties resolved
   the dispute themselves.

Solvers are always notified, whether or not mediation is enabled. Mediation
never delays notification.

## 7. Mediation

### 7.1 Eligibility

A dispute enters mediation when all of the following hold. Every check is
deterministic; no Jev call happens before the take.

- `[mediation].enabled = true` and the judge health check passed at startup.
- The dispute is `notified` and has no session and no prior handoff.
- The dispute has not been taken by a human.
- Optional operator filters pass (for example `max_fiat_amount`).

### 7.2 Opening

1. Send `AdminTakeDispute` to Mostro and wait for `AdminTookDispute` with
   `SolverDisputeInfo` (bounded timeout; failure → no session, notification
   continues as normal).
2. Store the parties' **trade** pubkeys and the order facts Serbero may render.
   `SolverDisputeInfo` carries `fiat_amount` and `payment_method` but not the
   currency or the order's creation time. Both are read from the order's public
   event (`kind 38383`, `d` = the order id from `SolverDisputeInfo.id`, tags `f`
   and `published_at`; see above). If the event cannot be fetched from the
   relays, templates use their `_noamount` form and the brief omits the
   order's age; the session goes on. Derive each party's chat keys
   (`K_conv`, `K_sign`) from Serbero's key and the party's trade pubkey
   ([§5.1](#51-mostro-protocol)). The keys are never stored: they are
   re-derived from the trade pubkeys at startup.
3. Subscribe to `kind 14` events authored by both parties' `pub(K_sign)` (one
   live subscription for all sessions, updated as sessions open and close;
   bounded by the stored `since` cursors; no polling).
4. Send each party the opening message in `default_language`: the intro line and
   the first role-specific question (`ask_buyer_sent` / `ask_seller_received`).
   No Jev call is needed for the opener.
5. Notify solvers that Serbero is assisting (they can still take over at any
   time).

### 7.3 Turn loop

A **turn** starts when a party message arrives and ends when Serbero has
decided and sent its response.

1. **Ingest.** Validate the event in the protocol's order
   ([§5.1](#51-mostro-protocol)): the inner signer must be that party's trade
   key, and the inner event id must be new (durable dedup). Store the message
   and advance the party's cursor, never past the local clock. Truncate stored
   text at `max_message_chars`; count attachments without storing them.
2. **Settle.** Wait `quiet_period` (default 20 s) after the latest inbound
   message so that bursts such as "hola" + "help" + "?" become one turn.
3. **Judge.** Build the state (see [judgments.md §1](judgments.md#1-state)) and
   send the full question set to the judge in one request.
4. **Decide.** Run `policy` over the answers, the session's history of questions
   already asked, and the thresholds. The result is one `Action`.
5. **Act.** Send templates, update the session, persist the evaluation and the
   action, and notify the solver if the action requires it.

```rust
enum Action {
    Ask { buyer: Option<TemplateId>, seller: Option<TemplateId> },
    Guide(Path),                    // explain a self-resolution path to the parties
    Handoff(HandoffReason),         // brief to solver, notice to parties
    Wait,                           // nothing new to ask, still inside limits
}

enum Path {
    PaymentArrived,                 // the seller says the fiat arrived
    PaymentNotSent,                 // the buyer says they have not paid
}
```

The decision table is in [judgments.md §4](judgments.md#4-decision-table).

### 7.4 Self-resolution paths

During a dispute, Mostro still accepts two actions from the parties
themselves: the seller can release the bitcoin, and both parties can request a
cooperative cancellation. Serbero's job is to establish which of these fits and
to explain it in plain words. It never performs either, and it never pushes a
party to act.

| Path | Established by | Seller is told | Buyer is told |
|---|---|---|---|
| `PaymentArrived` | The **seller** says the fiat arrived (`seller_received`) | If the payment is confirmed in their account, they can complete the trade by releasing from their Mostro app, which closes the dispute. | The seller reports the payment arrived and can complete the trade from their app. |
| `PaymentNotSent` | The **buyer** says they have not paid (`buyer_not_sent`) | The buyer reports not having paid. The two of them can agree how to continue: request a cooperative cancellation from the app, or wait for the payment if both agree. | Same options, from the buyer's side. |

Rules for the paths:

- A path starts only from the statement of the party it depends on (P3). A
  buyer saying "I paid" never starts `PaymentArrived`; that case keeps
  gathering facts or goes to a human.
- Guidance is sent once per path. After it, Serbero watches the dispute status
  and keeps judging turns; it still answers requests for a human and still
  escalates on fraud or contradiction.
- The session ends by itself when Mostro publishes the dispute as `released`
  (the seller released) or `cooperatively-canceled` (both parties cancelled).
  Mostro closes the dispute and publishes the updated dispute event in both
  cases. Serbero then sends `resolved_thanks`
  and the final report.
- If the dispute is not resolved within `self_resolution_timeout`, or a party
  rejects the path ("I did not receive anything", "I don't agree to cancel"),
  Serbero hands off with `self_resolution_stalled`.

### 7.5 Session states

```text
opening ─▶ active ─┬─▶ guiding ─┬─▶ closed          (parties resolved it)
                   │            └─▶ handed_off      (stalled, human requested, fraud)
                   ├─▶ handed_off ─▶ closed
                   └─▶ superseded                    (human solver took over)
```

- `active`: Serbero is gathering facts.
- `guiding`: a path was explained; Serbero watches for the resolution and keeps
  judging turns without asking new fact questions.
- `handed_off`: the brief was sent; Serbero sends no more questions. New party
  messages are appended to the transcript and forwarded to the solver in the
  next update DM, batched by `quiet_period`.
- `closed` / `superseded`: terminal.

### 7.6 Handoff reasons

| Reason | Trigger |
|---|---|
| `self_resolution_stalled` | A path was explained but the dispute did not resolve in time, or a party rejected it. |
| `facts_gathered` | Both payment claims are known, no self-resolution path applies, and nothing useful remains to ask. |
| `conflicting_claims` | The parties' accounts contradict each other with both sides answered. |
| `fraud_signal` | Fraud signal above threshold. |
| `human_requested` | A party explicitly asks for a person. Honored at any point. |
| `outside_scope` | The dispute is not about payment confirmation. |
| `unresponsive` | A party did not answer the question and the reminder within `response_timeout`. |
| `round_limit` | `max_rounds` question rounds sent without reaching a decision. |
| `uncertain` | The same fact stayed below threshold after its follow-up question. |
| `judge_unavailable` | The judge failed after retries. The brief carries the transcript without judgments. |
| `flood` | A party sent more than `max_messages_per_turn` in one turn repeatedly. |

Every handoff sends the parties the `handoff_notice` template and the solver
the brief ([messages.md §3](messages.md#3-solver-messages)). The recipient is
the assigned human solver if there is one, otherwise every `write` solver,
otherwise every solver.

## 8. Data model

SQLite, five tables. Timestamps are Unix seconds (UTC).

```sql
disputes (
  dispute_id        TEXT PRIMARY KEY,
  initiator         TEXT NOT NULL,           -- 'buyer' | 'seller'
  status            TEXT NOT NULL,           -- Mostro dispute status
  lifecycle         TEXT NOT NULL,           -- new | notified | taken | resolved
  assigned_solver   TEXT,
  first_seen_at     INTEGER NOT NULL,
  last_notified_at  INTEGER,
  updated_at        INTEGER NOT NULL
);

sessions (
  session_id          TEXT PRIMARY KEY,
  dispute_id          TEXT NOT NULL REFERENCES disputes,
  state               TEXT NOT NULL,         -- opening | active | guiding | handed_off | closed | superseded
  buyer_trade_pubkey  TEXT NOT NULL,
  seller_trade_pubkey TEXT NOT NULL,
  fiat_amount         TEXT,
  fiat_code           TEXT,
  order_published_at  INTEGER,               -- order creation time (published_at tag), if known
  buyer_lang          TEXT,
  seller_lang         TEXT,
  buyer_chat_cursor   INTEGER,               -- `since` for the buyer channel
  seller_chat_cursor  INTEGER,               -- `since` for the seller channel
  rounds              INTEGER NOT NULL DEFAULT 0,
  handoff_reason      TEXT,
  opened_at           INTEGER NOT NULL,
  updated_at          INTEGER NOT NULL
);
CREATE UNIQUE INDEX one_live_session ON sessions(dispute_id)
  WHERE state NOT IN ('closed', 'superseded');

messages (
  id              INTEGER PRIMARY KEY,
  session_id      TEXT NOT NULL REFERENCES sessions,
  direction       TEXT NOT NULL,             -- 'in' | 'out'
  party           TEXT NOT NULL,             -- 'buyer' | 'seller'
  template_id     TEXT,                      -- outbound only
  lang            TEXT,                      -- outbound only
  content         TEXT NOT NULL,
  attachments     INTEGER NOT NULL DEFAULT 0,
  inner_event_id  TEXT NOT NULL,
  created_at      INTEGER NOT NULL,
  UNIQUE (session_id, inner_event_id)
);

evaluations (
  id                    INTEGER PRIMARY KEY,
  session_id            TEXT NOT NULL REFERENCES sessions,
  question_set_version  TEXT NOT NULL,
  judge_id              TEXT NOT NULL,       -- e.g. 'typesafe/jev-1.13.0'
  last_message_id       INTEGER NOT NULL,    -- transcript cut-off used as state
  answers_json          TEXT NOT NULL,       -- provider-neutral answers
  action_json           TEXT NOT NULL,       -- policy output
  input_tokens          INTEGER,
  latency_ms            INTEGER,
  created_at            INTEGER NOT NULL
);

events (
  id           INTEGER PRIMARY KEY,
  dispute_id   TEXT NOT NULL,
  session_id   TEXT,
  kind         TEXT NOT NULL,                -- notification_sent, taken, handoff, ...
  payload_json TEXT NOT NULL,
  created_at   INTEGER NOT NULL
);
```

The state sent to the judge is always reproducible from `messages` up to
`last_message_id`, so an evaluation can be replayed exactly against a new
question-set version.

## 9. Configuration

```toml
[serbero]
private_key_env = "SERBERO_PRIVATE_KEY"   # never in the file
db_path = "serbero.db"
log_level = "info"

[mostro]
pubkey = "<hex>"
relays = ["wss://relay.mostro.network", "wss://nos.lol"]

[[solvers]]
pubkey = "<hex>"
permission = "write"                      # "read" | "write"

[notify]
renotify_after = "15m"

[mediation]
enabled = false
default_language = "en"                   # language of the opening message
languages = ["en", "es", "pt"]            # must exist in the template catalog
quiet_period = "20s"
response_timeout = "30m"                  # per question, before the reminder and again before handoff
max_rounds = 3
max_message_chars = 2000
max_messages_per_turn = 10
max_fiat_amount = 0                       # 0 = no limit
self_resolution_timeout = "2h"           # guiding → handed_off if not resolved

[judge]                                   # see §5.2; switching provider is a config change
provider = "typesafe"                     # typesafe | recorded | <future providers>
model = "jev-1.13.0"                      # pin a concrete version; aliases like jev-latest can move
api_base = "https://api.typesafe.ai"
api_key_env = "TYPESAFE_API_KEY"         # the operator's own key; each operator pays for its usage
timeout = "10s"
max_retries = 3                           # retryable errors only, exponential backoff

[judge.thresholds."typesafe/jev-1.13.0"]  # calibrated per provider/model, see judgments.md §3
guide = 0.90
fact = 0.80
human_request = 0.80
fraud = 0.60
conflict = 0.75
outside_scope = 0.80
```

Startup fails fast on a missing key, an unknown language, or a solver list that
cannot be parsed. With `[mediation].enabled = true` and no reachable judge, or
no calibrated thresholds for the configured provider and model,
Serbero logs an operator-actionable error and runs notification only.

## 10. Degraded mode

| Failure | Behavior |
|---|---|
| A relay drops | `nostr-sdk` reconnects; other relays keep serving. |
| A relay does not store `kind 14` | Offline party messages are lost on that relay. Operators must use relays verified to store them ([§5.1](#51-mostro-protocol)). |
| Chat flood from a party | Per-conversation rate limit drops excess before decryption; sustained flooding hands off with `flood`. |
| All relays drop | Retries continue; notifications resume on reconnect. |
| SQLite write fails on detect | The dispute is not notified until it is seen again; integrity over delivery. |
| A solver DM fails | Recorded; the reminder timer covers unattended disputes. |
| Take-dispute fails or times out | No session; the dispute stays a normal notified dispute. |
| Judge `Unavailable` (overload, rate limit, network, timeout) | Retry with backoff up to `max_retries`, then `Handoff(judge_unavailable)`. |
| Judge `Unauthorized` / `InvalidRequest` / `Malformed` | No retry; `Handoff(judge_unavailable)` and an operator error log (key, adapter, or question-set bug). |
| Restart mid-session | Chat keys re-derived from stored trade pubkeys; subscription rebuilt from the stored cursors; pending turns re-evaluated from `messages`. |
| Human solver takes over | Session `superseded`; Serbero goes silent immediately. |
| Serbero offline | Mostro and solvers work exactly as without Serbero. |

## 11. Privacy and security

- **What the judge receives:** trade roles, order amount and currency, the payment
  method text, and message text. No pubkeys, no event ids, no Nostr metadata.
- **Each operator brings its own judge account.** Every Mostro operator that
  enables mediation contracts the judge provider directly (for Jev, a TypeSafe
  account) and configures its own API key. Usage, billing, and the provider's
  terms are the operator's responsibility; Serbero ships no shared key.
- **Retention:** depends on the provider. TypeSafe does not train on requests,
  and operators handling real disputes should request zero-data retention.
  Any new provider is reviewed for the same guarantees before it is enabled.
- **Disclosure:** the opening message (`intro`) tells each party they are
  talking to an automated assistant and that messages in the chat may be
  monitored and processed by an automated service.
- **Untrusted input:** party text is only ever placed inside `state`, never in
  question instructions. A party cannot change the questions or the options,
  and every possible answer is one Serbero already handles.
- **Authority boundary:** because no generated text reaches anyone, the only
  surface to audit is the template catalog. A test allows fund-action words
  only in the reviewed `guide_*` templates, in every language ([messages.md §4](messages.md#4-template-rules)).
- **Secrets:** Serbero's private key and the judge API key come from the
  environment only. Neither is logged.
- **Limits:** message length, messages per turn, and rounds are capped, which
  also bounds the tokens per judge request well under the provider's context
  size (64k for Jev).

## 12. Observability

`tracing` spans for every turn: `session_id`, `turn`, `judge_id`, `judge_latency_ms`,
`input_tokens`, `action`, and threshold-relevant probabilities. Message text is
never logged. Useful operator queries (open sessions, handoffs by reason,
average judge latency, cost per day) ship as a documented list of `sqlite3`
one-liners.

## 13. Implementation

The work is split into phases of atomic, reviewable pull requests in
[plan.md](plan.md). Phase 1 (notification) is useful on its own and ships
before any mediation work.
