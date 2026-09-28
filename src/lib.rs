//! Serbero: helps the parties of a Mostro dispute resolve it themselves, and
//! escalates to a human solver when needed. See `docs/spec.md`.

pub mod catalog;
pub mod chat;
pub mod config;
pub mod daemon;
pub mod error;
pub mod eval;
pub mod judge;
pub mod logging;
pub mod mostro;
pub mod nostr;
pub mod notifier;
pub mod policy;
pub mod signal;
pub mod solver;
pub mod store;
