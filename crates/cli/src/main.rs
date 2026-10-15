//! `platform`: command-line HTTP client for agents-platform (`IF-CLI-001`).
//!
//! Exit codes: 0 success, 1 the server returned an error, 2 usage or
//! configuration error, 3 network error.

mod args;
mod client;
mod error;

use std::process::ExitCode;

use crate::args::{Command, Settings};
use crate::client::ApiClient;
use crate::error::CliError;

fn main() -> ExitCode {
    // clap prints usage errors itself and exits with 2.
    let settings = match Settings::from_env_and_args() {
        Ok(settings) => settings,
        Err(err) => return report(err),
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return report(CliError::Usage(format!("cannot start runtime: {err}"))),
    };

    match runtime.block_on(run(settings)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => report(err),
    }
}

async fn run(settings: Settings) -> Result<(), CliError> {
    let client = ApiClient::new(&settings.url, settings.token)?;
    match settings.command {
        Command::Health => health(&client, settings.json).await,
    }
}

async fn health(client: &ApiClient, json: bool) -> Result<(), CliError> {
    let response = client.health().await?;
    if json {
        println!("{}", response.raw);
        return Ok(());
    }
    println!(
        "{} (schema_version {})",
        response.value.status, response.value.schema_version
    );
    Ok(())
}

fn report(err: CliError) -> ExitCode {
    eprintln!("platform: {err}");
    ExitCode::from(err.exit_code())
}
