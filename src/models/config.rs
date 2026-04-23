use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub serbero: SerberoConfig,
    pub mostro: MostroConfig,
    #[serde(default)]
    pub relays: Vec<RelayConfig>,
    #[serde(default)]
    pub solvers: Vec<SolverConfig>,
    #[serde(default)]
    pub timeouts: TimeoutsConfig,

    // --- Phase 3 sections. All defaulted so Phase 1/2-only operators
    // --- can omit them entirely and the daemon runs unchanged.
    #[serde(default)]
    pub mediation: MediationConfig,
    #[serde(default)]
    pub reasoning: ReasoningConfig,
    #[serde(default)]
    pub prompts: PromptsConfig,
    #[serde(default)]
    pub chat: ChatConfig,

    // --- Phase 4 section. Defaulted (enabled = false) so Phase 1/2/3-only
    // --- operators can omit it entirely and the daemon still runs
    // --- without starting the escalation dispatcher.
    #[serde(default)]
    pub escalation: EscalationConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SerberoConfig {
    pub private_key: String,
    #[serde(default = "default_db_path")]
    pub db_path: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_db_path() -> String {
    "serbero.db".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct MostroConfig {
    pub pubkey: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RelayConfig {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SolverConfig {
    pub pubkey: String,
    #[serde(default = "default_permission")]
    pub permission: SolverPermission,
}

fn default_permission() -> SolverPermission {
    SolverPermission::Read
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SolverPermission {
    Read,
    Write,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimeoutsConfig {
    #[serde(default = "default_renotification_seconds")]
    pub renotification_seconds: u64,
    #[serde(default = "default_renotification_check_interval_seconds")]
    pub renotification_check_interval_seconds: u64,
}

impl Default for TimeoutsConfig {
    fn default() -> Self {
        Self {
            renotification_seconds: default_renotification_seconds(),
            renotification_check_interval_seconds: default_renotification_check_interval_seconds(),
        }
    }
}

fn default_renotification_seconds() -> u64 {
    300
}

fn default_renotification_check_interval_seconds() -> u64 {
    60
}

// ---------------------------------------------------------------------------
// Phase 3 config sections
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct MediationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,
    #[serde(default = "default_party_response_timeout_seconds")]
    pub party_response_timeout_seconds: u64,

    // Solver-auth bounded revalidation loop knobs. Defaults match
    // `specs/003-guided-mediation/spec.md`.
    #[serde(default = "default_solver_auth_retry_initial_seconds")]
    pub solver_auth_retry_initial_seconds: u64,
    #[serde(default = "default_solver_auth_retry_max_interval_seconds")]
    pub solver_auth_retry_max_interval_seconds: u64,
    #[serde(default = "default_solver_auth_retry_max_total_seconds")]
    pub solver_auth_retry_max_total_seconds: u64,
    #[serde(default = "default_solver_auth_retry_max_attempts")]
    pub solver_auth_retry_max_attempts: u32,
}

impl Default for MediationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_rounds: default_max_rounds(),
            party_response_timeout_seconds: default_party_response_timeout_seconds(),
            solver_auth_retry_initial_seconds: default_solver_auth_retry_initial_seconds(),
            solver_auth_retry_max_interval_seconds: default_solver_auth_retry_max_interval_seconds(
            ),
            solver_auth_retry_max_total_seconds: default_solver_auth_retry_max_total_seconds(),
            solver_auth_retry_max_attempts: default_solver_auth_retry_max_attempts(),
        }
    }
}

fn default_max_rounds() -> u32 {
    2
}
fn default_party_response_timeout_seconds() -> u64 {
    1800
}
fn default_solver_auth_retry_initial_seconds() -> u64 {
    60
}
fn default_solver_auth_retry_max_interval_seconds() -> u64 {
    3600
}
fn default_solver_auth_retry_max_total_seconds() -> u64 {
    86_400
}
fn default_solver_auth_retry_max_attempts() -> u32 {
    24
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReasoningConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_api_base")]
    pub api_base: String,
    /// Name of the environment variable holding the API credential.
    /// The secret value is resolved at config load time in
    /// `crate::config::load_config` and is NOT stored in TOML.
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
    #[serde(default = "default_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
    /// Bounded retry count for the reasoning adapter's HTTP calls
    /// (FR-104 + plan degraded-mode table). Lives here — alongside
    /// `request_timeout_seconds` — because the adapter is the only
    /// thing that actually performs these retries; the mediation
    /// engine just sees the final `ReasoningError`. Default: 1.
    #[serde(default = "default_followup_retry_count")]
    pub followup_retry_count: u32,
    /// Populated by the loader from the env var named by
    /// `api_key_env`. Skipped during deserialization so TOML cannot
    /// set it directly — secrets enter the `Config` only via the
    /// environment. When `enabled = true` the loader MUST return an
    /// error if the referenced env var is unset or empty.
    #[serde(skip_deserializing)]
    pub api_key: String,
}

impl Default for ReasoningConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_provider(),
            model: default_model(),
            api_base: default_api_base(),
            api_key_env: default_api_key_env(),
            request_timeout_seconds: default_request_timeout_seconds(),
            followup_retry_count: default_followup_retry_count(),
            api_key: String::new(),
        }
    }
}

