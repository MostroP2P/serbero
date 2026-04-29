# Feature Specification: Cooperative Self-Resolution Nudge

**Feature Branch**: `005-cooperative-self-resolution`
**Created**: 2026-04-27
**Status**: Draft
**Input**: User description: "When Serbero classifies a mediation case as
coordination_failure_resolvable with high confidence, send a subtle,
language-matched invitation to each party in the existing chat transport,
inviting them to coordinate the resolution among themselves and offering an
explicit opt-in to escalate to the assigned human solver. The invitation
MUST NOT name, instruct, suggest, or imply any specific fund-moving action.
The Phase 3 policy boundary prohibiting fund-action language stays
unchanged. Motivated by the 2026-04-27 production transcript where the
seller confirmed receipt of fiat in Spanish but heard nothing back from
Serbero, leaving them in silence while a human solver had to intervene
manually for a case that was already trending toward cooperative
resolution."

## User Scenarios & Testing *(mandatory)*

### User Story 1 — Cooperative case resolves without human solver action (Priority: P1)

A buyer files a dispute. Serbero opens a mediation session and asks both
parties what happened. The seller, in their preferred language, confirms
they have received the fiat payment. Serbero recognises this as a
cooperative case with high confidence, sends both parties a brief,
language-matched message acknowledging their input and noting that this
kind of case typically resolves through coordination between the parties
themselves, while explicitly offering them the option to ask for human
assistance if they'd prefer. The parties coordinate among themselves;
the human solver receives the same structured context they'd otherwise
receive (so they have full visibility) but does not need to actively
mediate. The dispute is closed cooperatively without solver intervention.

**Why this priority**: This is the canonical happy path the feature
exists for. It directly addresses the two operational gaps observed in
production: parties are no longer left in silence after a meaningful
reply, and human solvers stop being pulled into the loop for cases that
were already trending toward cooperative resolution. Without this story,
the feature has no value.

**Independent Test**: Can be fully tested in a controlled session where
the model returns the cooperative classification with high confidence;
verify both parties receive the templated invitation in their respective
detected languages and the assigned human solver still receives the
existing structured summary.

**Acceptance Scenarios**:

1. **Given** an active mediation session in which both parties have
   replied at least once and the most recent classifier output is
   "cooperative case, high confidence", **When** Serbero advances the
   session, **Then** each party receives one message in their detected
   language inviting them to coordinate the resolution themselves, with
   a single sentence offering human assistance as an opt-in.
2. **Given** the same session, **When** the invitations are sent,
   **Then** the assigned human solver receives the same structured
   summary they'd otherwise receive on a cooperative classification,
   marked so the solver can tell at a glance that parties have been
   invited to self-resolve.
3. **Given** the parties subsequently coordinate the resolution
   themselves and the underlying dispute is closed cooperatively,
   **When** the closure is observed, **Then** Serbero records the
   session as closed without ever having pulled the human solver into
   active mediation.

---

### User Story 2 — Party opts in to human assistance (Priority: P1)

After receiving the self-resolution invitation, one party (buyer or
seller) replies in their party chat asking for human help — for example
"I'd like a human to look at this please" or "necesito un humano que
revise esto". Serbero detects the explicit human-assistance request and
escalates the session to the assigned solver immediately, regardless of
what the latest classification would otherwise suggest. The solver
receives the same handoff package they would receive on any other
escalation path.

**Why this priority**: The opt-in is the entire reason the cooperative
nudge is acceptable. Without a working opt-in, Serbero would be nudging
parties toward self-resolution with no fall-back, which is unacceptable
for fund-related disputes. This story is what makes the policy boundary
defensible.

**Independent Test**: Continue a session that has already received the
self-resolution invitation; ingest a fresh inbound reply that contains
an explicit human-assistance request; verify the session escalates to
the assigned solver with a recognisable trigger.

**Acceptance Scenarios**:

1. **Given** a session that has already received the self-resolution
   invitation, **When** an inbound reply explicitly requests human
   assistance, **Then** the session is escalated to the assigned human
   solver with a clear "party requested human" indicator.
2. **Given** the same session, **When** an inbound reply is unrelated
   ("ok, esperamos"), **Then** the session is NOT escalated — Serbero
   continues normal handling.

---

### User Story 3 — Cooperative invitation does not lock the session into a cooperative branch (Priority: P2)

After Serbero invites the parties to self-resolve, the parties go quiet.
Some time later one party replies with new evidence that contradicts the
earlier cooperative tone (e.g., "the seller never released, they lied").
The classifier on this new round returns a non-cooperative label
("conflicting claims" or "suspected fraud"). Serbero must NOT keep
treating the case as cooperative; it must escalate immediately on the
new round, just as it would have without the prior self-resolution
invitation.

**Why this priority**: Confirms the nudge is a one-shot, evidence-driven
optimisation, not a state lock. Sessions that turn sour after the
invitation must follow the existing escalation rules. Without this
story, the feature could trap genuinely contentious cases in
"cooperative limbo".

