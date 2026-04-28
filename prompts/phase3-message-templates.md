# Phase 3 Message Templates

## Scope

Templates for outbound messages drafted by the reasoning provider.
Bracketed `[TOKENS]` are scaffolding for the drafter — the model MUST
substitute a concrete sentence in their place and MUST NOT echo the
literal bracketed token in any response. Returning unresolved
placeholders is a malformed output and will be rejected by the policy
layer.

## First Clarifying Question

"I'd like to understand your perspective. Could you please describe
what happened from your point of view? Specifically: [SPECIFIC_QUESTION]"

— Replace `[SPECIFIC_QUESTION]` with one concrete, dispute-specific
question. Do not return the literal token `[SPECIFIC_QUESTION]`.

The one-time "Hello, I'm Serbero, an automated mediation assistant
helping the assigned solver review this dispute. " self-introduction
is added by the runtime (`mediation::draft_and_send_initial_message`)
exactly once at session open, so the model MUST NOT repeat it inside
the clarification body. Repeating it produces a duplicated greeting in
a single chat message and is a defect.

## Follow-Up Clarification

"[SPECIFIC_QUESTION]"

— Substitute a concrete follow-up question and emit nothing else: no
greeting, no self-introduction, no "Thank you for your response"
preamble, no sign-off. Round 2+ messages travel through
`mediation::draft_and_send_followup_message`, which does NOT prefix a
greeting; the entire user-visible body is whatever the model returned.
Adding a preamble or signature here will surface verbatim in the
party's chat.

## Cooperative Summary Preamble

"Based on the responses from both parties, here is a summary of the
dispute and a suggested next step for the solver's review."

## Escalation Notice (solver-facing)

"Mediation session [SESSION_ID] (dispute [DISPUTE_ID]) escalated —
trigger: [TRIGGER]. Needs human judgment."

## Timeout Warning (reserved for future use)

"This mediation session will be escalated to a human solver if no
response is received within [REMAINING_TIME]."
