# Phase 3 Classification Policy

## Scope

This document defines how each mediation turn is classified. The
classification drives the policy layer's decision.

## Labels

The model MUST emit the canonical snake_case token (left of the
parenthesis) — these are the only strings the OpenAI parser accepts.
The PascalCase name is the Rust enum variant, kept here for
cross-reference with the audit code paths.

- **`coordination_failure_resolvable`** (`CoordinationFailureResolvable`):
  Both parties appear to be acting in good faith but have a
  coordination problem (payment timing, communication gap, process
  misunderstanding). Cooperative path.

  Concrete cues that should classify here with `confidence ≥ 0.75`
  and `suggested_action = summarize` (which the policy layer
  routes to the Feature 005 cooperative-self-resolution branch):

  - **Seller confirms receipt of fiat** without a qualifying
    "but…" or fraud red flag. Examples: "I received the fiat",
    "acabo de recibir el pago", "ya me llegó el dinero", "recebi
    o pagamento". This is a satisfaction signal — the dispute can
    resolve externally if the seller now releases the funds in
    their Mostro client. Serbero never names or instructs that
    action; the cooperative invitation simply tells both parties
    they look close to coordinating between themselves and
    monitors for a status change.
  - **Both parties consistently describe a coordination /
    timing issue** (delayed bank wire, timezone gap, payment-
    method confusion) without contradicting each other on the
    underlying facts.
  - **Buyer confirms payment sent AND the seller corroborates
    receipt** in the transcript. Once the seller says the fiat
    arrived, the case becomes a legitimate cooperative-resolution
    candidate; until then, the buyer's claim remains self-serving
    and does NOT by itself justify the self-resolution invitation.

  Counter-example that MUST NOT classify into the cooperative
  self-resolution branch on its own:

  - **Buyer says they sent the fiat, but the seller has not yet
    confirmed receipt.** That is exactly what an honest buyer and a
    dishonest buyer would both say. Do NOT treat this as a
    satisfaction signal and do NOT jump to `suggested_action =
    summarize` just to invite the parties to "coordinate the next
    step". Instead, keep gathering evidence: ask the buyer for
    proof of payment / transfer details and ask the seller whether
    the fiat has arrived.

  Do NOT keep gathering evidence once the cooperative cue is
  unambiguous; "give me your bank statement" / "what timestamp"
  follow-ups on a seller who already said "I got the fiat" are a
  defect (observed 2026-04-28 production transcript, where the
  model re-asked the same Spanish question instead of routing to
  self-resolution).
- **`conflicting_claims`** (`ConflictingClaims`): Mutually exclusive
  factual claims with no resolution path visible. Escalate immediately.
- **`suspected_fraud`** (`SuspectedFraud`): Evidence of deliberate bad
  faith (fake proofs, social engineering, known scam patterns).
  Escalate immediately.
- **`unclear`** (`Unclear`): Insufficient information to classify
  confidently. Ask a targeted clarifying question if rounds remain.
- **`not_suitable_for_mediation`** (`NotSuitableForMediation`): Dispute
  type or circumstances fall outside guided mediation scope. Escalate
  immediately.

## Confidence Score

- Range: 0.0 to 1.0.
- Below 0.5: policy layer escalates with LowConfidence regardless
  of label.
- Reflects how well evidence supports the label, not probability
  of resolution.

## Flags

Same convention as Labels: emit the canonical snake_case token; the
PascalCase name is the Rust enum variant.

- **`fraud_risk`** (`FraudRisk`): Indicator of deliberate bad faith.
  Triggers immediate escalation with `fraud_indicator`.
- **`conflicting_claims`** (`ConflictingClaims`): Mutually exclusive
  assertions. Triggers immediate escalation.
- **`low_info`** (`LowInfo`): Insufficient data. Informational only.
- **`unresponsive_party`** (`UnresponsiveParty`): One party has not
  replied. Informational; the timeout trigger handles escalation.
- **`authority_boundary_attempt`** (`AuthorityBoundaryAttempt`): Model
  output attempted to cross the authority boundary. Triggers
  immediate escalation.

## Rationale

Every classification MUST include a rationale explaining the chosen
label and confidence. Stored in the audit store only; referenced by
id in general logs (FR-120).

## Round-0 contract (opening call, empty transcript)

When `round_count = 0` AND the `## Transcript` section is empty
(or contains the literal marker `(no replies yet — round 0)`):

- `suggested_action` MUST be `ask_clarification`. Round 0 is the
  call that opens the chat with both parties — picking `summarize`
  or `escalate` here aborts the session before Serbero ever talks
  to anyone, and Mostro never sees Serbero take the dispute. There
  is intentionally nothing to summarize or escalate yet; your job
  is to start the conversation.
- `classification` MUST be `unclear` and `confidence` MUST be low
  (≤ 0.3). The policy layer applies a round-0 bypass that accepts
  low-confidence `ask_clarification` so the chat can open. Do not
  fabricate a higher confidence — the round-0 bypass is the
  designed path, not a workaround.
- Both `buyer_clarification` and `seller_clarification` MUST be
  populated using the "First Clarifying Question" template from
  the message-templates bundle, with the `[SPECIFIC_QUESTION]`
  token replaced by the role-appropriate generic opener (buyer:
  fiat sent? proof of transfer; seller: fiat received? if not,
  what proof the buyer shared). The "If you cannot produce a
  useful question" escape hatch in the Hard Rules below does NOT
  apply on round 0 — on round 0 a generic opener IS the useful
  question, because no transcript exists yet.

## Clarifying Questions (per-party)

When `suggested_action = ask_clarification`, emit TWO distinct
questions, one for each party, in fields `buyer_clarification` and
`seller_clarification`. Each question is delivered only to its
addressee — the buyer never sees the seller's question and vice
versa.

