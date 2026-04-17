# Feature Specification: Phased Dispute Coordination

**Feature Branch**: `002-phased-dispute-coordination`
**Created**: 2026-04-16
**Status**: Draft
**Input**: User description: "Phased specification for Cancerbero — dispute coordination, notification, and assistance system for the Mostro ecosystem, evolving from always-on dispute listener through guided mediation and escalation support"

## System Vision

Cancerbero is a dispute coordination system for the Mostro ecosystem.
It is not a chatbot, a prompt wrapper, or a replacement for Mostro's
authority model. It is a Nostr-native coordination layer that helps
operators notice disputes quickly, understand who is handling them,
assist with safe low-risk resolution flows, and escalate to a human
operator when ambiguity, fraud risk, or lack of cooperation makes
automation inappropriate.

Cancerbero evolves in phases. Each phase adds a bounded set of
responsibilities while preserving the constitutional constraints
that govern the project. The phases are:

| Phase | Scope                                              | Depends On |
|-------|-----------------------------------------------------|------------|
| 1     | Always-on dispute listener and solver notification  | —          |
| 2     | Dispute intake and assignment visibility            | Phase 1    |
| 3     | Guided mediation for low-risk disputes              | Phase 2    |
| 4     | Escalation support for human write operators        | Phase 3    |
| 5     | Optional reasoning backend and richer coordination  | Phase 4    |

Across all phases, the following invariants hold:

- Cancerbero MUST NOT move funds, sign `admin-settle` or
  `admin-cancel`, or close disputes.
- Mostro owns escrow state, permissions, solver roles, and
  dispute-closing authority.
- Cancerbero owns monitoring, notification, coordination, assistance,
  escalation support, and its own internal coordination state.
- If Cancerbero is offline, Mostro and its operators continue to
  resolve disputes manually.
- Cancerbero MUST record sufficient audit information for operator
  oversight, debugging, and postmortem analysis.

## Clarifications

### Session 2026-04-16

- Q: What does "initiator" mean in notification content — pubkey, identity, or trade role? → A: Initiator means the trade role (buyer or seller) as provided in the `initiator` tag of Mostro's kind 38386 dispute event. The initiator's pubkey MUST NOT be included in notifications to preserve privacy. Solvers learn pubkeys only after taking the dispute via Mostro's `admin-took-dispute` flow.
- Q: Should Phase 1 queue failed notifications for later delivery or only log failures? → A: Phase 1 logs failures only, no queuing or retry. New disputes detected after reconnection are notified normally. Notification queuing may be introduced in later phases if needed.
- Q: How should Cancerbero behave when SQLite persistence fails during deduplication? → A: Cancerbero halts notifications when SQLite is unreadable and resumes when persistence recovers. Deduplication integrity is prioritized over delivery.
- Q: Should Phase 2 require solver status queries via encrypted Nostr DMs? → A: No. Phase 2 tracks state internally and surfaces it passively in re-notifications and assignment update notifications. No interactive query protocol is required.
- Q: Should success criteria avoid invented KPI-style percentages? → A: Yes. Success criteria are rewritten as testable behavioral pass/fail properties. No invented percentages without empirical basis.

## Technical Constraints

- **TC-001**: Cancerbero MUST be implemented in Rust.
- **TC-002**: Cancerbero MUST use nostr-sdk v0.44.1 for all
  Nostr-related communication, subscriptions, event handling, and
  gift-wrap messaging flows. This is a fixed project constraint.
- **TC-003**: Cancerbero MUST use SQLite as its internal database.
  This is a fixed initial persistence choice, not a placeholder for
  later abstraction. The initial phases MUST NOT introduce a generic
  storage abstraction layer or support for alternative SQL or non-SQL
  databases.

## User Scenarios & Testing

### User Story 1 - Solver Receives New Dispute Notification (Priority: P1)

A Mostro solver is not actively watching the relay when a buyer
opens a dispute on a peer-to-peer trade. Within seconds, the solver
receives an encrypted Nostr direct message from Cancerbero informing
them that a new dispute has been initiated and requires attention.
The notification includes the dispute identifier, the trade role of
the party who initiated the dispute (buyer or seller), the event
timestamp, and a short instruction. All registered solvers receive the same notification
simultaneously.

