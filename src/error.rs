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

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
