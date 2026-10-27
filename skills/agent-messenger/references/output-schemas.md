# am Output Schemas

All `am` commands output JSON to stdout by default. Stderr receives error messages. This document defines exact output structures for every command.

## identity generate / identity import

```json
{
  "name": "string",   // identity name
  "npub": "string"    // bech32 public key (npub1...)
}
```

## identity show

Without `--secret`:

```json
{
  "name": "string",
  "npub": "string",
  "nsec": null
}
```

With `--secret`:

```json
{
  "name": "string",
  "npub": "string",
  "nsec": "string"    // bech32 secret key (nsec1...) — handle with care, never log
}
```

## identity list

Array of identity summaries (nsec is never included):

```json
[
  {"name": "default", "npub": "npub1..."},
  {"name": "private", "npub": "npub1..."}
]
```

Empty array if no identities exist:

```json
[]
```

## relay add

After saving the relay, `relay add` publishes the DM relay list (kind 10050) for the selected identity and reports the result under `relay_list`:

```json
{"added": "wss://nos.lol", "relay_list": {"npub": "npub1...", "event_id": "note1...", "relays": ["wss://nos.lol"], "results": [{"relay": "wss://nos.lol", "status": "ok"}]}}
```

If publishing fails, the relay is still added (exit 0) and the reason is in `relay_list_error`:

```json
{"added": "wss://nos.lol", "relay_list_error": "config error: identity 'default' not found"}
```

With `--no-publish`, only `{"added": "wss://nos.lol"}` is printed.

## relay remove

Same shape as `relay add`, with `removed` in place of `added`:

```json
{"removed": "wss://relay.damus.io", "relay_list": {...}}
```

## relay publish

Publishes the configured relays as the identity's NIP-17 DM relay list (kind 10050). Fails (exit 3) if no relay accepted it.

```json
{"npub": "npub1...", "event_id": "note1...", "relays": ["wss://nos.lol", "wss://relay.primal.net"], "results": [{"relay": "wss://nos.lol", "status": "ok"}]}
```

## relay list

```json
[
  {"url": "wss://relay.damus.io"},
  {"url": "wss://nos.lol"}
]
```

Empty array if no relays configured:

```json
[]
```

## send

Single recipient or multi-recipient (group message):

```json
{
  "id": "hex",                           // message id: NIP-17 rumor id / NIP-04 event id; same for every recipient
  "kind": 14,                            // 14 text, 15 file (--file-url), 4 NIP-04
  "to": ["npub1...", "npub2..."],        // recipients (array, always)
  "content": "string",                   // message content (the file URL for kind 15)
  "protocol": "nip17",                   // "nip17" (default) or "nip04" (--reply-protocol nip04)
  "reply_to": "hex",                     // only present with --reply-to
  "failed": [],                          // failed recipients (empty if all sent)
  "relays": [                            // per-relay delivery status
    {
      "relay": "wss://relay.damus.io",
      "status": "ok"
    },
    {
      "relay": "wss://inbox.nostr.wine",
      "status": "failed"
    }
  ]
}
```

With `-v` flag, relay array includes error details and attempt counts:

```json
{
  "relays": [
    {
      "relay": "wss://relay.damus.io",
      "status": "ok",
      "attempts": 1
    },
    {
      "relay": "wss://inbox.nostr.wine",
      "status": "failed",
      "error": "HTTP 403 Forbidden",
      "attempts": 3
    }
  ]
}
```

## listen

Each received message is a single JSON object. Both `--once` (batch) and streaming output use NDJSON — one JSON object per line, no array wrapper.

```json
{
  "id": "hex",                           // stable message id: NIP-17 rumor id / NIP-04 event id
  "from": "npub1...",                    // sender bech32 public key
  "content": "string",                   // decrypted message content (the file URL for kind 15)
  "timestamp": 1700000000,               // message's own Unix timestamp (seconds; the rumor's for NIP-17)
  "participants": ["npub1...", "npub1..."], // all conversation participants (sorted, deduplicated)
  "protocol": "nip17",                   // "nip17" or "nip04" (only with --nip04)
  "kind": 14,                            // 14 text, 15 file (NIP-17); 4 (NIP-04)
  "tags": [["p", "hex"], ["e", "hex"]],  // the rumor's tags (the event's tags for NIP-04), as string arrays
  "reply_to": "hex",                     // id of the parent message from the `e` tag, or null
  "file": { ... }                        // kind 15 only, see below; absent otherwise
}
```

- **`id`** is the same for every participant of a group message and equals the `id` that `am send` printed. The same message can arrive more than once (several relays, a resume overlap): dedup on `id`.
- **`kind`**: only kind-14 (text) and kind-15 (file) NIP-17 rumors are emitted. Other rumor kinds (reactions, etc.) are dropped, so nothing that isn't a message shows up looking like one.
- **`reply_to`**: taken from the `e` tag (one marked `reply` wins; `root`/`mention` markers are ignored). Answer a message with `am send --reply-to <its id>`, which adds `["e", <id>]` (kind 14 and NIP-04) or `["e", <id>, "", "reply"]` (kind 15), per NIP-17.
- The `participants` array contains all npubs in the conversation — sender, recipient(s), and self — derived from the NIP-17 rumor p-tags. For 1:1 DMs it has 2 entries; for group messages, 3+. Use it to derive a stable conversation ID (e.g., SHA-256 of sorted participants joined by comma).

### File messages (kind 15)

`content` is the URL of the encrypted file. `file` repeats it and adds the parsed NIP-17 tags; any tag the sender omitted is absent. `am` never downloads or decrypts the file.

