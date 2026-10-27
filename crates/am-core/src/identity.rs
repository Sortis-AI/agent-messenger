use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use nostr_sdk::prelude::*;
use serde::Serialize;

use crate::config::identity_dir;
use crate::error::{AmError, AmResult};

#[derive(Debug, Serialize)]
pub struct IdentityInfo {
    pub name: String,
    pub npub: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nsec: Option<String>,
    pub encrypted: bool,
}

fn identity_path(name: &str) -> AmResult<PathBuf> {
    Ok(identity_dir()?.join(format!("{name}.nsec")))
}

pub fn generate(name: Option<&str>, passphrase: Option<&str>) -> AmResult<IdentityInfo> {
    let keys = Keys::generate();
    let name = name.unwrap_or("default").to_string();
    store_keys(&name, &keys, passphrase)?;
    Ok(identity_info(&name, &keys, false, passphrase.is_some()))
}

pub fn import(nsec: &str, name: Option<&str>, passphrase: Option<&str>) -> AmResult<IdentityInfo> {
    // User-supplied input: a malformed nsec is an argument error (exit 2),
    // not a crypto failure.
    let secret_key =
        SecretKey::from_bech32(nsec).map_err(|e| AmError::Args(format!("invalid nsec: {e}")))?;
    let keys = Keys::new(secret_key);
    let name = name.unwrap_or("default").to_string();
    store_keys(&name, &keys, passphrase)?;
    Ok(identity_info(&name, &keys, false, passphrase.is_some()))
}

pub fn show(
    name: Option<&str>,
    show_secret: bool,
    passphrase: Option<&str>,
) -> AmResult<IdentityInfo> {
    let name = name.unwrap_or("default");
    let encrypted = is_encrypted(name)?;
    let keys = load_keys(name, passphrase)?;
    Ok(identity_info(name, &keys, show_secret, encrypted))
}

pub fn list() -> AmResult<Vec<IdentityInfo>> {
    let dir = identity_dir()?;
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut identities = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("nsec") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let content = fs::read_to_string(&path)?.trim().to_string();
                let encrypted = content.starts_with("ncryptsec1");
                if encrypted {
                    // Can't load encrypted keys without passphrase; show limited info
                    identities.push(IdentityInfo {
                        name: stem.to_string(),
                        npub: "(encrypted)".to_string(),
                        nsec: None,
                        encrypted: true,
                    });
                } else {
                    let keys = load_keys(stem, None)?;
                    identities.push(identity_info(stem, &keys, false, false));
                }
            }
        }
    }
    identities.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(identities)
}

pub fn encrypt_existing(name: &str, passphrase: &str) -> AmResult<IdentityInfo> {
    let path = identity_path(name)?;
    if !path.exists() {
        return Err(AmError::Config(format!("identity '{name}' not found")));
    }
    let content = fs::read_to_string(&path)?.trim().to_string();
    if content.starts_with("ncryptsec1") {
        return Err(AmError::Config(format!(
            "identity '{name}' is already encrypted"
        )));
    }
    let secret_key =
        SecretKey::from_bech32(&content).map_err(|e| AmError::Crypto(e.to_string()))?;
    let keys = Keys::new(secret_key.clone());
    let encrypted = EncryptedSecretKey::new(&secret_key, passphrase, 16, KeySecurity::Medium)
        .map_err(|e| AmError::Crypto(e.to_string()))?;
    let ncryptsec = encrypted
        .to_bech32()
        .map_err(|e| AmError::Crypto(e.to_string()))?;
    fs::write(&path, ncryptsec)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(identity_info(name, &keys, false, true))
}

pub fn decrypt_existing(name: &str, passphrase: &str) -> AmResult<IdentityInfo> {
    let path = identity_path(name)?;
    if !path.exists() {
        return Err(AmError::Config(format!("identity '{name}' not found")));
    }
    let content = fs::read_to_string(&path)?.trim().to_string();
    if !content.starts_with("ncryptsec1") {
        return Err(AmError::Config(format!(
            "identity '{name}' is not encrypted"
        )));
    }
    let encrypted =
        EncryptedSecretKey::from_bech32(&content).map_err(|e| AmError::Crypto(e.to_string()))?;
    let secret_key = encrypted
        .decrypt(passphrase)
        .map_err(|e| AmError::Crypto(e.to_string()))?;
    let keys = Keys::new(secret_key.clone());
    let nsec = secret_key
        .to_bech32()
        .map_err(|e| AmError::Crypto(e.to_string()))?;
    fs::write(&path, nsec)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(identity_info(name, &keys, false, false))
}

pub fn load_keys(name: &str, passphrase: Option<&str>) -> AmResult<Keys> {
    let path = identity_path(name)?;
    if !path.exists() {
        return Err(AmError::Config(format!("identity '{name}' not found")));
    }
    let content = fs::read_to_string(&path)?.trim().to_string();

    if content.starts_with("ncryptsec1") {
        let passphrase = passphrase.ok_or_else(|| {
            AmError::Crypto(format!(
                "identity '{name}' is encrypted; provide --passphrase or set AM_PASSPHRASE"
            ))
        })?;
        let encrypted = EncryptedSecretKey::from_bech32(&content)
            .map_err(|e| AmError::Crypto(e.to_string()))?;
        let secret_key = encrypted
            .decrypt(passphrase)
            .map_err(|e| AmError::Crypto(e.to_string()))?;
        Ok(Keys::new(secret_key))
    } else {
        let secret_key =
            SecretKey::from_bech32(&content).map_err(|e| AmError::Crypto(e.to_string()))?;
        Ok(Keys::new(secret_key))
    }
}

fn is_encrypted(name: &str) -> AmResult<bool> {
    let path = identity_path(name)?;
    if !path.exists() {
        return Err(AmError::Config(format!("identity '{name}' not found")));
    }
    let content = fs::read_to_string(&path)?.trim().to_string();
    Ok(content.starts_with("ncryptsec1"))
}

fn store_keys(name: &str, keys: &Keys, passphrase: Option<&str>) -> AmResult<()> {
    crate::config::ensure_dirs()?;
    let path = identity_path(name)?;
    if path.exists() {
        return Err(AmError::Config(format!("identity '{name}' already exists")));
    }

    let content = if let Some(pass) = passphrase {
        let secret_key = keys.secret_key();
        let encrypted = EncryptedSecretKey::new(secret_key, pass, 16, KeySecurity::Medium)
            .map_err(|e| AmError::Crypto(e.to_string()))?;
        encrypted
            .to_bech32()
            .map_err(|e| AmError::Crypto(e.to_string()))?
    } else {
        keys.secret_key()
            .to_bech32()
            .map_err(|e| AmError::Crypto(e.to_string()))?
    };

    fs::write(&path, content)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn identity_info(name: &str, keys: &Keys, show_secret: bool, encrypted: bool) -> IdentityInfo {
    let nsec = if show_secret {
        keys.secret_key().to_bech32().ok()
    } else {
        None
    };
    IdentityInfo {
        name: name.to_string(),
        npub: keys.public_key().to_bech32().unwrap_or_default(),
        nsec,
        encrypted,
    }
}
