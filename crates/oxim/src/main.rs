//! `oxim`: the OXIM clinical integration engine.

mod commands;
mod components;
mod init;
mod logging;
mod profile;
mod run;
mod service;
mod settings;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Result type of the command-line layer.
pub(crate) type CliResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Parser)]
#[command(name = "oxim", version, about = "OXIM clinical integration engine")]
struct Cli {
    /// The engine configuration file.
    #[arg(long, short, global = true, default_value = "oxim.yaml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the engine in the foreground until Ctrl+C.
    Run,
    /// Create a starter configuration in a directory.
    Init {
        /// Target directory.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Overwrite existing files.
        #[arg(long)]
        force: bool,
    },
    /// Check the configuration and every channel file.
    Validate,
    /// List channel files.
    Channels,
    /// Inspect and repair stored messages.
    Messages {
        #[command(subcommand)]
        command: MessagesCommand,
    },
    /// Check device profiles and run their fixtures.
    Profile {
        #[command(subcommand)]
        command: profile::ProfileCommand,
    },
    /// Install and control the operating system service.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
}

#[derive(Debug, Subcommand)]
enum MessagesCommand {
    /// List messages, newest first.
    List {
        /// Only this channel.
        #[arg(long)]
        channel: Option<String>,
        /// Only this status: received, filtered, transformed, completed or error.
        #[arg(long)]
        status: Option<String>,
        /// Only messages with a destination in this status, e.g. failed.
        #[arg(long)]
        destination_status: Option<String>,
        /// How many messages to show.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show a message and one stage of its content.
    Show {
        /// The message identifier.
        id: String,
        /// raw, normalized, transformed, encoded or response.
        #[arg(long, default_value = "raw")]
        stage: String,
        /// The destination, for encoded and response content.
        #[arg(long)]
        destination: Option<String>,
    },
    /// Process a message again from its raw content.
    Reprocess {
        /// The message identifier.
        id: String,
    },
    /// Queue a failed delivery again.
    Requeue {
        /// The message identifier.
        id: String,
        /// The destination identifier.
        destination: String,
    },
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// Register OXIM as a Windows service or systemd unit.
    Install,
    /// Remove the service.
    Uninstall,
    /// Start the service.
    Start,
    /// Stop the service.
    Stop,
    /// Print the systemd unit without installing it.
    Unit,
    /// Entry point used by the service manager.
    #[command(hide = true)]
    Run,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Not locked: engine threads log to standard output while commands run.
    let mut out = std::io::stdout();
    let result = match cli.command {
        Command::Run => run_foreground(&cli.config),
        Command::Init { dir, force } => init::init(&dir, force, &mut out),
        Command::Validate => settings::Settings::load(&cli.config)
            .and_then(|settings| commands::validate(&settings, &mut out))
            .and_then(|invalid| {
                if invalid == 0 {
                    Ok(())
                } else {
                    Err(format!("{invalid} invalid channel(s)").into())
                }
            }),
        Command::Channels => settings::Settings::load(&cli.config)
            .and_then(|settings| commands::channels(&settings, &mut out)),
        Command::Messages { command } => {
            settings::Settings::load(&cli.config).and_then(|settings| match command {
                MessagesCommand::List {
                    channel,
                    status,
                    destination_status,
                    limit,
                } => commands::list_messages(
                    &settings,
                    channel,
                    status,
                    destination_status,
                    limit,
                    &mut out,
                ),
                MessagesCommand::Show {
                    id,
                    stage,
                    destination,
                } => commands::show_message(&settings, &id, &stage, destination, &mut out),
                MessagesCommand::Reprocess { id } => commands::reprocess(&settings, &id, &mut out),
                MessagesCommand::Requeue { id, destination } => {
                    commands::requeue(&settings, &id, &destination, &mut out)
                }
            })
        }
        Command::Profile { command } => profile::run(command, &mut out),
        Command::Service { command } => match command {
            ServiceCommand::Install => service::install(&cli.config, &mut out),
            ServiceCommand::Uninstall => service::uninstall(&mut out),
            ServiceCommand::Start => service::start(&mut out),
            ServiceCommand::Stop => service::stop(&mut out),
            ServiceCommand::Unit => std::env::current_exe().map_err(Into::into).and_then(|exe| {
                let config = cli.config.canonicalize().unwrap_or(cli.config.clone());
                use std::io::Write;
                write!(out, "{}", service::systemd_unit(&exe, &config)).map_err(Into::into)
            }),
            ServiceCommand::Run => service::run(&cli.config),
        },
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_foreground(config: &std::path::Path) -> CliResult<()> {
    let settings = settings::Settings::load(config)?;
    let _guard = logging::init(&settings.log)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("oxim")
        .build()?;
    runtime.block_on(run::run(settings, run::shutdown_signal()))
}
