use std::collections::HashSet;

use nostr_sdk::prelude::*;
use serde::Serialize;

use crate::error::{AmError, AmResult};

#[derive(Debug, Clone, Serialize)]
pub struct RelayResult {
    pub relay: String,
    pub status: RelayStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u8>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RelayStatus {
    Ok,
    Failed,
}

/// Create a connected client with the given keys and relays.
pub async fn connect(keys: Keys, relays: &[String]) -> AmResult<Client> {
    let client = Client::new(keys);
    // NIP-42: answer AUTH challenges and retry `auth-required:` CLOSED/OK
    // responses. On by default in nostr-sdk 0.44; set explicitly so a future
    // default change can't silently break AUTH-gated inbox relays.
    client.automatic_authentication(true);
    for relay in relays {
        client
            .add_relay(relay.as_str())
            .await
            .map_err(|e| AmError::Network(e.to_string()))?;
    }
    client.connect().await;
    client
        .wait_for_connection(std::time::Duration::from_secs(5))
        .await;
    Ok(client)
}

/// Send an event to all relays with per-relay retry on failure.
///
/// Returns the per-relay results and the set of relay URLs that succeeded.
pub async fn send_with_retry(
    client: &Client,
    event: &Event,
    relays: &[String],
    max_retries: u8,
    verbosity: u8,
) -> (Vec<RelayResult>, HashSet<String>) {
    let output = client.send_event(event).await;

    let mut results = Vec::new();
    let mut succeeded = HashSet::new();

    // Parse initial output
    let (initial_success, initial_failed) = match output {
        Ok(out) => (out.success.clone(), out.failed.clone()),
        Err(e) => {
            // Total failure — mark all relays as failed
            tracing::warn!("send_event failed: {e}");
            let failed: std::collections::HashMap<RelayUrl, String> = relays
                .iter()
                .filter_map(|r| RelayUrl::parse(r).ok().map(|url| (url, e.to_string())))
                .collect();
            (HashSet::new(), failed)
        }
    };

    // Record successes
    for url in &initial_success {
        let relay_str = url.to_string();
        succeeded.insert(relay_str.clone());
        results.push(RelayResult {
            relay: relay_str,
            status: RelayStatus::Ok,
            error: None,
            attempts: if verbosity >= 1 { Some(1) } else { None },
        });
    }

    // Retry failed relays
    for (url, err) in &initial_failed {
        let relay_str = url.to_string();
        let mut last_error = err.clone();
        let mut attempt = 1u8;
        let mut ok = false;

        while attempt < max_retries {
            attempt += 1;
            tracing::debug!("retrying {relay_str} (attempt {attempt}/{max_retries})");

            match client.send_event_to([url.clone()], event).await {
                Ok(out) if !out.success.is_empty() => {
                    ok = true;
                    break;
                }
                Ok(out) => {
                    if let Some((_, e)) = out.failed.into_iter().next() {
                        last_error = e;
                    }
                }
                Err(e) => {
                    last_error = e.to_string();
                }
            }
        }

        if ok {
            succeeded.insert(relay_str.clone());
            results.push(RelayResult {
                relay: relay_str,
                status: RelayStatus::Ok,
                error: None,
                attempts: if verbosity >= 1 { Some(attempt) } else { None },
            });
        } else {
            results.push(RelayResult {
                relay: relay_str,
                status: RelayStatus::Failed,
                error: if verbosity >= 1 {
                    Some(last_error)
                } else {
                    None
                },
                attempts: if verbosity >= 1 { Some(attempt) } else { None },
            });
        }
    }

    // Relays that weren't in either set (not connected)
    for relay in relays {
        let in_results = results.iter().any(|r| r.relay == *relay);
        if !in_results {
            results.push(RelayResult {
                relay: relay.clone(),
                status: RelayStatus::Failed,
                error: if verbosity >= 1 {
                    Some("not connected".into())
                } else {
                    None
                },
                attempts: if verbosity >= 1 { Some(0) } else { None },
            });
        }
    }

    (results, succeeded)
}
