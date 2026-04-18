//! Dispute-chat interaction flow (US1 — not yet implemented).
//!
//! This module, when implemented, MUST follow the dispute-chat
//! interaction flow used by current Mostro clients (verified against
//! Mostrix `src/util/order_utils/execute_take_dispute.rs`). It MUST
//! NOT transcribe `protocol/chat.html` alone as authoritative, and
//! it MUST NOT substitute a generic `ECDH(Serbero.sk, party.pk)`
//! shortcut. See `research.md` R-101 and `contracts/mostro-chat.md`.
//!
//! Verification points still open (from R-101):
//! - Exact mechanism by which each party's chat-addressing key is
//!   obtained or reconstructed in current Mostro clients.
//! - Exact NIP-44 encryption context and associated-data strings.
//! - Exact gift-wrap extra-tag expectations beyond the `p` tag.
//!
//! Tasks T033–T036 cover this module. Leaving it unimplemented here
//! keeps Phase 3 scope-honest (Option A in the implementation plan).

// TODO(US1): implement run(dispute_id) following the verified flow.
