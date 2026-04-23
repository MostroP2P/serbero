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

// --- Phase 4: escalation execution surface (FR-121..FR-218).
// --- The module tree is scaffolded at Phase 2 of the Phase 4
// --- rollout; the consumer/router/dispatcher/tracker layers are
// --- filled in by T011–T016.
pub mod escalation;
