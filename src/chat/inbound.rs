//! Inbound mediation message ingestion (US2 — not yet implemented).
//!
//! When implemented, MUST fetch gift-wrap events addressed to each
//! session's per-party chat pubkey, unwrap with the reconstructed
//! shared keys, verify the inner event, and surface
//! `(inner_event_id, inner_created_at, content)` as authoritative.
//! See `contracts/mostro-chat.md` §Inbound Message Ingestion.
//!
//! Outer gift-wrap timestamps MUST NOT be used as session facts.

// TODO(US2): implement fetch_inbound(...) and unwrap_and_verify(...).