**Why this priority**: This is the foundational capability of
Cancerbero. Without reliable, prompt notification, disputes go
unnoticed. This is the core responsibility defined by Constitution
Principle IV and the entry point for all subsequent phases.

**Independent Test**: Can be fully tested by publishing a dispute
event with status `initiated` to a Nostr relay and verifying that
all configured solvers receive an encrypted gift-wrap notification
containing the correct dispute metadata within 30 seconds.

**Acceptance Scenarios**:

1. **Given** Cancerbero is running as a daemon connected to the
   configured Nostr relay(s),
   **When** a new dispute event with status `initiated` is published
   for the configured Mostro instance,
   **Then** Cancerbero detects the dispute and sends an encrypted
   gift-wrap notification to every registered solver within 30
   seconds, containing: dispute ID, initiator trade role (buyer or
   seller), event timestamp, and an instruction that a new dispute
   requires attention.

2. **Given** Cancerbero has already notified solvers about a dispute,
   **When** the same dispute event is received again (relay replay,
   reconnection, or duplicate),
   **Then** Cancerbero does not send a duplicate notification.

3. **Given** Cancerbero is restarted after a crash or planned
   restart,
   **When** it reconnects to the relay and receives dispute events
   that it already processed before the restart,
   **Then** it does not re-notify solvers for disputes it has
   already recorded in its internal database.

4. **Given** a relay disconnects unexpectedly,
   **When** Cancerbero detects the disconnection,
   **Then** it attempts to reconnect and resumes listening without
   manual intervention, logging the disconnection and reconnection
   events.

---

### User Story 2 - Solver Sees Dispute Assignment Status (Priority: P2)

After receiving a dispute notification, a solver wants to know
whether another solver has already taken the dispute before deciding
to act. Cancerbero tracks dispute lifecycle states and can inform
solvers whether a dispute is new, notified, taken by a specific
solver, waiting, or escalated. When a solver takes a dispute via
Mostro, Cancerbero detects the assignment and stops sending noisy
re-notifications for that dispute.

**Why this priority**: Without assignment visibility, multiple
solvers may unknowingly work the same dispute, or solvers may
hesitate to act because they cannot tell if someone else is already
handling it. This is the coordination layer that turns raw
notification into useful awareness.

**Independent Test**: Can be tested by creating a dispute, verifying
Cancerbero tracks it as "new," then simulating a solver taking the
dispute in Mostro and verifying Cancerbero transitions its state to
"taken" and suppresses further notifications for that dispute.

**Acceptance Scenarios**:

1. **Given** Cancerbero has detected and notified solvers about a
   dispute,
   **When** Cancerbero sends a re-notification or assignment update,
   **Then** the message includes the current lifecycle state
   (new, notified, taken, waiting, escalated, or resolved), the
   dispute identifier, and time elapsed since creation.

2. **Given** a dispute is in "notified" state,
   **When** a solver takes the dispute via Mostro,
   **Then** Cancerbero detects the assignment event, transitions the
   dispute to "taken" state, records the solver's public key, and
   suppresses further notifications for that dispute.

3. **Given** a dispute has been taken by a solver,
   **When** Cancerbero detects the assignment,
   **Then** Cancerbero sends an assignment notification to all
   registered solvers indicating the dispute is taken, without
   exposing private details of the dispute parties beyond what is
   necessary for coordination.

4. **Given** a dispute remains in "notified" state beyond a
   configurable timeout,
   **When** the timeout elapses and no solver has taken it,
   **Then** Cancerbero re-notifies solvers with a message indicating
   the dispute is still unattended.

---

### User Story 3 - Guided Mediation for Coordination Failure (Priority: P3)

