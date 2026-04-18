//! Per-party chat-addressing key reconstruction (US1 — not yet
//! implemented).
//!
//! Must mirror the reconstruction used by current Mostro clients as
//! modelled in Mostrix `src/util/chat_utils.rs`. The exact key
//! material source is one of the R-101 verification points — do not
//! invent it here.
//!
//! When this module is filled in:
//! - Return `nostr_sdk::Keys` for the per-party chat-addressing key.
//! - Document the Mostrix tag version this code was verified against.
//! - Hold the raw secret in process memory for the session's lifetime;
//!   do NOT persist it (only the derived `*_shared_pubkey` goes into
//!   `mediation_sessions`, per `data-model.md`).

// TODO(US1): implement reconstruct_party_keys(...).
