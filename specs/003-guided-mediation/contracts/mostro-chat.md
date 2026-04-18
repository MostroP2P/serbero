# Contract: Mostro Chat Transport

**Phase**: 3 (Guided Mediation)
**Status**: Implementation contract. Reference material: Mostro chat protocol at <https://mostro.network/protocol/chat.html> and Mostrix implementation at <https://github.com/MostroP2P/mostrix>.

## Purpose

Defines how Serbero participates in Mostro's chat protocol for party
mediation, without inventing a parallel transport. This contract
expands the `Mediation Transport Requirements` section of the spec.

## Dispute Chat Key Reconstruction

Serbero MUST use the same dispute-chat key reconstruction and
message addressing mechanism that is actually used by Mostro clients
for solver-visible dispute chat.

This contract does **not** assume, as a protocol guarantee, that
Mostro's public chat specification alone fully defines how a solver
obtains all chat-addressing key material. The public protocol
documentation describes the chat model at a level that is consistent
with multiple implementation shapes; the specific mechanism Serbero
must follow is the one actually exercised by current Mostro clients
and reflected in the Mostrix reference implementation. Phase 3
implementation MUST therefore be verified against the real
dispute-chat flow used by current Mostro clients and Mostrix, not
reduced to whatever the public specification alone might permit.

Serbero MUST NOT invent a generic shortcut such as deriving a chat
key directly from its own long-term secret key and a party's primary
pubkey unless that exact mechanism is verified in the dispute-chat
flow used by Mostro clients.

- **Entry conditions** (ALL of):
  - `[mediation].enabled = true`.
  - Configured reasoning provider is healthy
    (`ReasoningProvider::health_check` Ok).
  - Serbero's registered solver pubkey is authorized in the target
    Mostro instance at `read` permission or higher.
  - The target dispute is in a mediation-eligible Phase 2 state.
- **Implementation flow** (to be verified in-tree against current
  Mostro clients and the Mostrix reference — not a verbatim protocol
  quote):
  1. Serbero observes a Phase-2-eligible dispute and decides to
     mediate (mediation engine gate).
  2. Serbero follows the dispute-chat interaction flow used by
     current Mostro clients and validated against Mostrix reference
     behavior. Relevant implementation references:
     `src/util/order_utils/execute_take_dispute.rs` and
     `src/util/chat_utils.rs`. These are implementation references,
     not protocol-level definitions.
  3. That implementation flow yields, or allows reconstruction of,
     the per-party chat-addressing key material Serbero must use.
     The exact mechanism by which each party's chat key is obtained
     MUST be verified against current Mostro/Mostrix behavior during
     implementation and re-verified whenever Mostro clients change
     that mechanism.
  4. Serbero reconstructs the per-party chat keypair as
     `nostr_sdk::Keys` and stores only the derived
     `*_shared_pubkey` fields on `mediation_sessions` (see
     data-model.md). The raw chat-key secret is held in process
     memory for the session's lifetime; it is not persisted.
- **Failure modes**:
  - The dispute-chat interaction cannot be completed (another
    solver already took the dispute, a Mostro-side refusal, or an
    unexpected response): mediation does not open a session; Phase
    1/2 continues unaffected.
  - Mostro returns material that does not match the verified
    reconstruction mechanism: log ERROR, refuse to mediate this
    dispute, and do NOT invent a fallback transport or a
    secret-plus-primary-pubkey ECDH shortcut.

## Outbound Message Construction

- Inner event: `Kind::Custom(1)` (`kind 1`), signed by the
  per-party shared keypair.
- Inner event content: the mediation message text (drafted by the
  reasoning provider through a template from the prompt bundle and
  validated by the policy layer).
- Inner event tags: minimal by default (no identity leaks beyond
  what Mostro's protocol already expects).
- Wrap: NIP-59 gift-wrap (`kind 1059`) with `p` tag set to the
  **shared public key** (NOT the party's primary pubkey).
- Encryption: NIP-44 over the reconstructed shared key.
- Recipient: the relay set configured for the daemon; Mostro's chat
  model expects gift-wraps addressed to the shared pubkey on the
  same Nostr relays.

This matches Mostrix's `chat_utils.rs` outbound path.

## Inbound Message Ingestion

- Subscription: a filter on `kind 1059` gift-wraps with `p` equal to
  each active session's `*_shared_pubkey` for buyer and seller.
  Scheduled fetch interval: `[chat].inbound_fetch_interval_seconds`.
- Pipeline:
  1. Unwrap gift-wrap with the shared-key keypair.
  2. Verify inner event signature.
  3. Extract `content` and `created_at` from the **inner** event.
  4. Dedup by inner event id against the unique index on
     `mediation_messages(session_id, inner_event_id)`.
  5. On first ingest, advance the per-party
     `*_last_seen_inner_ts` and increment `round_count` on completed
     round boundaries.
- Stale handling: inner `created_at` predating the per-party
  last-seen marker results in a `mediation_messages` row with
  `stale = 1`; it is persisted for audit but does NOT advance state.
- Timestamp discipline: session ordering, round counting, and
  timeout evaluation are ALL driven by inner-event `created_at`.
  Outer gift-wrap timestamps are ignored for those purposes.

## Authority and Boundary Invariants

- Serbero MUST NOT send any inner event whose content encodes a
  fund-moving or dispute-closing instruction. The policy layer
  enforces this before the wrap step.
- Serbero MUST NOT wrap direct `kind 4` / NIP-17 / `kind 1059` DMs
  addressed to a party's **primary pubkey** as mediation traffic.
  That path is not a legal chat transport and is rejected.
- If Serbero has not completed the take flow for a dispute, it MUST
  NOT emit any mediation event for that dispute.
- On `authorization_lost` (Mostro rejects a message, or revokes
  solver permission mid-session), the session escalates and the
  auth-retry loop is re-entered (see the spec's Solver Identity and
  Authorization section and plan Module Architecture).

## Observability

- Every outbound inner event and every ingested inbound inner event
  is persisted as a `mediation_messages` row (FR-119).
- `outbound_sent` and `inbound_ingested` kinds are written to
  `mediation_events`.
- Tracing spans: one per outbound send (`dispute_id`,
  `session_id`, `party`, `shared_pubkey`, `inner_event_id`); one per
  inbound ingest with the same fields.

## Implementation references

These are **implementation references** that the Phase 3 code must
be verified against, not protocol-level definitions. Mostro's public
specification is the authoritative protocol source; the files below
describe how current Mostro clients (via Mostrix) actually exercise
dispute chat today, which is what Serbero must interoperate with.

| Behavior                                | Mostrix implementation reference                        |
|-----------------------------------------|---------------------------------------------------------|
| Dispute-chat interaction flow           | `src/util/order_utils/execute_take_dispute.rs`          |
| Chat-addressing key reconstruction      | `src/util/chat_utils.rs`                                |
| Gift-wrap outbound construction         | `src/util/chat_utils.rs`                                |
| Inbound unwrap / verify pipeline        | `src/util/chat_utils.rs`                                |
| Session-state modeling                  | `src/models.rs`                                         |
| Input construction patterns             | `src/ui/key_handler/input_helpers.rs`                   |

If Mostro clients later change any of these behaviors, Serbero's
in-tree implementation under `src/chat/` (built on `nostr-sdk 0.44.1`)
MUST be updated to track that change. This contract does not freeze
the details that are left to the implementation reference.