fn default_followup_retry_count() -> u32 {
    1
}

fn default_provider() -> String {
    "openai".to_string()
}
fn default_model() -> String {
    "gpt-4o-mini".to_string()
}
fn default_api_base() -> String {
    "https://api.openai.com/v1".to_string()
}
fn default_api_key_env() -> String {
    // Vendor-neutral. The shipped adapter is OpenAI-compatible; the
    // same variable name is what operators use to supply credentials
    // to any future adapter as well. Keep this free of vendor names
    // so new operators do not assume Serbero only works against
    // hosted OpenAI.
    "SERBERO_REASONING_API_KEY".to_string()
}
fn default_request_timeout_seconds() -> u64 {
    30
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptsConfig {
    #[serde(default = "default_system_instructions_path")]
    pub system_instructions_path: String,
    #[serde(default = "default_classification_policy_path")]
    pub classification_policy_path: String,
    #[serde(default = "default_escalation_policy_path")]
    pub escalation_policy_path: String,
    #[serde(default = "default_mediation_style_path")]
    pub mediation_style_path: String,
    #[serde(default = "default_message_templates_path")]
    pub message_templates_path: String,
}

impl Default for PromptsConfig {
    fn default() -> Self {
        Self {
            system_instructions_path: default_system_instructions_path(),
            classification_policy_path: default_classification_policy_path(),
            escalation_policy_path: default_escalation_policy_path(),
            mediation_style_path: default_mediation_style_path(),
            message_templates_path: default_message_templates_path(),
        }
    }
}

fn default_system_instructions_path() -> String {
    "./prompts/phase3-system.md".to_string()
}
fn default_classification_policy_path() -> String {
    "./prompts/phase3-classification.md".to_string()
}
fn default_escalation_policy_path() -> String {
    "./prompts/phase3-escalation-policy.md".to_string()
}
fn default_mediation_style_path() -> String {
    "./prompts/phase3-mediation-style.md".to_string()
}
fn default_message_templates_path() -> String {
    "./prompts/phase3-message-templates.md".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatConfig {
    #[serde(default = "default_inbound_fetch_interval_seconds")]
    pub inbound_fetch_interval_seconds: u64,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            inbound_fetch_interval_seconds: default_inbound_fetch_interval_seconds(),
        }
    }
}

fn default_inbound_fetch_interval_seconds() -> u64 {
    10
}

/// Phase 4 (`[escalation]`) configuration.
///
/// All fields are defaulted so `[escalation]` is optional in
/// `config.toml`. When the section is absent, `EscalationConfig::default()`
/// produces the safe defaults: dispatcher disabled,
/// fallback-to-all-solvers off, 30-second cycle. A deployment
/// running Phase 1/2/3 only continues to work unchanged (FR-216 /
/// SC-207).
///
/// Loud-validation discipline: `dispatch_interval_seconds == 0` is
/// not a valid value (it would busy-loop the dispatcher). Validation
/// lives in `crate::config::load_config`, not in a `serde` default,
/// so the operator sees a clear `Error::Config` at startup rather
/// than a silent clamp.
#[derive(Debug, Clone, Deserialize)]
pub struct EscalationConfig {
    /// Feature flag. `true` spawns the Phase 4 dispatcher task;
    /// `false` keeps Phase 4 entirely inert. Default: `false`.
    #[serde(default)]
    pub enabled: bool,

    /// How often the dispatcher scans `mediation_events` for pending
    /// `handoff_prepared` rows. Positive integer seconds. The
    /// SC-201 delivery target (60 s at default interval) assumes the
    /// default 30 here. Default: 30.
    #[serde(default = "default_dispatch_interval_seconds")]
    pub dispatch_interval_seconds: u64,

    /// When zero solvers with `Write` permission are configured,
    /// `true` broadcasts the handoff DM to every configured solver
    /// regardless of permission; `false` refuses to broadcast and
    /// records an `escalation_dispatch_unroutable` audit event.
    /// Default: `false` (fail loud and visible).
    #[serde(default)]
    pub fallback_to_all_solvers: bool,
}

impl Default for EscalationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dispatch_interval_seconds: default_dispatch_interval_seconds(),
            fallback_to_all_solvers: false,
        }
    }
}

fn default_dispatch_interval_seconds() -> u64 {
    30
}

#[cfg(test)]
mod phase3_tests {
    use super::*;

