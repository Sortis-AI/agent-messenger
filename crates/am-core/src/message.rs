use std::collections::HashSet;
use std::io::{self, Read};

use nostr_sdk::prelude::*;
use serde::Serialize;

use crate::client::{self, RelayResult};
use crate::config::load_config;
use crate::error::{AmError, AmResult};
use crate::identity::load_keys;

/// Rumor kind for NIP-17 chat messages.
pub const KIND_CHAT: u16 = 14;
/// Rumor kind for NIP-17 file messages.
pub const KIND_FILE: u16 = 15;

/// How far back NIP-59 may backdate a gift wrap's outer `created_at`
/// (nostr's `RANGE_RANDOM_TIMESTAMP_TWEAK` is 0..2 days), plus a margin for
/// clock skew between sender and relay.
pub const GIFT_WRAP_LOOKBACK_SECS: u64 = 2 * 24 * 60 * 60 + 10 * 60;

#[derive(Debug, Serialize)]
pub struct SentMessage {
    /// Rumor id (hex) for NIP-17, event id for NIP-04. Identical for every
    /// recipient, and equal to the `id` the recipients' `listen` reports.
    pub id: String,
    pub kind: u16,
    pub to: Vec<String>,
    pub content: String,
    pub protocol: Protocol,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub relays: Vec<RelayResult>,
}

/// DM protocol a message was sent or received with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// NIP-17 private DMs (kind 14/15 rumor in a NIP-59 gift wrap)
    #[default]
    Nip17,
    /// Legacy NIP-04 encrypted DMs (kind 4). Leaks metadata; interop only.
    Nip04,
}

/// NIP-17 kind-15 file metadata. The file itself is hosted elsewhere,
/// encrypted by the caller; `am` only carries the URL and the keys.
///
/// On receive every field but `url` is optional, since other clients may
/// omit tags. On send, [`FileMeta::validate`] enforces the NIP-17 required set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FileMeta {
    /// File URL (the rumor's `content`)
    pub url: String,
    /// `file-type`: MIME type before encryption
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_type: Option<String>,
    /// `encryption-algorithm` (NIP-17 defines `aes-gcm`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption_algorithm: Option<String>,
    /// `decryption-key`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decryption_key: Option<String>,
    /// `decryption-nonce`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decryption_nonce: Option<String>,
    /// `x`: SHA-256 (hex) of the encrypted file
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// `ox`: SHA-256 (hex) of the file before encryption
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_hash: Option<String>,
    /// `size`: encrypted size in bytes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// `dim`: `<width>x<height>` in pixels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dim: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blurhash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbhash: Option<String>,
    /// `thumb`: thumbnail URL (same key and nonce)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumb: Option<String>,
    /// `fallback`: alternative file URLs (same key and nonce)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fallback: Vec<String>,
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl FileMeta {
    /// Check the fields NIP-17 requires on a kind-15 rumor.
    pub fn validate(&self) -> AmResult<()> {
        let missing = |name: &str| AmError::Args(format!("file message requires {name}"));
        if self.url.trim().is_empty() {
            return Err(missing("--file-url"));
        }
        let required = [
            (&self.file_type, "--file-type"),
            (&self.encryption_algorithm, "--encryption-algorithm"),
            (&self.decryption_key, "--decryption-key"),
            (&self.decryption_nonce, "--decryption-nonce"),
            (&self.hash, "--file-hash"),
        ];
        for (value, name) in required {
            if value.as_deref().is_none_or(|v| v.trim().is_empty()) {
                return Err(missing(name));
            }
        }
        for (value, name) in [
            (&self.hash, "--file-hash"),
            (&self.original_hash, "--file-original-hash"),
        ] {
            if let Some(v) = value.as_deref().filter(|v| !is_sha256_hex(v)) {
                return Err(AmError::Args(format!(
                    "{name} must be a 64-character hex SHA-256, got '{v}'"
                )));
            }
        }
        Ok(())
    }

    /// The kind-15 tags for this file (everything except `p`/`e`).
    fn to_tags(&self) -> Vec<Tag> {
        let mut out: Vec<Vec<String>> = Vec::new();
        let mut push = |name: &str, value: &Option<String>| {
            if let Some(v) = value {
                out.push(vec![name.to_string(), v.clone()]);
            }
        };
        push("file-type", &self.file_type);
        push("encryption-algorithm", &self.encryption_algorithm);
        push("decryption-key", &self.decryption_key);
        push("decryption-nonce", &self.decryption_nonce);
        push("x", &self.hash);
        push("ox", &self.original_hash);
        push("size", &self.size.map(|s| s.to_string()));
        push("dim", &self.dim);
        push("blurhash", &self.blurhash);
        push("thumbhash", &self.thumbhash);
        push("thumb", &self.thumb);
        for url in &self.fallback {
            out.push(vec!["fallback".to_string(), url.clone()]);
        }
        out.into_iter().map(raw_tag).collect()
    }

    /// Parse kind-15 metadata from a rumor's content and tags.
    pub fn from_rumor(content: &str, tags: &[Vec<String>]) -> Self {
        let first = |name: &str| -> Option<String> {
            tags.iter()
                .find(|t| t.first().is_some_and(|k| k == name))
                .and_then(|t| t.get(1))
                .cloned()
        };
        FileMeta {
            url: content.to_string(),
            file_type: first("file-type"),
            encryption_algorithm: first("encryption-algorithm"),
            decryption_key: first("decryption-key"),
            decryption_nonce: first("decryption-nonce"),
            hash: first("x"),
            original_hash: first("ox"),
            size: first("size").and_then(|s| s.parse().ok()),
            dim: first("dim"),
            blurhash: first("blurhash"),
            thumbhash: first("thumbhash"),
            thumb: first("thumb"),
            fallback: tags
                .iter()
                .filter(|t| t.first().is_some_and(|k| k == "fallback"))
                .filter_map(|t| t.get(1).cloned())
                .collect(),
        }
    }
}

