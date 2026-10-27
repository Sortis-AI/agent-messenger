use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::error::{AmError, AmResult};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub default_identity: Option<String>,

    #[serde(default)]
    pub relays: Vec<String>,

    #[serde(default)]
    pub format: Option<String>,
}

/// Environment variable that relocates all am state (config + identities)
/// into a single directory.
pub const HOME_ENV: &str = "AM_HOME";

static HOME_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Set a process-global home directory. When set, config lives at
/// `<home>/config.toml` and identities at `<home>/identities/`.
///
/// Takes precedence over the `AM_HOME` environment variable. Only the first
/// call has an effect; returns `false` if a different home was already set.
pub fn set_home(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref().to_path_buf();
    match HOME_OVERRIDE.set(path.clone()) {
        Ok(()) => true,
        Err(_) => HOME_OVERRIDE.get() == Some(&path),
    }
}

/// The home override in effect: `set_home()` first, then a non-empty
/// `AM_HOME` environment variable. `None` means platform defaults apply.
pub fn home_override() -> Option<PathBuf> {
    if let Some(p) = HOME_OVERRIDE.get() {
        return Some(p.clone());
    }
    std::env::var_os(HOME_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Directory holding identities. `$AM_HOME` if set, else
/// `<platform data dir>/am` (e.g. `~/.local/share/am`).
pub fn data_dir() -> AmResult<PathBuf> {
    if let Some(home) = home_override() {
        return Ok(home);
    }
    let base = dirs::data_dir()
        .ok_or_else(|| AmError::Config("cannot determine data directory".into()))?;
    Ok(base.join("am"))
}

/// Directory holding `config.toml`. `$AM_HOME` if set, else
/// `<platform config dir>/am` (e.g. `~/.config/am`).
pub fn config_dir() -> AmResult<PathBuf> {
    if let Some(home) = home_override() {
        return Ok(home);
    }
    let base = dirs::config_dir()
        .ok_or_else(|| AmError::Config("cannot determine config directory".into()))?;
    Ok(base.join("am"))
}

pub fn config_path() -> AmResult<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn identity_dir() -> AmResult<PathBuf> {
    Ok(data_dir()?.join("identities"))
}

pub fn ensure_dirs() -> AmResult<()> {
    fs::create_dir_all(config_dir()?)?;
    fs::create_dir_all(identity_dir()?)?;
    Ok(())
}

pub fn load_config() -> AmResult<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let contents = fs::read_to_string(&path)?;
    let config: Config = toml::from_str(&contents)?;
    Ok(config)
}

pub fn save_config(config: &Config) -> AmResult<()> {
    ensure_dirs()?;
    let contents = toml::to_string_pretty(config)?;
    fs::write(config_path()?, contents)?;
    Ok(())
}
