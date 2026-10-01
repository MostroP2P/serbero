//! Tracing subscriber setup.

use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

use crate::error::{Error, Result};

/// Environment variable that overrides the configured log level.
pub const LOG_ENV: &str = "SERBERO_LOG";

/// Disables colors when set to a non-empty value (https://no-color.org).
const NO_COLOR_ENV: &str = "NO_COLOR";

/// Installs the global tracing subscriber.
///
/// `SERBERO_LOG` wins over `fallback` so operators can raise verbosity
/// without editing the config file. Both accept `EnvFilter` directives, such
/// as `info` or `serbero=debug,nostr_sdk=warn`.
pub fn init(fallback: &str) -> Result<()> {
    let directive = directive(std::env::var(LOG_ENV).ok(), fallback);
    let filter = EnvFilter::try_new(&directive)
        .map_err(|e| Error::Logging(format!("invalid log filter {directive:?}: {e}")))?;
    let ansi = use_ansi(
        std::io::stdout().is_terminal(),
        std::env::var(NO_COLOR_ENV).ok(),
    );
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(ansi)
        .try_init()
        .map_err(|e| Error::Logging(e.to_string()))
}

fn directive(env_value: Option<String>, fallback: &str) -> String {
    env_value
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

/// Colors only on a terminal: under Docker or systemd, escape codes would end
/// up as noise in `docker logs` and the journal.
fn use_ansi(stdout_is_terminal: bool, no_color: Option<String>) -> bool {
    stdout_is_terminal && no_color.is_none_or(|v| v.is_empty())
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
    fn colors_only_on_a_terminal() {
        assert!(use_ansi(true, None));
        assert!(!use_ansi(false, None));
    }

    #[test]
    fn no_color_disables_colors_on_a_terminal() {
        assert!(!use_ansi(true, Some("1".into())));
        // An empty NO_COLOR is ignored (https://no-color.org).
        assert!(use_ansi(true, Some(String::new())));
    }

    #[test]
    fn blank_env_value_uses_fallback() {
        assert_eq!(directive(Some("  ".into()), "warn"), "warn");
    }
}