A buyer opens a dispute because the seller has not confirmed fiat
payment receipt. This is a common coordination failure — the seller
may be delayed or confused about the process. Cancerbero contacts
both parties via encrypted Nostr messages, asks clarifying questions,
and attempts to guide them toward a cooperative resolution.
Cancerbero identifies itself as an assistance system, not the final
authority, and communicates that a human operator will review the
case if needed.

**Why this priority**: Many disputes are simple coordination failures
that can be resolved if parties are guided promptly. This reduces
solver workload while preserving human oversight for complex cases.

**Independent Test**: Can be tested by simulating a dispute where
both parties respond cooperatively, verifying Cancerbero sends
appropriate clarifying messages, collects responses, and either
suggests a cooperative resolution path or escalates.

**Acceptance Scenarios**:

1. **Given** a dispute is classified as a potential coordination
   failure (payment delay, unresponsive counterparty, confusion
   about next steps, anxiety-driven dispute),
   **When** Cancerbero begins guided mediation,
   **Then** it contacts both parties via encrypted gift-wrap messages,
   identifies itself as an assistance system (not the final
   authority), and asks targeted clarifying questions.

2. **Given** both parties respond cooperatively and the facts align,
   **When** Cancerbero determines the dispute appears resolvable,
   **Then** it summarizes the situation for both parties, suggests
   the cooperative next step, and notifies the assigned solver with
   a summary — but does NOT execute any settlement action itself.

3. **Given** one or both parties are unresponsive after a
   configurable timeout,
   **When** the mediation window expires,
   **Then** Cancerbero escalates to a human operator with a summary
   of what was attempted, what responses were received, and what
   remains unresolved.

4. **Given** information is incomplete or unclear during mediation,
   **When** Cancerbero generates messages or summaries,
   **Then** it surfaces uncertainty honestly and does not fabricate
   evidence or imply certainty it does not have.

---

### User Story 4 - Escalation to Write Operator (Priority: P3)

A dispute involves conflicting claims — the buyer says payment was
sent but the seller denies receiving it. Cancerbero recognizes that
the facts conflict, classifies this as requiring human judgment, and
escalates to a solver with write permissions. The escalation message
includes a structured summary: dispute timeline, what each party
claimed, what Cancerbero attempted, and why it is escalating.

**Why this priority**: Escalation is the safety net that ensures
complex, adversarial, or ambiguous disputes reach a qualified human.
This is constitutionally mandated (Principle III).

**Independent Test**: Can be tested by simulating a dispute with
conflicting party claims and verifying that Cancerbero sends a
structured escalation message to a write-permission solver.

**Acceptance Scenarios**:

1. **Given** a dispute meets any escalation trigger (conflicting
   claims, suspected fraud, low confidence, unresponsive parties,
   mediation timeout, or policy-mandated escalation),
   **When** Cancerbero initiates escalation,
   **Then** it sends an encrypted notification to at least one
   write-permission solver containing: dispute ID, dispute timeline,
   party claims summary, mediation actions taken, reason for
   escalation, and confidence assessment.

2. **Given** Cancerbero escalates a dispute,
   **When** the escalation message is sent,
   **Then** Cancerbero transitions the dispute state to "escalated"
   and ceases autonomous mediation activity on that dispute.

3. **Given** no write-permission solver acknowledges the escalation
   within a configurable timeout,
   **When** the timeout elapses,
   **Then** Cancerbero re-sends the escalation notification with
   increased urgency marking.

---

### User Story 5 - Reasoning Backend Portability (Priority: P3)

A system administrator deploys Cancerbero with a direct API-based
reasoning backend for dispute classification and mediation support.
Later, they want to switch to OpenClaw or a self-hosted model. The
reasoning backend is behind a defined interface, so the switch
requires configuration changes and a new backend adapter — no
changes to Cancerbero's core policy, notification, or escalation
logic.

**Why this priority**: Architectural portability ensures Cancerbero
is not locked into any single provider (Constitution Principle X).

**Independent Test**: Can be tested by running the same dispute
scenario against two different reasoning backend implementations and
verifying that Cancerbero's policy layer produces consistent behavior
regardless of which backend generated the classification.

**Acceptance Scenarios**:

