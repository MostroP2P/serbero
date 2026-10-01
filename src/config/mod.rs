//! Configuration: `config.toml` plus secrets from the environment.
//!
//! The file layout is specified in `docs/spec.md` §9. Secrets are never read
//! from the file: the file names the environment variables that hold them.

mod duration;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nostr_sdk::prelude::{PublicKey, SecretKey};
use serde::Deserialize;

use crate::error::{Error, Result};

/// Environment variable naming the config file; defaults to `./config.toml`.
pub const CONFIG_PATH_ENV: &str = "SERBERO_CONFIG";

/// Judge providers Serbero ships adapters for (`docs/spec.md` §5.2).
pub const KNOWN_PROVIDERS: &[&str] = &["typesafe", "recorded"];

/// Validated configuration and the secrets it points to.
#[derive(Debug)]
pub struct Settings {
    pub config: Config,
    pub secrets: Secrets,
}

impl Settings {
    /// Loads the file at `path` and resolves secrets from the process environment.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
        Self::parse(&text, |name| std::env::var(name).ok())
    }

    /// Parses and validates `text`, resolving secrets through `env`.
    pub fn parse(text: &str, env: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let config = Config::parse(text)?;
        let secrets = Secrets::resolve(&config, env)?;
        Ok(Self { config, secrets })
    }

    /// Path from `SERBERO_CONFIG`, or `config.toml` in the working directory.
    pub fn default_path() -> PathBuf {
        std::env::var_os(CONFIG_PATH_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("config.toml"))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub serbero: ServerConfig,
    pub mostro: MostroConfig,
    #[serde(default)]
    pub solvers: Vec<SolverConfig>,
    #[serde(default)]
    pub notify: NotifyConfig,
    #[serde(default)]
    pub mediation: MediationConfig,
    #[serde(default)]
    pub judge: JudgeConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    pub private_key_env: String,
    pub db_path: PathBuf,
    pub log_level: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            private_key_env: "SERBERO_PRIVATE_KEY".into(),
            db_path: PathBuf::from("serbero.db"),
            log_level: "info".into(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MostroConfig {
    pub pubkey: String,
    pub relays: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolverConfig {
    pub pubkey: String,
    pub permission: Permission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    Read,
    Write,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NotifyConfig {
    #[serde(deserialize_with = "duration::deserialize")]
    pub renotify_after: Duration,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            renotify_after: Duration::from_secs(15 * 60),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MediationConfig {
    pub enabled: bool,
    pub default_language: String,
    pub languages: Vec<String>,
    #[serde(deserialize_with = "duration::deserialize")]
    pub quiet_period: Duration,
    #[serde(deserialize_with = "duration::deserialize")]
    pub response_timeout: Duration,
    pub max_rounds: u32,
    pub max_message_chars: usize,
    pub max_messages_per_turn: u32,
    #[serde(deserialize_with = "duration::deserialize")]
    pub self_resolution_timeout: Duration,
}

impl Default for MediationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_language: "en".into(),
            // Every language with a catalog file (`messages/<code>.toml`).
            languages: crate::catalog::embedded_codes()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            quiet_period: Duration::from_secs(20),
            response_timeout: Duration::from_secs(30 * 60),
            max_rounds: 4,
            max_message_chars: 2_000,
            max_messages_per_turn: 10,
            self_resolution_timeout: Duration::from_secs(2 * 3_600),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct JudgeConfig {
    pub provider: String,
    pub model: String,
    pub api_base: String,
    pub api_key_env: String,
    #[serde(deserialize_with = "duration::deserialize")]
    pub timeout: Duration,
    pub max_retries: u32,
    /// Calibrated thresholds keyed by `"<provider>/<model>"`.
    pub thresholds: BTreeMap<String, Thresholds>,
}

impl Default for JudgeConfig {
    fn default() -> Self {
        Self {
            provider: "typesafe".into(),
            model: "jev-1.13.0".into(),
            api_base: "https://api.typesafe.ai".into(),
            api_key_env: "TYPESAFE_API_KEY".into(),
            timeout: Duration::from_secs(10),
            max_retries: 3,
            thresholds: BTreeMap::new(),
        }
    }
}

impl JudgeConfig {
    /// `"<provider>/<model>"`, the key thresholds are stored under.
    pub fn judge_key(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }

    /// Thresholds calibrated for the configured provider and model, if any.
    /// Without them mediation stays disabled (`docs/spec.md` §9).
    pub fn active_thresholds(&self) -> Option<&Thresholds> {
        self.thresholds.get(&self.judge_key())
    }
}

/// Decision thresholds (`docs/judgments.md` §3), and the languages whose
/// golden set passed for this judge (`docs/spec.md` §7.7).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
    pub guide: f64,
    pub fact: f64,
    pub human_request: f64,
    pub fraud: f64,
    pub conflict: f64,
    pub outside_scope: f64,
    /// Self-resolution guidance is offered only when both parties' languages
    /// are listed here. Empty until a calibration (T3.10) fills it.
    #[serde(default)]
    pub validated_languages: Vec<String>,
}

impl Thresholds {
    fn named(&self) -> [(&'static str, f64); 6] {
        [
            ("guide", self.guide),
            ("fact", self.fact),
            ("human_request", self.human_request),
            ("fraud", self.fraud),
            ("conflict", self.conflict),
            ("outside_scope", self.outside_scope),
        ]
    }
}

impl Config {
    /// Parses and validates `text` without resolving secrets, for tools such
    /// as the eval binary that need only part of the configuration.
    pub fn parse(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text).map_err(|e| Error::Config(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        check_env_name("serbero.private_key_env", &self.serbero.private_key_env)?;
        check_env_name("judge.api_key_env", &self.judge.api_key_env)?;
        check_pubkey("mostro.pubkey", &self.mostro.pubkey)?;
        if self.mostro.relays.is_empty() {
            return invalid("mostro.relays must list at least one relay");
        }
        for relay in &self.mostro.relays {
            if !(relay.starts_with("wss://") || relay.starts_with("ws://")) {
                return invalid(format!(
                    "mostro.relays: {relay:?} must start with wss:// or ws://"
                ));
            }
        }
        for (i, solver) in self.solvers.iter().enumerate() {
            check_pubkey(&format!("solvers[{i}].pubkey"), &solver.pubkey)?;
        }
        check_non_zero("notify.renotify_after", self.notify.renotify_after)?;
        self.validate_mediation()?;
        self.validate_judge()
    }

    fn validate_mediation(&self) -> Result<()> {
        let m = &self.mediation;
        if m.languages.is_empty() {
            return invalid("mediation.languages must list at least one language");
        }
        let available = crate::catalog::embedded_codes();
        for (i, lang) in m.languages.iter().enumerate() {
            // A repeated code would become a duplicate `<party>_language`
            // option, which the judge capability check refuses.
            if m.languages[..i].contains(lang) {
                return invalid(format!("mediation.languages: {lang:?} is listed twice"));
            }
            if !available.contains(&lang.as_str()) {
                return invalid(format!(
                    "mediation.languages: {lang:?} has no template catalog; available: {}",
                    available.join(", ")
                ));
            }
        }
        if !m.languages.contains(&m.default_language) {
            return invalid(format!(
                "mediation.default_language {:?} must be one of mediation.languages",
                m.default_language
            ));
        }
        check_non_zero("mediation.quiet_period", m.quiet_period)?;
        check_non_zero("mediation.response_timeout", m.response_timeout)?;
        check_non_zero(
            "mediation.self_resolution_timeout",
            m.self_resolution_timeout,
        )?;
        for (field, value) in [
            ("mediation.max_rounds", m.max_rounds as u64),
            ("mediation.max_message_chars", m.max_message_chars as u64),
            (
                "mediation.max_messages_per_turn",
                m.max_messages_per_turn as u64,
            ),
        ] {
            if value == 0 {
                return invalid(format!("{field} must be greater than 0"));
            }
        }
        Ok(())
    }

    fn validate_judge(&self) -> Result<()> {
        let j = &self.judge;
        if !KNOWN_PROVIDERS.contains(&j.provider.as_str()) {
            return invalid(format!(
                "judge.provider {:?} is unknown; available: {}",
                j.provider,
                KNOWN_PROVIDERS.join(", ")
            ));
        }
        if j.model.trim().is_empty() {
            return invalid("judge.model must not be empty");
        }
        check_non_zero("judge.timeout", j.timeout)?;
        for (key, thresholds) in &j.thresholds {
            for (name, value) in thresholds.named() {
                if !(value > 0.0 && value <= 1.0) {
                    return invalid(format!(
                        "judge.thresholds.\"{key}\".{name} = {value} must be in (0, 1]"
                    ));
                }
            }
            let available = crate::catalog::embedded_codes();
            if let Some(lang) = thresholds
                .validated_languages
                .iter()
                .find(|lang| !available.contains(&lang.as_str()))
            {
                return invalid(format!(
                    "judge.thresholds.\"{key}\".validated_languages: {lang:?} has no template catalog"
                ));
            }
            // `fact` and `guide` pick one option of a choice; above one half,
            // two options of the same question can never both be known.
            for (name, value) in [("guide", thresholds.guide), ("fact", thresholds.fact)] {
                if value <= 0.5 {
                    return invalid(format!(
                        "judge.thresholds.\"{key}\".{name} = {value} must be above 0.5"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Secrets resolved from the environment variables the config names.
pub struct Secrets {
    pub private_key: Secret,
    /// Present when the judge provider needs a key and it is set.
    pub judge_api_key: Option<Secret>,
}

impl Secrets {
    fn resolve(config: &Config, env: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let read = |name: &str| {
            env(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };

        let key_env = &config.serbero.private_key_env;
        let private_key = read(key_env).ok_or_else(|| {
            Error::Config(format!(
                "environment variable {key_env} (serbero.private_key_env) is not set"
            ))
        })?;
        if !is_hex_key(&private_key) || SecretKey::from_hex(&private_key).is_err() {
            return invalid(format!(
                "{key_env} must be a valid 64-character hex secp256k1 private key"
            ));
        }

        let judge_api_key = read(&config.judge.api_key_env).map(Secret);
        let needs_key = config.mediation.enabled && config.judge.provider != "recorded";
        if needs_key && judge_api_key.is_none() {
            return invalid(format!(
                "mediation is enabled but environment variable {} (judge.api_key_env) is not set",
                config.judge.api_key_env
            ));
        }

        Ok(Self {
            private_key: Secret(private_key),
            judge_api_key,
        })
    }
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secrets")
            .field("private_key", &self.private_key)
            .field("judge_api_key", &self.judge_api_key)
            .finish()
    }
}

/// A secret value whose `Debug` output never shows the value.
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

fn is_hex_key(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn check_pubkey(field: &str, value: &str) -> Result<()> {
    if is_hex_key(value) && PublicKey::from_hex(value).and_then(|pk| pk.xonly()).is_ok() {
        Ok(())
    } else {
        invalid(format!(
            "{field} must be a 64-character hex public key, got {value:?}"
        ))
    }
}

fn check_env_name(field: &str, value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && !value.starts_with(|c: char| c.is_ascii_digit())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if valid {
        Ok(())
    } else {
        invalid(format!(
            "{field} must name an environment variable (letters, digits, underscore), got {value:?}"
        ))
    }
}

fn check_non_zero(field: &str, value: Duration) -> Result<()> {
    if value.is_zero() {
        invalid(format!("{field} must be greater than 0"))
    } else {
        Ok(())
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::Config(message.into()))
}

#[cfg(test)]
mod tests;
