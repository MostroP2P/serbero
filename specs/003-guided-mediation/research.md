# Research: Guided Mediation (Phase 3)

**Date**: 2026-04-17
**Spec**: [spec.md](spec.md)
**Plan**: [plan.md](plan.md)

## R-101: Mostro chat transport — dispute-chat interaction flow

**Decision**: Phase 3 uses the same dispute-chat interaction flow and
chat-addressing key reconstruction that current Mostro clients use,
as reflected in the Mostrix reference implementation. The exact
chat-addressing key reconstruction used for solver-visible dispute
chat must be validated against current Mostro / Mostrix behavior; it
MUST NOT be reduced to a generic ECDH shortcut between Serbero's
long-term secret and a party's primary pubkey. Outbound messages are
addressed to the per-party chat pubkey produced by that reconstruction
via NIP-59 gift-wrapped `kind 1` inner events with a `p` tag pointing
at that pubkey.

**Rationale**:

- Mostro's public chat documentation at
  <https://mostro.network/protocol/chat.html> describes the chat
  model, but it is consistent with multiple possible implementation
  shapes. What Serbero must interoperate with is the specific
  mechanism that current Mostro clients actually use — which is what
  Mostrix demonstrates.
- Mostrix 0.x (<https://github.com/MostroP2P/mostrix>) is the
  **implementation reference** that shows how the dispute-chat key
  material is actually reconstructed today. It is NOT a freestanding
  protocol spec, and this research entry does not treat it as one.
- A simpler "ECDH(Serbero.sk, party.pk)" shortcut would produce keys
  that do not match the ones Mostro clients address, so the earlier
  spec wording that tolerated that framing has been rejected and the
  current spec forbids it (see `Mediation Transport Requirements`).

**Implementation references** (not protocol-level definitions):

- `src/util/chat_utils.rs` — chat-addressing key reconstruction,
  gift-wrap construction (NIP-44 inner event + NIP-59 wrap + `p`
  tag), and the unwrap / verify pipeline for inbound fetches.
- `src/util/order_utils/execute_take_dispute.rs` — the dispute-chat
  interaction flow that precedes mediation and lets the solver obtain
  (or reconstruct) the per-party chat-addressing key material tied
  to the dispute.
- `src/models.rs` — chat role and session-state shapes.
- `src/ui/key_handler/input_helpers.rs` — input-to-event construction
  patterns mirrored by Serbero's outbound path.

These files guide the in-tree Rust implementation under `src/chat/`.
If Mostro clients change any of these behaviors, Serbero's code MUST
be updated to track the change; this research entry does not freeze
mechanisms that are ultimately determined by client behavior.

**Alternatives considered**:

- Direct NIP-17 / NIP-59 DMs addressed to a party's primary pubkey
  (convenient but fundamentally the wrong transport — rejected by
  TC-101 / FR-101).
- Building a parallel Serbero-specific chat protocol (rejected by
  the Mostro Compatibility principle).
- Treating the public `protocol/chat.html` specification alone as
  sufficient to derive the key-reconstruction mechanism: rejected.
  The specification is the protocol contract; the exact mechanism
  must be cross-checked against the implementation actually deployed
  by Mostro clients.

**Verification points** (re-confirm during implementation and whenever
Mostro clients change behavior):

- Exact mechanism by which each party's chat-addressing key is
  obtained or reconstructed in current Mostro clients. This is the
  load-bearing verification point; do not assume it from the public
  spec alone.
- Exact NIP-44 encryption context expected by parties (inner key,
  any context / associated-data strings used by Mostro clients).
- Exact gift-wrap extra-tag expectations beyond the `p` tag (none
  expected at the time of writing, but worth re-checking against
  Mostrix at implementation time).

## R-102: Serbero as a Mostro solver identity

**Decision**: Phase 3 uses the same Nostr keypair that Phases 1/2
already load from `config.toml` as the operational solver identity.
Registration of the corresponding pubkey as a Mostro solver (at least
`read` permission) is an **operator precondition**, not something
Serbero does for itself. Startup runs a verification and, on failure,
enters the bounded revalidation loop defined in the spec.

**Rationale**:

- Reuses the existing config surface; no new key management.
- Keeps Phase 3 aligned with the constitutional separation of
  concerns: Mostro owns permissions, Serbero operates under them.
- The revalidation loop handles the common case of "operator
  registers the key after Serbero was deployed" without requiring a
  restart.

**Verification points**:

- Exact mechanism for verifying Serbero's solver registration. Two
  candidates:
  1. A Mostro-provided Nostr event announcement for registered
     solvers that Serbero can subscribe to.
  2. An implicit verification via a successful no-op interaction
     (e.g. successfully completing the dispute-chat interaction flow
     for the first eligible dispute).
  We default to (1) if available; otherwise we verify implicitly at
  the first mediation attempt and treat the auth-retry loop as a
  time-bound wait on that attempt.
- Whether a separate "write" permission layer exists today in Mostro
  or whether Phase 4 will introduce it. Phase 3 only requires
  `read`.

**Alternatives considered**:

- Serbero auto-registering its own pubkey as a solver: rejected by
  the constitution (Protocol-Enforced Security Boundaries).
- A second identity separate from the daemon key: rejected — adds
  key-material surface area without operational benefit in Phase 3.

## R-103: Reasoning provider adapter boundary

**Decision**: Introduce a small `ReasoningProvider` trait with a
single request/response shape. Ship exactly one adapter
(`openai`) in Phase 3; the adapter also covers any "openai-compatible"
endpoint via config alone (different `api_base`). Anthropic, PPQ.ai,
and OpenClaw enter as `NotYetImplemented` variants so the mediation
call sites stay provider-agnostic but Phase 3 is not blocked on
shipping four adapters.

**Rationale**:

- Matches constitutional principle X (Portable Reasoning Backends)
  while keeping Phase 3 implementation scope small.
- Vendor switching between OpenAI and OpenAI-compatible endpoints
  (a common real-world case: self-hosted, router proxies, some
  providers including PPQ.ai) requires only `api_base` + env var
  changes — no code.
- A small adapter trait is cheaper than a full provider plugin
  framework and can grow naturally in Phase 5 without rewrites.

**Alternatives considered**:

- Hardcoding OpenAI in `mediation/` call sites: rejected, violates
  principle X and locks in a vendor.
- A trait crate separate from `serbero`: rejected for Phase 3, too
  much overhead for one adapter. Revisit in Phase 5 if multiple
  adapters actually ship.
- JSON-shaped config for arbitrary HTTP endpoints: rejected — not
  every provider tolerates the same request shape, and Phase 3 needs
  at least one concrete adapter to actually execute.

**Verification points**:

- Request shape we standardize on. Default: `classify(system_prompt,
  classification_policy, transcript) -> {classification, confidence,
  rationale}` and `summarize(...) -> {summary_text, suggested_next_
  step}`. Exact field set lives in `contracts/reasoning-provider.md`.
- Reasoning output schema we enforce on the adapter side; JSON
  mode / function-calling vs. plain text with a parser. JSON mode
  preferred for classification; plain text acceptable for summaries.

## R-104: HTTP client for the reasoning adapter

**Decision**: Use `reqwest` (async, tokio-native) as the HTTP client
for the single Phase 3 adapter. No separate retry crate is added in
Phase 3; `request_timeout_seconds` + `followup_retry_count` are
handled in `reasoning/mod.rs` around the single call site.

**Rationale**:

- Tokio-native and interoperates cleanly with the existing
  async runtime.
- Familiar in the Rust ecosystem; avoids pulling in hyper directly.
- Avoids introducing a second HTTP / retry framework for what is a
  handful of call sites.

**Alternatives considered**:

- `ureq` (blocking): rejected — would require thread-pool offload to
  interoperate with tokio.
- `hyper` directly: rejected for the amount of boilerplate, not
  worth it for the call-site count in Phase 3.
- A retry crate like `tokio-retry`: rejected for Phase 3 to keep the
  dependency surface minimal. A plain `for _ in 0..retries` loop is
  sufficient for the bounded retry policy the spec requires.

## R-105: Prompt bundle hashing

**Decision**: Compute `policy_hash` as the SHA-256 over a
deterministic concatenation of the loaded prompt-file bytes, in a
fixed order by logical name (`system`, `classification`, `escalation`,
`mediation_style`, `message_templates`), separated by a stable
null-byte delimiter so adjacent file contents cannot collide. Add
`sha2` as the hashing crate.

**Rationale**:

- Deterministic + collision-resistant enough for an audit / pinning
  use case.
- File-order independence at the concat layer avoids accidental hash
  changes if the configured paths are listed in a different order.
- Null-byte delimiter is a well-known trick for unambiguous
  concatenation hashing.

**Alternatives considered**:

- BLAKE3 (faster, but overkill and adds a dependency).
- Hash each file separately and store a list: possible but makes the
  per-session "pin" surface more verbose; a single hash is simpler.
- Store the prompt bytes themselves in SQLite: rejected by TC-103
  (versioned files MUST be the source of truth).

## R-106: Mediation session persistence and restart resume

**Decision**: Persist every session-state transition and every
inbound/outbound message synchronously in SQLite before the next
Nostr-chat or reasoning-provider step. On restart, Serbero loads all
open sessions (`state NOT IN ('summary_delivered','closed','superseded_by_human')`),
re-binds them to the pinned `policy_hash`, and resumes the loop.
`last_seen_inner_event_ts` markers per party are used to filter the
inbound fetch so already-ingested events are not double-counted.

**Rationale**:

- Matches the Phases 1/2 discipline: persistence-first, no
  in-memory queue.
- Restart resume is straightforward because we only need (a) the
  pinned bundle hash to reload the right prompts and (b) the
  per-party last-seen marker to filter inbound.

**Verification points**:

- Whether the inbound fetch uses relay `since=` filters or client-
  side filtering. Client-side dedup by inner-event id is
  authoritative regardless; `since=` is an optimization.

## R-107: Rationale audit store (FR-120)

**Decision**: Create a dedicated `reasoning_rationales` SQLite table
that holds the full rationale text, keyed by a `rationale_id` that is
the SHA-256 of the rationale text (content-addressed). General logs
carry only `classification`, `confidence`, and `rationale_id`. Access
to the audit store is an operator concern — filesystem permissions on
the SQLite file, just like for the rest of Serbero state.

**Rationale**:

- Content-addressed ids give audit integrity for free (tampering
  with the rationale changes the id) and deduplicate identical
  rationales.
- No separate audit service needed — reuses the existing persistence
  layer.
- Keeps party statements and dispute details out of aggregate log
  streams (e.g. journald → centralized log aggregator) where PII
  leakage is easiest.

**Alternatives considered**:

- Writing rationales to a separate file per session: rejected, adds
  filesystem layout and rotation concerns.
- Sealed-box encrypting rationales at rest: out of scope for Phase 3;
  filesystem permissions on the SQLite file are the Phase 3 control.
  Revisit in Phase 4 or later if an operator requires it.

## R-108: Solver-Facing Routing (spec anchor)

**Decision**: The routing rule is now documented in the spec's
`Solver-Facing Routing` section and implemented in
`src/mediation/router.rs`. Targeted when `disputes.assigned_solver`
is set; broadcast via the Phase 1/2 notifier otherwise. Re-routed at
the next notification when assignment state changes.

**Rationale**: See the spec section; this entry exists so the plan's
research index links back to it.

## R-109: Auth revalidation loop (scope-controlled)

**Decision**: Implement as a single `tokio::task` with:

- Immediate startup verification.
- Truncated exponential backoff between
  `solver_auth_retry_initial_seconds` (default 60) and
  `solver_auth_retry_max_interval_seconds` (default 3600).
- Re-verification hook on operator-triggered config reload.
- Terminal behavior at
  `min(solver_auth_retry_max_total_seconds=86400, solver_auth_retry_
  max_attempts=24)` — one WARN-or-higher alert, then stop retrying.
- Phase 1/2 runs independently of this task at all times.

**Rationale**: Honors the scope-control note in the plan. This is the
minimal surface the spec requires. No general-purpose retry framework,
no state machine beyond `Authorized` / `Unauthorized` / `Terminated`,
no plugin points.

**Alternatives considered**:

- A shared retry / circuit-breaker framework: rejected for Phase 3
  scope discipline.
- Per-operation retry inside each mediation call site: rejected —
  duplicates logic across multiple modules; better to centralize.

## R-110: Test harness for Mostro chat

**Decision**: Extend the existing `tests/common/mod.rs` harness (from
Phases 1/2) with a `MostroChatSim` helper that: replays the subset
of the dispute-chat interaction flow Phase 3 exercises (modeled on
current Mostro / Mostrix behavior and verified against it at test-
fixture time), exposes the resulting per-party chat-key material to
the test, and publishes inbound chat events on the
`nostr-relay-builder::MockRelay` that Phases 1/2 already use. A
second helper `MockReasoningProvider` stubs the HTTP surface of the
reasoning adapter with deterministic responses.

**Rationale**:

- Reuses the existing MockRelay harness, no new relay server.
- Simulator is intentionally small: only the slice of Mostro the
  Phase 3 tests actually exercise (dispute-chat interaction flow +
  chat relay of inner events on the per-party chat pubkey).
- `MockReasoningProvider` keeps integration tests hermetic (no
  outbound HTTP to a real LLM vendor).

**Alternatives considered**:

- Running an actual Mostro instance in-process for integration
  tests: rejected — heavy, flaky, and out of scope.
- Mocking only the Nostr layer without modeling the take flow:
  rejected — we would be testing the wrong thing. The take flow is
  what produces the shared-key material.
