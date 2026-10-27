use clap::Subcommand;

use am_core::error::AmResult;
use am_core::output::Format;
use am_core::{output, relay};

#[derive(Debug, Subcommand)]
pub enum RelayCmd {
    /// Add a relay (and publish the updated DM relay list)
    Add {
        /// Relay websocket URL (wss://...)
        url: String,

        /// Don't publish the DM relay list (kind 10050) after adding
        #[arg(long)]
        no_publish: bool,
    },

    /// Remove a relay (and publish the updated DM relay list)
    Remove {
        /// Relay websocket URL
        url: String,

        /// Don't publish the DM relay list (kind 10050) after removing
        #[arg(long)]
        no_publish: bool,
    },

    /// List configured relays
    List,

    /// Publish configured relays as this identity's DM relay list (NIP-17 kind 10050)
    Publish,
}

pub async fn run(
    cmd: RelayCmd,
    identity: Option<&str>,
    passphrase: Option<&str>,
    format: Format,
    verbosity: u8,
) -> AmResult<()> {
    match cmd {
        RelayCmd::Add { url, no_publish } => {
            relay::add(&url)?;
            let mut out = serde_json::json!({ "added": url });
            if format == Format::Text {
                println!("Added relay: {url}");
            }
            if !no_publish {
                auto_publish(&mut out, identity, passphrase, format, verbosity).await;
            }
            if format == Format::Json {
                output::print_json(&out)?;
            }
        }
        RelayCmd::Remove { url, no_publish } => {
            relay::remove(&url)?;
            let mut out = serde_json::json!({ "removed": url });
            if format == Format::Text {
                println!("Removed relay: {url}");
            }
            if !no_publish {
                auto_publish(&mut out, identity, passphrase, format, verbosity).await;
            }
            if format == Format::Json {
                output::print_json(&out)?;
            }
        }
        RelayCmd::List => {
            let relays = relay::list()?;
            match format {
                Format::Json => output::print_json(&relays)?,
                Format::Text => {
                    for r in &relays {
                        println!("{}", r.url);
                    }
                }
            }
        }
        RelayCmd::Publish => {
            let published = relay::publish(identity, passphrase, verbosity).await?;
            match format {
                Format::Json => output::print_json(&published)?,
                Format::Text => {
                    println!("Published DM relay list for {}", published.npub);
                    for r in &published.relays {
                        println!("  {r}");
                    }
                }
            }
        }
    }
    Ok(())
}

/// Publish the relay list after a config change. Failure is reported but
/// non-fatal: the config change itself already succeeded.
async fn auto_publish(
    out: &mut serde_json::Value,
    identity: Option<&str>,
    passphrase: Option<&str>,
    format: Format,
    verbosity: u8,
) {
    match relay::publish(identity, passphrase, verbosity).await {
        Ok(published) => {
            if format == Format::Text {
                println!("Published DM relay list for {}", published.npub);
            }
            out["relay_list"] = serde_json::to_value(&published).unwrap_or_default();
        }
        Err(e) => {
            if format == Format::Text {
                eprintln!("warning: DM relay list not published: {e}");
            }
            out["relay_list_error"] = serde_json::Value::String(e.to_string());
        }
    }
}
