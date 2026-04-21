# Feature Specification: Guided Mediation (Phase 3)

**Feature Branch**: `003-guided-mediation`
**Created**: 2026-04-17
**Status**: Draft
**Input**: Design notes at `serbero-phase3-notes.md` plus the Phase 3 hard-requirements brief from the operator.

## System Vision

Phase 3 introduces **guided mediation for low-risk coordination disputes**
to the Serbero daemon. Serbero begins contacting dispute participants
over Mostro's chat protocol to ask clarifying questions, collect
responses, and produce structured summaries that help the assigned
human solver close the dispute. Phase 3 expands Serbero's *assistance*
surface while preserving every invariant from Phases 1 and 2: no fund
movement, no dispute closure, and no authority that belongs to Mostro
or the human operator.

Phase 3 is deliberately bounded. Disputes that exhibit conflicting
factual claims, suspected fraud, unresponsive parties past the
mediation window, or low classification confidence are *not* resolved
inside Phase 3 — they are routed to the Phase 4 escalation surface with
a structured handoff package.

## Invariants (carried forward from Phases 1 and 2)

- Serbero MUST NOT move funds, sign `admin-settle` or `admin-cancel`,
  or close disputes.
- Mostro owns escrow state, permissions, solver roles, and
  dispute-closing authority.
- If Serbero is offline, Mostro and its operators continue to
  resolve disputes manually.
- Serbero MUST record sufficient audit information about its actions,
  state transitions, notifications, and mediation messages for
  operator oversight, debugging, and postmortem analysis.
- Serbero MUST identify itself as an assistance system in all
  user-facing messages and MUST NOT present itself as the final
  dispute authority.
- Serbero MUST surface uncertainty honestly and MUST NOT fabricate
  evidence or imply certainty it does not have.

## Relationship to Other Phases

| Phase | Scope                                                        | Status at Phase 3 cut-in |
|-------|--------------------------------------------------------------|--------------------------|
| 1     | Always-on dispute listener and solver notification           | Implemented              |
| 2     | Intake tracking, assignment visibility, re-notification      | Implemented              |
| **3** | **Guided mediation for low-risk disputes**                   | **This spec**            |
| 4     | Escalation support for write-permission operators            | Planned; Phase 3 prepares the handoff but does NOT execute escalations |
| 5     | Optional richer reasoning backend and advanced assistance    | Planned                  |

Phase 3 is additive. Phases 1 and 2 keep running on their own
subscriptions and tables; Phase 3 subscribes to additional chat events
and writes to additional tables, but does not alter Phase 1/2
behavior.

## Technical Constraints

- **TC-101**: Mediation party communication MUST use the Mostro
  peer-to-peer chat transport defined at
  <https://mostro.network/protocol/chat.html>. Direct NIP-17 / NIP-59
  DMs addressed to buyer or seller pubkeys are NOT a substitute for
  this transport in any mediation path.
- **TC-102**: Phase 3 MUST require a configured and reachable reasoning
  provider to operate. No "no-model" fallback mediation mode exists
  for this phase. If the model is unreachable or the provider config
  is invalid, the mediation path MUST halt — but Phase 1/2 detection
  and notification MUST remain operational and unaffected.
- **TC-103**: Behavior-controlling instructions — system prompt,
  classification policy, escalation policy, message templates,
  mediation style, honesty and tone rules — MUST be stored as
  versioned files in the repository (e.g. under `prompts/`). SQLite
  MUST NOT become the primary source of truth for these artifacts.
- **TC-104**: Serbero MUST act as a solver identity using the Nostr
  keypair configured for the daemon. The corresponding public key
  MUST already be registered in the target Mostro node as a solver
  with at least `read` permission *before* Phase 3 starts mediating.
  Serbero MUST NOT assume or grant that permission itself.
- **TC-105**: Reasoning provider access MUST be configurable by
  provider, model, API base URL, and credential source. The codebase
  MUST NOT hard-code a specific vendor in the mediation call sites.

## Reference Implementations

