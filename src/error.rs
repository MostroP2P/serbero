use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("toml parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("nostr error: {0}")]
    Nostr(String),

    #[error("notification error: {0}")]
    Notification(String),

    #[error("invalid key: {0}")]
    InvalidKey(String),

    #[error("invalid event: {0}")]
    InvalidEvent(String),

    #[error("invalid state transition: {from} -> {to}")]
    InvalidStateTransition { from: String, to: String },

    // --- Phase 3 additions ---
    #[error("Phase 3 mediation is disabled by configuration")]
    MediationDisabled,

    #[error("reasoning provider unavailable: {0}")]
    ReasoningUnavailable(String),

    #[error("failed to load Phase 3 prompt bundle: {0}")]
    PromptBundleLoad(String),

    #[error("Serbero's solver pubkey is not registered in the target Mostro instance")]
    AuthNotRegistered,

    #[error("solver-auth revalidation loop reached its terminal cap without success")]
    AuthTerminated,

    #[error("Mostro chat transport error: {0}")]
    ChatTransport(String),

    #[error("reasoning provider '{0}' is declared but not yet implemented in Phase 3")]
    ProviderNotYetImplemented(String),
    // --- end Phase 3 additions ---
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
