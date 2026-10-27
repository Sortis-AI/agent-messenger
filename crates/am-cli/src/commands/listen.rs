use clap::Args;

use am_core::error::AmResult;
use am_core::output::Format;
use am_core::{message, output};

#[derive(Debug, Args)]
pub struct ListenArgs {
    /// Only messages whose own timestamp is >= this Unix time (inclusive).
    /// Resume by passing the last `timestamp` you saw and dedup by `id`
    #[arg(long)]
    pub since: Option<u64>,

    /// Maximum number of messages to fetch (the newest are kept)
    #[arg(long)]
    pub limit: Option<usize>,

    /// Fetch once and exit (no streaming)
    #[arg(long)]
    pub once: bool,

    /// Timeout in seconds for --once mode
    #[arg(long, default_value = "30")]
    pub timeout: u64,

    /// Also receive legacy NIP-04 (kind 4) DMs. Off by default: weaker privacy
    #[arg(long)]
    pub nip04: bool,
}

pub async fn run(
    args: ListenArgs,
    identity: Option<&str>,
    passphrase: Option<&str>,
    format: Format,
    verbosity: u8,
) -> AmResult<()> {
    let messages = message::listen(
        identity,
        args.since,
        args.limit,
        args.once,
        args.timeout,
        args.nip04,
        passphrase,
        verbosity,
    )
    .await?;

    if args.once {
        match format {
            Format::Json => {
                for msg in &messages {
                    output::print_json(msg)?;
                }
            }
            Format::Text => {
                for msg in &messages {
                    println!("[{}] {}: {}", msg.timestamp, msg.from, msg.content);
                }
            }
        }
    }
    // Streaming mode prints NDJSON inline in the message module.
    Ok(())
}