The Mostro chat transport has a working reference implementation in
Mostrix (<https://github.com/MostroP2P/mostrix>). The following files are
the primary references for transport and mediation-session modeling
in this phase and SHOULD be consulted during design and
implementation:

- `src/util/chat_utils.rs` — chat-addressing key reconstruction,
  gift-wrap construction, and the decrypt / verify pipeline.
- `src/models.rs` — session-state shapes, chat role handling.
- `src/util/order_utils/execute_take_dispute.rs` — the dispute-chat
  interaction flow that precedes mediation, including how a solver
  obtains or reconstructs the per-party chat-addressing key material
  tied to the dispute.
- `src/ui/key_handler/input_helpers.rs` — input-to-event construction
  patterns that inform how Serbero assembles its outgoing messages.

These files are reference material. Serbero will re-implement the
necessary parts in Rust against `nostr-sdk 0.44.1`; it will not
depend on Mostrix at build time.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Serbero opens a mediation session for a low-risk dispute (Priority: P1)

A buyer opens a dispute on a peer-to-peer trade because the seller has
not yet confirmed fiat receipt. Phases 1 and 2 have already detected
the dispute and notified the configured solver set. Serbero now opens
a guided mediation session: it derives the shared chat keys required
by Mostro's chat protocol, contacts the buyer and the seller through
that transport, identifies itself as an assistance system (not a final
authority), and asks each party a small set of targeted clarifying
questions drawn from the classification policy.

**Why this priority**: This is the entry point to the entire Phase 3
capability. Without it, none of the subsequent mediation-round,
summary, or escalation-handoff features can run. It is also the
riskiest behavior to get wrong: if Serbero addresses parties through
the wrong transport (direct DM instead of Mostro chat), the feature
violates its protocol contract from the first message. This story
must land correctly before any other Phase 3 story is meaningful.

**Independent Test**: Can be fully tested by publishing a dispute
event that Phases 1 and 2 recognize, registering Serbero's pubkey as
a solver on the target Mostro instance, simulating the dispute-chat
interaction flow that current Mostro clients use (verified against
the Mostrix reference implementation at test-fixture time), and then
verifying that Serbero emits the first mediation message into the
Mostro chat channel (not a direct DM), with content sourced from the
configured prompt bundle and identifying itself as an assistance
system.

**Acceptance Scenarios**:

1. **Given** Phases 1 and 2 are running, a dispute has just been
   detected and is mediation-eligible under the composed predicate
   defined in FR-123, and a configured reasoning provider is reachable,
   **When** the dispute-detection event fires (not a later periodic
   sweep),
   **Then** Serbero synchronously invokes the reasoning layer, and if
   the reasoning verdict classifies the dispute as low-risk
   coordination (payment delay, confusion, unresponsive counterparty)
   Serbero issues `TakeDispute` on the strength of that verdict (per
   FR-122), derives the per-party shared chat keys using ECDH as
   specified by Mostro's chat protocol, publishes an initial clarifying
   message via gift-wrapped chat events addressed to the shared
   pubkeys of each party, records a new `mediation_sessions` row with
   state `awaiting_response`, and records the exact prompt bundle
   version used — all within the same event-handling path, without
   waiting for a background tick.

2. **Given** the reasoning provider is not configured or not reachable,
   **When** the policy would otherwise start a mediation session,
   **Then** Serbero MUST NOT contact any party, MUST log the halt
   reason at WARN level, MUST leave the Phase 2 dispute state
   untouched, and MUST continue to notify and re-notify solvers
   through Phase 1/2 paths.

3. **Given** Serbero's configured pubkey is NOT registered as a solver
   in the target Mostro instance,
   **When** Phase 3 attempts to start a mediation session,
   **Then** Serbero MUST refuse to send any chat event, MUST log the
   missing precondition at ERROR level, and MUST enter the bounded
   revalidation loop defined in the Solver Identity and Authorization
   section (immediate check, config-reload re-check, truncated
   exponential backoff between the configured initial and maximum
   intervals, termination at the configured total-time or
   total-attempts cap with one terminal WARN-or-higher alert). Phase
   1/2 behavior MUST remain fully unaffected throughout the
   revalidation window and after termination. If revalidation later
   succeeds, Serbero resumes Phase 3 operation without a restart.

---

### User Story 2 - Serbero collects responses and maintains session state (Priority: P1)

Parties reply to Serbero's clarifying messages through the Mostro chat
channel. Serbero fetches gift-wrapped events addressed to each party's
shared pubkey, decrypts them with the ECDH-derived shared key,
verifies the inner event, and persists the decrypted content and the
inner event's timestamp as authoritative session history. The session
round counter advances, last-seen markers update so the same response
is never ingested twice, and the runtime context for the next model
interaction is reconstructed from SQLite session history, the current
dispute/session state, the loaded prompt bundle, and live config.

**Why this priority**: Mediation without durable, deduplicated
session memory produces incoherent conversations and duplicate
follow-ups. Session persistence is also the audit trail for every
outbound message, every inbound reply, and every classification the
model produced.

**Independent Test**: Can be tested by issuing a mediation session
against a mocked Mostro chat transport (gift-wrap events written to a
local test relay), publishing simulated party responses with known
inner-event timestamps, and verifying that: the decrypted content is
stored verbatim in `mediation_messages`, the session's last-seen
markers prevent re-ingesting the same event on restart, and the round
counter matches the number of inbound-message boundaries.

**Acceptance Scenarios**:

1. **Given** an open mediation session in `awaiting_response` state,
   **When** a party posts a reply through Mostro chat,
   **Then** Serbero fetches the gift wrap, decrypts and verifies the
   inner event, appends a `mediation_messages` row with direction
   `inbound`, body equal to the decrypted inner-event content, and
   message timestamp equal to the inner event's `created_at`.

2. **Given** a session has already ingested an inbound message with
   event id X,
   **When** the same event is delivered again (relay replay, restart),
   **Then** the message is NOT inserted a second time and no duplicate
   round is counted.

3. **Given** the daemon restarts mid-session,
   **When** Serbero resumes,
   **Then** the next model call reconstructs context from the session
   row, the stored message history, the prompt bundle identified by
   the session's `policy_hash`, and the current configuration — no
   runtime-only in-memory state is required to continue.

---

### User Story 3 - Serbero summarizes a cooperative resolution for the assigned solver (Priority: P2)

Both parties respond cooperatively within the configured mediation
window, and the facts they describe are aligned (for example, the
seller confirms fiat payment was received; the buyer confirms they
have no further dispute). Serbero asks the configured reasoning
provider to produce a structured summary — not a decision — and
delivers that summary to the assigned solver via the Phase 1/2
notifier (encrypted gift-wrap DM). The summary includes the dispute
identifier, the mediation classification, the suggested cooperative
next step, the prompt bundle version, and an explicit reminder that
the solver retains final authority.

**Why this priority**: The summary is the primary user-visible value
of Phase 3: it turns a cooperative low-risk dispute into a
ready-to-close artifact for the solver, without Serbero ever executing
the close. It depends on US1 and US2 being in place but is otherwise
independently testable.

**Independent Test**: Can be tested end-to-end by driving a cooperative
two-round mediation session against a mock reasoning provider whose
response is a known JSON blob, and verifying the solver receives a
gift-wrapped DM containing the summary text with the expected fields,
and that `mediation_summaries` has a row referencing the session and
the same prompt bundle.

**Acceptance Scenarios**:

1. **Given** a mediation session where both parties have responded
   cooperatively and the classification policy marks the outcome as
   `coordination_failure_resolvable`,
   **When** Serbero runs the summarization step,
   **Then** the reasoning provider's structured response is persisted
   to `mediation_summaries`, a gift-wrapped DM is sent via the
   Phase 1/2 notifier following the routing model defined in
   "Solver-Facing Routing" below (targeted to the assigned solver if
   one exists, broadcast to all configured solvers otherwise), and the
   session transitions to `summary_delivered`.

2. **Given** the summary has been delivered,
   **When** Serbero runs any further mediation logic for the same
   dispute,
   **Then** Serbero MUST NOT send autonomous resolution actions — it
   sends only further advisory summaries or escalation handoffs if the
   underlying state changes (e.g. a party recants).

---

### User Story 4 - Serbero detects an escalation trigger and prepares a Phase 4 handoff (Priority: P2)

During mediation, one of several escalation conditions becomes true:
the parties make conflicting factual claims, a fraud indicator is
surfaced by the classification policy, the model returns a
low-confidence classification, a party is unresponsive past the
configured party-response timeout, or the session hits the configured
max-round limit without convergence. Serbero transitions the session
to `escalation_recommended`, records the trigger and its evidence,
and assembles a structured escalation package for Phase 4. Phase 3
does NOT execute the escalation itself — it makes the handoff ready.

**Why this priority**: Escalation detection is what prevents Phase 3
from being a rubber stamp on disputes it should not touch. Without it,
adversarial or ambiguous disputes silently exhaust mediation rounds
and timeout, wasting time that should have gone to a human operator.

**Independent Test**: Can be tested by scripting mediation sessions
that trigger each of the five escalation conditions in turn
(conflicting claims, fraud indicator, low confidence, party timeout,
round-limit reached) and verifying: the session transitions to
`escalation_recommended`, the `mediation_events` table records the
exact trigger, the Phase 4 handoff row contains the dispute
identifier, session id, trigger reason, mediation transcript summary
reference, and prompt bundle version, and no further mediation
messages are sent to parties after the transition.

**Acceptance Scenarios**:

1. **Given** any of the escalation triggers is met during mediation,
   **When** Serbero evaluates session state,
   **Then** the session transitions to `escalation_recommended`, the
   trigger and its evidence are persisted, and no further clarifying
   messages are sent to parties on that session.

2. **Given** the session is in `escalation_recommended`,
   **When** Phase 4 is not yet deployed,
   **Then** Serbero MUST leave the handoff package in place for Phase 4
   to consume later, and MUST surface the recommendation in the
   Phase 1/2 solver notification stream with a clear "needs human
   judgment" label — without pretending Phase 4 routing has completed.

---

### User Story 5 - Operator swaps the reasoning provider without code changes (Priority: P3)

A system operator wants to move the reasoning endpoint — e.g. from
OpenAI's hosted API to a self-hosted OpenAI-compatible gateway, a
router proxy, or a third-party OpenAI-compatible provider. They
update the `[reasoning]` block of `config.toml` (primarily
`api_base`), rotate credentials via `api_key_env`, and restart
Serbero. Mediation resumes against the new endpoint with no code
changes. Selecting a provider that is not yet implemented in Phase 3
(`anthropic`, `ppqai`, `openclaw`) MUST fail at startup with an
actionable error rather than silently falling back to OpenAI —
landing those additional adapters is explicit future work.

**Why this priority**: Portability across OpenAI-compatible endpoints
(the shipped Phase 3 scope) is what makes Phase 3 safe to deploy and
iterate on. The boundary design is what keeps the constitutional
"no lock-in" principle reachable as additional adapters land later.

**Independent Test**: Can be tested by running the same mediation
fixture against two different provider configurations and asserting
that the mediation state transitions, summary shape, and message
format are identical, while the outbound HTTP surface differs per
provider.

**Acceptance Scenarios**:

1. **Given** Serbero is running with `provider = "openai"` and a
   credential exported via the configured `api_key_env` (e.g.
   `SERBERO_REASONING_API_KEY`),
   **When** the operator keeps `provider = "openai"` but points
   `api_base` at an OpenAI-compatible endpoint and rotates the
   credential via `api_key_env`, then restarts Serbero,
   **Then** new mediation sessions call the new endpoint and succeed
   without any code rebuild.

2. **Given** the configured provider returns an error or a timeout,
   **When** Serbero's reasoning call fails,
   **Then** Serbero MUST record the failure, MUST NOT fabricate a
   classification or summary, and MUST either retry (bounded by
   `[reasoning].followup_retry_count` — the adapter owns the retry
   budget — and by the overall mediation timeout) or transition the
   session to `escalation_recommended` with trigger
   `reasoning_unavailable`.

3. **Given** the operator sets `provider = "anthropic"` (or any
   other value not implemented in Phase 3) in `config.toml` and
   restarts Serbero,
   **Then** Serbero MUST fail Phase 3 initialization at startup with
   an actionable error naming the unsupported provider and MUST NOT
   silently coerce to OpenAI. Phase 1/2 behavior MUST remain
   unaffected.

---

### User Story 6 - Serbero reports externally-resolved disputes to the solver (Priority: P1)

After Serbero has opened a mediation context for a dispute — either a
full session with outbound party messages, a session that reached
`escalation_recommended`, or at least a reasoning verdict and
classification — the dispute is sometimes resolved in a path Serbero
did not fully drive. The parties coordinate outside the mediation
channel and signal Mostro, or an assigned solver closes it through
Mostro directly. When that resolution is observed as a Phase 1/2
lifecycle transition, Serbero must still tell the solver set what it
knows, so there is no silent disappearance of a dispute Serbero had
touched.

**Why this priority**: Without this report, Serbero's audit surface
has a blind spot exactly where it matters — disputes it started
mediating but never saw through. Operators need a consistent
closing-loop message any time Serbero was involved, including the
cases where Serbero's own mediation messages did not converge.

**Independent Test**: Can be tested by driving four resolution
scenarios — (a) session with both-parties exchange that resolves
externally, (b) session with only outbound messages sent (no party
replies) that resolves externally, (c) session that reached
`escalation_recommended` and then resolves externally, (d) a
detected dispute for which reasoning ran and produced a verdict but
no session was ever opened, that then resolves externally — and
verifying that in each case Serbero sends exactly one solver-facing
DM via the Phase 1/2 notifier whose body includes the fields
required by FR-124 and "Final Solver Report on External Resolution".

**Acceptance Scenarios**:

1. **Given** a mediation session for a dispute has been opened and
   Serbero has exchanged at least one outbound message with a party,
   **When** the dispute transitions to a resolved terminal state via
   a Phase 1/2 lifecycle event (Mostro settled, cancelled, or
   otherwise closed it — without Serbero executing that action),
   **Then** Serbero MUST emit exactly one solver-facing DM via the
   Phase 1/2 notifier routed per "Solver-Facing Routing", whose body
   includes the dispute id, session id, latest known classification
   and confidence, an outbound-message counter, the final observed
   dispute status, and a short narrative derived from lifecycle
   transitions; the session outcome is recorded as
   `resolved_externally_reported`.

2. **Given** Serbero produced a reasoning verdict and classification
   for a dispute but `TakeDispute` failed or was never attempted (and
   so no `mediation_sessions` row exists) — mediation context
   nonetheless exists in the form of a `mediation_events` row,
   **When** the dispute transitions to a resolved terminal state,
   **Then** Serbero MUST emit the final report per FR-124 with
   `outbound_party_messages = 0` and session id explicitly absent or
   null.

3. **Given** a session reached `escalation_recommended` and the
   Phase 4 handoff package is pending consumption,
   **When** the dispute transitions to a resolved terminal state
   before Phase 4 acts,
   **Then** Serbero MUST emit the final report AND the report
   narrative MUST note that an escalation was recommended but the
   dispute resolved before Phase 4 acted. The escalation handoff
   package MUST remain in place for Phase 4 historical context.

4. **Given** a dispute that Phase 1/2 detected but for which Serbero
   produced NO mediation context (no reasoning verdict, no session,
   no events),
   **When** the dispute later transitions to a resolved terminal
   state,
   **Then** Serbero MUST NOT emit the final report (this case belongs
   entirely to Phase 1/2 notifications).

---

### Edge Cases

- **Mostro chat protocol changes shape**: if Mostro updates the chat
  transport in a backwards-incompatible way, Phase 3 mediation MUST
  halt new sessions and log an actionable error. It MUST NOT fall
  back to direct DMs.
- **Party posts an out-of-order message**: an inbound message whose
  inner-event `created_at` predates the last-seen marker MUST be
  ignored for state-transition purposes but MAY be persisted for
  audit with a `stale=true` flag.
- **Both parties go silent mid-session**: after
  `party_response_timeout_seconds` elapses with no inbound reply, the
  session transitions to `escalation_recommended` with trigger
  `party_unresponsive`.
- **Mediation already completed, dispute is re-disputed**: if Mostro
  re-publishes a dispute event for the same dispute id after Serbero
  has produced a summary, Phase 3 opens a *new* mediation session
  linked to the same dispute id rather than mutating the closed one.
- **Prompt bundle changes mid-session**: sessions MUST pin
  `instructions_version` / `policy_hash` / `prompt_bundle_id` at
  creation time. Changes to the `prompts/` tree do not retroactively
  alter open sessions; they only affect sessions opened after the
  change.
- **Reasoning provider returns malformed JSON**: Serbero MUST treat
  this as a reasoning failure (see US5 Scenario 2), not as
  "classification = Unclear".
- **Serbero's configured pubkey loses solver permission in Mostro
  mid-session**: outbound mediation messages will fail at the
  transport or policy layer. Serbero MUST record the failure, stop
  sending new messages on that session, and transition to
  `escalation_recommended` with trigger `authorization_lost`.
- **Multiple Serbero instances run against the same DB**: out of
  scope for Phase 3 (inherits the single-instance assumption from
  Phases 1 and 2).
- **Dispute resolves before the first outbound party message**: if a
  dispute transitions to a resolved terminal state after Serbero
  produced any mediation context (reasoning verdict, classification,
  session row, events) but before the first outbound party message
  was sent, Serbero MUST still emit the FR-124 final solver-facing
  report with `outbound_party_messages = 0`. The absence of a
  party-facing message does NOT exempt Serbero from reporting.
- **Dispute resolves while the session is `escalation_recommended`**:
  the escalation handoff package remains in place for Phase 4; the
  FR-124 final report fires and notes that an escalation was
  recommended but the dispute resolved before Phase 4 acted.
- **Dispute detected but mediation start fails transiently** (reasoning
  timeout inside the retry budget, chat-transport backpressure): the
  event-driven path (FR-121) reports the failure via
  `mediation_events`, does NOT commit a session row, does NOT issue
  `TakeDispute`, and leaves the dispute eligible under FR-123 so the
  engine-tick safety net retries it on the next pass.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-101**: Serbero MUST communicate with dispute parties during
  mediation exclusively through the Mostro chat transport
  (ECDH-derived shared keys, gift-wrapped events addressed to the
  shared pubkey). Direct NIP-17 / NIP-59 DMs to a party's own pubkey
  are forbidden for mediation traffic.

- **FR-102**: Serbero MUST require a configured and reachable
  reasoning provider before opening a mediation session. Absence or
  unreachability of the provider MUST halt mediation without
  affecting Phase 1/2 behavior.

- **FR-103**: The reasoning provider boundary MUST be designed so new
  providers can be added without changing mediation call sites. The
  configuration surface MUST be provider-agnostic, and Phase 3 MUST
  deliver configuration-only portability across **OpenAI and
  OpenAI-compatible endpoints** (any endpoint that speaks the same
  chat-completions shape — e.g. self-hosted OpenAI-compatible
  gateways, router proxies, some third-party providers). Adapters for
  Anthropic, PPQ.ai, and OpenClaw are in scope for the boundary but
  out of scope as shipped implementations for this phase: they are
  declared as `NotYetImplemented` stubs so selecting them fails
  loudly at startup rather than silently coercing to OpenAI. Landing
  those additional adapters is explicit future work (see Explicit
  Non-Goals) and does NOT require changes to mediation call sites.

- **FR-104**: The reasoning provider configuration MUST include at
  minimum: provider name, model identifier, API base URL (when
  relevant), credential source (typically an environment variable
  name), request timeout, and retry / failure behavior.

- **FR-105**: Instructions, classification policy, escalation policy,
  message templates, and style/honesty constraints MUST be sourced
  from versioned files in the repository (e.g. `prompts/phase3-*.md`).
  The SQLite schema MUST NOT be the primary source of truth for these
  artifacts.

- **FR-106**: Every mediation session MUST persist enough metadata to
  make behavior reproducible from git history alone: at minimum an
  `instructions_version`, a `policy_hash` or equivalent content hash,
  and a `prompt_bundle_id` naming the bundle used.

- **FR-107**: Serbero MUST persist mediation sessions, inbound and
  outbound messages, summaries, outcome transitions, and escalation
  triggers to SQLite, keyed by mediation session id and linked to the
  Phase 1/2 dispute id.

- **FR-108**: Serbero MUST identify itself as an assistance system in
  every mediation message sent to a party, and MUST NOT present itself
  as the final dispute authority in any message.

- **FR-109**: Serbero MUST surface uncertainty honestly in summaries
  and messages and MUST NOT fabricate evidence, confirmations, or
  details that were not actually produced by the parties.

- **FR-110**: Serbero MUST use the configured Serbero private key as
  its operational solver identity. Before opening the first mediation
  session, Serbero MUST NOT assume that its corresponding public key
  is registered as a solver in Mostro — operators are responsible for
  registering it with at least `read` permission, and Serbero MUST
  refuse to send mediation messages if that precondition cannot be
  confirmed.

- **FR-111**: Serbero MUST detect and act on the following escalation
  triggers: conflicting factual claims between parties, fraud
  indicators surfaced by the classification policy, classification
  confidence below a configured threshold, party unresponsiveness
  past the configured party-response timeout, reaching the configured
  max-round limit without convergence, and reasoning provider
  unavailability bounded by the retry policy.

- **FR-112**: On escalation trigger, Serbero MUST transition the
  mediation session to `escalation_recommended`, persist the trigger
  reason and supporting evidence, prepare the Phase 4 handoff package,
  and stop sending further clarifying messages to parties on that
  session.

- **FR-113**: Serbero MUST continue to surface coordination status
  through the Phase 1/2 solver notification surface during and after
  mediation (e.g. `mediation_in_progress`, `mediation_summary`,
  `escalation_recommended`). Mediation MUST NOT replace or silence
  Phase 1/2 notifications.

- **FR-114**: Serbero MUST enforce a configurable maximum number of
  mediation rounds per session and a configurable per-party response
  timeout.

- **FR-115**: Serbero MUST NOT execute or sign any fund-moving action.
  In particular, `admin-settle`, `admin-cancel`, or any other
  dispute-closing Mostro action are forbidden to the mediator in all
  circumstances, including when the reasoning provider explicitly
  recommends them. Such recommendations are advisory text only.

- **FR-116**: The reasoning provider's output is advisory. Serbero's
  policy layer MUST independently validate every reasoning output
  against the escalation and authority rules before taking any action
  derived from it. In particular, any suggested action that would
  cross Phase 3's authority boundary MUST be suppressed and, depending
  on the trigger, may itself escalate.

- **FR-117**: On restart, Serbero MUST resume open mediation sessions
  from the persisted SQLite state without losing round counts,
  last-seen markers, or prompt-bundle pinning.

- **FR-118**: Mediation MUST run on any dispute that satisfies the
  composed mediation-eligibility predicate defined in FR-123.
  Eligibility MUST NOT be pinned to a single narrow persisted state
  name (e.g. `lifecycle_state = 'notified'` alone); the authoritative
  rule is the composed predicate. Mediation MUST NOT preempt Phase 2
  assignment detection: if a human solver takes the dispute via
  Mostro, mediation MUST either defer to the solver or close as
  `superseded_by_human`, per the mediation style policy.

- **FR-119**: Serbero MUST record every outbound chat event it sends
  as a row in `mediation_messages` with direction `outbound`, the
  decrypted content that was actually wrapped, the outbound event id,
  and the prompt bundle that produced it.

- **FR-120**: Serbero MUST emit, in general application logs, only
  the classification label, the confidence score, and a reference id
  (content-addressed hash or session-scoped rationale id) pointing to
  the full rationale. The full rationale text returned by the
  reasoning provider MUST NOT be written to general logs; it MUST be
  persisted to a controlled audit store (a dedicated SQLite table is
  the default — see the Mediation Memory Model section) alongside the
  session id, `prompt_bundle_id`, `policy_hash`, and provider/model
  identifiers, with the log-side reference id linking back to the
  stored rationale. This protects against accidentally leaking
  dispute details, party statements, or PII into aggregate log
  streams while preserving the full evidence trail for operator
  review.

- **FR-121** *(immediate, event-driven start)*: When Phase 1/2 detects
  a new dispute and persists it, Serbero MUST attempt to open a
  guided-mediation session from within that same event-handling path.
  The mediation start flow (reasoning verdict → `TakeDispute` → first
  party-facing message) MUST NOT depend on a later periodic sweep.
  The background mediation engine tick MAY continue to exist — but
  only as a resumption and retry safety net (restart recovery, open
  sessions whose first attempt failed transiently, eligibility
  re-evaluation after config reload). A session MUST NOT be reachable
  exclusively via the tick; every new-dispute case MUST also be
  reachable via the event-driven handoff. If the event-driven attempt
  fails transiently (reasoning provider timeout inside the retry
  budget, chat-transport backpressure), the dispute MUST remain
  eligible under FR-123 so the tick can pick it up on the next pass.

- **FR-122** *(take strictly coupled to reasoning)*: Serbero MUST NOT
  issue `TakeDispute` on a dispute unless the reasoning layer has
  produced a verdict that the dispute is mediation-eligible under the
  classification policy, for that same session-open attempt. The
  required ordering within a session-open attempt is:
  1. verify reasoning provider health (TC-102, FR-102);
  2. compute a reasoning verdict for the specific dispute;
  3. only if the verdict is positive, issue `TakeDispute`;
  4. only after `TakeDispute` succeeds, insert the
     `mediation_sessions` row and send the first party-facing
     message.
  Serbero MUST NOT take a dispute via a manual or scripted fallback,
  a "model-absent" heuristic, or a cached previous verdict for a
  different session. If the reasoning provider is unavailable or
  returns a non-eligible verdict, Serbero MUST NOT take the dispute;
  Phase 1/2 notification continues unchanged. A session row that was
  committed before `TakeDispute` succeeded or before the first
  party-facing message was sent is a spec violation.

- **FR-123** *(composed mediation-eligibility predicate)*: A dispute
  is mediation-eligible if and only if ALL of the following hold:
  - the dispute has NOT transitioned to a resolved terminal state
    (from Phase 2's perspective: `resolved`, `cancelled_settled`, or
    any state that denotes Mostro has closed the dispute);
  - the dispute has NOT been handed off to Phase 4 escalation
    (no `mediation_sessions` row for this dispute is in
    `escalation_recommended`);
  - there is NO currently active (non-terminal, non-escalated)
    `mediation_sessions` row for this dispute;
  - a human solver has NOT taken the dispute and moved it to
    `superseded_by_human` (FR-118).

  Eligibility MUST NOT be implemented as a single-state check like
  `lifecycle_state = 'notified'`. The detection path (FR-121) and
  the engine tick (resumption/retry role) MUST both evaluate the
  same composed predicate, so a dispute cannot be skipped because it
  transitioned through a short-lived intermediate state between
  detection and the next sweep.

- **FR-124** *(final solver report on external resolution)*: When a
  dispute transitions to a resolved terminal state and Serbero has
  collected **substantive** mediation context for that dispute,
  Serbero MUST emit exactly one final solver-facing report via the
  Phase 1/2 notifier. Substantive context is defined as ANY of the
  following:
  - a prior reasoning verdict (a `reasoning_verdict` row or a
    persisted `reasoning_rationales` row) — Serbero reasoned about
    the dispute;
  - a `mediation_sessions` row in any state (including
    `escalation_recommended`, `closed`, `superseded_by_human`) —
    Serbero took the dispute;
  - one or more `mediation_messages` rows — Serbero talked to at
    least one party;
  - any session-scoped `mediation_events` row (`session_id IS NOT
    NULL`) that records a mediation side effect (e.g.
    `classification_produced`, `escalation_recommended`,
    `handoff_prepared`, `summary_generated`, `state_transition`).

  Pre-reasoning bookkeeping rows do NOT count as substantive context
  on their own: a dispute whose only `mediation_events` entries are
  `start_attempt_started` and `start_attempt_stopped` (the attempt
  never reached reasoning or take-dispute, e.g. the eligibility
  gate or the auth gate refused) MUST NOT trigger an FR-124 report.
  The rule of thumb: the report fires only if Serbero either
  reasoned about the dispute, took it, or spoke to a party. Routing follows "Solver-Facing Routing". The report MUST
  include, at minimum:
  - the dispute id and the linked mediation session id (if any);
  - the latest known mediation classification and confidence (if
    any), or an explicit "no classification recorded" marker;
  - a counter of outbound party-facing messages Serbero sent
    (0, 1, or 2), distinguishing the three cases where Serbero spoke
    to neither party, one party, or both;
  - the final observed dispute status from the Phase 1/2 lifecycle;
  - a short narrative stating that the dispute resolved without
    further Serbero-driven escalation, derived from lifecycle
    transitions — Serbero MUST NOT infer party intent from chat it
    did not observe.

  The report MUST fire even when (a) no first outbound party message
  was ever sent, (b) the session is `escalation_recommended`, or
  (c) the session was closed before the resolution event. The report
  MUST NOT fire when Serbero never produced any mediation context for
  the dispute (a Phase 1/2-only case). Free-text rationale MUST follow
  FR-120 (reference id only in general logs).

- **FR-125** *(event-driven mid-session advancement)*: After the
  ingest tick persists a fresh inbound envelope for a live session
  (`awaiting_response`) AND the existing US4 round-limit check does
  NOT escalate, Serbero MUST evaluate whether the session can advance
  within one ingest-tick cycle. The evaluator MUST reconstruct the
  session transcript, call the reasoning provider's `classify` with
  that transcript and the session's pinned prompt bundle, feed the
  `ClassificationResponse` into `policy::evaluate`, and dispatch the
  returned `PolicyDecision`. The evaluator MUST be invoked at most
  once per `round_count` value per session (see FR-127).

- **FR-126** *(dispatch on PolicyDecision mid-session)*: Given a
  `PolicyDecision` from `policy::evaluate`, mid-session dispatch MUST
  mirror open-time dispatch:

  - `AskClarification(text)` → draft per-party outbound messages,
    persist two `mediation_messages` rows (one per party) and the
    state transition atomically inside one DB transaction, then
    publish both gift-wraps. On success the session progresses to
    `awaiting_response`. Failure handling MUST match the existing
    open-time drafter (`draft_and_send_initial_message`): the DB
    commit happens before publish, so a publish failure for either
    party returns `Err` with the rows already committed and NO
    automatic retry. The operator sees the failure via the log line
    and (for Phase 11) via the `consecutive_eval_failures` counter;
    the session keeps its existing drafter semantics rather than
    inventing a new recovery path mid-session. Partial-publish
    recovery is explicitly out of scope for Phase 11 — see "Non-Goals
    (Phase 11)" below.
  - `Summarize { classification, confidence }` → delegate to the
    existing `deliver_summary` entrypoint with the mid-session
    transcript; the session progresses `classified → summary_pending
    → summary_delivered → closed` as it does today on the open-time
    cooperative-summary path.
  - `Escalate(trigger)` → delegate to `escalation::recommend(...)`
    and transition the session to `escalation_recommended`; Phase 4
    consumes the handoff as today.

  The mid-session drafter for `AskClarification` MAY share code with
  the open-time drafter but MUST emit a round-number marker in the
  outbound body so parties can distinguish a follow-up question from
  the opening one. The drafter MUST NOT quote or re-send earlier
  Serbero outbound text in the next outbound body.

- **FR-127** *(evaluation idempotency)*: `mediation_sessions` MUST
  carry a `round_count_last_evaluated` column. The evaluator MUST
  read `round_count` and `round_count_last_evaluated` under the
  session lock, skip the evaluation when they are equal, and write
  `round_count_last_evaluated = round_count` atomically with the
  **DB commit** of the dispatched side effect (inside the same
  transaction that writes the outbound rows or the state
  transition). "Atomic with the DB commit" is branch (A) of the
  commit-vs-publish choice: a successful publish of the gift-wraps
  is NOT a precondition for the marker advance. Rationale:
  mirroring the open-time drafter's commit-then-publish pattern
  keeps the two paths symmetric and keeps the DB state inspectable
  without cross-referencing relay state. The consequence —
  partial-publish failures leave the marker advanced and the rows
  committed, blocking automatic retry — is a documented limitation
  (see "Non-Goals (Phase 11)" on partial-publish recovery). A
  crash between the reasoning call and the dispatch DB commit MUST
  NOT advance the marker, which means the next tick retries
  cleanly.

- **FR-128** *(transcript construction)*: The transcript passed to
  `classify` MUST include every `mediation_messages` row for the
  session with `direction = 'outbound'` (Serbero-authored) and
  `direction = 'inbound'` with `stale = 0`. Rows MUST be ordered by
  `inner_event_created_at` ascending, with a stable tie-breaker on
  the row `id` so identical timestamps do not cause
  non-deterministic ordering. Party role MUST be tagged from the
  session's `buyer_shared_pubkey` / `seller_shared_pubkey` mapping;
  an inbound whose shared pubkey matches neither side MUST be
  excluded and logged at `warn!`. No more than the last 40 rows are
  included (a guard against runaway token cost). Rows with
  `stale = 1` are NEVER included — they exist for audit purposes
  (see `phase3_stale_message` in the Phase 3 integration suite) but
  MUST NOT participate in classification. Duplicate inbound rows
  never land in the first place (the unique index
  `uq_mediation_messages_inner_event` blocks them), so no separate
  "duplicate" filter is needed.

- **FR-129** *(mid-session state transitions)*: The mid-session
  loop keeps the session in `awaiting_response` throughout. Only
  these transitions are written — and only when the corresponding
  decision fires, reusing the existing open-time helpers that
  already own those state flips:

  - `awaiting_response → summary_pending`: when `policy::evaluate`
    returns `Summarize`, by the existing `deliver_summary` entry
    point (which then progresses `summary_pending →
    summary_delivered → closed` as it does on the open-time
    cooperative-summary path).
  - `awaiting_response → escalation_recommended`: when the decision
    is `Escalate(trigger)`, by `escalation::recommend`.

  When the decision is `AskClarification`, the loop does NOT
  transition the session state at all: the follow-up drafter
  refreshes `last_transition_at` to mark that Serbero acted on this
  round and leaves the `state` column as `awaiting_response`. The
  single authoritative gate against re-dispatch is
  `round_count_last_evaluated` (FR-127).

  The `classified` and `follow_up_pending` states defined in
  `mediation_sessions.state` are NOT written by this loop. The
  state machine (`MediationSessionState::can_transition_to`)
  rejects the direct `classified → awaiting_response` edge, and
  composing the legal `classified → follow_up_pending →
  awaiting_response` pair inside a single transaction is ceremonial
  churn for outside observers because no ingest-tick observer ever
  sees the intermediate state. Earlier drafts of this FR called for
  using those states; the implementation (T119 drafter + T120
  orchestrator) drops them as a known, documented divergence. A
  future cleanup PR may remove the two unused variants from the
  enum; see "Non-Goals (Phase 11)".

- **FR-130** *(failure isolation and bounded retry)*: A reasoning-
  call or DB failure during mid-session evaluation MUST log at
  `warn!`, leave `round_count_last_evaluated` unchanged, and NOT
  block other sessions on the same tick. The next tick retries the
  same round. Consecutive failures for the same session MUST be
  tracked on `mediation_sessions.consecutive_eval_failures`; at the
  third consecutive failure the session MUST escalate with
  `EscalationTrigger::ReasoningUnavailable` using the existing
  escalation machinery. Any successful evaluation resets
  `consecutive_eval_failures` to 0.

- **FR-131** *(concurrency guard)*: A single session's evaluator
  MUST NOT run concurrently with another evaluation of the same
  session. The current single-task ingest tick satisfies this by
  construction; a regression test MUST assert that a manual second
  invocation on the same `round_count` is rejected by the FR-127
  marker without emitting duplicate outbounds.

### Key Entities *(include if feature involves data)*

- **MediationSession**: A single mediation attempt for one dispute.
  Key attributes: session id, dispute id (FK to Phase 1/2 `disputes`),
  assigned solver reference, session state
  (`opening`, `awaiting_response`, `classified`, `summary_pending`,
  `summary_delivered`, `escalation_recommended`, `closed`), round
  count, started-at / last-transition-at timestamps,
  `prompt_bundle_id`, `policy_hash`, current classification and
  confidence.

- **MediationMessage**: A single chat message, inbound or outbound,
  linked to a session. Key attributes: message id, session id,
  direction (`inbound` / `outbound`), party identifier (buyer /
  seller / solver / serbero), shared pubkey used for addressing,
  inner event id, inner event timestamp (authoritative), content,
  prompt-bundle reference (outbound only), persistence timestamp,
  stale flag.

- **MediationSummary**: A structured summary produced by the reasoning
  provider at the cooperative-resolution path. Key attributes:
  summary id, session id, dispute id, classification, confidence,
  suggested cooperative next step, prompt bundle reference,
  generated-at timestamp.

- **MediationEvent**: A session-level audit entry, recording
  transitions, escalation triggers, timeout firings, reasoning
  failures, and authorization losses. Key attributes: event id,
  session id, event kind, payload (structured, reference-only — no
  large free-text blobs), timestamp.

- **ReasoningRationale**: A controlled audit store for the full,
  unredacted rationale text returned by the reasoning provider, kept
  separate from general application logs (see FR-120). Key
  attributes: rationale id (content-addressed hash and/or
  session-scoped id — the same id that appears in general logs),
  session id, provider name, model identifier, `policy_hash`,
  `prompt_bundle_id`, rationale text, generated-at timestamp. Access
  MUST be operator-scoped; general log readers MUST see only the
  reference id.

- **PromptBundle**: The versioned set of prompt and policy files
  loaded at daemon startup (or on demand). Not a DB table per se, but
  its content hash MUST be referenced from every MediationSession.

- **ReasoningProviderConfig**: The effective provider/model/endpoint
  used by the daemon, derived from `[reasoning]` in `config.toml` plus
  environment-variable credentials. Not persisted per session — but
  the provider/model pair used for each reasoning call MUST be logged
  in `MediationEvent` rows for audit.

## Mediation Transport Requirements

This section is normative. It expands TC-101.

Serbero MUST use the same dispute-chat key reconstruction and
message addressing mechanism that is actually used by Mostro clients
for solver-visible dispute chat. This contract does **not** assume
that Mostro's public chat specification alone fully defines that
mechanism; it MUST be verified against the real flow used by current
Mostro clients and the Mostrix reference implementation. A generic
ECDH shortcut between Serbero's long-term secret and a party's
primary pubkey is explicitly forbidden unless that exact mechanism
is verified in the implementation flow used by Mostro clients.

- **Chat-addressing key material is obtained by following the
  dispute-chat interaction flow used by current Mostro clients**,
  not invented by Serbero. When Serbero, acting with its registered
  solver identity, participates in that flow, the flow yields or
  allows reconstruction of the per-party chat key material required
  to address party messages. The concrete mechanism — which key
  material is exchanged, how it is tied to the dispute and the
  counterparties, how each side reconstructs a chat-addressing key —
  is the one exercised in the Mostrix reference
  (`src/util/order_utils/execute_take_dispute.rs` and
  `src/util/chat_utils.rs`) and described at a higher level in
  `https://mostro.network/protocol/chat.html`. Serbero MUST verify
  that mechanism against current Mostro / Mostrix behavior and MUST
  NOT substitute a standalone ECDH between its own secret key and a
  party's primary pubkey as a generic shortcut.
- **Outbound messages are addressed to the per-party chat pubkey
  produced by that reconstruction**, not to a party's own primary
  pubkey. Serbero reconstructs the chat-key material as Nostr `Keys`
  (matching the Mostrix `chat_utils.rs` pattern, to be verified at
  implementation time) and uses that keypair for addressing and
  signing the inner event.
- **Outbound mediation content MUST be wrapped as a NIP-44-encrypted
  inner event (`kind 1`) embedded in a NIP-59 gift-wrap
  (`kind 1059`)** with a `p` tag pointing at the per-party chat
  pubkey, as modeled in Mostrix's `src/util/chat_utils.rs`.
- **On the inbound path**, Serbero MUST: fetch gift-wraps addressed
  to the per-party chat pubkey, decrypt with the reconstructed chat
  key material, parse and verify the inner event, and treat the
  inner event's content and `created_at` as authoritative
  mediation-session facts.
- The outer gift-wrap's timestamp MUST NOT be used as a session-fact
  timestamp. All mediation ordering, round counting, and timeout
  evaluation MUST be driven by inner-event timestamps.
- Direct DMs to a party's primary pubkey MUST NOT be used for
  mediation content. Solver-facing DMs (Phase 1/2 notifications) are
  unrelated and continue to use the Phase 1/2 notifier unchanged.
- If Serbero has not completed the dispute-chat interaction flow for
  a dispute (e.g. another solver already took it, the flow failed,
  or the flow returns material that does not match the verified
  reconstruction mechanism), Serbero MUST NOT address mediation
  messages for that dispute. In that case the dispute is handled by
  the human solver via Mostro; Phase 3 transitions the session to
  `superseded_by_human` or closes it without sending party messages.

## Mediation Start-Flow Ordering

This section is normative. It expands FR-121, FR-122, and FR-123 and
fixes the sequence by which a new dispute enters guided mediation.

The intended product behavior is: a dispute is opened → Serbero
evaluates it immediately → if mediation-eligible, Serbero takes the
dispute immediately → Serbero contacts both parties. The mediation
engine exists to make that flow resumable and retry-able, not to be
the only way a session can begin.

### Trigger

- The canonical trigger for opening a new mediation session is the
  Phase 1/2 dispute-detection event (FR-121). The handler for that
  event MUST attempt the start flow synchronously, in-path, before
  returning.
- A periodic engine tick MAY exist, but its role is bounded to:
  resuming open sessions after a daemon restart; retrying start
  attempts that failed transiently; re-evaluating eligibility after a
  config reload or after `mediation_sessions` state changes; and
  handling disputes observed via backfill after downtime. The tick
  MUST NOT be the only path by which a new dispute can reach
  mediation.
- The same eligibility predicate (FR-123) MUST be used by both the
  event-driven path and the tick, so they cannot disagree about what
  is eligible.

### Strict ordering within a start attempt

Within a single attempt to open a mediation session for a dispute,
Serbero MUST perform these steps in order. Each step is a precondition
for the next:

1. **Eligibility**: evaluate the composed predicate from FR-123
   against the current dispute and session state. If the dispute is
   not eligible, stop.
2. **Reasoning health**: confirm the reasoning provider is reachable
   (TC-102). If it is not, stop — do not take the dispute.
3. **Reasoning verdict**: ask the reasoning layer to classify the
   dispute under the loaded policy bundle (TC-103, FR-105). If the
   verdict is not that the dispute is mediation-eligible, stop — do
   not take the dispute.
4. **TakeDispute**: only now, issue the `TakeDispute` Mostro action
   using Serbero's solver identity. If this fails, stop and let the
   engine tick retry under FR-121's safety-net role.
5. **Commit session**: only after `TakeDispute` succeeds, insert the
   `mediation_sessions` row (pinning `prompt_bundle_id`,
   `policy_hash`, `instructions_version`) and emit a
   `mediation_events` row recording the verdict and take.
6. **First party-facing message**: send the first clarifying message
   via the Mostro chat transport (FR-101, Transport Requirements).

This ordering is normative. In particular:

- `TakeDispute` MUST NOT precede the reasoning verdict (FR-122). A
  run that takes the dispute first and classifies afterward is a spec
  violation.
- A `mediation_sessions` row MUST NOT be committed before
  `TakeDispute` succeeds. An orphan session row with no take and no
  outbound message is a spec violation.
- If the reasoning provider becomes unreachable between step 2 and
  step 3, Serbero MUST NOT fall back to a scripted or manual take.
  The attempt stops; the dispute remains eligible per FR-123 so the
  engine tick can retry once reasoning recovers.

### Relationship to the background engine

The background engine continues to exist. Its responsibilities after
this ordering becomes normative are limited to:

- resuming already-open sessions (FR-117);
- retrying start attempts that failed transiently, for disputes still
  eligible under FR-123;
- handling disputes that were missed by the event-driven path during
  daemon downtime and observed through Phase 1/2 backfill;
- advancing session state for timeouts, inbound responses, and
  escalation triggers (unchanged from prior sections).

It is explicitly NOT a gatekeeper for whether a new dispute ever
reaches mediation. The detection event is.

## Reasoning Provider Configuration

This section is normative and expands TC-102, TC-105, and
FR-102–FR-104. The mandatory configuration fields (provider, model,
API base URL, credential source, request timeout, retry / failure
behavior) are defined in FR-104 and are not restated here.

- **Provider abstraction boundary**: provider-specific request
  shaping MAY live behind a small provider adapter in code, but the
  mediation call sites MUST NOT switch on provider-specific types —
  they see a single "reasoning request → reasoning response" shape.
  A new provider is added by writing an adapter, not by editing
  mediation logic.
- **Credentials**: MUST NOT be stored in `config.toml` directly. The
  config references an environment variable name (`api_key_env`);
  the operator supplies the credential via the environment or a
  secrets mechanism that populates the environment.
- **Failure behavior**: if the provider is absent, unreachable, or
  returns malformed output, mediation MUST NOT fabricate
  classifications, summaries, or messages. Bounded retry is allowed
  per the failure-behavior config; if the bound is exhausted, the
  session escalates with trigger `reasoning_unavailable`.
- **Health checks**: a provider health check SHOULD run at startup
  and whenever the configuration is reloaded, so unreachability
  surfaces before it impacts an actual mediation session.

## Instruction and Policy Storage

This section is normative and expands TC-103 and FR-105–FR-106. The
core "versioned files are the source of truth" rule is stated in
those requirements and is not restated here.

- **Repository layout**: the repository MUST contain a `prompts/`
  (or similarly named) directory with versioned files covering at
  least: system instructions and mediator identity, classification
  criteria, escalation policy, message templates, mediation style
  and tone, and honesty / uncertainty rules. Example:

  ```text
  prompts/
    phase3-system.md
    phase3-classification.md
    phase3-escalation-policy.md
    phase3-message-templates.md
    phase3-mediation-style.md
  ```

- **Bundle hashing and pinning**: the daemon MUST compute a
  deterministic `policy_hash` over the exact bytes of the bundle it
  loaded at session start, and MUST store that hash alongside every
  `MediationSession`, `MediationEvent`, and `MediationSummary` row it
  produces during that session.
- **Why files, not DB**: committed prompt files are auditable and
  git-reviewable, diffable in PRs, and deterministically reconstructable
  from history alone. Primary-sourcing these artifacts from the
  database erases every one of those properties.

## Mediation Memory Model

This section is normative. It expands FR-106, FR-107, FR-117, and
FR-119 and makes the "agent memory" surface concrete.

**A. Operational dispute/session memory lives in SQLite**, as
auditable system memory. This includes mediation session id, linked
dispute id, session state, outbound and inbound messages, message
timestamps derived from inner events, round counters, classification
state, current confidence, last generated summary reference,
escalation reasons, outcome transitions, last-seen markers for chat
session continuity, prompt-bundle references, and the effective
provider/model for each reasoning call.

**B. Stable behavior and policy memory lives in versioned repo
files**, as described in "Instruction and Policy Storage" above.
Agent behavior, identity, tone, limits, and policies MUST NOT be
stored primarily in SQLite.

**C. Runtime context is reconstructed** per reasoning call from: (i)
the current dispute/session row in SQLite, (ii) the mediation message
history for that session in SQLite, (iii) the prompt and policy files
loaded from the repository (pinned by the session's `policy_hash`),
and (iv) the effective reasoning-provider configuration. The
reasoning provider is stateless from Serbero's perspective — there is
no privileged "agent memory" outside these inputs.

Memory separation enforces two properties that are easy to lose in
LLM-based systems: the agent's behavior is reproducible from committed
artifacts, and the dispute record is auditable from database rows
without replaying a model.

## AI Agent Behavior Boundaries

Serbero's use of a reasoning provider during Phase 3 is scoped by
explicit behavioral and authority boundaries.

- **Mediation identity**: The agent identifies itself as Serbero, an
  assistance system that helps the assigned solver by gathering
  information and drafting summaries. It does not claim authority
  over the dispute outcome. This identity is pinned in the system
  prompt file, not re-invented at runtime.
- **Authority boundaries**: The agent MUST NOT produce outputs whose
  only reasonable execution is a fund-moving or dispute-closing
  action. Any such output MUST be suppressed and escalated per
  FR-116.
- **Escalation policy constraints**: The agent MUST NOT override the
  escalation policy. If the classification policy marks a case as
  out-of-scope for guided mediation (fraud indicators, conflicting
  factual claims, low confidence), mediation stops and the session
  escalates, regardless of any "suggested mediation" the model may
  add to its output.
- **Tone**: Neutral, clear, calm, and explicit about not being the
  final authority. Specified in `phase3-mediation-style.md`.
- **Honesty**: The agent MUST state uncertainty rather than invent
  facts. If it cannot confidently determine what happened from party
  responses, the correct output is either "ask another targeted
  clarifying question" or "escalate to human", never "pretend I know".
  Specified in the system and mediation-style policy files.
- **Allowed outputs**: classification labels with a confidence score,
  next-message drafts sourced from configured templates, structured
  summaries for the assigned solver, and explicit escalation
  recommendations.
- **Disallowed outputs**: autonomous dispute closure, any text framed
  as a binding decision, fund-related instructions, or fabricated
  factual claims about who sent what or who is telling the truth.
- The enforcement of these boundaries happens in Serbero's policy
  layer (code + prompt files), not in the model alone. "The model
  will handle it" is not an acceptable control.

## Solver-Facing Routing

Phase 3 solver-facing notifications (progress updates, cooperative
summaries, escalation recommendations) use the Phase 1/2 notifier but
follow a single, explicit routing rule, resolving the ambiguity
between "the assigned solver" and "every configured solver":

- **If the underlying dispute has a Phase 2 `assigned_solver` set**
  (Phase 2 recorded an `s=in-progress` assignment event and captured
  the taking solver's pubkey), all Phase 3 solver-facing DMs for that
  dispute MUST route ONLY to that assigned solver. A human has taken
  ownership; broadcasting Phase 3 output to every configured solver
  at that point is noise.
- **If the underlying dispute has no `assigned_solver` yet**, Phase 3
  solver-facing DMs MUST be broadcast to every configured solver via
  the Phase 1/2 notifier, mirroring how Phase 1/2 handles initial and
  re-notifications. There is no separate "assigned solver" concept
  for Phase 3 prior to Phase 2 assignment.
- **`MediationSession.assigned_solver`** is a mirror of the Phase 2
  `disputes.assigned_solver` column at the time the routing decision
  is made. Phase 3 does not introduce an independent assignment.
- **Escalation-recommendation notifications** follow the same rule:
  broadcast while unassigned, targeted once assigned. When Phase 4 is
  deployed, it MAY additionally re-route escalation handoffs to
  write-permission solvers — that routing layer is Phase 4's
  responsibility and sits on top of this rule, not in place of it.
- **Re-routing on assignment change**: if a dispute gains an
  `assigned_solver` while a mediation session is open, subsequent
  Phase 3 solver-facing DMs MUST switch from broadcast to targeted.
  Already-delivered broadcasts are not recalled.

Every reference to "the assigned solver" elsewhere in this spec
resolves through this section. US3 Scenario 1, FR-112, FR-113, the
`assigned solver reference` attribute in the `MediationSession`
entity, and any later reference all use the same rule.

## Final Solver Report on External Resolution

This section is normative. It expands FR-124 and specifies exactly
what Serbero owes the solver set when a dispute resolves in a path
Serbero did not fully drive.

### When it fires

A final solver-facing report MUST be emitted when ALL of:

- a Phase 1/2 dispute-lifecycle transition moves the dispute to a
  resolved terminal state (Mostro has closed the dispute — settled,
  cancelled, released to buyer, released to seller, or the equivalent
  terminal label in use);
- Serbero has collected at least one piece of mediation context for
  that dispute (see FR-124 — a reasoning verdict, classification, any
  `mediation_sessions` row including `escalation_recommended`, any
  `mediation_messages` row, or any `mediation_events` row).

The report MUST fire exactly once per resolved dispute, independent of
how many mediation sessions existed for it (there may be more than
one, e.g. a `superseded_by_human` session followed by a re-dispute).
Duplicate delivery MUST be prevented (idempotency key: dispute id +
final observed status).

### When it does NOT fire

- When no mediation context exists for the dispute (pure Phase 1/2
  case). Phase 1/2 notifications stand on their own.
- Before the dispute reaches a resolved terminal state. A session
  closing internally (e.g. `summary_delivered`) is not a trigger for
  the external-resolution report — the Phase 3 summary flow (US3)
  covers that case.

### What the report contains

At minimum, and consistent with FR-124:

- dispute id and linked session id(s);
- latest known mediation classification and confidence, if any,
  OR an explicit "no classification recorded" marker;
- a counter of outbound party-messages sent by Serbero (0 / 1 / 2+),
  so the solver can tell whether Serbero spoke to both parties, one
  party, or neither;
- final observed dispute status from the Phase 1/2 lifecycle;
- a short narrative derived from lifecycle transitions, stating that
  the dispute resolved without further Serbero-driven escalation.

The report MUST NOT reconstruct or quote party-to-party chat that
Serbero never received. Serbero does not subscribe to buyer-seller
chat, only to the mediation channel. If Serbero's own mediation
exchange produced content, that content MAY be summarized in the
report per FR-120 constraints (reference id in logs, full text in
the controlled audit store).

### Routing and transport

The report is a solver-facing DM and uses the Phase 1/2 notifier
unchanged. Routing follows "Solver-Facing Routing" above: targeted to
the assigned solver when one exists, broadcast otherwise. The report
MUST NOT be sent into the Mostro chat transport (that transport is
party-facing only).

### Interaction with escalation

If the session is in `escalation_recommended` at the moment the
resolved-terminal transition is observed:

- the escalation handoff package remains in place for Phase 4 to
  consume as historical context;
- the final report MUST still fire (this is the most common case the
  issue behind FR-124 calls out);
- the report narrative MUST note that an escalation was recommended
  but the dispute resolved before Phase 4 acted on it.

## Mid-Session Follow-Up Loop

### Background

Phase 3 US1 opens a mediation session and dispatches the first
clarifying outbound. Phase 3 US2 ingests party replies into
`mediation_messages` and increments `round_count`. A 2026-04-21 audit
confirmed that the link between those two — the step that, after a
party replies, re-classifies the transcript and dispatches the next
side effect — is missing in `main`: `policy::evaluate` has zero
production call sites, no code path drives `awaiting_response →
classified` in response to an inbound, and the `follow_up_pending`
state is never written. The runtime-observed symptom: parties reply,
Serbero persists the replies, and then nothing happens.

This section closes that loop. Scope is deliberately narrow — one
new trigger, one idempotency marker, one bounded-failure counter,
three new integration tests. No prompt-bundle changes, no new
states, no transport changes.

### Flow

```text
run_ingest_tick persists a fresh inbound envelope for session S
       │
       │ US4 round-limit check (existing) does NOT escalate
       ▼
┌─────────────────────────────────────────────────────────┐
│ advance_session_round(S, round_count)                   │
│                                                         │
│ (1) gate: round_count > round_count_last_evaluated      │
│     — else skip (FR-127 idempotency)                    │
│ (2) reconstruct transcript from mediation_messages      │
│     (FR-128: ordered, annotated by party, ≤ 40 rows)    │
│ (3) reasoning.classify(transcript, pinned_bundle)       │
│ (4) policy::evaluate → PolicyDecision                   │
│     (writes classification_produced in its own tx)      │
│ (5) dispatch on decision (FR-126 + FR-129):             │
│       AskClarification → T119 drafter:                  │
│         - state stays `awaiting_response` throughout    │
│           (no mid-session classified transition)        │
│         - 2 outbound rows + advance_evaluator_marker    │
│           committed in one tx                           │
│         - publish gift-wraps outside the tx             │
│       Summarize → pre-transition                        │
│         `awaiting_response → classified`, then          │
│         deliver_summary walks classified →              │
│         summary_pending → summary_delivered → closed,   │
│         then advance_evaluator_marker in a short tx     │
│       Escalate → escalation::recommend (session →       │
│         escalation_recommended); marker is moot         │
│ (6) on AskClarification, the marker advanced at (5)     │
│     and consecutive_eval_failures reset to 0 in the     │
│     same tx that committed the outbound rows            │
└─────────────────────────────────────────────────────────┘
```

Failure handling by step:

- **(3) reasoning call** or **(4) policy evaluation** error: log at
  `warn!`, leave `round_count_last_evaluated` unchanged, increment
  `consecutive_eval_failures`. The next ingest tick retries the same
  round. At the third consecutive failure the session escalates with
  `ReasoningUnavailable` (FR-130). No partial state is written.
- **(5) dispatch** error:
  - For `AskClarification`, the drafter commits the two
    `mediation_messages` rows AND `advance_evaluator_marker`
    atomically, THEN publishes gift-wraps. Per FR-127 branch (A),
    a publish failure returns `Err` with rows + marker already
    committed — partial-publish recovery is a Non-Goal. The
    dispatch-error path bumps `consecutive_eval_failures`; the
    unified failure handler escalates with `ReasoningUnavailable`
    on the third consecutive failure (FR-130) so persistent
    dispatch issues eventually surface to a human.
  - For `Summarize`, `deliver_summary` owns its own transaction and
    state progression (`classified → summary_pending → ...`). On
    failure the existing `deliver_summary` error path applies
    unchanged; `advance_evaluator_marker` is called in a post-commit
    DB op on success only.
  - For `Escalate`, `escalation::recommend` transitions the session
    out of `awaiting_response`; `consecutive_eval_failures` is moot
    at that point because no further evaluations will run on this
    session.

### What this loop does NOT do

- No new state definitions. `classified`, `summary_pending`,
  `summary_delivered`, `escalation_recommended` already exist.
  `follow_up_pending` stays unused (kept for a future cleanup PR;
  see "Non-Goals" below).
- No prompt-bundle changes. Uses the same `policy_hash` the session
  was opened with.
- No change to the transport or key lifecycle. Mid-session outbounds
  are gift-wraps on the Mostro chat transport, keyed by the same
  per-party shared pubkeys the session was opened with.
- No multi-tick concurrency. Assumes single-task ingest tick (current
  behavior).
- No free-text escalation reasons. `escalation::recommend` receives
  one of the existing `EscalationTrigger` variants.

### Non-Goals (Phase 11)

The following are deliberately out of scope. Each belongs in a
subsequent slice with its own spec amendment.

- **Partial-publish recovery.** When the mid-session `AskClarification`
  dispatch commits both `mediation_messages` rows but the gift-wrap
  publish for one of the two parties fails, Phase 11 returns `Err`,
  leaves the rows committed (consistent with the open-time drafter),
  and does NOT emit a dedicated `outbound_send_failed` audit row, does
  NOT maintain a retry queue, and does NOT automatically republish.
  The session keeps the same visibility / recovery posture it has
  today when the opening drafter fails mid-publish. Known consequence:
  the "silent" party does not receive the clarifying question and
  will not reply; the round-limit check (US4) eventually escalates
  that session.
- **Removing the `follow_up_pending` enum variant + CHECK constraint.**
  Phase 11 does not write the state and does not touch the schema
  around it. A future housekeeping PR can remove it after confirming
  no downstream consumer (Phase 4 handoff, analytics, replay tooling)
  depends on the string form.
- **Promoting the 40-row transcript cap to config.** `N = 40` stays
  hardcoded for this increment; a later PR may expose it as
  `[mediation].max_transcript_messages`.
- **Multi-tick concurrency.** If future scale requires parallel
  per-session tick workers, `round_count_last_evaluated` is the
  correct idempotency primitive, but the concurrency harness itself
  is out of scope here.

## Solver Identity and Authorization

- Serbero MUST act under a Nostr keypair loaded from config (same
  mechanism as the Phase 1/2 daemon identity). This is Serbero's
  operational solver identity during Phase 3.
- The pubkey corresponding to Serbero's configured secret key MUST be
  registered in the target Mostro node as a solver with at least
  `read` permission *before* Phase 3 starts mediating.
- Registration is an operator action outside Serbero's scope. Serbero
  MUST NOT attempt to grant, elevate, or repair its own permission.
- If the precondition cannot be confirmed (the startup verification
  against Mostro fails or returns no solver record for Serbero's
  pubkey), Serbero MUST log an actionable ERROR-level message,
  refuse to open any new mediation session, and enter a bounded
  revalidation loop. The loop MUST:
  - run an immediate verification attempt at daemon startup;
  - re-run verification on any operator-triggered configuration
    reload;
  - otherwise retry on a truncated exponential backoff starting at
    `solver_auth_retry_initial_seconds` (default 60) and doubling up
    to `solver_auth_retry_max_interval_seconds` (default 3600);
  - terminate after the first of
    `solver_auth_retry_max_total_seconds` (default 86400) or
    `solver_auth_retry_max_attempts` (default 24) is exceeded, at
    which point Serbero MUST stop retrying and emit a terminal
    WARN-or-higher alert explicitly recommending operator action;
  - run entirely in the background: Phase 1/2 detection, notification,
    and re-notification MUST remain fully operational throughout the
    retry window and after termination.
  If revalidation succeeds at any point — Mostro returns a valid
  solver record for Serbero's pubkey with at least `read` permission
  — Serbero resumes normal Phase 3 operation without requiring a
  restart and MAY open mediation sessions for disputes that are still
  in a mediation-eligible state.
- If solver permission is revoked mid-session, outbound mediation
  traffic will fail or be rejected. Serbero MUST treat this as
  `authorization_lost` and escalate the affected session; it MUST
  also re-enter the revalidation loop described above rather than
  silently continuing to attempt mediation on unaffected sessions
  under presumed-good permission.

## Configuration Surface

Phase 3 extends Serbero's `config.toml` with functional sections.
Section names reflect function (`[mediation]`, `[reasoning]`,
`[prompts]`), not roadmap numbering — there is no `[phase3]` section.
A representative example:

```toml
[mediation]
enabled = true
max_rounds = 2
party_response_timeout_seconds = 1800

[reasoning]
enabled = true
provider = "openai"
model = "gpt-5"
api_base = "https://api.openai.com/v1"
api_key_env = "SERBERO_REASONING_API_KEY"
request_timeout_seconds = 30
# Bounded HTTP-level retry budget for the reasoning adapter. Lives
# here — not under [mediation] — because the adapter is what performs
# these retries (FR-104, plan degraded-mode table).
followup_retry_count = 1

[prompts]
system_instructions_path      = "./prompts/phase3-system.md"
classification_policy_path    = "./prompts/phase3-classification.md"
escalation_policy_path        = "./prompts/phase3-escalation-policy.md"
mediation_style_path          = "./prompts/phase3-mediation-style.md"
message_templates_path        = "./prompts/phase3-message-templates.md"

[chat]
# optional: tuning knobs for the Mostro-chat transport
inbound_fetch_interval_seconds = 10

[timeouts]
# optional: cross-cutting overrides of defaults declared elsewhere
```

Exact field names MAY be refined during planning, but the
architecture MUST preserve the same functional separation:

- `[mediation]`: feature gating and behavioral knobs (round limits,
  party timeouts, retry counts).
- `[reasoning]`: provider, model, endpoint, credentials, timeouts,
  failure behavior.
- `[prompts]`: paths to the versioned prompt bundle files.
- `[chat]` (optional): Mostro-chat transport tuning.
- `[timeouts]` (optional): cross-cutting timeout overrides.

Environment variable overrides (including `SERBERO_PRIVATE_KEY`,
`SERBERO_DB_PATH`, `SERBERO_LOG` from prior phases, plus
`api_key_env`-referenced credentials) continue to apply.

## Outcomes and Non-Goals

### Allowed Phase 3 outcomes

- `additional_clarification_requested` — Serbero produced a follow-up
  question for one or both parties.
- `awaiting_response` — Serbero is waiting on an inbound reply.
- `coordination_summary_delivered` — Serbero delivered a structured
  summary of a cooperative resolution to the assigned solver.
- `escalation_recommended` — Serbero has identified an escalation
  trigger and assembled the Phase 4 handoff package.
- `mediation_session_timed_out` — the mediation window closed without
  convergence; equivalent to an escalation with trigger
  `mediation_timeout`.
- `superseded_by_human` — a human solver took the dispute via Mostro
  while mediation was in progress.
- `resolved_externally_reported` — the dispute transitioned to a
  resolved terminal state (parties coordinated outside Serbero's
  mediation channel, or a solver closed it through Mostro) and
  Serbero emitted the final solver-facing report per FR-124 and
  "Final Solver Report on External Resolution".

### Forbidden Phase 3 outcomes

- Dispute closure executed by Serbero with final authority.
- `admin-settle` executed by Serbero.
- `admin-cancel` executed by Serbero.
- Any output that encodes "I have decided the dispute" rather than
  "here is a structured recommendation for the solver".
- Fabricated party statements, fabricated confirmations, or any
  content presented as fact that was not produced by a party response.

### Out of Phase 3 scope

- Implementing Phase 4 escalation execution (notifying
  write-permission solvers, tracking acknowledgements, re-escalation
  under load). Phase 3 prepares the handoff; Phase 4 consumes it.
- Implementing a reasoning-backend-agnostic trait layer beyond what
  the provider configuration already requires. Deeper backend
  portability (e.g. OpenClaw routing) is Phase 5.
- Direct negotiation or settlement coordination between parties where
  funds are involved — this remains Mostro's authority.
- Multi-instance Serbero coordination.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-101**: A cooperative low-risk coordination dispute (payment
  delay with responsive parties) can be walked through a full guided
  mediation round — classification → outbound question → inbound
  response → summary delivered to the assigned solver — without any
  human operator intervention during mediation.
- **SC-102**: Zero dispute-closing actions are executed by Serbero.
  Verifiable by: no `admin-settle` or `admin-cancel` events signed by
  Serbero's pubkey exist, at any point, for any dispute touched by
  Phase 3.
- **SC-103**: Every mediation session references a specific
  `policy_hash`. Given only git history and the session's
  `policy_hash`, an auditor can reconstruct the exact prompt and
  policy bundle that governed that session's behavior.
- **SC-104**: Within the shipped Phase 3 scope, swapping the
  reasoning endpoint between OpenAI (`api_base =
  https://api.openai.com/v1`) and any OpenAI-compatible endpoint
  (different `api_base`, compatible chat-completions shape) requires
  only `config.toml` and environment-variable changes — no code
  change. Selecting `provider = "anthropic"` or `provider = "ppqai"`
  in Phase 3 MUST fail at startup with an actionable error (via the
  `NotYetImplemented` adapter stubs) rather than silently falling
  back to OpenAI. Shipping those additional adapters is future work;
  the boundary is designed so they can land without editing
  mediation call sites.
- **SC-105**: When the reasoning provider is unconfigured or
  unreachable, Phase 3 mediation is halted, the reason is visible in
  logs and in any affected session row, and Phase 1/2 dispute
  detection and solver notification continue unaffected.
- **SC-106**: 100 % of disputes that trigger any of: conflicting
  factual claims, fraud indicators, confidence below the configured
  threshold, party unresponsive past timeout, round-limit exceeded —
  are transitioned to `escalation_recommended` with the corresponding
  trigger recorded. No disputes with those conditions silently remain
  in mediation.
- **SC-107**: All mediation traffic between Serbero and parties is
  observable on the Mostro-chat transport (gift-wrapped events
  addressed to the ECDH-derived shared pubkeys). No mediation
  content is observable as a direct NIP-17 DM addressed to a party's
  primary pubkey.
- **SC-108**: After a daemon restart mid-session, every open
  mediation session can be resumed from SQLite state alone without
  losing round counts, last-seen markers, or prompt-bundle pinning.
- **SC-109** *(immediate event-driven start)*: For a dispute that is
  mediation-eligible under FR-123 and for which the reasoning
  provider is healthy, the session-open attempt (reasoning verdict →
  `TakeDispute` → first party-facing message) MUST be initiated from
  within the Phase 1/2 dispute-detection event-handling path, not
  from a later periodic sweep. Verifiable by: disabling the
  background engine tick in a test harness and confirming that a
  newly detected eligible dispute still reaches the first party-
  facing message without the tick running.
- **SC-110** *(take coupled to reasoning)*: No `TakeDispute` action
  signed by Serbero's pubkey exists for any dispute unless a
  preceding `mediation_events` row records a positive reasoning
  verdict for that dispute within the same session-open attempt, and
  no `mediation_sessions` row exists whose insertion predates a
  successful `TakeDispute` for that dispute. Verifiable by SQL audit
  over `mediation_events` + `mediation_sessions` + the outbound
  Mostro-action log.
- **SC-111** *(external-resolution report)*: For 100 % of disputes
  that transition to a resolved terminal state AND for which Serbero
  had collected any mediation context (per FR-124), exactly one
  solver-facing final report was emitted via the Phase 1/2 notifier.
  Verifiable by joining dispute lifecycle transitions against
  `mediation_sessions` / `mediation_events` / `mediation_messages`
  and the notifier outbound log, and confirming no duplicates and no
  omissions.

- **SC-112** *(mid-session advancement happy path)*: Given a session
  in `awaiting_response` with one outbound on file, when a party
  replies and the ingest tick persists the reply, within one
  subsequent ingest-tick cycle Serbero publishes a second outbound
  to both parties (for a happy-path `AskClarification` classification).
  Verifiable by integration test `tests/phase3_followup_round.rs`.

- **SC-113** *(mid-session idempotency)*: After SC-112 completes,
  when the next ingest tick fires without any new inbound, Serbero
  does NOT publish additional outbounds for the same round.
  Verifiable by the same test (assertion on outbound row count and
  absence of duplicate `classification_produced` rows for the same
  `round_count`).

- **SC-114** *(mid-session summarize branch)*: Given a session where
  `policy::evaluate` returns `Summarize { classification, confidence }`
  on a mid-session call, when the ingest tick processes the
  triggering inbound, then `deliver_summary` fires exactly once and
  the session ends in `closed` with a `summary_delivered` event on
  file. Verifiable by integration test
  `tests/phase3_followup_summary.rs`.

- **SC-115** *(mid-session reasoning-failure escalation)*: Given a
  session whose reasoning calls fail three consecutive times on the
  mid-session path, the session MUST transition to
  `escalation_recommended` with trigger `ReasoningUnavailable`.
  Verifiable by integration test
  `tests/phase3_followup_reasoning_failure.rs`.

## Assumptions

- Phases 1 and 2 are deployed and operating as specified in
  `specs/002-phased-dispute-coordination/`. Phase 3 extends, and does
  not replace, the behaviors defined there.
- Mostro's chat protocol at
  <https://mostro.network/protocol/chat.html> is the authoritative
  reference for party communication. Material changes to that
  protocol will require an amendment to this spec.
- The Mostrix source files listed under "Reference Implementations"
  remain informative reference material for the Phase 3 transport and
  mediation-session modeling, but Serbero re-implements the
  functionality in-tree.
- Operators are responsible for registering Serbero's pubkey as a
  solver on the target Mostro instance, with at least `read`
  permission, before Phase 3 is enabled.
- The reasoning provider is operated by the operator (or a trusted
  third party the operator has selected). Model availability,
  billing, and abuse controls are outside Serbero's responsibility.
- No-model mediation is not a supported mode. Operators who do not
  want to run a model MUST leave `[reasoning].enabled = false` and
  `[mediation].enabled = false`, and Serbero remains a Phase 1/2
  daemon.
- The single-instance deployment assumption carries over from prior
  phases. Multi-instance coordination (leader election, shared queue)
  is out of scope.
- `nostr-sdk 0.44.1` remains the Nostr library of record (per TC-002
  in the earlier spec). Protocol-level constants
  (`NOSTR_DISPUTE_EVENT_KIND = 38386`, etc.) continue to be consumed
  via `mostro-core`.

## System Boundaries (Phase 3 view)

### Mostro owns (unchanged)

- Escrow state, fund custody, `admin-settle` / `admin-cancel`
  execution, dispute-closing authority, solver permission
  enforcement, and order lifecycle management.

### Serbero owns (Phase 3 additions, all additive)

- Guided mediation sessions over Mostro's chat transport.
- Persistent mediation memory in SQLite (sessions, messages,
  summaries, events).
- Reasoning-provider orchestration behind a vendor-neutral
  configuration.
- Enforcement of authority / honesty / escalation boundaries against
  reasoning-provider outputs.
- Preparation of Phase 4 escalation handoff packages.

### Boundary rules (restated for Phase 3)

- Serbero reads dispute state and writes mediation traffic only.
  Serbero never writes dispute-closing actions to Mostro.
- Serbero may suggest cooperative next steps to the assigned solver;
  the solver acts through Mostro, not through Serbero.
- If Serbero is offline or halted, Mostro and its operators continue
  to resolve disputes manually, exactly as in Phases 1 and 2.

## Explicit Non-Goals (Phase 3)

- Dispute closure by Serbero (any mechanism).
- Autonomous enforcement of party decisions.
- Fund movement or Lightning interaction.
- Phase 4 escalation execution mechanics (notification routing to
  write-permission solvers, acknowledgement tracking, repeated
  escalation under no-acknowledge). Phase 3 prepares the handoff
  only.
- Generic reasoning-backend abstraction beyond the provider-neutral
  configuration surface specified here (deeper abstraction is
  Phase 5).
- Shipping reasoning adapters other than OpenAI / OpenAI-compatible
  in this phase. Anthropic, PPQ.ai, and OpenClaw adapters are
  declared as `NotYetImplemented` at the boundary so the mediation
  call sites stay provider-agnostic and the config surface is stable,
  but selecting them in Phase 3 fails at startup. Landing those
  adapters is explicit future work and does not require changes to
  mediation call sites.
- Multi-instance Serbero coordination.
- Replacing the Phase 1/2 notifier. Solver-facing DMs continue to use
  the existing notifier unchanged.
