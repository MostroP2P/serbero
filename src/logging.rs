//! Tracing subscriber setup.

use tracing_subscriber::EnvFilter;

use crate::error::{Error, Result};

/// Environment variable that overrides the configured log level.
pub const LOG_ENV: &str = "SERBERO_LOG";

/// Installs the global tracing subscriber.
///
/// `SERBERO_LOG` wins over `fallback` so operators can raise verbosity
/// without editing the config file. Both accept `EnvFilter` directives, such
/// as `info` or `serbero=debug,nostr_sdk=warn`.
pub fn init(fallback: &str) -> Result<()> {
    let directive = directive(std::env::var(LOG_ENV).ok(), fallback);
    let filter = EnvFilter::try_new(&directive)
        .map_err(|e| Error::Logging(format!("invalid log filter {directive:?}: {e}")))?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .try_init()
        .map_err(|e| Error::Logging(e.to_string()))
}

fn directive(env_value: Option<String>, fallback: &str) -> String {
    env_value
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_value_overrides_fallback() {
        assert_eq!(directive(Some("debug".into()), "info"), "debug");
    }

    #[test]
    fn missing_env_value_uses_fallback() {
        assert_eq!(directive(None, "info"), "info");
    }

    #[test]
    fn blank_env_value_uses_fallback() {
        assert_eq!(directive(Some("  ".into()), "warn"), "warn");
    }
}
