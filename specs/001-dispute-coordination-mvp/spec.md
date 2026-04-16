# Feature Specification: Dispute Coordination MVP

**Feature Branch**: `001-dispute-coordination-mvp`
**Created**: 2026-04-16
**Status**: Draft
**Input**: User description: "Initial Cancerbero specification — dispute detection, operator notification, assignment visibility, guided mediation, and escalation support for the Mostro ecosystem"

## User Scenarios & Testing

### User Story 1 - Operator Receives Dispute Notification (Priority: P1)

A Mostro operator is away from their desk when a buyer opens a
dispute on a peer-to-peer Bitcoin trade. Within seconds, the operator
receives an encrypted Nostr direct message (gift wrap) from Cancerbero
informing them that a new dispute requires attention. The notification
includes the dispute identifier, the order identifier, a brief
summary of what triggered the dispute, and a timestamp. The operator
can then decide whether to take the dispute or let another operator
handle it.

**Why this priority**: Without reliable operator notification,
disputes go unattended. This is the foundation of Cancerbero's value
and a core constitutional responsibility (Principle IV).

**Independent Test**: Can be fully tested by creating a dispute event
on a Nostr relay and verifying that the configured operator receives
an encrypted notification containing the correct dispute metadata.

**Acceptance Scenarios**:

1. **Given** a new dispute event is published to the Mostro relay,
   **When** Cancerbero detects the dispute event,
   **Then** all configured operators with read permissions receive an
   encrypted gift-wrap notification within 30 seconds containing the
   dispute ID, order ID, dispute reason, and timestamp.

2. **Given** a dispute notification was sent but no operator has taken
   the dispute within a configurable timeout (default: 5 minutes),
   **When** the timeout elapses,
   **Then** Cancerbero re-notifies the same operators and, if
   configured, escalates to write-permission operators with a message
   indicating the dispute is unattended.

3. **Given** Cancerbero has already processed a dispute event,
   **When** the same dispute event is received again (duplicate or
   relay replay),
   **Then** Cancerbero does not send a duplicate notification.

---

### User Story 2 - Dispute Intake and Assignment Visibility (Priority: P2)

An operator receives a dispute notification and wants to understand
the current state of the dispute before deciding to take it. Cancerbero
tracks dispute lifecycle states (new, notified, taken, being-assisted,
waiting, escalated) and can inform operators about the current status
when queried. When an operator or another solver takes the dispute
via Mostro, Cancerbero detects the assignment and updates its internal
state accordingly.

**Why this priority**: Operators need to know whether a dispute is
already being handled before they intervene. Without visibility,
multiple operators may work the same dispute or disputes may fall
through the cracks.

**Independent Test**: Can be tested by creating a dispute, verifying
Cancerbero tracks it as "new," then simulating an operator taking
the dispute via Mostro and verifying Cancerbero transitions its
state to "taken."

**Acceptance Scenarios**:

1. **Given** Cancerbero has detected a new dispute,
   **When** an operator queries the dispute status,
   **Then** Cancerbero responds with the current state (new, notified,
   taken, being-assisted, waiting, or escalated), the dispute
   identifier, order identifier, creation time, and time elapsed
   since creation.

2. **Given** a dispute is in "notified" state,
   **When** an operator or solver takes the dispute in Mostro,
   **Then** Cancerbero detects the assignment event and transitions
   the dispute state to "taken," recording the solver's identity.

3. **Given** a dispute is in any active state,
   **When** an operator requests dispute status,
   **Then** Cancerbero provides only the minimum information necessary
   for the requesting operator's role, without exposing private
   details of the parties beyond what is required for resolution.

---

### User Story 3 - Basic Guided Mediation (Priority: P3)

