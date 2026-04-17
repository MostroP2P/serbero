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
