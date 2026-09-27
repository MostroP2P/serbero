use std::process::ExitCode;

fn main() -> ExitCode {
    if let Err(e) = serbero::logging::init("info") {
        // The subscriber is not installed, so stderr is the only channel left.
        eprintln!("serbero: {e}");
        return ExitCode::FAILURE;
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "serbero starting");
    ExitCode::SUCCESS
}
