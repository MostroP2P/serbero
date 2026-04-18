pub mod config;
pub mod daemon;
pub mod db;
pub mod dispatcher;
pub mod error;
pub mod handlers;
pub mod models;
pub mod nostr;

// --- Phase 3 skeletons. `chat` and `mediation` are intentional stubs
// --- pending US1+; see their module headers for open verification
// --- points. `prompts` and `reasoning` are fully implemented as the
// --- foundational boundary.
pub mod chat;
pub mod mediation;
pub mod prompts;
pub mod reasoning;