A buyer opens a dispute because the seller has not confirmed fiat
payment receipt. This is a common coordination failure — the seller
may simply be delayed or confused about the process. Cancerbero
contacts both parties via encrypted Nostr messages, asks clarifying
questions (e.g., "Has the fiat payment been sent?", "Can you confirm
you received the payment?"), and attempts to guide them toward a
cooperative resolution. Cancerbero communicates that it is an
assistance system, not the final authority, and that a human operator
will review the case if needed.

**Why this priority**: Many disputes are simple coordination failures
that can be resolved without operator intervention if parties are
guided promptly. This reduces operator workload while preserving
human oversight for complex cases.

**Independent Test**: Can be tested by simulating a dispute where
both parties are responsive, verifying Cancerbero sends appropriate
clarifying messages, collects responses, and either guides toward
resolution or escalates when the situation is unclear.

**Acceptance Scenarios**:

1. **Given** a dispute is classified as a potential coordination
   failure (e.g., payment delay, unresponsive counterparty,
   confusion about next steps),
   **When** Cancerbero begins assisted mediation,
   **Then** it contacts both parties via encrypted gift-wrap messages,
   identifies itself as an assistance system (not the final authority),
   and asks targeted clarifying questions.

2. **Given** both parties respond cooperatively and the facts align
   (e.g., seller confirms payment was received),
   **When** Cancerbero determines the dispute appears resolvable,
   **Then** it summarizes the situation for both parties, suggests
   the cooperative next step, and notifies the assigned operator
   with a summary recommending resolution — but does NOT execute
   any settlement action itself.

3. **Given** one or both parties are unresponsive after a
   configurable timeout,
   **When** the mediation window expires,
   **Then** Cancerbero escalates to a human operator with a summary
   of what was attempted, what responses were received, and what
   remains unresolved.

4. **Given** party claims materially conflict, fraud indicators are
   detected, or the reasoning backend returns low confidence,
   **When** Cancerbero evaluates the dispute,
   **Then** it immediately escalates to a write-permission operator
   without attempting autonomous resolution, including a structured
   summary of the conflicting claims and the basis for escalation.

---

### User Story 4 - Escalation to Human Operator (Priority: P3)

A dispute involves conflicting claims — the buyer says payment was
sent but the seller denies receiving it. Cancerbero recognizes that
the facts conflict, classifies this as requiring human judgment, and
escalates to a write-permission operator. The escalation message
includes a structured summary: dispute timeline, what each party
claimed, what evidence is available, what Cancerbero attempted, and
why it is escalating.

**Why this priority**: Escalation is the safety net that ensures
complex, adversarial, or ambiguous disputes reach a qualified human.
This is constitutionally mandated (Principle III).

**Independent Test**: Can be tested by simulating a dispute with
conflicting party claims and verifying that Cancerbero sends a
structured escalation message to a write-permission operator
containing all required summary fields.

**Acceptance Scenarios**:

1. **Given** a dispute meets any escalation trigger (conflicting
   claims, suspected fraud, low confidence, unresponsive parties,
   mediation timeout, or policy-mandated escalation),
   **When** Cancerbero initiates escalation,
   **Then** it sends an encrypted notification to at least one
   write-permission operator containing: dispute ID, order ID,
   dispute timeline, party claims summary, evidence available,
   mediation actions taken, reason for escalation, and confidence
   assessment.

2. **Given** Cancerbero escalates a dispute,
   **When** the escalation message is sent,
   **Then** Cancerbero transitions the dispute state to "escalated"
   and ceases autonomous mediation activity on that dispute.

3. **Given** no write-permission operator acknowledges the escalation
   within a configurable timeout,
   **When** the timeout elapses,
   **Then** Cancerbero re-sends the escalation notification with
   increased urgency marking.

---

### User Story 5 - Reasoning Backend Abstraction (Priority: P3)

The system administrator deploys Cancerbero with a direct API-based
reasoning backend (e.g., Claude API) for dispute classification,
mediation message generation, and escalation decisions. Later, they
want to switch to an OpenClaw-based backend or a self-hosted model.
The reasoning backend is behind a defined interface, so the switch
requires only configuration changes and a new backend adapter —
no changes to Cancerbero's core policy, notification, or escalation
logic.

**Why this priority**: Architectural portability ensures Cancerbero
is not locked into any single provider and supports diverse
deployment scenarios. This is constitutionally mandated (Principle X).

**Independent Test**: Can be tested by running the same dispute
scenario against two different reasoning backend implementations
and verifying that Cancerbero's policy layer produces consistent
escalation and notification behavior regardless of which backend
generated the classification.

**Acceptance Scenarios**:

1. **Given** Cancerbero is configured with a direct API-based
   reasoning backend,
   **When** a dispute requires classification or mediation support,
   **Then** the reasoning request is routed through a defined
   interface that accepts structured input (dispute context, party
   messages, available evidence) and returns structured output
   (classification, confidence, suggested actions, reasoning trace).

2. **Given** the reasoning backend returns a classification and
   suggested action,
   **When** Cancerbero's policy layer receives the output,
   **Then** the policy layer independently validates the suggestion
   against escalation rules and operator-routing policies before
   acting — the reasoning backend's suggestion is advisory, not
   authoritative.

3. **Given** the reasoning backend is unavailable or returns an error,
   **When** Cancerbero cannot obtain a classification,
   **Then** it falls back to immediate operator escalation with a
   note that automated classification was unavailable, and continues
   to fulfill notification responsibilities.

---

### Edge Cases

- What happens when the Nostr relay is unreachable during
  notification? Cancerbero retries with exponential backoff and logs
  the failure. If retries are exhausted, the dispute is marked as
  "notification-failed" and included in the next escalation cycle.

- What happens when Cancerbero restarts and there are disputes it
  previously detected but did not finish processing? On startup,
  Cancerbero reconciles its local state against current dispute
  events from the relay and resumes processing for any disputes
  still in an active state.

- What happens when a dispute is resolved in Mostro while Cancerbero
  is mid-mediation? Cancerbero detects the resolution event,
  transitions the dispute to a terminal state, and ceases mediation.

- What happens when the reasoning backend returns a classification
  that contradicts Cancerbero's policy rules (e.g., suggests
  settlement when escalation is required)? The policy layer overrides
  the suggestion — reasoning output is advisory only.

