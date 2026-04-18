//! Outbound mediation message construction (US1 — not yet
//! implemented).
//!
//! When implemented, MUST produce a NIP-59 gift-wrap (`kind 1059`)
//! whose inner event is a NIP-44-encrypted `kind 1` signed by the
//! reconstructed per-party shared keys. The outer `p` tag points at
//! the per-party chat pubkey (NOT the party's primary pubkey). See
//! `contracts/mostro-chat.md` §Outbound Message Construction.
//!
//! The function signature MUST make it impossible to accidentally
//! address mediation content to a party's primary pubkey.

// TODO(US1): implement send_mediation_message(...).