1. **Given** Cancerbero is configured with a reasoning backend,
   **When** a dispute requires classification or mediation support,
   **Then** the reasoning request is routed through a defined
   interface that accepts structured input and returns structured
   output (classification, confidence, suggested actions, structured
   rationale).

2. **Given** the reasoning backend returns a suggestion,
   **When** Cancerbero's policy layer receives the output,
   **Then** the policy layer independently validates the suggestion
   against escalation rules before acting — the reasoning backend's
   output is advisory, not authoritative.

3. **Given** the reasoning backend is unavailable or returns an error,
   **When** Cancerbero cannot obtain a classification,
   **Then** it falls back to immediate operator escalation with a
   note that automated classification was unavailable.

---

### Edge Cases

- What happens when the Nostr relay is unreachable during
  notification? Cancerbero retries reconnection with backoff and
  logs the failure. In Phase 1, failed notifications are logged but
  not queued for retry. New disputes detected after reconnection are
  notified normally.

- What happens when Cancerbero restarts and there are disputes it
  previously detected but did not finish processing? On startup,
  Cancerbero checks its SQLite state against fresh relay events and
  resumes processing for any disputes still in an active state.

- What happens when a dispute is resolved in Mostro while Cancerbero
  is mid-mediation? Cancerbero detects the resolution event,
  transitions the dispute to a terminal state, and ceases mediation.

- What happens when the reasoning backend suggests settlement but
  escalation rules require human review? The policy layer overrides
  the suggestion — reasoning output is advisory only.

- What happens when two solvers attempt to take the same dispute
  simultaneously? Mostro enforces the assignment. Cancerbero
  observes whichever assignment event arrives and updates state
  accordingly.

- What happens when no solvers are configured? Cancerbero logs a
  warning at startup and does not attempt notifications. It still
  records detected disputes for audit purposes.

## Phase Definitions

### Phase 1: Always-On Dispute Listener and Solver Notification

**Scope**: A long-lived daemon that connects to Nostr, listens for
Mostro dispute events, detects newly initiated disputes, and
sends an encrypted notification to all registered solvers.

#### Continuous Dispute Listening

- Cancerbero MUST run as a long-lived daemon process.
- It MUST connect to the configured Nostr relay(s) on startup and
  maintain persistent subscriptions.
- It MUST subscribe to Mostro dispute events filtered to kind 38386,
  the configured Mostro instance's public key, and the required tags
  (`z` = `dispute`, `s` = `initiated`).
- If a relay connection drops, Cancerbero MUST attempt reconnection
  with backoff and resume listening without manual intervention.

#### New Dispute Detection

- A dispute is considered "new" when Cancerbero observes a dispute
  event with status `initiated` that it has not previously recorded
  in its SQLite database.
- Cancerbero MUST persist the dispute ID in SQLite upon first
  detection to prevent duplicate notifications across relay replays,
  reconnections, and process restarts.
- On restart, Cancerbero MUST consult its SQLite state to determine
  which disputes have already been processed, ensuring it does not
  re-notify for known disputes.

#### Solver Notification

- The set of solvers to notify MUST be provided via configuration
  (a list of Nostr public keys).
- Phase 1 MUST notify ALL registered solvers when a new dispute is
  detected — there is no selective routing or role-based filtering
  in this phase.
- Notifications MUST be sent as Nostr direct encrypted messages
  using the gift-wrap protocol.

#### Notification Content

Each notification MUST contain at minimum:

- Dispute ID (from the `d` tag of the kind 38386 event)
- Initiator trade role — buyer or seller (from the `initiator` tag).
  The initiator's pubkey MUST NOT be included in the notification to
  preserve privacy.
- Event timestamp
- A short instruction indicating that a new dispute requires
  attention

#### Internal Persistence (Phase 1)

- Cancerbero MUST use SQLite directly. No storage abstraction layer.
- Phase 1 MUST persist at minimum:
  - Detected dispute records (dispute ID, event ID, initiator role,
    dispute status, timestamp, detection time)
  - Notification attempts (solver public key, timestamp, success or
    failure status)
