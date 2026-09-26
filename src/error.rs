//! Crate-wide error type.

/// Errors surfaced by Serbero's modules.
#[derive(Debug, thiserror::Error)]
pub enum Error {
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
}
