use std::process::ExitCode;
use std::time::Duration;

use serbero::config::Settings;
use serbero::store::Store;

/// How long startup waits for the first relay connections.
const RELAY_CONNECT_WAIT: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> ExitCode {
    let path = Settings::default_path();
    let settings = match Settings::load(&path) {
        Ok(settings) => settings,
        Err(e) => {
            // Logging is configured from the file, so stderr is the only channel yet.
            eprintln!("serbero: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = serbero::logging::init(&settings.config.serbero.log_level) {
        eprintln!("serbero: {e}");
        return ExitCode::FAILURE;
    }
    match run(&settings).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "serbero stopped");
            ExitCode::FAILURE
        }
    }
}

async fn run(settings: &Settings) -> serbero::error::Result<()> {
    let config = &settings.config;
    let store = Store::open(&config.serbero.db_path)?;
    let keys = serbero::nostr::keys_from_secret(&settings.secrets.private_key)?;
    let client = serbero::nostr::connect(&config.mostro.relays, RELAY_CONNECT_WAIT).await?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        pubkey = %keys.public_key(),
        schema = store.schema_version()?,
        relays = config.mostro.relays.len(),
        solvers = config.solvers.len(),
        mediation = config.mediation.enabled,
        "serbero started"
    );

    if let Err(e) = tokio::signal::ctrl_c().await {
        tracing::error!(error = %e, "cannot listen for shutdown signal");
    }
    tracing::info!("shutting down");
    serbero::nostr::shutdown(&client).await;
    Ok(())
}
