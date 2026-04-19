# Phase 3 Escalation Policy

## Scope

Defines when a session MUST transition to escalation_recommended and
what evidence the Phase 4 handoff package contains.

## Triggers

- **ConflictingClaims**: ConflictingClaims flag present. Evidence:
  rationale id from the flagging classification.
- **FraudIndicator**: FraudRisk flag present. Evidence: rationale id.
- **LowConfidence**: Confidence below 0.5. Evidence: rationale id,
  confidence score.
- **PartyUnresponsive**: No response within party_response_timeout_seconds.
  Evidence: session last-seen timestamps.
- **RoundLimit**: round_count >= max_rounds. Evidence: round count,
  last inbound event id.
- **ReasoningUnavailable**: Provider error on classify/summarize.
  Evidence: error_category from reasoning_call_failed event.
- **AuthorizationLost**: Outbound auth failure mid-session. Evidence:
  error message.
- **AuthorityBoundaryAttempt**: Model output instructed fund-moving
  or dispute-closing action. Suppressed and escalated. Evidence:
  rationale id (full text in audit store, not event payload — FR-120).
- **MediationTimeout**: Session exceeded overall time limit. Evidence:
  timestamps. (Reserved; not currently wired.)
- **PolicyBundleMissing**: Prompt bundle unloadable. Evidence: bundle
  id, error. (Handled at startup.)
- **InvalidModelOutput**: Structurally inconsistent response (e.g.,
  Summarize + non-cooperative label). Evidence: rationale id.
- **NotificationFailed**: Summary/escalation notification undeliverable.
  Evidence: notification error.

## Handoff Package

Every escalation produces a handoff_prepared event containing:
dispute_id, session_id, trigger, evidence_refs, rationale_refs,
prompt_bundle_id, policy_hash, assembled_at. Phase 4 consumes this;
Phase 3 does NOT execute the escalation.