- What happens when both parties send messages simultaneously during
  mediation? Cancerbero processes messages in received order and
  maintains a coherent conversation state per dispute.

## Requirements

### Functional Requirements

- **FR-001**: Cancerbero MUST subscribe to Mostro's dispute-related
  Nostr events and detect new disputes within 30 seconds of event
  publication.

- **FR-002**: Cancerbero MUST send encrypted gift-wrap notifications
  to all configured operators when a new dispute is detected.

- **FR-003**: Cancerbero MUST de-duplicate dispute events so that the
  same dispute does not trigger multiple notification cycles.

- **FR-004**: Cancerbero MUST re-notify operators when a dispute
  remains unattended beyond a configurable timeout threshold.

- **FR-005**: Cancerbero MUST track dispute lifecycle states: new,
  notified, taken, being-assisted, waiting, escalated, resolved.

- **FR-006**: Cancerbero MUST detect when an operator or solver takes
  a dispute in Mostro and update its internal state accordingly.

- **FR-007**: Cancerbero MUST communicate with dispute parties via
  encrypted gift-wrap messages during guided mediation.

- **FR-008**: Cancerbero MUST identify itself as an assistance system
  in all user-facing messages and MUST NOT present itself as the
  final dispute authority.

- **FR-009**: Cancerbero MUST escalate to a write-permission operator
  when: party claims conflict, fraud is suspected, confidence is low,
  parties are unresponsive beyond timeout, or policy mandates human
  review.

- **FR-010**: Escalation messages MUST include: dispute ID, order ID,
  timeline, party claims summary, evidence available, mediation
  actions taken, escalation reason, and confidence assessment.

- **FR-011**: Cancerbero MUST route reasoning requests through a
  defined backend interface that accepts structured input and returns
  structured output.

- **FR-012**: Cancerbero's policy layer MUST independently validate
  all reasoning backend suggestions before acting on them.

- **FR-013**: Cancerbero MUST fall back to immediate operator
  escalation when the reasoning backend is unavailable.

- **FR-014**: Cancerbero MUST NOT execute or sign `admin-settle`,
  `admin-cancel`, or any action that moves funds or closes disputes.

- **FR-015**: Cancerbero MUST log all actions, classifications,
  messages sent, and state transitions for audit purposes.

- **FR-016**: Cancerbero MUST expose only the minimum information
  necessary to each participant and operator, scoped to their role
  and the dispute context.

- **FR-017**: Cancerbero MUST reconcile its dispute state against
  relay events on startup to recover from restarts or crashes.

- **FR-018**: Cancerbero MUST support configurable operator lists
  with role distinctions (read-permission vs. write-permission
  operators).

### Key Entities

- **Dispute**: Represents an active dispute detected by Cancerbero.
  Key attributes: dispute ID, order ID, creation timestamp, current
  state, parties involved, assigned solver (if any), escalation
  history, mediation transcript references.

- **Operator**: A human dispute resolver registered with Cancerbero.
  Key attributes: Nostr public key, permission level (read or write),
  notification preferences, availability status.

- **MediationSession**: A guided mediation interaction for a specific
  dispute. Key attributes: dispute reference, messages exchanged,
  party responses, classification result, confidence score, outcome
  (resolved suggestion, escalated, timed out).

- **ReasoningRequest**: A structured request sent to the reasoning
  backend. Key attributes: dispute context, party messages, evidence
  summary, requested action (classify, suggest mediation response,
  generate escalation summary).

- **ReasoningResponse**: Structured output from the reasoning backend.
  Key attributes: classification, confidence score, suggested actions,
  reasoning trace, flags (fraud-risk, conflicting-claims, low-info).

