use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use platformd::config::Config;
use platformd::identity::keygen;
use platformd::store::backup;
use tracing::error;
use tracing_subscriber::EnvFilter;

/// Exit code for configuration errors (`IF-CFG-001`). Other startup and
/// runtime failures exit with 1.
const EXIT_CONFIG: u8 = 2;
const EXIT_FAILURE: u8 = 1;

#[derive(Debug, Parser)]
#[command(name = "platformd", version = platformd::VERSION, about = "agents-platform message hub")]
struct Args {
    /// Path to the TOML configuration file.
    #[arg(
        long,
        global = true,
        env = "PLATFORMD_CONFIG",
        default_value = "platformd.toml"
    )]
    config: PathBuf,

    /// Without a subcommand, platformd runs the server.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Write an online, integrity-checked snapshot of the database to a new
    /// file. Safe while the server is running.
    Backup {
        /// Snapshot file to create; an existing file is refused.
        #[arg(long, value_name = "FILE")]
        out: PathBuf,
    },
    /// Identity attestation signing key management
    #[command(subcommand)]
    Identity(IdentityCommand),
}

#[derive(Debug, Subcommand)]
enum IdentityCommand {
    /// Generate a new Ed25519 signing key (raw 32-byte seed, mode 0640) and
    /// print its kid and public key. Does not read the configuration.
    Keygen {
        /// Key file to create; an existing file is refused.
        #[arg(long, value_name = "FILE")]
        out: PathBuf,
    },
}

fn main() -> ExitCode {
    let args = Args::parse();
    match args.command {
        // keygen needs no configuration (IDENTITY_ATTESTATION §7.1).
        Some(Command::Identity(IdentityCommand::Keygen { out })) => generate_identity_key(&out),
        Some(Command::Backup { out }) => match load_config(&args.config) {
            Ok(config) => write_backup(&config, &out),
            Err(code) => code,
        },
        None => match load_config(&args.config) {
            Ok(config) => serve(config),
            Err(code) => code,
        },
    }
}

/// Starts logging and loads the configuration; a config error is one line
/// on stderr and exit 2.
fn load_config(path: &std::path::Path) -> Result<Config, ExitCode> {
    init_tracing();
    Config::load(path).map_err(|err| {
        eprintln!("platformd: {err}");
        ExitCode::from(EXIT_CONFIG)
    })
}

/// Prints exactly `kid: <8 hex>` and `public_key: <base64>` on stdout. The
/// seed goes only into the new file.
fn generate_identity_key(out: &std::path::Path) -> ExitCode {
    match keygen::generate(out) {
        Ok(key) => {
            println!("kid: {}", key.kid);
            println!("public_key: {}", key.public_key);
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("platformd: identity keygen failed: {err}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// Prints the snapshot summary on stdout: one `key: value` line each, then
/// one `row_count <table> <rows>` line per table for comparison elsewhere.
fn write_backup(config: &Config, out: &std::path::Path) -> ExitCode {
    let snapshot = match backup::snapshot(&config.db_path, out) {
        Ok(snapshot) => snapshot,
        Err(err) => {
            eprintln!("platformd: backup failed: {err}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    println!("backup: {}", snapshot.path.display());
    println!("bytes: {}", snapshot.bytes);
    println!("schema_version: {}", snapshot.schema_version);
    println!("integrity_check: ok");
    for (table, rows) in &snapshot.row_counts {
        println!("row_count {table} {rows}");
    }
    ExitCode::SUCCESS
}

fn serve(config: Config) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("platformd: cannot start async runtime: {err}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };

    runtime.block_on(run(config))
}

async fn run(config: Config) -> ExitCode {
    let server = match platformd::start(&config).await {
        Ok(server) => server,
        Err(err) => {
            error!(error = %err, "startup failed");
            eprintln!("platformd: {err}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    if let Err(err) = server.serve(shutdown_signal()).await {
        error!(error = %err, "server stopped with an error");
        return ExitCode::from(EXIT_FAILURE);
    }
    ExitCode::SUCCESS
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}

/// Resolves on the first SIGINT or SIGTERM.
async fn shutdown_signal() {
    let interrupt = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            error!(error = %err, "cannot listen for SIGINT");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(err) => {
                error!(error = %err, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}
