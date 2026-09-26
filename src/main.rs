use std::process::ExitCode;

use serbero::config::Settings;
use serbero::store::Store;

fn main() -> ExitCode {
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
    let store = match Store::open(&settings.config.serbero.db_path) {
        Ok(store) => store,
        Err(e) => {
            tracing::error!(error = %e, "cannot open database");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        schema = store.schema_version().unwrap_or_default(),
        config = %path.display(),
        solvers = settings.config.solvers.len(),
        mediation = settings.config.mediation.enabled,
        "serbero starting"
    );
    ExitCode::SUCCESS
}