```json
"file": {
  "url": "https://...",                  // = content
  "file_type": "image/jpeg",             // file-type: MIME type before encryption
  "encryption_algorithm": "aes-gcm",     // encryption-algorithm
  "decryption_key": "string",            // decryption-key
  "decryption_nonce": "string",          // decryption-nonce
  "hash": "hex",                         // x: SHA-256 of the encrypted file
  "original_hash": "hex",                // ox: SHA-256 before encryption (optional)
  "size": 12345,                         // size in bytes (optional)
  "dim": "1920x1080",                    // dim (optional)
  "blurhash": "string",                  // optional
  "thumbhash": "string",                 // optional
  "thumb": "https://...",                // thumbnail URL, same key/nonce (optional)
  "fallback": ["https://..."]            // fallback URLs (optional; absent when empty)
}
```

Send one with `am send --to <npub> --file-url <url> --file-type <mime> --decryption-key <k> --decryption-nonce <n> --file-hash <sha256>` (plus the optional `--encryption-algorithm` [default `aes-gcm`], `--file-original-hash`, `--file-size`, `--file-dim`, `--file-blurhash`, `--file-thumbhash`, `--file-thumb`, `--file-fallback`). You upload and encrypt the file yourself. File args can't be combined with a text message or `--reply-protocol nip04` (exit 2).

### `--since` and resuming

`--since <ts>` keeps messages whose own `timestamp` is `>= ts`. NIP-59 backdates the outer gift wrap's timestamp by up to two days, so `am` asks relays for two extra days and filters on the decoded timestamp itself. To resume after a restart, pass the last `timestamp` you processed and drop any `id` you have already seen (messages sharing that second are delivered again). `--limit <n>` keeps the newest `n` messages by `timestamp` in `--once` mode; in streaming mode it only caps the stored backlog relays return.

### Status lines (streaming only)

Streaming `am listen` interleaves status lines with messages. They always have an `event` key; messages never do, so skip any line with `event` if you only want messages.

```json
{"event":"ready","relays":["wss://nos.lol","wss://relay.primal.net"]}   // once, after the subscription is sent
{"event":"relay","relay":"wss://nos.lol","status":"disconnected"}       // a relay's status changed
{"event":"auth","relay":"wss://auth.nostr1.com","status":"ok"}          // NIP-42 AUTH result: "ok" or "failed"
```

- **`ready`** lists the relays connected when the subscription was opened. If that list is empty, `am listen` exits with code 3 right after printing it (as does `--once`, without the line), so a supervisor restarts it with backoff instead of it idling with no relays.
- **`relay`** `status` is one of `connected`, `disconnected`, `terminated`, `banned`, `sleeping`. Lines are only printed on a change, so a relay that keeps failing to reconnect produces one `disconnected`. `am` reconnects and resubscribes automatically.
- **`auth`** reports NIP-42 authentication. `am` answers AUTH challenges automatically; `failed` means the relay rejected it (often a paid or allow-listed relay).

### Parsing streaming output

```bash
# Process each message as it arrives, skipping status lines
am listen | while IFS= read -r line; do
  echo "$line" | jq -r 'select(has("event") | not) | "[\(.timestamp)] from \(.from): \(.content)"'
done

# Collect batch into a JSON array (--once never prints status lines)
messages=$(am listen --once | jq -s '.')
count=$(echo "$messages" | jq 'length')
```

### Distinguishing batch vs. streaming

Both use the same per-line JSON format for messages. The difference is termination and status lines: `--once` exits after fetching and prints only messages; streaming blocks indefinitely and also prints `event` lines. Use `--once` in scripts; use streaming for daemon processes.

## config show

```json
{
  "default_identity": "default",      // null if not set
  "relays": ["wss://relay.damus.io"],
  "format": null                       // null if not set; "json" or "text" if configured
}
```

## config set

No stdout output. Exit code 0 on success, 2 on invalid key, 5 on write failure.

## profile set

```json
{
  "npub": "npub1...",
  "name": "string or null",
  "about": "string or null",
  "picture": "string or null",
  "website": "string or null",
  "event_id": "string",      // bech32 note ID
  "relays": [                // per-relay delivery status (v0.2.1+)
    {
      "relay": "wss://relay.damus.io",
      "status": "ok"
    }
  ]
}
```

With `-v` flag, relay array includes error details and attempt counts (same as `send` command).

## Error Output

Errors are written to stderr. Stdout is empty (or contains partial output if the error occurred mid-operation).

```
Error: <description>
```

Exit codes are the authoritative signal — do not parse error message text, as its format is not guaranteed stable across versions.

### Common error scenarios

| Exit code | Common causes |
|-----------|--------------|
| 2 | Malformed npub or nsec (user-supplied), unknown config key, bad `--reply-to` id, incomplete or conflicting file args |
| 3 | Relay unreachable, WebSocket failure, no relays configured, no relay connected (`listen`) |
| 4 | Corrupted stored nsec file, wrong or missing passphrase for an encrypted identity |
| 5 | Malformed config TOML, identity not found or already exists |

### Detecting no messages

`am listen --once` with no messages available exits 0 and produces no output (empty stdout). If no relay could be reached it exits 3 instead, so empty output with exit 0 really means no messages:

```bash
output=$(am listen --once --since "$SINCE")
if [ -z "$output" ]; then
  echo "No new messages"
else
  echo "$output" | while IFS= read -r msg; do
    # process msg
    :
  done
fi
```
