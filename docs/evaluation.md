# Evaluation

Judges like Jev return calibrated probabilities, but calibration is only useful
once it has been checked on Serbero's own conversations. This document defines
how the question set is validated before a language or a judge provider is
enabled, how thresholds are
chosen, and how quality is tracked in production.

## 1. Golden set

A versioned directory `eval/golden/` of JSON cases. A **case** is one
conversation: a turn state exactly as sent to the judge
([judgments.md §1](judgments.md#1-state)), plus the expected answer for every
question a human can answer with confidence. One case therefore labels several
questions at once.

```json
{
  "id": "es-seller-denies-after-check-01",
  "lang": "es",
  "source": "synthetic",
  "state": { "...": "..." },
  "expect": {
    "seller_receipt": "says_not_received",
    "seller_checked": true,
    "buyer_payment": "not_stated",
    "seller_message_kind": "answers",
    "seller_language": "es",
    "seller_wants_human": false
  }
}
```

Unlabeled questions are not scored for that case.

### Sources

- **Recorded test sessions.** Conversations from staging trades, stripped of
  identifiers. They show how people really write: greetings alone, "help",
  "¿hablas español?", "estás ahí?", typos, several short messages in a row.
- **Synthetic cases** written to cover every option of every question in each
  language, including the hard ones:
  - buyer claims to have paid with no details vs. with details;
  - seller says the money arrived vs. "it's pending" vs. "not yet";
  - a party asks for a human vs. is only impatient ("this is taking forever");
  - a party asks which language Serbero speaks;
  - a buyer says "I haven't paid yet, I was driving";
  - fraud signals: third-party payer, reversed payment, a request to continue
    on another app, a request for seed words;
  - disputes outside scope: wrong amount, wrong account, app problems.
- **Production cases** (after release, with operator consent): turns where a
  solver later marked the brief as wrong (§5).

### Drafting

Cases may be drafted with the help of a generative model, but every case is
read and corrected by a person before it is committed, and Spanish and
Portuguese cases are reviewed by native speakers. Drafts must imitate how
people really write in a Mostro chat: short, informal, regional slang, typos,
several messages in a row.

### Size

The set grows in three steps:

| Step | Per language | Purpose |
|---|---|---|
| **Spike** ([plan.md T0.7](plan.md#phase-0--foundation)) | ~40 conversations, Spanish only | Early answer to one question: does the judge understand how Mostro users write? Focused on the critical judgments. |
| **Validation** | ~150 conversations per language (en and es for v1) | Enough to validate a language and calibrate thresholds. |
| **Production** | grows over time | Real turns marked wrong by solvers (§5) become new cases. |

Within the release-gate set, every option of the critical questions
(`buyer_payment`, `seller_receipt`, `<party>_wants_human`, `fraud_signal`)
appears in at least 15 cases, and every option of the other questions in at
least 5.

## 2. Metrics and targets

Run with `cargo run --bin eval -- --lang es` against the judge configured in
`[judge]` (or `--provider typesafe --model jev-1.13.0`; pin a version, since answers from any other model are rejected). It sends each case, compares the answers with the labels, and writes a report.

| Question | Metric | Target to validate a language |
|---|---|---|
| `seller_receipt = says_received` | Precision at threshold `guide` | ≥ 0.98 |
| `buyer_payment = says_not_sent` | Precision at threshold `guide` | ≥ 0.98 |
| `buyer_payment`, `seller_receipt` | Accuracy on cases above threshold | ≥ 0.95 |
| `buyer_payment`, `seller_receipt` | Coverage (share above threshold) | ≥ 0.70 |
| `<party>_wants_human` | Recall at threshold `human_request` | ≥ 0.90 |
| `fraud_signal` | Recall at threshold `fraud` | ≥ 0.85 |
| `<party>_language` | Accuracy | ≥ 0.95 |
| `<party>_message_kind` | Accuracy | ≥ 0.85 |
| all | Calibration error (ECE, 10 bins) | reported, no gate |
| all | Median / p95 latency, mean input tokens | reported, no gate |

The precision targets protect the two actions that change the conversation
without a human: the two self-resolution paths (`PaymentArrived` and
`PaymentNotSent`).
Recall targets protect the two paths toward a human that must not be missed.

With a validation set of about 150 conversations the estimates are coarse. A
0.98 precision target over the 15–25 cases the judge marks positive effectively
allows no false positives among them; it says nothing about positives the
judge misses, which the coverage and recall targets measure. Targets are
re-checked as the set grows.

Meeting every target makes a language **validated** for the judge that was
evaluated: its code is added to that judge's `validated_languages`, and
self-resolution guidance becomes available in it. A language without a golden
set can still be enabled as **conversational**
([spec.md §7.7](spec.md#77-languages)). English and Spanish are validated for
the v1 release; other languages follow the same procedure whenever a golden
set exists.

## 3. Choosing thresholds

Thresholds are chosen per provider and model. For each threshold, the report
plots precision and coverage against the threshold value on the golden set.
The chosen value is the lowest one that meets the precision or recall target
above, rounded up to 0.05. For a given provider and model, one value covers all
enabled languages; if one language needs a stricter value, that value applies
to all.

Changing a threshold is a config change and needs no new version. The report
that justified it is committed next to the golden set.

### Enabling a new provider or model

The same targets gate every provider and model. To switch Serbero to a new one:

1. Implement its adapter (one file in `src/judge/providers/`), unless an
   existing adapter already speaks its API.
2. Run the golden set for every enabled language with the new provider.
3. Calibrate its thresholds (§3) and commit them as
   `[judge.thresholds."<provider>/<model>"]` together with the report.
4. Change `[judge]` in the deployment config and restart.

A new model version from the same provider (for example a new Jev release) goes
through steps 2–4 as well.

## 4. Tests without a live judge

- **Recorded answers.** Every golden case also stores the answers the judge returned
  when the case was last evaluated (`eval/recorded/<judge_id>/<question_set_version>/`).
  A `RecordedJudge` implementation of the `judge` trait replays them, so the
  policy, templates, and brief are tested end to end, deterministically and
  offline, in CI.
- **Policy tables.** `policy::decide` is tested with hand-written `Facts` for
  every row of the decision table and every branch of §4.1 in
  [judgments.md](judgments.md#41-next-question-for-each-party).
- **Session scripts.** Full sessions are scripted as a sequence of party
  messages and recorded answers, asserting the exact templates sent, the state
  transitions, and the handoff reason.
- **Snapshot.** The serialized question set is hashed; a change without a
  version bump fails ([judgments.md §6](judgments.md#6-versioning)).

A live test against the configured judge (`cargo test --features live-judge`) runs a small smoke
subset and is not part of the default CI run.

## 5. Production monitoring

- **Every evaluation is stored** with the question-set version, answers,
  action, tokens, and latency. Any turn can be replayed against a new version
  with the eval binary.
- **Solver feedback.** A solver may reply to a brief with `wrong <fact>` (for
  example `wrong seller_receipt`). Serbero records it as an event and the turn
  becomes a candidate golden case.
- **Outcome signal.** When a dispute resolves, its final status (for example
  `settled` or `seller-refunded`) is stored next to the last
  `evidence_balance`. Over time this measures how well the advisory reading
  matches solver decisions. It is never used to automate a decision.
- **Weekly report** (a `sqlite3` script): sessions opened, sessions resolved by
  the parties themselves, handoffs by reason, share of `uncertain` and `round_limit` handoffs, feedback
  count, median latency, total tokens and cost.

A rise in `uncertain` handoffs or in solver feedback for one question is the
signal to add golden cases and revise that question in the next version.
