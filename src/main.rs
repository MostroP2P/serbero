use std::process::ExitCode;

use serbero::config::Settings;

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
    match serbero::daemon::run(&settings).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "serbero stopped");
            ExitCode::FAILURE
        }
    }
}
