use nostr_sdk::prelude::*;
use serde::Serialize;

use crate::client::{self, RelayResult};
use crate::config::{load_config, save_config};
use crate::error::{AmError, AmResult};
use crate::identity::load_keys;

#[derive(Debug, Serialize)]
pub struct RelayInfo {
    pub url: String,
}

/// Result of publishing the NIP-17 DM relay list (kind 10050).
#[derive(Debug, Serialize)]
pub struct PublishedRelayList {
    pub npub: String,
    pub event_id: String,
    pub relays: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<RelayResult>,
}

pub fn add(url: &str) -> AmResult<()> {
    let mut config = load_config()?;
    if config.relays.iter().any(|r| r == url) {
        return Err(AmError::Config(format!("relay '{url}' already exists")));
    }
    config.relays.push(url.to_string());
    save_config(&config)?;
    Ok(())
}

pub fn remove(url: &str) -> AmResult<()> {
    let mut config = load_config()?;
    let before = config.relays.len();
    config.relays.retain(|r| r != url);
    if config.relays.len() == before {
        return Err(AmError::Config(format!("relay '{url}' not found")));
    }
    save_config(&config)?;
    Ok(())
}

pub fn list() -> AmResult<Vec<RelayInfo>> {
    let config = load_config()?;
    Ok(config
        .relays
        .into_iter()
        .map(|url| RelayInfo { url })
        .collect())
}

/// Publish the configured relays as this identity's NIP-17 DM relay list
/// (kind 10050), so other clients know where to deliver gift wraps.
///
/// The list is replaceable: each publish supersedes the previous one.
pub async fn publish(
    identity: Option<&str>,
    passphrase: Option<&str>,
    verbosity: u8,
) -> AmResult<PublishedRelayList> {
    let keys = load_keys(identity.unwrap_or("default"), passphrase)?;
    let config = load_config()?;

    if config.relays.is_empty() {
        return Err(AmError::Network(
            "no relays configured; run `am relay add <url>`".into(),
        ));
    }

    let mut tags = Vec::new();
    for url in &config.relays {
        let relay_url = RelayUrl::parse(url)
            .map_err(|e| AmError::Config(format!("invalid relay url '{url}': {e}")))?;
        tags.push(Tag::from_standardized(TagStandard::Relay(relay_url)));
    }

    let npub = keys.public_key().to_bech32().unwrap_or_default();
    let client = client::connect(keys, &config.relays).await?;

    let event = client
        .sign_event_builder(EventBuilder::new(Kind::InboxRelays, "").tags(tags))
        .await
        .map_err(|e| AmError::Crypto(e.to_string()))?;
    let event_id = event.id.to_bech32().unwrap_or_default();

    let (results, succeeded) =
        client::send_with_retry(&client, &event, &config.relays, 3, verbosity).await;

    client.disconnect().await;

    if succeeded.is_empty() {
        return Err(AmError::Network(
            "failed to publish relay list to any relay".into(),
        ));
    }

    Ok(PublishedRelayList {
        npub,
        event_id,
        relays: config.relays,
        results,
    })
}
