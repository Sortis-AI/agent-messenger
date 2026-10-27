use clap::Subcommand;

use am_core::config;
use am_core::error::AmResult;
use am_core::output;
use am_core::output::Format;

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Show current configuration
    Show,

    /// Set a configuration value
    Set {
        /// Config key (e.g. default_identity, format)
        key: String,

        /// Config value
        value: String,
    },
}

pub async fn run(cmd: ConfigCmd, format: Format) -> AmResult<()> {
    match cmd {
        ConfigCmd::Show => {
            let cfg = config::load_config()?;
            match format {
                Format::Json => output::print_json(&cfg)?,
                Format::Text => {
                    println!("{}", toml::to_string_pretty(&cfg).unwrap_or_default());
                }
            }
        }
        ConfigCmd::Set { key, value } => {
            let mut cfg = config::load_config()?;
            match key.as_str() {
                "default_identity" => cfg.default_identity = Some(value.clone()),
                "format" => cfg.format = Some(value.clone()),
                _ => {
                    return Err(am_core::error::AmError::Args(format!(
                        "unknown config key: {key}"
                    )));
                }
            }
            config::save_config(&cfg)?;
            match format {
                Format::Json => output::print_json(&serde_json::json!({ "set": { key: value } }))?,
                Format::Text => println!("Set {key} = {value}"),
            }
        }
    }
    Ok(())
}