**Independent Test**: Continue a session from User Story 1; ingest a
fresh round of replies whose classification shifts to a non-cooperative
label; verify the policy emits the standard escalation for that label.

**Acceptance Scenarios**:

1. **Given** a session that received the self-resolution invitation in
   round N, **When** round N+1 produces a classification that would
   normally trigger escalation, **Then** the session is escalated under
   the standard escalation trigger for that classification — not held
   back by the prior cooperative branch.

---

### Edge Cases

- **Repeated cooperative classifications across rounds**: the
  invitation MUST be sent at most once per session. A second cooperative
  classification on a later round must not re-send the invitation; it
  falls through to the existing solver-summary path.
- **Buyer says they sent fiat, but seller has not yet confirmed
  receipt**: this MUST NOT trigger the self-resolution invitation by
  itself. That claim is self-serving and can be true or false. Serbero
  should continue evidence gathering instead (for example, requesting
  payment proof from the buyer and receipt confirmation from the
  seller). If the seller later confirms the fiat arrived, the
  invitation becomes eligible on that later round.
- **Mixed-language reply on the round where the invitation fires**:
  language detection is applied independently to buyer and seller; if
  one side's language confidence is very low, the invitation defaults
  to English for that side only.
- **Both parties opt in to human assistance simultaneously**: only one
  escalation event fires (idempotent escalation).
- **Confidence sits exactly at the threshold**: the threshold is
  inclusive (`≥`); a confidence of exactly the configured value
  triggers the invitation.
- **Sub-threshold confidence on what would otherwise be a cooperative
  case**: the existing solver-summary path runs unchanged; no party-
  facing invitation is sent.
- **Operator wants to disable the feature globally** (e.g., during an
  incident or audit window): a single configuration switch turns the
  whole cooperative invitation behaviour off without redeploying.
- **Dispute is resolved on the underlying platform between the
  invitation send and the parties' next reply**: the existing
  dispute-resolved path closes the session normally; no further
  Serbero action against the dispute.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: When the latest classification of an active mediation
  session is "cooperative case" with confidence at or above a
  configured threshold, and the transcript contains seller-side
  corroboration that the fiat was received, the system MUST invite
  both parties (buyer and seller) to coordinate the resolution
  themselves, in the existing party chat transport.
- **FR-002**: Each party-facing invitation MUST be written in that
  party's detected language. Buyer and seller languages are detected
  independently; one party's invitation MUST NOT switch into the
  other's language.
- **FR-003**: The invitation text MUST come from a static, repository-
  hosted set of templates. The model is allowed to decide whether to
  send the invitation; the model MUST NOT author the party-facing text.
- **FR-004**: The invitation text MUST NOT name, instruct, suggest, or
  imply any specific fund-moving action (release, settle, cancel,
  disburse, transfer, or any equivalent in any supported language).
  This MUST be verified by an automated check against the template set
  on every change.
