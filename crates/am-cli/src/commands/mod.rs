pub mod config;
pub mod identity;
pub mod listen;
pub mod profile;
pub mod relay;
pub mod send;
pub mod skill;

use clap::{Parser, Subcommand, ValueEnum};

use am_core::error::AmResult;

#[derive(Debug, Clone, ValueEnum)]
pub enum OutputFormat {
    Json,
    Text,
}

#[derive(Debug, Parser)]
#[command(
    name = "am",
    about = "Agent messenger — E2E encrypted messaging over Nostr"
)]
#[command(version)]
pub struct Cli {
    /// Output format
    #[arg(long, default_value = "json", global = true)]
    pub format: OutputFormat,

    /// Identity name to use
    #[arg(long, global = true)]
    pub identity: Option<String>,

    /// Quiet mode — suppress non-essential output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Increase verbosity (-v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Passphrase for encrypted key files (NIP-49)
    #[arg(long, env = "AM_PASSPHRASE", global = true)]
    pub passphrase: Option<String>,

    /// Directory for all am state (config.toml + identities/), overriding
    /// the platform data/config dirs
    #[arg(long, env = "AM_HOME", global = true, value_name = "DIR")]
    pub home: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Manage identities (keypairs)
    #[command(subcommand)]
    Identity(identity::IdentityCmd),

    /// Send an encrypted message
    Send(Box<send::SendArgs>),

    /// Listen for incoming messages
    Listen(listen::ListenArgs),

    /// Manage profile metadata
    #[command(subcommand)]
    Profile(profile::ProfileCmd),

    /// Manage relays
    #[command(subcommand)]
    Relay(relay::RelayCmd),

    /// Manage configuration
    #[command(subcommand)]
    Config(config::ConfigCmd),

    /// Print instructions for installing the am agent skill
    Skill,
}

pub async fn run(cli: Cli) -> AmResult<()> {
    if let Some(home) = cli.home.as_deref().filter(|p| !p.as_os_str().is_empty()) {
        am_core::config::set_home(home);
    }

    let no_dna = std::env::var("NO_DNA").is_ok_and(|v| !v.is_empty());

    let mut format = match cli.format {
        OutputFormat::Json => am_core::output::Format::Json,
        OutputFormat::Text => am_core::output::Format::Text,
    };
    if no_dna {
        format = am_core::output::Format::Json;
    }

    let id = cli.identity.as_deref();
    let passphrase = cli.passphrase.as_deref();

    let mut verbosity = cli.verbose;
    if no_dna && verbosity == 0 {
        verbosity = 1;
    }

    match cli.command {
        Commands::Identity(cmd) => identity::run(cmd, passphrase, format).await,
        Commands::Send(args) => send::run(*args, id, passphrase, format, verbosity).await,
        Commands::Listen(args) => listen::run(args, id, passphrase, format, verbosity).await,
        Commands::Profile(cmd) => profile::run(cmd, id, passphrase, format, verbosity).await,
        Commands::Relay(cmd) => relay::run(cmd, id, passphrase, format, verbosity).await,
        Commands::Config(cmd) => config::run(cmd, format).await,
        Commands::Skill => {
            skill::print_instructions(format);
            Ok(())
        }
    }
}
