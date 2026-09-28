# Judge spike (Spanish), T0.7

**Question:** does Jev understand how Mostro users write in Spanish, well
enough to build the judge on the qs-1 questions of
[`docs/judgments.md`](../../docs/judgments.md)?

**Answer: yes.** On 51 Spanish conversations with regional slang, typos and
bursts of short messages, Jev (`jev-1.13.0`) answered every critical judgment
correctly, with no false positive at the default thresholds. The spike found
one weakness in the questions, not in the model: a seller who reports a
payment that arrived *with a problem* (wrong sender, short amount, charged
back) was read as `says_received` with P ≥ 0.93. The question was reworded
and the fix is folded into `judgments.md`.

## Method

- **Cases.** 51 hand-written synthetic conversations in
  [`cases.json`](cases.json), each one a turn state as in
  [judgments.md §1](../../docs/judgments.md#1-state). Serbero's own messages
  are rendered from the real catalogs (`messages/*.toml`), so the judge sees
  exactly what Serbero would send. Regions: AR 11, VE 9, ES 8, MX 7, CO 6,
  CL 4, PE 3, CU 2, and one Brazilian buyer writing Portuguese. Only
  judgments a person can answer with confidence are labelled; ambiguous ones
  are left unlabelled and inspected by hand below.
- **Focus.** The critical judgments of
  [evaluation.md §1](../../docs/evaluation.md#size): `buyer_payment`,
  `seller_receipt`, `<party>_wants_human`, `fraud_signal`; the other turn
  questions are labelled where the answer is clear.
- **Questions.** The turn questions (§2.1 and §2.2) extracted verbatim from
  `judgments.md`. The guiding-only question and the brief request are out of
  scope for the spike.
- **Runs.** [`run.py`](run.py), standard library only, one request per case
  with all questions in parallel. `qs-1-draft` is the wording before this
  spike ([`questions-draft.json`](questions-draft.json)); `qs-1` is the
  wording now in `judgments.md` ([`questions.json`](questions.json)). Raw
  answers are saved next to the report, so every number here can be
  re-scored offline.

```sh
set -a; . ./.env; set +a
python3 eval/spike/run.py                                   # qs-1, live
python3 eval/spike/run.py --questions questions-draft.json  # qs-1-draft, live
python3 eval/spike/run.py --score                           # re-score saved answers
```

### Label coverage

| Question | Labels per option |
|---|---|
| `buyer_payment` | says_sent 16, says_not_sent 8, not_stated 13 |
| `seller_receipt` | says_received 7, says_received_with_problem 3, says_not_received 10, not_stated 8 |
| `<party>_wants_human` | true 6, false 52 (including impatience, irony and "soporte de mi banco") |
| `fraud_signal` | true 6 (third-party payer, chargeback, move off Mostro, seed words, edited receipt, threat), false 45 |
| `<party>_message_kind` | answers 28, greeting 2, asks_language 2, not_understood 1, asks_next_step 1 |
| `<party>_language` | es 53, en 1 (asked in Spanish), pt 1 |
| others | `buyer_details` 16, `seller_checked` 9, `claims_conflict` 8, `dispute_topic` 8 |

These counts are below the release-gate minimums (15 per critical option),
as expected for a spike.

## Results (`qs-1`)

Full tables: [`metrics-qs-1.md`](metrics-qs-1.md) and
[`metrics-qs-1-draft.md`](metrics-qs-1-draft.md).

| Question | Accuracy | Mean P(label) |
|---|---:|---:|
| `buyer_payment` | 37/37 | 1.00 |
| `buyer_details` | 16/16 | 0.95 |
| `seller_receipt` | 28/28 | 0.99 |
| `seller_checked` | 8/9 | 0.89 |
| `claims_conflict` | 8/8 | 0.89 |
| `fraud_signal` | 51/51 | 0.91 |
| `dispute_topic` | 7/8 | 0.79 |
| `<party>_message_kind` | 34/34 | 0.98 |
| `<party>_language` | 55/55 | 0.99 |
| `<party>_wants_human` | 58/58 | 0.97 |

At the default thresholds of [judgments.md §3](../../docs/judgments.md#3-from-answers-to-facts),
every fact the policy uses fired on every labelled positive and on nothing
else: precision and recall 1.00 for `says_sent`, `says_not_sent` (at `fact`
and `guide`), `says_received` (at `fact` and `guide`), `says_not_received`,
`wants_human`, `fraud` and `conflict`.

Latency: median 334 ms, max 459 ms. Input tokens: mean 2,062, max 2,456,
in line with the estimate of [judgments.md §2.3](../../docs/judgments.md#23-size).

**Stability.** A second live run of `qs-1` changed no labelled answer; the
largest probability change on any question was 0.08, and the only winner that
changed was an unlabelled near-tie (`dispute_topic` on
`es-ar-buyer-wants-person`, 0.53 → 0.54).

### What the model handled

- Slang and regionalisms: "50 lucas", "me entró la guita", "la lana no me ha
  caído wey", "listo parce", "estoy laburando", "currando", "pantallaso".
- Negation and hedging: "no es que no haya pagado, pagué" (says_sent 0.98),
  "aun no", "todavía no transfiero … jaja", a Nequi transfer shown as
  *pendiente* (says_not_received 0.99, never says_received).
- A seller who first said nothing arrived, then found it after the
  `ask_seller_check_account` question: the whole transcript is read and the
  later statement wins (says_received).
- Human requests versus frustration: "quiero hablar con una persona",
  "pásame con alguien de soporte", "que me atienda alguien" are all true;
  "esto está tardando demasiado", "genial, un bot 🙄" and "ya hablé con
  soporte de mi banco" are all false.
- Language: a Spanish request to switch to English gives `en`, a buyer
  writing Portuguese gives `pt`, and "hablas español?" after an English
  opening gives `es` with `asks_language`.

## Findings

### 1. `says_received` absorbed payments the seller objects to (fixed)

With the draft wording (`"The seller states the fiat payment arrived."`),
three sellers who report a problem with a payment that did arrive got:

| Case | Seller says | P(says_received), draft | qs-1 |
|---|---|---:|---:|
| `es-fraud-third-party-payer` | a transfer arrived, but from someone other than the buyer | **0.93** | 0.00 |
| `es-fraud-chargeback` | paid, then charged back two days later | **0.95** | 0.00 |
| `es-pe-yape-short-amount` | the Yape arrived 20 soles short | 0.78 | 0.00 |

The first two are above `guide`, so `seller_received_for_guide` held and
only the row order of the decision table prevented `Guide(PaymentArrived)`,
which invites the seller to release: `fraud` (row 2) caught both, and
`outside_scope` (row 3) caught the third-party case. For the chargeback
`outside_scope` was only 0.61, so a single noul stood between the seller and
a release suggestion. That is too thin for the path the precision targets
protect.

**Change folded into `judgments.md`:**

- `seller_receipt.says_received` now reads "The seller states the full fiat
  payment arrived in their account and raises no problem with it."
- New option `says_received_with_problem`: "The seller states a payment
  arrived but objects to it, for example a different amount, a sender other
  than the buyer, or a payment later reversed or charged back."
- `outside_scope` also holds when `P(says_received_with_problem) ≥
  outside_scope`, and such a seller is not asked the receipt question again
  (§3); `spec.md` §7.6 describes the handoff reason accordingly.

With the new wording, all three cases give `says_received_with_problem` at
1.00, and all 51 cases keep every other answer.

### 2. Minor, no change

- **`seller_checked` on `es-ar-me-entro-la-guita`** ("me entró la guita a las
  15:32"): 0.43 against a `true` label. Giving the arrival time implies the
  seller looked, but the label is debatable, and the fact only matters when
  the seller says nothing arrived.
- **`dispute_topic` on a buyer who only greets:** `payment_not_confirmed`
  instead of `not_yet_clear`. Both lead to the same action, since neither is
  in the `outside_scope` set.
- **A buyer who could not pay because the app would not load** gets
  `technical_problem` as topic, but only P(outside) = 0.62, and
  `says_not_sent` at 1.00, so the policy would offer `Guide(PaymentNotSent)`
  (cooperative cancel or finish the payment). That is a reasonable outcome
  for this case.

## Limits

- **Synthetic and fairly clear.** The conversations were drafted with a
  generative model and still need review by a native Spanish speaker, as
  [evaluation.md §1](../../docs/evaluation.md#drafting) requires, before they
  seed the T3.9 golden set. Real chats are messier (long off-topic messages,
  several topics at once, sarcasm), and the perfect scores here should not
  be read as the accuracy to expect in production.
- **Small.** 6 positives for `wants_human` and `fraud_signal`, 3 for
  `claims_conflict`: enough to find a wording problem, not to calibrate
  thresholds (T3.10).
- **One model.** Every number applies to `jev-1.13.0` only.
- **Turn questions only.** The brief request (T3.11) and the guiding-only
  question were not tested.

## For T3.5 and T3.9

- Build `qs-1` from the wording now in `judgments.md` (`questions.json` here
  is extracted from it).
- Start the Spanish golden set from `cases.json` after native review, and add
  cases for the weakest spots: payments with a problem (short, duplicate,
  reversed, third party) and more `claims_conflict`, `asks_next_step` and
  `not_understood`.