    const PHASE3_ENABLED: &str = r#"
[serbero]
private_key = "aa11"

[mostro]
pubkey = "bb22"

[mediation]
enabled = true
max_rounds = 3

[reasoning]
enabled = true
provider = "openai"
model = "gpt-5"
api_base = "https://example.test/v1"
api_key_env = "X_API_KEY"

[prompts]
system_instructions_path = "./prompts/phase3-system.md"
classification_policy_path = "./prompts/phase3-classification.md"
escalation_policy_path = "./prompts/phase3-escalation-policy.md"
mediation_style_path = "./prompts/phase3-mediation-style.md"
message_templates_path = "./prompts/phase3-message-templates.md"

[chat]
inbound_fetch_interval_seconds = 7
"#;

    const PHASE3_DISABLED: &str = r#"
[serbero]
private_key = "aa11"

[mostro]
pubkey = "bb22"
"#;

    #[test]
    fn phase3_enabled_config_parses_all_sections() {
        let cfg: Config = toml::from_str(PHASE3_ENABLED).expect("parse");
        assert!(cfg.mediation.enabled);
        assert_eq!(cfg.mediation.max_rounds, 3);
        // Defaults still apply to unspecified fields.
        assert_eq!(cfg.mediation.solver_auth_retry_max_attempts, 24);
        assert!(cfg.reasoning.enabled);
        assert_eq!(cfg.reasoning.provider, "openai");
        assert_eq!(cfg.reasoning.model, "gpt-5");
        assert_eq!(cfg.reasoning.api_base, "https://example.test/v1");
        assert_eq!(cfg.reasoning.api_key_env, "X_API_KEY");
        // api_key MUST NOT come from TOML — skip_deserializing.
        assert_eq!(cfg.reasoning.api_key, "");
        assert_eq!(
            cfg.prompts.system_instructions_path,
            "./prompts/phase3-system.md"
        );
        assert_eq!(cfg.chat.inbound_fetch_interval_seconds, 7);
    }

    #[test]
    fn phase3_disabled_config_leaves_defaults() {
        let cfg: Config = toml::from_str(PHASE3_DISABLED).expect("parse");
        assert!(!cfg.mediation.enabled);
        assert!(!cfg.reasoning.enabled);
        // Defaults pre-populated so partial operators don't fail.
        assert_eq!(cfg.reasoning.api_base, "https://api.openai.com/v1");
        assert_eq!(cfg.chat.inbound_fetch_interval_seconds, 10);
        assert_eq!(cfg.reasoning.api_key, "");
    }

    #[test]
    fn api_key_cannot_be_set_from_toml() {
        let malicious = r#"
[serbero]
private_key = "aa11"

[mostro]
pubkey = "bb22"

[reasoning]
enabled = false
api_key = "SECRET_FROM_TOML"
"#;
        let cfg: Config = toml::from_str(malicious).expect("parse");
        // Even when the TOML tries to inject it, skip_deserializing
        // keeps the field empty — secrets come only from the env.
        assert_eq!(cfg.reasoning.api_key, "");
    }
}

#[cfg(test)]
mod phase4_tests {
    use super::*;

    const MINIMAL: &str = r#"
[serbero]
private_key = "aa11"

[mostro]
pubkey = "bb22"
"#;

    const ESCALATION_ENABLED: &str = r#"
[serbero]
private_key = "aa11"

[mostro]
pubkey = "bb22"

[escalation]
enabled = true
dispatch_interval_seconds = 45
fallback_to_all_solvers = true
"#;

    #[test]
    fn escalation_section_absent_applies_safe_defaults() {
        // FR-215 / FR-216: omitting `[escalation]` entirely MUST yield
        // enabled=false, interval=30, fallback_to_all_solvers=false.
        let cfg: Config = toml::from_str(MINIMAL).expect("parse");
        assert!(!cfg.escalation.enabled);
        assert_eq!(cfg.escalation.dispatch_interval_seconds, 30);
        assert!(!cfg.escalation.fallback_to_all_solvers);
    }

    #[test]
    fn escalation_section_explicit_values_override_defaults() {
        let cfg: Config = toml::from_str(ESCALATION_ENABLED).expect("parse");
        assert!(cfg.escalation.enabled);
        assert_eq!(cfg.escalation.dispatch_interval_seconds, 45);
        assert!(cfg.escalation.fallback_to_all_solvers);
    }

    #[test]
    fn escalation_config_default_matches_safe_values() {
        // Belt-and-braces: EscalationConfig::default() must match
        // the "section absent" case so callers that bypass serde
        // (e.g. tests constructing a Config directly) get the same
        // safe posture.
        let d = EscalationConfig::default();
        assert!(!d.enabled);
        assert_eq!(d.dispatch_interval_seconds, 30);
        assert!(!d.fallback_to_all_solvers);
    }
}
