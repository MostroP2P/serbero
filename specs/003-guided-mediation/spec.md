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

- `src/util/chat_utils.rs` — shared-key derivation, gift-wrap
  construction, decrypt/verify pipeline.
- `src/models.rs` — session-state shapes, chat role handling.
- `src/util/order_utils/execute_take_dispute.rs` — the solver-take flow
  that precedes mediation, including the shared-key material used to
  address chat events.
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
a solver on the target Mostro instance, simulating the solver-take
flow that produces the shared chat keys, and then verifying that
Serbero emits the first mediation message into the Mostro chat channel
(not a direct DM), with content sourced from the configured prompt
bundle and identifying itself as an assistance system.

**Acceptance Scenarios**:

1. **Given** Phases 1 and 2 are running, a dispute is in `notified`
   state, and a configured reasoning provider is reachable,
   **When** the policy classifies the dispute as low-risk coordination
   (payment delay, confusion, unresponsive counterparty),
   **Then** Serbero derives the per-party shared chat keys using ECDH
   as specified by Mostro's chat protocol, publishes an initial
   clarifying message via gift-wrapped chat events addressed to the
   shared pubkeys of each party, records a new `mediation_sessions`
   row with state `awaiting_response`, and records the exact prompt
   bundle version used.

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
   missing precondition at ERROR level, MUST NOT retry in a loop, and
   MUST leave Phase 1/2 behavior unchanged.

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

A system operator wants to change the reasoning provider — for example
from OpenAI to Anthropic, to a self-hosted OpenAI-compatible endpoint,
or to PPQ.ai. They update the `[reasoning]` block of `config.toml`,
rotate credentials via environment variables, and restart Serbero.
Mediation resumes against the new provider with no code changes,
using the same prompt bundle and the same mediation session tables.

**Why this priority**: Portability is a stated constitutional
principle (no lock-in to any single vendor). It is not what unlocks
mediation but what makes Phase 3 safe to deploy and iterate on.

**Independent Test**: Can be tested by running the same mediation
fixture against two different provider configurations and asserting
that the mediation state transitions, summary shape, and message
format are identical, while the outbound HTTP surface differs per
provider.

**Acceptance Scenarios**:

1. **Given** Serbero is running with `provider = "openai"` and an
   `OPENAI_API_KEY`,
   **When** the operator changes `provider` and `api_base` to point at
   an OpenAI-compatible endpoint and restarts Serbero,
   **Then** new mediation sessions call the new endpoint and succeed
   without any code rebuild.

2. **Given** the configured provider returns an error or a timeout,
   **When** Serbero's reasoning call fails,
   **Then** Serbero MUST record the failure, MUST NOT fabricate a
   classification or summary, and MUST either retry (bounded by
   `followup_retry_count` and overall mediation timeout) or transition
   the session to `escalation_recommended` with trigger
   `reasoning_unavailable`.

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

- **FR-103**: The reasoning provider configuration MUST be
  vendor-neutral in the sense that switching between OpenAI,
  Anthropic, PPQ.ai, or an OpenAI-compatible endpoint requires only
  configuration and credential changes, not code changes.

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

- **FR-118**: Mediation MUST run only on disputes that Phase 2 has
  transitioned to a state compatible with guided mediation (e.g.
  `notified` or a Phase 3-specific state introduced by this spec).
  Mediation MUST NOT preempt Phase 2 assignment detection: if a human
  solver takes the dispute via Mostro, mediation MUST either defer to
  the solver or close as `superseded_by_human`, per the mediation
  style policy.

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

The Mostro chat transport is defined by Mostro's own protocol and its
solver-take flow. It is NOT a generic ECDH shortcut between Serbero's
identity and a party's pubkey. Implementers MUST follow the
protocol-defined flow and the Mostrix reference implementation; any
apparent simplification that skips the take-dispute handshake is a
protocol violation and MUST be rejected in review.

- **Shared-key material is obtained through the Mostro-defined
  solver-take flow**, not invented by Serbero. When Serbero, acting
  with its registered solver identity, takes a dispute through
  Mostro, the protocol establishes the shared-key context required to
  address party chat messages for that dispute. The concrete flow —
  including which key material is exchanged, how it is tied to the
  dispute and the counterparties, and how both sides reconstruct a
  chat-addressing key — is the one implemented in Mostrix's
  `src/util/order_utils/execute_take_dispute.rs` and documented in
  `https://mostro.network/protocol/chat.html`. Serbero MUST follow
  that flow; it MUST NOT derive a chat-addressing key from a
  standalone ECDH between its own secret key and a party pubkey as a
  generic shortcut.
- **Outbound messages are addressed to the shared pubkey produced by
  the protocol**, not to a party's own pubkey. Serbero reconstructs
  the protocol-provided shared-key material as Nostr `Keys` (matching
  Mostrix's `chat_utils.rs`) and uses that keypair for addressing and
  signing the inner event, per protocol/chat.html.
- **Outbound mediation content MUST be wrapped as a NIP-44-encrypted
  inner event (`kind 1`) embedded in a NIP-59 gift-wrap
  (`kind 1059`)** with a `p` tag pointing at the shared pubkey, as
  modeled in Mostrix's `src/util/chat_utils.rs`.
- **On the inbound path**, Serbero MUST: fetch gift-wraps addressed
  to the shared pubkey, decrypt with the shared-key material
  established by the take flow, parse and verify the inner event, and
  treat the inner event's content and `created_at` as authoritative
  mediation-session facts.
- The outer gift-wrap's timestamp MUST NOT be used as a session-fact
  timestamp. All mediation ordering, round counting, and timeout
  evaluation MUST be driven by inner-event timestamps.
- Direct DMs to a party's primary pubkey MUST NOT be used for
  mediation content. Solver-facing DMs (Phase 1/2 notifications) are
  unrelated and continue to use the Phase 1/2 notifier unchanged.
- If Serbero has not completed the Mostro-defined solver-take flow
  for a dispute (e.g. another solver took it first, or the flow
  failed), Serbero MUST NOT address mediation messages for that
  dispute. In that case the dispute is handled by the human solver
  via Mostro; Phase 3 transitions the session to `superseded_by_human`
  or closes it without sending party messages.

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
followup_retry_count = 1

[reasoning]
enabled = true
provider = "openai"
model = "gpt-5"
api_base = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
request_timeout_seconds = 30

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
- **SC-104**: Swapping the reasoning provider from one vendor to
  another (e.g. OpenAI → OpenAI-compatible endpoint, or → Anthropic)
  requires only `config.toml` and environment-variable changes. No
  code change is required to complete the swap.
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
- Multi-instance Serbero coordination.
- Replacing the Phase 1/2 notifier. Solver-facing DMs continue to use
  the existing notifier unchanged.
