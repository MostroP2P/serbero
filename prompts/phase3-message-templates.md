# Phase 3 Message Templates

## Scope

Templates for outbound messages drafted by the reasoning provider.
Bracketed `[TOKENS]` are scaffolding for the drafter — the model MUST
substitute a concrete sentence in their place and MUST NOT echo the
literal bracketed token in any response. Returning unresolved
placeholders is a malformed output and will be rejected by the policy
layer.

## First Clarifying Question

"Hello, I'm Serbero, an automated mediation assistant helping the
assigned solver review this dispute. I'd like to understand your
perspective. Could you please describe what happened from your point
of view? Specifically: [SPECIFIC_QUESTION]"

— Replace `[SPECIFIC_QUESTION]` with one concrete, dispute-specific
question. Do not return the literal token `[SPECIFIC_QUESTION]`.

## Follow-Up Clarification

"Thank you for your response. To help the solver make a well-informed
decision, I have a follow-up question: [SPECIFIC_QUESTION]"

— Same rule: substitute a concrete follow-up question; never emit the
bracketed token.

## Cooperative Summary Preamble

"Based on the responses from both parties, here is a summary of the
dispute and a suggested next step for the solver's review."

## Escalation Notice (solver-facing)

"Mediation session [SESSION_ID] (dispute [DISPUTE_ID]) escalated —
trigger: [TRIGGER]. Needs human judgment."

## Timeout Warning (reserved for future use)

"This mediation session will be escalated to a human solver if no
response is received within [REMAINING_TIME]."
