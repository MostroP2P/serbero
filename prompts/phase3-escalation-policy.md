# Phase 3 Escalation Policy

## Scope

Defines when a session MUST transition to escalation_recommended and
what evidence the Phase 4 handoff package contains.

## Triggers

Each heading lists the runtime-serialized snake_case identifier
(emitted into `mediation_events.payload_json`, escalation notices, and
tracing logs) followed by the Rust enum variant in parentheses. The
serialized form is canonical; the enum name is the cross-reference.

- **`conflicting_claims`** (`ConflictingClaims`): ConflictingClaims flag
  present. Evidence: rationale id from the flagging classification.
- **`fraud_indicator`** (`FraudIndicator`): FraudRisk flag present.
  Evidence: rationale id.
- **`low_confidence`** (`LowConfidence`): Confidence below 0.5.
  Evidence: rationale id, confidence score.
- **`party_unresponsive`** (`PartyUnresponsive`): No response within
  `party_response_timeout_seconds`. Evidence: session last-seen
  timestamps.
- **`round_limit`** (`RoundLimit`): `round_count >= max_rounds`.
  Evidence: round count, last inbound event id.
- **`reasoning_unavailable`** (`ReasoningUnavailable`): Provider error
  on classify/summarize. Evidence: `error_category` from
  `reasoning_call_failed` event.
- **`authorization_lost`** (`AuthorizationLost`): Outbound auth failure
  mid-session. Evidence: error message.
- **`authority_boundary_attempt`** (`AuthorityBoundaryAttempt`): Model
  output instructed fund-moving or dispute-closing action. Suppressed
  and escalated. Evidence: rationale id (full text in audit store, not
  event payload — FR-120).
- **`mediation_timeout`** (`MediationTimeout`): Session exceeded
  overall time limit. Evidence: timestamps. (Reserved; not currently
  wired.)
- **`policy_bundle_missing`** (`PolicyBundleMissing`): Prompt bundle
  unloadable. Evidence: bundle id, error. (Handled at startup.)
- **`invalid_model_output`** (`InvalidModelOutput`): Structurally
  inconsistent response (e.g., Summarize + non-cooperative label).
  Evidence: rationale id.
- **`notification_failed`** (`NotificationFailed`): Summary/escalation
  notification undeliverable. Evidence: notification error.

## Handoff Package

Every escalation produces a handoff_prepared event containing:
dispute_id, session_id, trigger, evidence_refs, rationale_refs,
prompt_bundle_id, policy_hash, assembled_at. Phase 4 consumes this;
Phase 3 does NOT execute the escalation.