/// Build a tag from raw strings (never empty here, so `parse` can't fail).
fn raw_tag(parts: Vec<String>) -> Tag {
    Tag::parse(parts).expect("tag has a name")
}

/// Tags as plain string arrays, as they appear on the wire.
pub fn tags_to_vec(tags: &Tags) -> Vec<Vec<String>> {
    tags.iter().map(|t| t.as_slice().to_vec()).collect()
}

/// The id of the message this one replies to, from its `e` tags.
///
/// NIP-17 uses a single `e` tag for the direct parent (`["e", id, relay]`,
/// with a `"reply"` marker in the kind-15 template). A `reply`-marked tag
/// wins; otherwise the first `e` tag not marked `root`/`mention`.
pub fn reply_to_from_tags(tags: &[Vec<String>]) -> Option<String> {
    let e_tags: Vec<&Vec<String>> = tags
        .iter()
        .filter(|t| t.first().is_some_and(|k| k == "e"))
        .filter(|t| t.get(1).is_some_and(|id| EventId::from_hex(id).is_ok()))
        .collect();
    let marker = |t: &Vec<String>| t.get(3).cloned().unwrap_or_default();
    e_tags
        .iter()
        .find(|t| marker(t) == "reply")
        .or_else(|| {
            e_tags
                .iter()
                .find(|t| !matches!(marker(t).as_str(), "root" | "mention"))
        })
        .map(|t| t[1].to_lowercase())
}

/// The `e` tag `send --reply-to` adds: `["e", id]` on kind 14 and kind 4,
/// `["e", id, "", "reply"]` on kind 15 (matching each NIP-17 template).
fn reply_tag(id: &EventId, kind: u16) -> Tag {
    let mut parts = vec!["e".to_string(), id.to_hex()];
    if kind == KIND_FILE {
        parts.extend([String::new(), "reply".to_string()]);
    }
    raw_tag(parts)
}

/// Build the single NIP-17 rumor that is gift-wrapped to every recipient.
///
/// The p-tag set defines the chat room: every recipient, in the order given,
/// plus the sender for groups (0xchat matches rooms by the p-tag set, so the
/// sender must be included for the room to be the same on every side). For
/// 1:1 the rumor carries only the recipient's p-tag. One rumor means one id,
/// so every participant sees the same `id`.
///
/// `recipients` must already be deduplicated.
pub fn build_rumor(
    sender: PublicKey,
    recipients: &[PublicKey],
    content: &str,
    reply_to: Option<&EventId>,
    file: Option<&FileMeta>,
) -> UnsignedEvent {
    let kind = if file.is_some() { KIND_FILE } else { KIND_CHAT };
    let mut tags: Vec<Tag> = recipients.iter().map(|pk| Tag::public_key(*pk)).collect();
    if recipients.len() > 1 && !recipients.contains(&sender) {
        tags.push(Tag::public_key(sender));
    }
    if let Some(id) = reply_to {
        tags.push(reply_tag(id, kind));
    }
    if let Some(f) = file {
        tags.extend(f.to_tags());
    }
    // Built by hand rather than with EventBuilder::private_msg, which strips
    // p-tags matching the author; group rooms need the sender's own p-tag.
    let mut rumor = EventBuilder::new(Kind::from(kind), content)
        .tags(tags)
        .allow_self_tagging()
        .build(sender);
    rumor.ensure_id();
    rumor
}