- This persistence ensures deduplication survives restarts and
  notification failures are recorded for debugging.

#### Failure Behavior (Phase 1)

- If a relay disconnects, Cancerbero MUST log the event and attempt
  reconnection. It MUST NOT crash or exit.
- If a notification to a specific solver fails (e.g., relay rejects
  the message), Cancerbero MUST log the failure. Phase 1 does not
  require automatic retries for individual notification failures,
  but failures MUST be recorded in SQLite for observability.
- If all relay connections fail, Cancerbero MUST continue attempting
  reconnection and log degraded-mode status.

#### Phase 1 Non-Goals

Phase 1 explicitly excludes:

- Guided mediation with users
- Operator assignment workflows beyond notification
- Escalation summaries
- Group Nostr notifications
- Dispute closure or any fund-related action
- Signing or sending `admin-settle` / `admin-cancel`
- Any Lightning interaction
- Any mandatory dependency on OpenClaw
- Storage backend abstraction
- Support for alternative SQL or non-SQL databases
- Re-notification or timeout-based follow-up (deferred to Phase 2)

---

### Phase 2: Dispute Intake and Assignment Visibility

**Scope**: Extends Phase 1 with dispute lifecycle tracking,
assignment detection, re-notification for unattended disputes,
and solver-facing status queries.

#### Dispute Lifecycle States

Phase 2 introduces the following states:

- **new**: Dispute event detected, not yet notified.
- **notified**: Solvers have been notified.
- **taken**: A solver has claimed the dispute via Mostro.
- **waiting**: Dispute is in progress but awaiting action.
- **escalated**: Dispute has been escalated to a write-permission
  solver (used in later phases, tracked here for schema readiness).
- **resolved**: Dispute has been resolved in Mostro.

State transitions MUST be persisted in SQLite with timestamps.

#### Assignment Detection

- Cancerbero MUST subscribe to Mostro events that indicate a solver
  has taken a dispute.
- When an assignment event is detected, Cancerbero MUST transition
  the dispute to "taken" state and record the solver's public key.
- Once a dispute is taken, Cancerbero MUST suppress further
  notifications for that dispute.

#### Re-Notification

- If a dispute remains in "notified" state beyond a configurable
  timeout (default: 5 minutes), Cancerbero MUST re-notify all
  registered solvers with a message indicating the dispute is still
  unattended.
- Re-notification frequency MUST be configurable.

#### Status Visibility

- Phase 2 does NOT require an interactive solver query protocol.
- Cancerbero MUST surface dispute status passively: through
  re-notification messages (which include current state) and through
  assignment update notifications.
- Solvers learn dispute status from the notifications they receive,
  not by querying Cancerbero directly.
- An interactive query protocol may be introduced in a later phase
  if justified.

#### Persistence (Phase 2)

- Extends the Phase 1 SQLite schema with:
  - Dispute state and state transition history
  - Assignment records (solver public key, assignment timestamp)
  - Re-notification records

---

### Phase 3: Guided Mediation for Low-Risk Disputes

**Scope**: Introduces user-facing assistance for low-risk,
coordination-oriented disputes.

#### Appropriate Mediation Cases

Phase 3 mediation is appropriate for:

- Payment delays (seller has not confirmed receipt)
- Temporary lack of response from one party
- Confusion about the next step in the process
- Anxiety-driven disputes (buyer worried about delay)
- Simple coordination failures

Phase 3 mediation is NOT appropriate for:

- Conflicting factual claims
- Suspected fraud or deception
- Cases where one party is uncooperative or hostile
- Disputes requiring evidence evaluation beyond party statements

#### User Communication

- Cancerbero MUST contact dispute parties via encrypted gift-wrap
  messages.
- All messages MUST identify Cancerbero as an assistance system, not
  the final authority.
- Cancerbero MUST ask targeted clarifying questions relevant to the
  dispute category.
- Cancerbero MUST communicate uncertainty honestly and MUST NOT
  fabricate evidence or imply certainty it does not have.

#### Mediation Outcomes

