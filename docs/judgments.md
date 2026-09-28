# Judgments: the Jev question set

This document is the contract between Serbero and Jev: the state Serbero
sends, the questions it asks, how answers become facts, and the decision
table that turns facts into actions. Question set version: **`qs-1`**.

Serbero makes two kinds of Jev request:

| Request | When | Purpose |
|---|---|---|
| **Turn** | After every settled turn (see [spec.md §7.3](spec.md#73-turn-loop)) | Update the case facts and classify the latest messages |
| **Brief** | Once, when the session hands off or starts a self-resolution path | Select quotes and rate the evidence for the solver |

The question set is written once in Serbero's provider-neutral types
([spec.md §5.2](spec.md#52-judge-providers)). The JSON in this document is its
canonical serialization, which is also what the `typesafe` adapter sends to Jev
(`POST https://api.typesafe.ai/v1/systemone`, with the pinned `[judge].model`). Other
adapters translate it to their own format.

## 1. State

The state is JSON built by code from the `sessions` and `messages` tables. It
never contains pubkeys, event ids, or anything a party did not write, apart
from the order facts below.

```json
{
  "roles": {
    "buyer": "Pays the fiat money to the seller outside Mostro, then waits for the bitcoin.",
    "seller": "Has bitcoin locked in Mostro escrow and must confirm the fiat arrived before the trade can finish.",
    "serbero": "Automated assistant that asks both parties questions for the human solver. It cannot move funds or decide the dispute."
  },
  "order": {
    "fiat_amount": "50000",
    "fiat_code": "ARS",
    "payment_method": "Mercado Pago",
    "dispute_opened_by": "seller"
  },
  "transcript": [
    { "id": "m1", "from": "serbero", "to": "buyer",  "text": "…" },
    { "id": "m2", "from": "serbero", "to": "seller", "text": "…" },
    { "id": "m3", "from": "seller", "text": "hola, no me ha llegado el dinero" },
    { "id": "m4", "from": "buyer",  "text": "ya envié el pago a las 14:10", "attachments": 1 }
  ],
  "latest": {
    "buyer":  ["m4"],
    "seller": []
  }
}
```

- `transcript` holds the whole session in order. Serbero's own messages are
  included, so Jev can tell which question a party is answering.
- `latest.<party>` lists the ids of the party's messages since Serbero last
  wrote to them. Per-message questions look only at these.
- Message text is truncated at `max_message_chars`. Attachments are counted,
  never sent.
- Nostr identifiers are replaced with `[redacted]` even inside party text,
  in case a party pastes one: any 64-character hex word (a key or event id)
  and any word that parses as NIP-19 (`npub`, `nsec`, `note`, `nprofile`,
  `nevent`, `naddr`), whether or not it belongs to this session.
- Order facts Serbero does not know (the order event could not be fetched, or
  Mostro published the initiator as `unknown`) are left out, never guessed.
- Message ids are the only link between answers and text. Party text never
  appears in instructions or criteria, so it cannot change a question.

## 2. Turn request

All questions run in parallel in one request. Per-party questions for a party
with an empty `latest` list are omitted.

### 2.1 Case facts (whole transcript)

```json
{
  "buyer_payment": {
    "type": "choice",
    "instructions": "Across the whole `transcript`, what has the buyer said about sending the fiat payment for this order?",
    "criteria": {
      "says_sent": "The buyer states they already sent or completed the fiat payment.",
      "says_not_sent": "The buyer states they have not sent the payment, or not yet.",
      "not_stated": "The buyer has not said either way, or their messages are too vague to tell."
    }
  },
  "buyer_details": {
    "type": "noul",
    "instructions": "Has the buyer given at least one concrete, checkable detail about the fiat payment they say they sent, such as the time, the exact amount, a transaction reference, the account or app used, or a receipt attached to their message?",
    "criteria": {
      "true": "At least one specific detail about the transfer is present in the buyer's messages.",
      "false": "The buyer only claims to have paid, or has given no payment details."
    }
  },
  "seller_receipt": {
    "type": "choice",
    "instructions": "Across the whole `transcript`, what has the seller said about receiving the fiat payment for this order?",
    "criteria": {
      "says_received": "The seller states the full fiat payment arrived in their account and raises no problem with it.",
      "says_received_with_problem": "The seller states a payment arrived but objects to it, for example a different amount, a sender other than the buyer, or a payment later reversed or charged back.",
      "says_not_received": "The seller states the fiat payment has not arrived.",
      "not_stated": "The seller has not said either way, or their messages are too vague to tell."
    }
  },
  "seller_checked": {
    "type": "noul",
    "instructions": "Does the seller say they checked the account or app where the payment should arrive?",
    "criteria": {
      "true": "The seller describes having looked at their bank account, app, or statement for this payment.",
      "false": "The seller has not said they checked."
    }
  },
  "claims_conflict": {
    "type": "noul",
    "instructions": "Do the buyer and the seller make factual claims about the payment that cannot both be true?",
    "criteria": {
      "true": "For example, the buyer says the payment was sent and completed while the seller says nothing arrived in the account.",
      "false": "Their accounts are compatible, or at least one party has not made a claim yet."
    }
  },
  "fraud_signal": {
    "type": "noul",
    "instructions": "Does anything in the `transcript` suggest deliberate bad faith by either party?",
    "criteria": {
      "true": "Signs such as an admitted or described altered receipt, a payment from a third party's account, a reversed or charged-back payment, asking to move the conversation or the trade to another app or contact (for example a Telegram or WhatsApp username, or a phone number), pointing to a supposed Mostro support or administrator outside this chat, requests for passwords, seed words or private keys, or threats.",
      "false": "Nothing beyond an ordinary disagreement or delay."
    }
  },
  "dispute_topic": {
    "type": "choice",
    "instructions": "What is this dispute mainly about, based on the `transcript`?",
    "criteria": {
      "payment_not_confirmed": "Whether the fiat payment was sent or received.",
      "wrong_amount": "The fiat payment arrived but for a different amount.",
      "wrong_account_or_method": "The payment was sent to a different account, person, or payment method than agreed.",
      "counterpart_unresponsive": "One party stopped answering during the trade.",
      "technical_problem": "An app, wallet, invoice, or Lightning problem rather than a payment disagreement.",
      "other": "Something else.",
      "not_yet_clear": "The parties have not said enough to tell."
    }
  }
}
```

### 2.2 Per-party questions (latest messages)

Asked once for each party with new messages, with `<party>` replaced by
`buyer` or `seller`.

```json
{
  "<party>_message_kind": {
    "type": "choice",
    "instructions": "Look only at the <party>'s messages listed in `latest.<party>`. What are they mainly doing?",
    "criteria": {
      "answers": "Giving information about the trade or the payment.",
      "greeting": "Only greeting, saying hello, or calling for attention.",
      "asks_language": "Asking whether Serbero speaks a particular language, or asking to switch language.",
      "not_understood": "Saying they do not understand the question or the situation.",
      "asks_next_step": "Asking what they should do or what happens next.",
      "other": "Anything else."
    }
  },
  "<party>_language": {
    "type": "choice",
    "instructions": "Which language does the <party> want to be addressed in? Use the language they write in, unless they explicitly ask for another one.",
    "criteria": {
      "en": "English",
      "es": "Spanish",
      "pt": "Portuguese",
      "other": "Another language",
      "unknown": "The messages are too short or mixed to tell."
    }
  },
  "<party>_wants_human": {
    "type": "noul",
    "instructions": "In the messages listed in `latest.<party>`, does the <party> explicitly ask to talk to a human person, a solver, an administrator, or support staff instead of the automated assistant?",
    "criteria": {
      "true": "A direct request for a person, such as 'I want a human' or 'let me talk to someone from support'.",
      "false": "No such request. Impatience or frustration alone is not a request."
    }
  }
}
```

The `<party>_language` options are generated from `[mediation].languages`,
each described by the English name in its catalog file
([spec.md §7.7](spec.md#77-languages)), so enabling a language adds it to the
question. The three languages above are the initial ones.

While the session is `guiding`, one more question is asked for each party with
new messages:

```json
{
  "<party>_rejects_path": {
    "type": "noul",
    "instructions": "In the messages listed in `latest.<party>`, does the <party> reject the way forward Serbero described, or deny what the other party reported?",
    "criteria": {
      "true": "For example, the buyer says they never got the bitcoin and disagrees, the seller says the payment did not actually arrive, or a party refuses to cancel.",
      "false": "The party agrees, asks a practical question, or says nothing against it."
    }
  }
}
```

### 2.3 Size

A turn request with a 20-message transcript is about 2,000–3,000 input tokens.
With the configured caps, the largest possible request stays far below the
64k context.

## 3. From answers to facts

Code converts raw answers into a `Facts` struct. For a `choice`, Serbero uses
the probability of the specific option it cares about, not just the winner,
as a share of the choice's total (an accepted distribution may sum to up to
1.01).

| Fact | Known when | Otherwise |
|---|---|---|
| `buyer_sent` | `P(says_sent) ≥ fact` | — |
| `buyer_not_sent` | `P(says_not_sent) ≥ fact` | — |
| `buyer_not_sent_for_guide` | `P(says_not_sent) ≥ guide` | — |
| `buyer_payment_unknown` | neither of the above | ask the buyer |
| `buyer_has_details` | `noul ≥ fact` | ask for details if `buyer_sent` |
| `seller_received` | `P(says_received) ≥ fact` | — |
| `seller_received_for_guide` | `P(says_received) ≥ guide` | — |
| `seller_not_received` | `P(says_not_received) ≥ fact` | — |
| `seller_receipt_unknown` | neither of the above, and not `outside_scope` | ask the seller |
| `seller_has_checked` | `noul ≥ fact` | ask to check if `seller_not_received` |
| `conflict` | `noul ≥ conflict` | — |
| `fraud` | `noul ≥ fraud` | — |
| `wants_human(p)` | `noul ≥ human_request` | — |
| `rejects_path(p)` | `noul ≥ conflict` (only while `guiding`) | — |
| `outside_scope` | `P(topic ∈ {wrong_amount, wrong_account_or_method, technical_problem, other}) ≥ outside_scope`, or `P(says_received_with_problem) ≥ outside_scope` | — |
| `language(p)` | winner ∈ `languages` and `P ≥ 0.7` | keep the current language |
| `message_kind(p)` | winner, if Serbero's computed `confidence ≥ 0.5` | treat as `answers` |

### Threshold rationale

| Threshold | Default | Cost of a false positive | Cost of a false negative |
|---|---|---|---|
| `guide` | 0.90 | A party is told about an option based on a statement they did not make | One more question, or a handoff |
| `fact` | 0.80 | Acting on a claim not made (wrong question or early handoff) | One more question |
| `human_request` | 0.80 | An early handoff to a human | The party asks again |
| `fraud` | 0.60 | A human looks at an honest case | A scam continues in automated mediation |
| `conflict` | 0.75 | A human sees a case a question could have clarified | One more round |
| `outside_scope` | 0.80 | A human handles a simple case | Serbero asks payment questions that do not fit |

`fact` and `guide` must be above 0.5 (config validation enforces it), so two
options of the same choice can never both be known.

Every false positive leads to a human, never to a fund action. Defaults are
starting points and are calibrated separately for each provider and model; [evaluation.md](evaluation.md) describes how to calibrate them.

## 4. Decision table

`policy::decide(session, facts, config) -> Action` is a pure function. Rows are
checked in order and the first match wins.

| # | Condition | Action |
|---|---|---|
| 1 | `wants_human(buyer)` or `wants_human(seller)` | `Handoff(human_requested)` |
| 2 | `fraud` | `Handoff(fraud_signal)` |
| 3 | `outside_scope` | `Handoff(outside_scope)` |
| 4 | State is `guiding` and `rejects_path(buyer)` or `rejects_path(seller)` | `Handoff(self_resolution_stalled)` |
| 5 | State is `guiding` | `Wait` (resolution or `self_resolution_timeout` ends it) |
| 6 | `seller_received_for_guide` | `Guide(PaymentArrived)` if both parties' languages are validated ([spec.md §7.7](spec.md#77-languages)), otherwise `Handoff(facts_gathered)` |
| 7 | `buyer_not_sent_for_guide` | `Guide(PaymentNotSent)` if both parties' languages are validated, otherwise `Handoff(facts_gathered)` |
| 8 | `buyer_sent` and `seller_not_received` and `conflict` and details and check already asked (or known) | `Handoff(conflicting_claims)` |
| 9 | `rounds ≥ max_rounds` | `Handoff(round_limit)` |
| 10 | Next questions (§4.1) produce at least one template | `Ask { … }` |
| 11 | A needed fact is still unknown after both of its variants were sent | `Handoff(uncertain)` |
| 12 | Both payment facts are known | `Handoff(facts_gathered)` |
| 13 | Otherwise | `Wait` (the response timeout covers silence) |

### 4.1 Next question for each party

Serbero only writes to a party who wrote in this turn, or to a party whose
needed question changed and who has no question outstanding.

1. **Message kind first.**
   - `asks_language`: send the last template again in the newly detected
     language. This is the only allowed repeat, and it does not count as a
     round.
   - `not_understood`: send the `_simple` variant of the last question.
   - `asks_next_step`: send `what_happens_next` (once per party), then continue
     to step 2.
   - `greeting` / `other`: continue to step 2; the needed question is sent as
     its `_simple` variant if the normal one was already sent.
2. **Needed fact.**

   | Party | Fact state | Template |
   |---|---|---|
   | buyer | payment unknown | `ask_buyer_sent`, then `ask_buyer_sent_simple` |
   | buyer | `buyer_sent`, no details | `ask_buyer_details` |
   | seller | receipt unknown | `ask_seller_received`, then `ask_seller_received_simple` |
   | seller | `seller_not_received`, not checked | `ask_seller_check_account` |
   | either | nothing needed | `thanks_waiting` (once per party) |

3. **Never repeat.** A template already sent to a party (in any language) is
   never sent to that party again, except for the language rule above. If the
   needed template was already used, fall through the table.

A round is counted whenever a turn sends at least one question template.

### 4.2 Timers

| Timer | Action |
|---|---|
| `response_timeout` after a question with no reply from that party | Send `reminder` (once per party) |
| `response_timeout` after the reminder | `Handoff(unresponsive)` |
| Party sends more than `max_messages_per_turn` twice | `Handoff(flood)` |
| `self_resolution_timeout` after a `Guide` without the dispute resolving | `Handoff(self_resolution_stalled)` |

Timers never call Jev.

## 5. Brief request

Sent once, when the action is `Handoff` or `Guide`, on the same
state as the last turn. Options are message ids from `transcript` plus `none`,
so every quote in the brief is a message a party actually wrote. Options are
limited to the relevant party's messages, at most the newest 100 of them (so a
long session stays below provider option limits); criteria are `null` because
the text lives in the state.

```json
{
  "quote_buyer_payment": {
    "type": "choice",
    "instructions": "Which message in `transcript` is the buyer's clearest statement about whether they sent the fiat payment?",
    "criteria": { "m4": null, "m7": null, "none": "No buyer message addresses this." }
  },
  "quote_buyer_details": {
    "type": "choice",
    "instructions": "Which message in `transcript` contains the buyer's most specific payment details?",
    "criteria": { "m4": null, "m7": null, "none": "No buyer message contains payment details." }
  },
  "quote_seller_receipt": {
    "type": "choice",
    "instructions": "Which message in `transcript` is the seller's clearest statement about whether the fiat payment arrived?",
    "criteria": { "m3": null, "m6": null, "none": "No seller message addresses this." }
  },
  "quote_concern": {
    "type": "choice",
    "instructions": "Which message in `transcript` is the strongest sign of bad faith, contradiction, or a problem a human should read first?",
    "criteria": { "m3": null, "m4": null, "m6": null, "m7": null, "none": "Nothing stands out." }
  },
  "evidence_balance": {
    "type": "score",
    "instructions": "Considering only what the parties wrote in `transcript`, how strongly does the conversation indicate that the buyer actually sent the fiat payment?",
    "criteria": [
      "The buyer says they did not pay, or the conversation clearly indicates no payment was made.",
      "The buyer's claim to have paid is vague or contradicted and unsupported by details.",
      "There is not enough information to lean either way.",
      "The buyer gives specific, consistent payment details, but the seller has not confirmed receipt.",
      "The seller confirms the payment arrived."
    ]
  }
}
```

A quote is included in the brief only if its option is not `none` and
`P ≥ fact`. `evidence_balance` is shown to the solver as an advisory number
with its distribution, labelled as a reading of the conversation and not a
verdict. It is never shown to the parties.

## 6. Versioning

- The question set is defined in `src/judge/questions.rs` in provider-neutral
  types and serialized to the JSON above. The `<party>_language` options are
  generated from `[mediation].languages`; the `en`, `es` and `pt` entries shown
  are the initial set. A test compares the code with the JSON blocks of §2.1
  and §2.2 of this document, so the two cannot drift apart.
- Every evaluation stores the question-set identifier:
  `QUESTION_SET_VERSION`, a dash, and the first 8 hex characters of the
  SHA-256 of the full set's canonical JSON (every question of §2.1 and §2.2,
  for both parties), for example `qs-1-1e7ce156` with `en`, `es` and `pt`.
  Each turn sends a subset of the full set under the full set's identifier.
  The rendered questions include the options generated from
  `[mediation].languages`, so enabling a language changes the identifier even
  though the version is unchanged; the order of the languages does not, since
  options are rendered keyed by code. Recorded answers and evaluation reports
  are keyed by this identifier and are never reused across language sets.
- The brief request (§5) has its own identifier, computed the same way over
  its fixed text: the instructions, the `none` descriptions and the
  `evidence_balance` levels, since its other options are the transcript's
  message ids. It is stored with every brief evaluation.
- A snapshot test lists every released version with its hash for `en`, `es`
  and `pt`, fixed in the test so that adding a catalog changes neither the
  snapshot nor the comparison with this document. The brief has its own
  snapshot, and a test compares it with §5 for the example transcript there. Changing any instruction, option, or criterion fails the test
  until the version is bumped, a line is added for it, and the golden set is
  re-run ([evaluation.md](evaluation.md)).
- Thresholds live in config, per provider and model, and are not part of the
  version. Their values are
  logged with each evaluation's action.
