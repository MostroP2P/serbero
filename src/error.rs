//! Crate-wide error type.

/// Errors surfaced by Serbero's modules.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The configuration file or the secrets it names are invalid.
    #[error("invalid configuration: {0}")]
    Config(String),

    /// A database operation failed.
    #[error("database error: {0}")]
    Store(#[from] rusqlite::Error),

    /// The database schema cannot be brought to the expected version.
    #[error("database schema error: {0}")]
    Schema(String),

    /// A Nostr key, relay, or event operation failed.
    #[error("nostr error: {0}")]
    Nostr(String),

    /// The tracing subscriber could not be installed.
    #[error("logging setup failed: {0}")]
    Logging(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_error_names_the_failing_step() {
        let err = Error::Logging("invalid filter".into());

        assert_eq!(err.to_string(), "logging setup failed: invalid filter");
    }

    #[test]
    fn config_error_is_prefixed() {
        let err = Error::Config("mostro.relays must list at least one relay".into());

        assert_eq!(
            err.to_string(),
            "invalid configuration: mostro.relays must list at least one relay"
        );
    }
}