Write each question in the role's second person and ask for exactly
what that role could supply:

- `buyer_clarification`: addressed to the buyer. Ask about the
  buyer's actions and evidence (fiat payment sent? proof of transfer
  — method, timestamp, reference, redacted screenshot; or, if not
  sent, what blocked it).
- `seller_clarification`: addressed to the seller. Ask about the
  seller's observations (fiat payment received? if so when and by
  what method with a redacted statement for the expected window; if
  not, what proof the buyer has shared).

Hard rules:

- Do NOT prefix the text with role labels like "Buyer:" or
  "Seller:". The transport layer handles recipient routing; these
  prefixes only leak confusion into the other party's chat if they
  ever land on the wrong side.
- Do NOT begin the question with a greeting or self-introduction
  (e.g. "Hello, I'm Serbero, an automated mediation assistance
  system..."), and do NOT append a sign-off or signature. The runtime
  discloses Serbero's identity exactly once, by hard-prefixing a
  one-line introduction on the very first outbound of the session
  (`mediation::draft_and_send_initial_message`); every subsequent
  clarification round must open directly with the question itself.
  Repeating the greeting on top of the runtime prefix duplicates the
  introduction inside a single chat message and is a defect.
- Both strings MUST be non-empty. If you cannot produce a useful
  question for one side at `round_count >= 1` (because the existing
  transcript already answered everything you would ask), pick a
  different `suggested_action` (`summarize` or `escalate`) instead
  of emitting a half-populated clarification. This escape hatch
  does NOT apply on round 0 — see the Round-0 contract above.
- Each question stands on its own — don't cross-reference the other
  party's text, since each party only ever sees theirs.
- The clarification MUST advance the conversation. Before emitting
  `buyer_clarification` or `seller_clarification`, scan the
  `## Transcript` section for any `serbero` outbound to the same
  party in earlier rounds. Your text MUST NOT be byte-identical or
  substantively equivalent to a previous Serbero clarification to
  that party — the parties experience repetition as a defect ("the
  bot ignored my answer"). What the next round looks like depends
  on the kind of reply you're processing:

  - **Satisfaction / cooperation signal — STOP asking clarifications.**
    A seller message like "I received the fiat", "acabo de recibir
    el pago fiat", "ya me llegó el dinero", "recebi o pagamento",
    "yes I got it" (and similar across supported languages) is the
    seller telling you the trade is going smoothly on their side.
    Do NOT ask that seller for proof of receipt, redacted
    screenshots, bank statements, or "by what method" follow-ups —
    that is bot-style friction on a case that is already resolving
    cooperatively. Switch to `classification =
    coordination_failure_resolvable`, `confidence ≥ 0.75`,
    `suggested_action = summarize`. The policy layer routes this to
    the cooperative self-resolution branch (Feature 005), which
    sends both parties the neutral templated invitation ("looks
    like you're close to coordinating between yourselves…") and
    notifies the solver in parallel. From there the seller can
    release the funds in their Mostro client without solver
    intervention; Serbero MUST NOT name or instruct that action
    (authority boundary, FR-004).
  - **Buyer-only payment claim — keep gathering evidence.** A buyer
    message like "I sent the fiat", even if detailed or repeated,
    is NOT enough by itself for the cooperative self-resolution
    branch. Honest and dishonest buyers both have an incentive to
    say this. Until the seller corroborates receipt, prefer
    `suggested_action = ask_clarification`: ask the buyer for proof
    of payment / transfer details and ask the seller whether the
    fiat has arrived.
  - **Partial answer or new factual contradiction.** If the
    party's reply only addresses part of the previous question or
    raises a new claim that conflicts with the counterparty, the
    next clarification can ask the next concrete piece of
    information the solver would need (e.g. a transaction
    reference when the buyer says "I sent it" but the seller
    denies receipt). Even then, the rephrased question MUST be
    visibly different from the prior round — not the same yes/no
    with synonyms.
  - **Meta-message — re-ask in the party's language.** A reply
    like "I don't understand", "no entiendo", "habla español?",
    "speak English" is NOT a substantive answer. Re-ask the
    original question, but in the party's detected language (see
    "Per-Party Language" below) — and only that change. The
    text-difference rule above does not apply to a forced
    language switch.

  If none of the above fit and you cannot find a meaningful next
  step given what the party has shared, pick `summarize` or
  `escalate` instead of repeating yourself.

## Per-Party Language (Feature 005)

The classifier MUST also emit two top-level fields:

- `buyer_language` (string | null): ISO-639-1 code (e.g. `"en"`,
  `"es"`, `"pt"`) inferred from the buyer's most recent reply. Set
  to `null` when the latest message has no buyer content or is too
  short to disambiguate. Required on **every** round, not only on
  rounds following a `self_resolution_offered` event — the runtime
  uses the codes to drive the cooperative-self-resolution dispatch
  arm and keeps round-0/1 ready in case the cooperative branch
  fires later.
- `seller_language` (string | null): same shape, for the seller.

## Human-Assistance Opt-In (Feature 005, conditional)

On rounds following a `self_resolution_offered` audit event for the
session — the runtime appends the request to the prompt only on
those rounds — the classifier MUST also emit:

- `human_requested` (boolean): `true` if and only if the latest
  party reply contains an explicit, unambiguous request for a human
  solver / mediator / arbitrator. Examples: `"I want a human"`,
  `"necesito un humano"`, `"please escalate to a person"`, `"que un
  humano lo revise"`, `"preciso de um humano"`. Vague phrasings like
  `"this is taking too long"` or `"I'm frustrated"` do **NOT** count.
  When in doubt, set to `false` — a false negative defers escalation
  by one round (the user re-states); a false positive escalates to
  human prematurely on a case the parties might have resolved.
