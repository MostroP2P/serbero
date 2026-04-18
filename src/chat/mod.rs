//! Mostro dispute chat transport.
//!
//! **Phase 3 status (US1+): not yet implemented.**
//!
//! The modules in this tree are intentional skeletons. The dispute-chat
//! interaction flow used by current Mostro clients — and the exact
//! mechanism by which each party's chat-addressing key is obtained or
//! reconstructed — has NOT been verified yet. See
//! `specs/003-guided-mediation/research.md` §R-101 (Verification
//! points) and `contracts/mostro-chat.md` §Dispute Chat Key
//! Reconstruction.
//!
//! Do not fill in these files by transcribing `protocol/chat.html`
//! alone, and do not invent a generic ECDH shortcut between Serbero's
//! long-term secret and a party's primary pubkey. Landing US1 requires
//! verifying the real mechanism against current Mostro clients and the
//! Mostrix reference implementation
//! (`src/util/order_utils/execute_take_dispute.rs`,
//! `src/util/chat_utils.rs`).

pub mod dispute_chat_flow;
pub mod inbound;
pub mod outbound;
pub mod shared_key;