- If both parties respond cooperatively and facts align, Cancerbero
  summarizes the situation and suggests the cooperative next step.
  It notifies the assigned solver with a resolution recommendation
  but does NOT execute any settlement action.
- If mediation stalls (unresponsive parties, conflicting claims,
  or low confidence), Cancerbero escalates to a human operator.

#### Persistence (Phase 3)

- Extends SQLite schema with:
  - Mediation session records
  - Message history references (message IDs, timestamps, direction)
  - Classification results and confidence scores
  - Mediation outcome records

---

### Phase 4: Escalation Support for Human Write Operators

**Scope**: Defines how Cancerbero escalates cases that MUST NOT
remain in assisted flow to human write operators.

#### Escalation Triggers

Cancerbero MUST escalate when:

- Party claims materially conflict
- Fraud or deception is suspected
- Confidence in classification is below a configurable threshold
- Both parties are unresponsive beyond the mediation timeout
- Mediation has been attempted and failed
- Policy requires human judgment for the dispute category

#### Escalation Summaries

Each escalation message MUST include:

- Dispute ID and timeline
- Summary of each party's claims
- Mediation actions taken and responses received
- Reason for escalation
- Confidence assessment
- Any flags (fraud-risk, conflicting-claims, insufficient-info)

#### Escalation Routing

- Escalation notifications MUST be sent to solvers with write
  permissions.
- If no write-permission solver acknowledges within a configurable
  timeout, Cancerbero MUST re-escalate with increased urgency.
- Once escalated, Cancerbero MUST cease autonomous mediation on
  that dispute.

#### Persistence (Phase 4)

- Extends SQLite schema with:
  - Escalation records (trigger, timestamp, target solver)
  - Escalation summaries
  - Acknowledgment tracking

---

### Phase 5: Optional Reasoning Backend and Richer Coordination

**Scope**: Introduces a replaceable reasoning backend and optionally
richer coordination workflows.

#### Reasoning Backend Boundary

- Cancerbero's policy layer owns all decisions about escalation
  routing, permissions, and dispute authority.
- The reasoning backend provides advisory outputs: classification,
  confidence scores, suggested mediation responses, structured
  rationale (key factors and reasons for the classification), and
  escalation summaries.
- The policy layer MUST independently validate all reasoning output
  before acting on it.

#### Default and Optional Backends

- The default backend is direct API-based reasoning (e.g., an LLM
  API).
- OpenClaw integration is supported as an optional backend.
- OpenClaw MUST NOT be a mandatory dependency.
- Switching backends MUST require only configuration changes and a
  new adapter — no changes to core logic.

#### Structured Outputs

The reasoning backend MUST produce structured outputs containing:

- Classification (dispute category)
- Confidence score
- Suggested actions
- Structured rationale (key factors considered and reasons for the
  classification — not a full execution trace)
- Flags (fraud-risk, conflicting-claims, low-info)

#### Fallback Behavior

- If the reasoning backend is unavailable, Cancerbero MUST fall back
  to immediate operator escalation.
- If the reasoning backend returns low confidence, Cancerbero MUST
  escalate rather than act on uncertain classifications.

#### Future Coordination

Phase 5 may introduce richer coordination workflows (multi-operator
handoff, structured dispute queues) only through explicit
specification amendments that preserve constitutional constraints.

## Requirements

### Functional Requirements

- **FR-001**: Cancerbero MUST run as a long-lived daemon that
  maintains persistent connections to configured Nostr relay(s).

- **FR-002**: Cancerbero MUST subscribe to dispute events scoped to
  the configured Mostro instance and detect new disputes with status
  `initiated`.

- **FR-003**: Cancerbero MUST send encrypted gift-wrap notifications
  to all registered solvers when a new dispute is detected (Phase 1).

- **FR-004**: Cancerbero MUST persist detected dispute records and
  notification attempts in SQLite to prevent duplicate notifications
  across restarts and relay replays.

- **FR-005**: Cancerbero MUST attempt reconnection with backoff when
  relay connections drop, and MUST NOT crash on relay disconnection.

