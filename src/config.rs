use std::path::Path;

use crate::error::{Error, Result};
use crate::models::Config;

pub fn load_config(path: &Path) -> Result<Config> {
    let contents = std::fs::read_to_string(path).map_err(|e| {
        Error::Config(format!(
            "failed to read config file at {}: {e}",
            path.display()
        ))
    })?;
    let mut config: Config = toml::from_str(&contents)?;
    apply_env_overrides(&mut config);
    resolve_reasoning_api_key(&mut config)?;
    Ok(config)
}

/// Populate `config.reasoning.api_key` from the environment variable
/// named by `config.reasoning.api_key_env`. This is the single place
/// secrets enter the `Config` struct — they never come from TOML
/// directly (spec FR-104 / plan Configuration Surface).
///
/// - If `reasoning.enabled == false`, absence is fine.
/// - If `reasoning.enabled == true`, the named env var MUST be set
///   and non-empty; otherwise we return a loud `Error::Config`.
fn resolve_reasoning_api_key(config: &mut Config) -> Result<()> {
    let var = config.reasoning.api_key_env.trim().to_string();
    if var.is_empty() {
        if config.reasoning.enabled {
            return Err(Error::Config(
                "[reasoning].api_key_env is empty but [reasoning].enabled = true".into(),
            ));
        }
        return Ok(());
    }
    match std::env::var(&var) {
        Ok(v) if !v.trim().is_empty() => {
            config.reasoning.api_key = v;
            Ok(())
        }
        _ if config.reasoning.enabled => Err(Error::Config(format!(
            "reasoning provider enabled but credential env var `{var}` is unset or empty"
        ))),
        _ => Ok(()),
    }
}

fn apply_env_overrides(config: &mut Config) {
    // Only non-empty env values override file values — an empty env var
    // is treated as "not set" rather than as a blank overwrite, so an
    // accidentally-unset shell variable does not wipe a valid config
    // entry.
    if let Some(key) = non_empty_env("SERBERO_PRIVATE_KEY") {
        config.serbero.private_key = key;
    }
    if let Some(path) = non_empty_env("SERBERO_DB_PATH") {
        config.serbero.db_path = path;
    }
    if let Some(level) = non_empty_env("SERBERO_LOG") {
        config.serbero.log_level = level;
    }
}

fn non_empty_env(var: &str) -> Option<String> {
    // Return the trimmed value so callers like EnvFilter / hex parsers
    // don't have to defend against accidental leading/trailing whitespace
    // in shell exports.
    std::env::var(var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SolverPermission;
    use std::io::Write;
    use std::sync::Mutex;

    static ENV_GUARD: Mutex<()> = Mutex::new(());

    fn clear_env() {
        std::env::remove_var("SERBERO_PRIVATE_KEY");
        std::env::remove_var("SERBERO_DB_PATH");
        std::env::remove_var("SERBERO_LOG");
    }

    /// RAII guard that holds the global env mutex and restores the env
    /// on drop — including on test panic — so one flaky test cannot
    /// poison the ENV_GUARD or leak vars into neighbouring tests.
    struct EnvLock<'a> {
        _guard: std::sync::MutexGuard<'a, ()>,
    }

    impl<'a> EnvLock<'a> {
        fn new() -> Self {
            let guard = match ENV_GUARD.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            clear_env();
            Self { _guard: guard }
        }
    }

    impl Drop for EnvLock<'_> {
        fn drop(&mut self) {
            clear_env();
        }
    }

    fn write_tmp(contents: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    const VALID_CONFIG: &str = r#"
[serbero]
private_key = "aa11"
db_path = "serbero.db"
log_level = "info"

[mostro]
pubkey = "bb22"

[[relays]]
url = "wss://relay.example.com"

[[solvers]]
pubkey = "cc33"
permission = "read"

[[solvers]]
pubkey = "dd44"
permission = "write"

[timeouts]
renotification_seconds = 120
renotification_check_interval_seconds = 30
"#;

    #[test]
    fn parses_full_valid_config() {
        let _lock = EnvLock::new();
        let f = write_tmp(VALID_CONFIG);
        let cfg = load_config(f.path()).expect("parse");
        assert_eq!(cfg.serbero.private_key, "aa11");
        assert_eq!(cfg.serbero.db_path, "serbero.db");
        assert_eq!(cfg.mostro.pubkey, "bb22");
        assert_eq!(cfg.relays.len(), 1);
        assert_eq!(cfg.solvers.len(), 2);
        assert_eq!(cfg.solvers[0].permission, SolverPermission::Read);
        assert_eq!(cfg.solvers[1].permission, SolverPermission::Write);
        assert_eq!(cfg.timeouts.renotification_seconds, 120);
        assert_eq!(cfg.timeouts.renotification_check_interval_seconds, 30);
    }

    #[test]
    fn env_overrides_apply() {
        let _lock = EnvLock::new();
        let f = write_tmp(VALID_CONFIG);
        std::env::set_var("SERBERO_PRIVATE_KEY", "env_override_key");
        std::env::set_var("SERBERO_DB_PATH", "/tmp/env.db");
        std::env::set_var("SERBERO_LOG", "debug");
        let cfg = load_config(f.path()).expect("parse");
        assert_eq!(cfg.serbero.private_key, "env_override_key");
        assert_eq!(cfg.serbero.db_path, "/tmp/env.db");
        assert_eq!(cfg.serbero.log_level, "debug");
    }

    #[test]
    fn env_overrides_are_trimmed() {
        let _lock = EnvLock::new();
        let f = write_tmp(VALID_CONFIG);
        std::env::set_var("SERBERO_PRIVATE_KEY", "  abcd1234  ");
        std::env::set_var("SERBERO_LOG", "  debug  ");
        let cfg = load_config(f.path()).expect("parse");
        assert_eq!(cfg.serbero.private_key, "abcd1234");
        assert_eq!(cfg.serbero.log_level, "debug");
    }

    #[test]
    fn malformed_toml_yields_config_error() {
        let f = write_tmp("not = valid\n[unclosed");
        let err = load_config(f.path()).unwrap_err();
        assert!(matches!(err, Error::TomlParse(_)));
    }

    #[test]
    fn missing_file_yields_config_error() {
        let err = load_config(Path::new("/no/such/path/config.toml")).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn empty_env_vars_are_ignored() {
        let _lock = EnvLock::new();
        let f = write_tmp(VALID_CONFIG);
        std::env::set_var("SERBERO_PRIVATE_KEY", "");
        std::env::set_var("SERBERO_DB_PATH", "   ");
        std::env::set_var("SERBERO_LOG", "");
        let cfg = load_config(f.path()).expect("parse");
        // File values survive when env vars are empty/whitespace.
        assert_eq!(cfg.serbero.private_key, "aa11");
        assert_eq!(cfg.serbero.db_path, "serbero.db");
        assert_eq!(cfg.serbero.log_level, "info");
    }
}
