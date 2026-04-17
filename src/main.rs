use std::path::PathBuf;
use std::process::ExitCode;

use serbero::{config, daemon};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    let config_path = PathBuf::from(
        std::env::var("SERBERO_CONFIG").unwrap_or_else(|_| "config.toml".to_string()),
    );

    let cfg = match config::load_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to load config from {}: {e}", config_path.display());
            return ExitCode::FAILURE;
        }
    };

    let filter = EnvFilter::try_from_env("SERBERO_LOG")
        .unwrap_or_else(|_| EnvFilter::new(&cfg.serbero.log_level));
    if tracing_subscriber::fmt()
        .with_env_filter(filter)
        .try_init()
        .is_err()
    {
        eprintln!("tracing subscriber already initialised");
    }

    info!(
        version = env!("CARGO_PKG_VERSION"),
        config_path = %config_path.display(),
        "starting serbero"
    );

    match daemon::run(cfg).await {
        Ok(()) => {
            info!("serbero exited cleanly");
            ExitCode::SUCCESS
        }
        Err(e) => {
            error!(error = %e, "serbero terminated with error");
            ExitCode::FAILURE
        }
    }
}