- **AuditEntry**: A log record of any Cancerbero action. Key
  attributes: timestamp, dispute reference, action type, actor,
  input data summary, output data summary, reasoning trace reference.

## Success Criteria

### Measurable Outcomes

- **SC-001**: Operators receive dispute notifications within 30
  seconds of dispute creation at least 99% of the time when relays
  are reachable.

- **SC-002**: No dispute goes unnotified for more than the configured
  re-notification timeout (default: 5 minutes) without at least one
  re-notification attempt.

- **SC-003**: At least 30% of coordination-type disputes (payment
  delays, unresponsive counterparties, process confusion) reach a
  cooperative resolution suggestion without requiring direct operator
  intervention.

- **SC-004**: 100% of disputes involving conflicting claims, fraud
  indicators, or low-confidence classifications are escalated to a
  write-permission operator — zero autonomous closures.

- **SC-005**: Operators receiving escalation summaries can understand
  the dispute context and Cancerbero's assessment within 2 minutes
  of reading the summary.

- **SC-006**: Mostro continues to operate normally (disputes can be
  resolved manually by operators) when Cancerbero is offline or
  unavailable.

- **SC-007**: Switching the reasoning backend requires only
  configuration changes and a new adapter implementation — no
  modifications to Cancerbero's core policy, notification, or
  escalation logic.

- **SC-008**: All Cancerbero actions, classifications, and state
  transitions are retrievable from audit logs for any dispute.

## Assumptions

- Mostro publishes dispute-related events to Nostr relays that
  Cancerbero can subscribe to. The event format and kinds are
  defined by Mostro's existing protocol.

- Operators have Nostr key pairs and can receive encrypted gift-wrap
  messages via a Nostr client.

- The operator list (public keys and permission levels) is provided
  to Cancerbero via configuration, not dynamically discovered.

- The initial deployment uses a single Cancerbero instance (no
  multi-instance coordination or leader election in v1).

- The reasoning backend (direct API) is available via HTTPS and
  supports structured input/output. Specific API provider selection
  is a deployment-time configuration choice.

- Dispute events on the relay contain sufficient metadata (dispute
  ID, order ID, parties, reason) for Cancerbero to begin processing
  without additional Mostro API calls in the common case.

- Cancerbero uses local persistent storage (e.g., SQLite or
  equivalent) for dispute state tracking and audit logs. The storage
  engine choice is an implementation decision.

- Gift-wrap message construction and decryption are handled via
  nostr-sdk v0.44.1 primitives.

## Technical Constraints

- **TC-001**: Cancerbero MUST be implemented in Rust.

- **TC-002**: Cancerbero MUST use nostr-sdk v0.44.1 for all
  Nostr-related communication, subscriptions, event handling, and
  gift-wrap messaging flows. This is a fixed project constraint, not
  a replaceable assumption.

## Explicit Non-Goals

The following are explicitly out of scope for this first
specification:

- **Autonomous dispute closure**: Cancerbero MUST NOT close disputes.
  It may suggest resolution to operators but never execute it.

- **Fund movement**: Cancerbero MUST NOT sign or send `admin-settle`,
  `admin-cancel`, or interact with Lightning or escrow systems.

- **Mandatory OpenClaw dependency**: OpenClaw is an optional backend.
  The system MUST work without it.

- **Rich dashboards**: No web UI or dashboard is required for v1.
  All interaction is Nostr-native.

- **Advanced group-chat coordination**: Multi-operator group
  coordination channels are out of scope for v1.

- **Mostro dependency on Cancerbero**: Mostro MUST NOT require
  Cancerbero for dispute closure or any critical path operation.

## System Boundaries

### Mostro Owns

- Escrow state and fund custody
- `admin-settle` and `admin-cancel` execution
- Dispute-closing authority
- Operator permission enforcement (read/write solver roles)
- Order lifecycle management

### Cancerbero Owns

- Dispute detection and monitoring
- Operator notification and re-notification
- Dispute intake tracking and assignment visibility
- Guided mediation communication with parties
- Escalation decisions and escalation summaries
- Reasoning backend orchestration
- Audit logging of its own actions

### Boundary Rules

- Cancerbero reads dispute state from Mostro/Nostr events but never
  writes dispute-closing actions.
- Cancerbero may suggest outcomes to operators but operators act
  through Mostro, not through Cancerbero.
- If Cancerbero is offline, Mostro and its operators continue to
  resolve disputes manually as they do today.