- **FR-006**: Cancerbero MUST log notification failures in SQLite for
  observability.

- **FR-007**: Cancerbero MUST track dispute lifecycle states (new,
  notified, taken, waiting, escalated, resolved) starting in Phase 2.

- **FR-008**: Cancerbero MUST detect solver assignment events from
  Mostro and transition disputes to "taken" state (Phase 2).

- **FR-009**: Cancerbero MUST re-notify solvers when disputes remain
  unattended beyond a configurable timeout (Phase 2).

- **FR-010**: Cancerbero MUST communicate with dispute parties via
  encrypted gift-wrap messages during guided mediation (Phase 3).

- **FR-011**: Cancerbero MUST identify itself as an assistance system
  in all user-facing messages and MUST NOT present itself as the
  final dispute authority (Phase 3+).

- **FR-012**: Cancerbero MUST escalate to write-permission solvers
  when escalation triggers are met (Phase 4).

- **FR-013**: Escalation messages MUST include dispute timeline,
  party claims, mediation actions, escalation reason, and confidence
  assessment (Phase 4).

- **FR-014**: Cancerbero MUST route reasoning requests through a
  defined backend interface and independently validate all reasoning
  output before acting (Phase 5).

- **FR-015**: Cancerbero MUST fall back to immediate operator
  escalation when the reasoning backend is unavailable (Phase 5).

- **FR-016**: Cancerbero MUST NOT execute or sign `admin-settle`,
  `admin-cancel`, or any action that moves funds or closes disputes
  (all phases).

- **FR-017**: Cancerbero MUST record sufficient audit information
  about its actions, state transitions, and notification attempts for
  operator oversight, debugging, and postmortem analysis (all phases).

- **FR-018**: Cancerbero MUST expose only the minimum information
  necessary to each participant, scoped to their role (all phases).

- **FR-019**: Cancerbero MUST use SQLite directly without a storage
  abstraction layer in all initial phases.

### Key Entities

- **Dispute**: An active dispute detected by Cancerbero. Key
  attributes: dispute ID (from `d` tag), event ID, initiator role
  (buyer or seller, from `initiator` tag), status (from `s` tag),
  timestamp, current lifecycle state, assigned solver, creation time,
  last state transition time.

- **Solver**: A human dispute resolver registered with Cancerbero.
  Key attributes: Nostr public key, permission level (read or
  write), configured via static configuration.

- **Notification**: A record of a notification sent to a solver. Key
  attributes: dispute reference, solver public key, timestamp,
  delivery status (sent, failed), notification type (initial,
  re-notification, escalation).

- **MediationSession** (Phase 3+): A guided mediation interaction.
  Key attributes: dispute reference, messages exchanged, party
  responses, classification, confidence score, outcome.

- **EscalationRecord** (Phase 4+): A record of an escalation. Key
  attributes: dispute reference, trigger, target solver, summary,
  timestamp, acknowledgment status.

- **AuditEntry**: A log of any Cancerbero action. Key attributes:
  timestamp, dispute reference, action type, input summary, output
  summary.

## Success Criteria

### Phase 1 Acceptance Criteria

- **SC-001**: When relays are reachable, solvers receive dispute
  notifications within 30 seconds of a new dispute event being
  published.

- **SC-002**: No dispute is notified more than once for the same
  detection (zero duplicate notifications).

- **SC-003**: After a restart, Cancerbero correctly identifies
  already-processed disputes and does not re-notify.

- **SC-004**: Relay disconnections are recovered automatically
  without manual intervention.

- **SC-005**: All notification attempts and failures are recorded
  and retrievable from the internal database.

- **SC-006**: Mostro continues to operate normally when Cancerbero
  is offline — disputes can be resolved manually by operators.

### Phase 2 Success Criteria

- **SC-007**: Solvers receive assignment notifications that indicate
  when a dispute has been taken, so they can determine whether a
  dispute is already being handled.

- **SC-008**: Once a solver takes a dispute, further notifications
  for that dispute are suppressed.