/// Relay-side `since` for a caller's `--since`: NIP-59 backdates the outer
/// gift-wrap timestamp by up to two days, so ask relays for that much more
/// and filter on the decoded (inner) timestamp afterwards.
pub fn relay_since(since: u64) -> u64 {
    since.saturating_sub(GIFT_WRAP_LOOKBACK_SECS)
}

/// Whether a decoded message passes the caller's `--since` (inclusive, on
/// the rumor timestamp for NIP-17 / the event timestamp for NIP-04).
pub fn passes_since(timestamp: u64, since: Option<u64>) -> bool {
    since.is_none_or(|s| timestamp >= s)
}

#[derive(Debug, Serialize)]
pub struct ReceivedMessage {
    /// Stable message id: the rumor id (hex) for NIP-17, the event id for
    /// NIP-04. The same message seen twice has the same id; use it to dedup.
    pub id: String,
    pub from: String,
    pub content: String,
    pub timestamp: u64,
    pub participants: Vec<String>,
    pub protocol: Protocol,
    /// Kind of the inner message: 14 (text) or 15 (file) for NIP-17, 4 for NIP-04
    pub kind: u16,
    /// The rumor's tags (the event's tags for NIP-04)
    pub tags: Vec<Vec<String>>,
    /// Id of the message this one replies to, from its `e` tag
    pub reply_to: Option<String>,
    /// File metadata, present only for kind-15 file messages (`content` is the URL)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<FileMeta>,
}

/// Non-message lines in streaming `listen` output. They always carry an
/// `event` key, which messages never have, so consumers can tell them apart.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum StreamEvent {
    /// The subscription is open. `relays` lists the connected relays;
    /// an empty list means none connected (and `listen` exits with code 3).
    Ready { relays: Vec<String> },
    /// A relay's connection status changed.
    Relay { relay: String, status: String },
    /// NIP-42 authentication result for a relay (`ok` or `failed`).
    Auth { relay: String, status: String },
}

fn emit(event: &StreamEvent) {
    if let Ok(json) = serde_json::to_string(event) {
        println!("{json}");
    }
}

/// Lowercase name for a relay status worth reporting, or `None` for the
/// transient states (`initialized`, `pending`, `connecting`): a dead relay
/// cycles connecting/disconnected on every retry, which is just noise.
fn relay_status_name(status: RelayStatus) -> Option<&'static str> {
    Some(match status {
        RelayStatus::Initialized | RelayStatus::Pending | RelayStatus::Connecting => {
            return None;
        }
        RelayStatus::Connected => "connected",
        RelayStatus::Disconnected => "disconnected",
        RelayStatus::Terminated => "terminated",
        RelayStatus::Banned => "banned",
        RelayStatus::Sleeping => "sleeping",
    })
}

/// Extract sorted, deduplicated participant npubs from an unwrapped gift wrap.
///
/// For group chats, the rumor contains p-tags for all participants.
/// For 1:1 DMs, the rumor has only one p-tag (the recipient), so we
/// construct participants from the sender + our own pubkey.
fn extract_participants(unwrapped: &UnwrappedGift, our_pk: &PublicKey) -> Vec<String> {
    let mut participants: Vec<String> = unwrapped
        .rumor
        .tags
        .public_keys()
        .filter_map(|pk| pk.to_bech32().ok())
        .collect();

    // Always include the sender
    let sender_npub = unwrapped.sender.to_bech32().unwrap_or_default();
    if !participants.contains(&sender_npub) {
        participants.push(sender_npub);
    }

    // Always include ourselves
    let our_npub = our_pk.to_bech32().unwrap_or_default();
    if !participants.contains(&our_npub) {
        participants.push(our_npub);
    }

    participants.sort();
    participants.dedup();
    participants
}

/// What to send: a text message (from `message` or stdin) or a file.
#[derive(Debug, Default)]
pub struct SendRequest<'a> {
    pub to: &'a [String],
    /// Text content; read from stdin when `None` and no `file` is given
    pub message: Option<&'a str>,
    pub protocol: Protocol,
    /// Id (hex) of the message being replied to; adds an `e` tag
    pub reply_to: Option<&'a str>,
    /// Kind-15 file message (NIP-17 only); `content` becomes the file URL
    pub file: Option<FileMeta>,
}