- **FR-005**: Every invitation MUST end with one short sentence
  offering human assistance as an opt-in (e.g., "if you'd prefer human
  assistance, let me know and I'll route you to the assigned solver"),
  in the party's language.
- **FR-006**: The invitation MUST fire at most once per mediation
  session — repeat cooperative classifications on later rounds MUST NOT
  re-send it.
- **FR-007**: When the invitation fires, the system MUST also deliver
  the existing structured summary to the assigned human solver, marked
  in a way that lets the solver tell at a glance that parties have been
  invited to self-resolve. This guarantees the solver still has full
  context if the parties do not resolve cooperatively.
- **FR-008**: When a party reply explicitly requests human assistance
  (in any supported language), the system MUST escalate the session to
  the assigned human solver immediately on the next round, regardless
  of what the round's classification would otherwise suggest.
- **FR-009**: The system MUST record an auditable event the moment the
  invitation is dispatched, capturing at minimum: which session, which
  classification confidence, and which prompt-bundle version was active
  — enough for an operator reviewing logs after the fact to reconstruct
  why the invitation fired without the audit trail itself revealing
  party content beyond what existing logs already reveal.
- **FR-010**: A configurable confidence threshold MUST gate FR-001;
  the default value is 0.75 and operators MUST be able to raise it
  (e.g., to 0.90) without code changes.
- **FR-011**: A configuration kill-switch MUST allow operators to
  disable the entire cooperative-invitation behaviour without removing
  the templates or modifying code. With the kill-switch off, the
  pipeline behaves exactly as it did before this feature shipped.
- **FR-012**: When confidence falls below the configured threshold for
  a cooperative classification, the existing behaviour (solver-only
  summary, no party invitation) MUST remain unchanged.
- **FR-013**: The session lifecycle MUST end at the same state used by
  the existing cooperative summary path (the state that already blocks
  the eligibility predicate from re-opening a duplicate session). The
  cooperative invitation does NOT change the session lifecycle
  contract.
- **FR-014**: New supported languages MUST be addable by appending a
  template section without code changes; the automated fund-action
  keyword check (FR-004) MUST extend to the new section before it is
  shipped.
- **FR-015**: A buyer-only claim that fiat was sent, without seller
  confirmation of receipt, MUST NOT by itself trigger the invitation.
  The system MUST treat that state as still requiring evidence
  gathering or other non-invitation handling.

### Key Entities

- **Self-Resolution Invitation**: A static, language-keyed message pair
  (cooperative-coordination text + human-assistance opt-in sentence)
  delivered to a single party. Identified by language and audience
  (buyer / seller). MUST satisfy FR-004 (no fund-action keywords) for
  every supported language.
- **Self-Resolution Audit Record**: A record on the session timeline
  noting the dispatch of the invitation, including session id,
  classification confidence at the moment of dispatch, and the
  prompt-bundle version active at that moment. Does NOT carry the
  rationale text itself.
- **Human-Assistance Request Marker**: A boolean signal derived from
  the classifier on a round following an invitation, indicating that
  the most recent party reply explicitly asks for a human solver.
  Triggers FR-008.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: For mediation sessions where the cooperative invitation
  fires, the share that resolve without active human solver
  intervention increases by at least 30% compared to the baseline
  measured during the seven days before this feature ships.
- **SC-002**: Median time from "Serbero detects cooperative case" to
  "dispute closed on the underlying platform" decreases by at least
  20% for sessions where the invitation fires, versus the pre-feature
  baseline. (No solver-side wait on cooperative cases.)
- **SC-003**: Zero invitations leak any banned fund-action keyword
  into the user-facing text across all supported languages, verified
  by an automated keyword check that runs on every change to the
  template set.
- **SC-004**: For at least 95% of sessions where the invitation fires,
  both parties either resolve the dispute or use the human-assistance
  opt-in within seven days. Sessions where both parties simply go
  silent stay below 5% of invitations sent; if this rate is exceeded
  for two consecutive review windows, the invitation phrasing is
  revisited.
- **SC-005**: When a party explicitly requests human assistance after
  an invitation, the session reaches the assigned human solver within
  one engine cycle (the same latency as any other escalation path in
  the system today).
- **SC-006**: The cooperative invitation is sent at most once per
  session in 100% of audited cases (one-shot guarantee under FR-006).
- **SC-007**: With the kill-switch disabled, the system's externally
  observable behaviour is byte-for-byte identical to the system's
  behaviour the day before this feature shipped (zero regression on
  the legacy cooperative-summary path).

## Assumptions

- The existing party-language detection in the transcript pipeline is
  reliable enough to route invitations to the correct language with
  acceptable error. English, Spanish, and Portuguese are the initial
  supported set; new languages are added by template append per
  FR-014.
- "Cooperative case" classification with high confidence is a
  meaningful share of overall mediation traffic; if it is rare, the
  feature still works correctly but its absolute operational value
  (SC-001, SC-002) is smaller.
- The assigned human solver is reachable via the same notification
  channel used by the existing solver-summary path; this feature does
  not introduce a new delivery surface for solvers.
- The platform's existing dispute-resolved path correctly closes
  sessions when the underlying dispute is resolved cooperatively,
  including sessions that received the invitation. This was the
  subject of a recently-shipped fix in `main` and is treated here as
  a prerequisite, not part of this feature's scope.
- Existing audit-event sinks, notification routing, and
  prompt-bundle hashing infrastructure cover the new variants this
  feature introduces; no new operational surface is required for
  solvers or operators.

## Dependencies

- **Hard dependency**: the recently-shipped fix in `main` that stops
  the system from auto-closing mediation sessions at the legacy
  terminal state, so the eligibility predicate cannot reopen a
  duplicate session while parties are still coordinating. Without
  this fix, the cooperative invitation would still trigger duplicate
  sessions on the next engine tick.
- **Soft dependency**: the existing Phase 3 policy boundary in the
  system prompt continues to forbid Serbero from authoring fund-action
  language in any free-form output. This feature relies on that
  boundary; it does not weaken it.

## Out of Scope

- Modifying the policy for any classification other than the
  cooperative case (conflicting claims, suspected fraud, unclear, and
  not-suitable-for-mediation continue to follow their existing
  escalation / clarification flows).
- Allowing Serbero to suggest, instruct, or imply fund-moving actions
  by name. The existing prohibition is unchanged and re-stated.
- Auto-closing disputes on the underlying platform. Serbero does not
  invoke admin-settle, admin-cancel, or any other platform admin
  command, in this feature or anywhere else.
- Localisation beyond the languages supported by the existing
  transcript pipeline at the time this feature ships (English,
  Spanish, Portuguese initially). New languages are added by
  appending template sections per FR-014; translator tooling and
  workflow are out of scope for this feature.