- **SC-009**: Unattended disputes trigger re-notification within the
  configured timeout window.

### Phase 3 Success Criteria

- **SC-010**: Coordination-type disputes (payment delays,
  unresponsive counterparties, process confusion) that receive
  cooperative responses from both parties result in a resolution
  suggestion delivered to the assigned solver without requiring
  direct solver intervention in the mediation flow.

- **SC-011**: Cancerbero never presents itself as the final authority
  in any user-facing message.

### Phase 4 Success Criteria

- **SC-012**: 100% of disputes involving conflicting claims, fraud
  indicators, or low confidence are escalated to a write-permission
  solver — zero autonomous closures.

- **SC-013**: Escalation summaries include all required fields
  (dispute ID, timeline, party claims, mediation actions, escalation
  reason, confidence assessment) so that a solver can act on the
  dispute without requesting additional context from Cancerbero.

### Phase 5 Success Criteria

- **SC-014**: Switching the reasoning backend requires only
  configuration changes and a new adapter — no modifications to
  core logic.

- **SC-015**: Reasoning backend unavailability results in graceful
  fallback to operator escalation, not system failure.

## Degraded-Mode Expectations

| Failure                        | Behavior                                                  |
|--------------------------------|-----------------------------------------------------------|
| All relays unreachable         | Cancerbero retries reconnection; logs degraded mode       |
| Single relay drops             | Continues on remaining relays; reconnects dropped relay   |
| SQLite read/write failure      | Halts notifications; logs error; retries SQLite access. Resumes notification when persistence recovers. Deduplication integrity is prioritized over delivery. |
| Notification delivery failure  | Logs failure in SQLite; does not retry in Phase 1         |
| Reasoning backend unavailable  | Falls back to immediate operator escalation (Phase 5)     |
| Cancerbero fully offline       | Mostro operates normally; solvers resolve manually        |

## Assumptions

- Mostro publishes dispute events to Nostr relays that Cancerbero
  can subscribe to. The event kinds and format are defined by
  Mostro's existing protocol.

- Solvers have Nostr key pairs and can receive encrypted gift-wrap
  messages via a Nostr client.

- The solver list (public keys and optionally permission levels) is
  provided to Cancerbero via static configuration.

- The initial deployment uses a single Cancerbero instance (no
  multi-instance coordination or leader election).

- Mostro dispute events are kind 38386 addressable events with tags:
  `d` (dispute ID), `s` (status: `initiated` or `in-progress`),
  `initiator` (buyer or seller), `y` (Mostro instance), `z`
  (dispute). These tags provide sufficient metadata for Cancerbero
  to process without additional Mostro API calls.

- SQLite is adequate for the expected dispute volume in initial
  deployment. No database migration to another engine is planned
  for the initial phases.

## Explicit Non-Goals (Phase 1)

- Guided mediation with users
- Operator assignment workflows beyond notification
- Escalation summaries
- Group Nostr notifications
- Dispute closure
- Signing or sending `admin-settle` / `admin-cancel`
- Any Lightning interaction
- Any mandatory dependency on OpenClaw
- Storage backend abstraction
- Support for alternative SQL or non-SQL databases
- Re-notification or timeout-based follow-up (deferred to Phase 2)

## System Boundaries

### Mostro Owns

- Escrow state and fund custody
- `admin-settle` and `admin-cancel` execution
- Dispute-closing authority
- Solver permission enforcement (read/write roles)
- Order lifecycle management

### Cancerbero Owns

- Dispute monitoring and detection
- Solver notification and re-notification
- Dispute intake tracking and assignment visibility
- Guided mediation communication with parties
- Escalation decisions and escalation summaries
- Reasoning backend orchestration
- Internal SQLite-backed coordination state
- Audit logging of its own actions

### Boundary Rules

- Cancerbero reads dispute state from Mostro/Nostr events but never
  writes dispute-closing actions.
- Cancerbero may suggest outcomes to solvers, but solvers act through
  Mostro, not through Cancerbero.
- If Cancerbero is offline, Mostro and its operators continue to
  resolve disputes manually as they do today.