pub async fn send(
    identity: Option<&str>,
    req: SendRequest<'_>,
    passphrase: Option<&str>,
    verbosity: u8,
) -> AmResult<SentMessage> {
    let SendRequest {
        to: to_npubs,
        message,
        protocol,
        reply_to,
        file,
    } = req;

    // Validate arguments before touching keys, config or the network.
    if to_npubs.is_empty() {
        return Err(AmError::Args(
            "at least one --to recipient is required".into(),
        ));
    }
    if let Some(f) = &file {
        if protocol == Protocol::Nip04 {
            return Err(AmError::Args(
                "file messages are NIP-17 only; drop --reply-protocol nip04".into(),
            ));
        }
        if message.is_some() {
            return Err(AmError::Args(
                "a file message takes no text message argument".into(),
            ));
        }
        f.validate()?;
    }
    let reply_to = reply_to
        .map(|id| {
            EventId::from_hex(id)
                .map_err(|e| AmError::Args(format!("invalid --reply-to id '{id}': {e}")))
        })
        .transpose()?;

    let mut recipients: Vec<(String, PublicKey)> = Vec::new();
    for npub in to_npubs {
        let pk = PublicKey::from_bech32(npub)
            .map_err(|e| AmError::Args(format!("invalid recipient '{npub}': {e}")))?;
        if !recipients.iter().any(|(_, p)| *p == pk) {
            recipients.push((npub.clone(), pk));
        }
    }

    // NIP-04 has no notion of a group: one ciphertext per recipient would
    // fan out as unrelated 1:1 DMs, so refuse rather than mislead.
    if protocol == Protocol::Nip04 && recipients.len() > 1 {
        return Err(AmError::Args(
            "--reply-protocol nip04 supports a single --to recipient".into(),
        ));
    }

    let keys = load_keys(identity.unwrap_or("default"), passphrase)?;
    let config = load_config()?;

    if config.relays.is_empty() {
        return Err(AmError::Network(
            "no relays configured; run `am relay add <url>`".into(),
        ));
    }

    let content = match (&file, message) {
        (Some(f), _) => f.url.clone(),
        (None, Some(m)) => m.to_string(),
        (None, None) => {
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf)?;
            buf.trim_end().to_string()
        }
    };

    if content.is_empty() {
        return Err(AmError::Args("message is empty".into()));
    }

    let client = client::connect(keys, &config.relays).await?;

    let signer = client
        .signer()
        .await
        .map_err(|e| AmError::Crypto(e.to_string()))?;

    let our_pk = signer
        .get_public_key()
        .await
        .map_err(|e| AmError::Crypto(e.to_string()))?;

    // Build the events to publish (sequential — each needs unique encryption)
    let mut wrapped_events = Vec::new();
    let (id, kind) = match protocol {
        Protocol::Nip04 => {
            let (npub, pk) = &recipients[0];
            let ciphertext = signer
                .nip04_encrypt(pk, &content)
                .await
                .map_err(|e| AmError::Crypto(e.to_string()))?;
            let mut builder = EventBuilder::new(Kind::EncryptedDirectMessage, ciphertext)
                .tag(Tag::public_key(*pk));
            if let Some(parent) = &reply_to {
                builder = builder.tag(reply_tag(parent, Kind::EncryptedDirectMessage.as_u16()));
            }
            let event = builder
                .sign(&signer)
                .await
                .map_err(|e| AmError::Crypto(e.to_string()))?;
            let id = (event.id, event.kind.as_u16());
            wrapped_events.push((npub.clone(), event));
            id
        }
        Protocol::Nip17 => {
            // ONE rumor for everyone, so its id is the same for every
            // participant; only the seal/gift wrap differ per recipient.
            let pks: Vec<PublicKey> = recipients.iter().map(|(_, pk)| *pk).collect();
            let rumor = build_rumor(our_pk, &pks, &content, reply_to.as_ref(), file.as_ref());
            let id = (
                rumor.id.expect("build_rumor sets the id"),
                rumor.kind.as_u16(),
            );
            for (npub, pk) in &recipients {
                let event = EventBuilder::gift_wrap(&signer, pk, rumor.clone(), [])
                    .await
                    .map_err(|e| AmError::Crypto(e.to_string()))?;
                wrapped_events.push((npub.clone(), event));
            }
            id
        }
    };

    // Send all events concurrently with retry
    let mut handles = Vec::new();
    for (npub, event) in wrapped_events {
        let client = client.clone();
        let relays = config.relays.clone();
        handles.push(tokio::spawn(async move {
            let (relay_results, succeeded) =
                client::send_with_retry(&client, &event, &relays, 3, verbosity).await;
            (npub, relay_results, !succeeded.is_empty())
        }));
    }

    let join_results = futures::future::join_all(handles).await;

    let mut sent = Vec::new();
    let mut failed = Vec::new();
    let mut all_relay_results = Vec::new();

    for result in join_results {
        match result {
            Ok((npub, relay_results, any_success)) => {
                if any_success {
                    sent.push(npub);
                } else {
                    failed.push(npub);
                }
                // Merge relay results (dedup by relay URL, keep worst status)
                for rr in relay_results {
                    if let Some(existing) = all_relay_results
                        .iter_mut()
                        .find(|r: &&mut RelayResult| r.relay == rr.relay)
                    {
                        // If any send to this relay failed, mark it failed
                        if matches!(rr.status, client::RelayStatus::Failed) {
                            existing.status = client::RelayStatus::Failed;
                            existing.error = rr.error;
                        }
                    } else {
                        all_relay_results.push(rr);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("send task panicked: {e}");
            }
        }
    }

    client.disconnect().await;

    if sent.is_empty() {
        return Err(AmError::Network("failed to send to all recipients".into()));
    }

    Ok(SentMessage {
        id: id.to_hex(),
        kind,
        to: sent,
        content,
        protocol,
        reply_to: reply_to.map(|e| e.to_hex()),
        failed,
        relays: all_relay_results,
    })
}

/// Decode an incoming event into a message, if it's one we can read.
///
/// Gift wraps (kind 1059) are unwrapped per NIP-59; only kind-14 (text) and
/// kind-15 (file) rumors are returned. Kind-4 DMs are only decoded when
/// `nip04` is set; otherwise they're ignored.
async fn decode(
    client: &Client,
    event: &Event,
    our_pk: &PublicKey,
    nip04: bool,
) -> Option<ReceivedMessage> {
    if event.kind == Kind::GiftWrap {
        let unwrapped = client
            .unwrap_gift_wrap(event)
            .await
            .map_err(|e| tracing::debug!("unwrap failed for {}: {e}", event.id))
            .ok()?;
        let rumor = &unwrapped.rumor;
        let kind = rumor.kind.as_u16();
        if kind != KIND_CHAT && kind != KIND_FILE {
            tracing::debug!("dropping rumor of kind {kind} in gift wrap {}", event.id);
            return None;
        }
        // Recompute rather than trust the id field the sender serialized.
        let id = EventId::new(
            &rumor.pubkey,
            &rumor.created_at,
            &rumor.kind,
            &rumor.tags,
            &rumor.content,
        );
        let tags = tags_to_vec(&rumor.tags);
        let participants = extract_participants(&unwrapped, our_pk);
        return Some(ReceivedMessage {
            id: id.to_hex(),
            from: unwrapped.sender.to_bech32().unwrap_or_default(),
            content: rumor.content.clone(),
            timestamp: rumor.created_at.as_secs(),
            participants,
            protocol: Protocol::Nip17,
            kind,
            reply_to: reply_to_from_tags(&tags),
            file: (kind == KIND_FILE).then(|| FileMeta::from_rumor(&rumor.content, &tags)),
            tags,
        });
    }

    if nip04 && event.kind == Kind::EncryptedDirectMessage {
        let signer = client.signer().await.ok()?;
        let content = signer
            .nip04_decrypt(&event.pubkey, &event.content)
            .await
            .map_err(|e| tracing::debug!("nip04 decrypt failed for {}: {e}", event.id))
            .ok()?;
        let mut participants = vec![
            event.pubkey.to_bech32().unwrap_or_default(),
            our_pk.to_bech32().unwrap_or_default(),
        ];
        participants.sort();
        participants.dedup();
        let tags = tags_to_vec(&event.tags);
        return Some(ReceivedMessage {
            id: event.id.to_hex(),
            from: event.pubkey.to_bech32().unwrap_or_default(),
            content,
            timestamp: event.created_at.as_secs(),
            participants,
            protocol: Protocol::Nip04,
            kind: event.kind.as_u16(),
            reply_to: reply_to_from_tags(&tags),
            file: None,
            tags,
        });
    }

    None
}

/// Report status and NIP-42 AUTH changes of one relay as stream event lines.
///
/// A status line is emitted only when the status differs from the last one
/// reported (starting from the status at spawn time, which the `ready` line
/// already reflects), so a relay stuck retrying yields one `disconnected`.
fn watch_relay(url: String, relay: &Relay) {
    use tokio::sync::broadcast::error::RecvError;

    let mut rx = relay.notifications();
    let mut last = relay_status_name(relay.status());
    tokio::spawn(async move {
        loop {
            let line = match rx.recv().await {
                Ok(RelayNotification::RelayStatus { status }) => match relay_status_name(status) {
                    Some(s) if last != Some(s) => {
                        last = Some(s);
                        Some(StreamEvent::Relay {
                            relay: url.clone(),
                            status: s.into(),
                        })
                    }
                    _ => None,
                },
                Ok(RelayNotification::Authenticated) => Some(StreamEvent::Auth {
                    relay: url.clone(),
                    status: "ok".into(),
                }),
                Ok(RelayNotification::AuthenticationFailed) => Some(StreamEvent::Auth {
                    relay: url.clone(),
                    status: "failed".into(),
                }),
                Ok(RelayNotification::Shutdown) | Err(RecvError::Closed) => break,
                Ok(_) | Err(RecvError::Lagged(_)) => None,
            };
            if let Some(line) = line {
                emit(&line);
            }
        }
    });
}

/// Sorted URLs of the relays that are currently connected.
fn connected_urls(relays: &std::collections::HashMap<RelayUrl, Relay>) -> Vec<String> {
    let mut urls: Vec<String> = relays
        .iter()
        .filter(|(_, r)| r.status() == RelayStatus::Connected)
        .map(|(url, _)| url.to_string())
        .collect();
    urls.sort();
    urls
}

#[allow(clippy::too_many_arguments)]
pub async fn listen(
    identity: Option<&str>,
    since: Option<u64>,
    limit: Option<usize>,
    once: bool,
    timeout_secs: u64,
    nip04: bool,
    passphrase: Option<&str>,
    _verbosity: u8,
) -> AmResult<Vec<ReceivedMessage>> {
    let keys = load_keys(identity.unwrap_or("default"), passphrase)?;
    let config = load_config()?;

    if config.relays.is_empty() {
        return Err(AmError::Network(
            "no relays configured; run `am relay add <url>`".into(),
        ));
    }

    let client = client::connect(keys, &config.relays).await?;

    let our_pk = client
        .public_key()
        .await
        .map_err(|e| AmError::Crypto(e.to_string()))?;

    let filter = {
        let mut kinds = vec![Kind::GiftWrap];
        if nip04 {
            kinds.push(Kind::EncryptedDirectMessage);
        }
        let mut f = Filter::new().kinds(kinds).pubkey(our_pk);
        if let Some(ts) = since {
            // Widen for NIP-59's backdated outer timestamp; the real cut is
            // applied to the decoded timestamp below.
            f = f.since(Timestamp::from(relay_since(ts)));
        }
        // A relay applies `limit` to the newest *outer* timestamps, which
        // NIP-59 randomizes, so for --once we fetch the window and keep the
        // newest by message timestamp ourselves. Streaming can only bound
        // the stored backlog, so it still passes the limit to relays.
        if let Some(l) = limit.filter(|_| !once) {
            f = f.limit(l);
        }
        f
    };

    let relays = client.relays().await;

    if once {
        // Empty output must mean "no messages", never "couldn't ask anyone".
        if connected_urls(&relays).is_empty() {
            client.disconnect().await;
            return Err(AmError::Network(
                "no relays connected; nothing to listen on".into(),
            ));
        }
        let events = client
            .fetch_events(filter, std::time::Duration::from_secs(timeout_secs))
            .await
            .map_err(|e| AmError::Network(e.to_string()))?;

        let mut seen = HashSet::new();
        let mut messages = Vec::new();
        for event in events.into_iter() {
            if let Some(msg) = decode(&client, &event, &our_pk, nip04).await {
                if !passes_since(msg.timestamp, since) {
                    tracing::debug!("dropping {} (before --since)", msg.id);
                } else if seen.insert(msg.id.clone()) {
                    messages.push(msg);
                }
            }
        }
        messages.sort_by_key(|m| m.timestamp);
        if let Some(l) = limit {
            let excess = messages.len().saturating_sub(l);
            messages.drain(..excess);
        }
        client.disconnect().await;
        Ok(messages)
    } else {
        // Watch every relay before reporting ready, so no transition after
        // the ready line is missed.
        for (url, relay) in &relays {
            watch_relay(url.to_string(), relay);
        }

        let connected = connected_urls(&relays);
        if connected.is_empty() {
            emit(&StreamEvent::Ready { relays: vec![] });
            client.disconnect().await;
            return Err(AmError::Network(
                "no relays connected; nothing to listen on".into(),
            ));
        }

        client
            .subscribe(filter, None)
            .await
            .map_err(|e| AmError::Network(e.to_string()))?;

        emit(&StreamEvent::Ready { relays: connected });

        client
            .handle_notifications(|notification| async {
                if let RelayPoolNotification::Event { event, .. } = notification {
                    if let Some(msg) = decode(&client, &event, &our_pk, nip04).await {
                        if !passes_since(msg.timestamp, since) {
                            tracing::debug!("dropping {} (before --since)", msg.id);
                        } else if let Ok(json) = serde_json::to_string(&msg) {
                            println!("{json}");
                        }
                    }
                }
                Ok(false)
            })
            .await
            .map_err(|e| AmError::Network(e.to_string()))?;

        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(n: u8) -> PublicKey {
        Keys::new(SecretKey::from_slice(&[n; 32]).unwrap()).public_key()
    }

    fn sample_file() -> FileMeta {
        FileMeta {
            url: "https://files.example/abc".into(),
            file_type: Some("image/png".into()),
            encryption_algorithm: Some("aes-gcm".into()),
            decryption_key: Some("k3y".into()),
            decryption_nonce: Some("n0nce".into()),
            hash: Some("a".repeat(64)),
            original_hash: Some("b".repeat(64)),
            size: Some(1234),
            dim: Some("640x480".into()),
            blurhash: None,
            thumbhash: None,
            thumb: Some("https://files.example/thumb".into()),
            fallback: vec!["https://mirror1/abc".into(), "https://mirror2/abc".into()],
        }
    }

    fn p_tags(rumor: &UnsignedEvent) -> Vec<PublicKey> {
        rumor.tags.public_keys().copied().collect()
    }

    #[test]
    fn one_to_one_rumor_has_only_recipient_p_tag() {
        let rumor = build_rumor(pk(1), &[pk(2)], "hi", None, None);
        assert_eq!(rumor.kind.as_u16(), KIND_CHAT);
        assert_eq!(p_tags(&rumor), vec![pk(2)]);
        assert_eq!(rumor.tags.len(), 1);
        rumor.verify_id().unwrap();
    }

    #[test]
    fn group_rumor_p_tag_set_matches_previous_per_recipient_rumors() {
        let (me, a, b) = (pk(1), pk(2), pk(3));
        let rumor = build_rumor(me, &[a, b], "hi all", None, None);
        let mut got = p_tags(&rumor);
        got.sort();
        // Old per-recipient rumor for `a`: [a, b, me]; for `b`: [b, a, me].
        let mut expected = vec![a, b, me];
        expected.sort();
        assert_eq!(got, expected);
    }

    #[test]
    fn group_rumor_does_not_duplicate_sender() {
        let rumor = build_rumor(pk(1), &[pk(2), pk(1)], "x", None, None);
        assert_eq!(p_tags(&rumor).len(), 2);
    }

    #[test]
    fn rumor_id_survives_clone_for_each_recipient() {
        let rumor = build_rumor(pk(1), &[pk(2), pk(3)], "x", None, None);
        let id = rumor.id.unwrap();
        let recomputed = EventId::new(
            &rumor.pubkey,
            &rumor.created_at,
            &rumor.kind,
            &rumor.tags,
            &rumor.content,
        );
        assert_eq!(id, recomputed);
        assert_eq!(rumor.clone().id(), id);
    }

    #[test]
    fn reply_tag_shapes() {
        let parent = EventId::from_hex(&"c".repeat(64)).unwrap();
        let text = build_rumor(pk(1), &[pk(2)], "re", Some(&parent), None);
        let tags = tags_to_vec(&text.tags);
        assert!(tags.contains(&vec!["e".to_string(), "c".repeat(64)]));

        let file = sample_file();
        let f = build_rumor(pk(1), &[pk(2)], &file.url, Some(&parent), Some(&file));
        let tags = tags_to_vec(&f.tags);
        assert!(tags.contains(&vec![
            "e".to_string(),
            "c".repeat(64),
            String::new(),
            "reply".to_string()
        ]));
        assert_eq!(reply_to_from_tags(&tags), Some("c".repeat(64)));
    }

    #[test]
    fn reply_to_prefers_reply_marker_and_skips_invalid() {
        let id = |c: char| c.to_string().repeat(64);
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let tags = vec![
            s(&["e", "not-hex"]),
            s(&["e", &id('1'), "", "root"]),
            s(&["e", &id('2'), "", "reply"]),
        ];
        assert_eq!(reply_to_from_tags(&tags), Some(id('2')));

        let tags = vec![s(&["e", &id('1'), "", "root"]), s(&["e", &id('3')])];
        assert_eq!(reply_to_from_tags(&tags), Some(id('3')));

        let tags = vec![s(&["p", &id('4')]), s(&["e", &id('5'), "", "mention"])];
        assert_eq!(reply_to_from_tags(&tags), None);

        let upper = id('A');
        assert_eq!(reply_to_from_tags(&[s(&["e", &upper])]), Some(id('a')));
    }

    #[test]
    fn file_rumor_round_trips_metadata() {
        let file = sample_file();
        let rumor = build_rumor(pk(1), &[pk(2)], &file.url, None, Some(&file));
        assert_eq!(rumor.kind.as_u16(), KIND_FILE);
        assert_eq!(rumor.content, file.url);
        let tags = tags_to_vec(&rumor.tags);
        assert!(tags.contains(&vec!["x".to_string(), "a".repeat(64)]));
        assert!(tags.contains(&vec!["size".to_string(), "1234".to_string()]));
        assert_eq!(FileMeta::from_rumor(&rumor.content, &tags), file);
    }

    #[test]
    fn file_from_rumor_tolerates_missing_tags() {
        let meta = FileMeta::from_rumor("https://u", &[vec!["size".into(), "nan".into()]]);
        assert_eq!(meta.url, "https://u");
        assert_eq!(meta.size, None);
        assert_eq!(meta.file_type, None);
        assert!(meta.fallback.is_empty());
    }

    #[test]
    fn file_validate() {
        sample_file().validate().unwrap();

        let mut f = sample_file();
        f.decryption_nonce = None;
        let err = f.validate().unwrap_err();
        assert!(err.to_string().contains("--decryption-nonce"));
        assert_eq!(err.exit_code(), 2);

        let mut f = sample_file();
        f.hash = Some("xyz".into());
        assert!(
            f.validate()
                .unwrap_err()
                .to_string()
                .contains("--file-hash")
        );

        let mut f = sample_file();
        f.original_hash = Some("1".repeat(63));
        assert!(f.validate().is_err());

        let mut f = sample_file();
        f.url = "  ".into();
        assert!(f.validate().is_err());
    }

    #[test]
    fn since_window() {
        assert_eq!(relay_since(1_000_000), 1_000_000 - GIFT_WRAP_LOOKBACK_SECS);
        assert_eq!(relay_since(5), 0);
        const {
            assert!(
                GIFT_WRAP_LOOKBACK_SECS > nostr_sdk::nips::nip59::RANGE_RANDOM_TIMESTAMP_TWEAK.end
            )
        };
        assert!(passes_since(100, None));
        assert!(passes_since(100, Some(100)));
        assert!(passes_since(101, Some(100)));
        assert!(!passes_since(99, Some(100)));
    }

    #[test]
    fn stream_event_lines_carry_event_key() {
        let ready = serde_json::to_value(StreamEvent::Ready {
            relays: vec!["wss://nos.lol".into()],
        })
        .unwrap();
        assert_eq!(
            ready,
            serde_json::json!({"event": "ready", "relays": ["wss://nos.lol"]})
        );
        let relay = serde_json::to_value(StreamEvent::Relay {
            relay: "wss://a".into(),
            status: "disconnected".into(),
        })
        .unwrap();
        assert_eq!(
            relay,
            serde_json::json!({"event": "relay", "relay": "wss://a", "status": "disconnected"})
        );
        let auth = serde_json::to_value(StreamEvent::Auth {
            relay: "wss://a".into(),
            status: "ok".into(),
        })
        .unwrap();
        assert_eq!(auth["event"], "auth");
        assert_eq!(relay_status_name(RelayStatus::Pending), None);
        assert_eq!(relay_status_name(RelayStatus::Connected), Some("connected"));
    }

    #[test]
    fn received_message_shape() {
        let msg = ReceivedMessage {
            id: "ab".into(),
            from: "npub1x".into(),
            content: "hi".into(),
            timestamp: 1,
            participants: vec![],
            protocol: Protocol::Nip17,
            kind: KIND_CHAT,
            tags: vec![vec!["p".into(), "00".into()]],
            reply_to: None,
            file: None,
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert!(v.get("event").is_none());
        assert!(v.get("file").is_none());
        assert!(v["reply_to"].is_null());
        assert_eq!(v["tags"], serde_json::json!([["p", "00"]]));
    }
}
