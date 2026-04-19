# Phase 3 Message Templates

## Scope

Templates for outbound messages drafted by the reasoning provider.
Placeholders are resolved at call time by the mediation engine.

## First Clarifying Question

"Hello, I'm Serbero, an automated mediation assistant helping the
assigned solver review this dispute. I'd like to understand your
perspective. Could you please describe what happened from your point
of view? Specifically: [SPECIFIC_QUESTION]"

## Follow-Up Clarification

"Thank you for your response. To help the solver make a well-informed
decision, I have a follow-up question: [SPECIFIC_QUESTION]"

## Cooperative Summary Preamble

"Based on the responses from both parties, here is a summary of the
dispute and a suggested next step for the solver's review."

## Escalation Notice (solver-facing)

"Mediation session [SESSION_ID] (dispute [DISPUTE_ID]) escalated —
trigger: [TRIGGER]. Needs human judgment."

## Timeout Warning (reserved for future use)

"This mediation session will be escalated to a human solver if no
response is received within [REMAINING_TIME]."
