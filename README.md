# am — Agent Messenger

E2E encrypted CLI messenger for agent-to-agent communication over Nostr (NIP-17).

Built in Rust. JSON output by default. Zero interactive prompts. Designed for composability via stdin/stdout.

## Quick Start

```bash
# Build
cargo build --release

# Generate an identity
am identity generate --name alice

# Add a relay
am relay add wss://relay.damus.io

# Send a message
am send --to <npub> "hello from alice"

# Or pipe from stdin
echo "hello" | am send --to <npub>

# Listen for messages (streaming NDJSON)
am listen

# Listen once and exit
am listen --once --since 1700000000

# Reply to a message (use the `id` from listen output)
am send --to <npub> --reply-to <id> "replying to you"
```

## CLI Reference

```
am [--format json|text] [--identity <name>] [--passphrase <pass>] [--home <dir>] [-q] [-v...] <command>

Global flags:
  --passphrase <pass>    Passphrase for encrypted keys (or set AM_PASSPHRASE env var)
  --home <dir>           Keep all state (config.toml, identities/) under <dir> (or set AM_HOME)

am identity generate [--name <name>]        # Create keypair (encrypts if --passphrase given)
am identity show [--secret] [--name <name>] # Show pubkey (nsec with --secret)
am identity import <nsec> [--name <name>]   # Import existing key
am identity list                            # List all identities
am identity encrypt --name <name>           # Encrypt key at rest (requires --passphrase)
am identity decrypt --name <name>           # Decrypt key to plaintext (requires --passphrase)

am send --to <npub> [--to <npub2> ...] [--reply-protocol nip17|nip04] [--reply-to <id>] [message]  # Send encrypted DM (multi-recipient supported)
am send --to <npub> --file-url <url> --file-type <mime> --decryption-key <k> \
        --decryption-nonce <n> --file-hash <sha256> [--encryption-algorithm aes-gcm] \
        [--file-original-hash <sha256>] [--file-size <bytes>] [--file-dim <WxH>] \
        [--file-blurhash <h>] [--file-thumbhash <h>] [--file-thumb <url>] [--file-fallback <url>...]
                                            # Send a NIP-17 kind-15 file message (NIP-17 only)
am listen [--since <ts>] [--limit <n>] [--once] [--nip04]  # Receive messages (NDJSON stream)

am profile set [--name <n>] [--about <a>] [--picture <url>] [--website <url>]  # Publish profile (NIP-01 kind:0)

am relay add <url> [--no-publish]           # Add relay, then publish DM relay list (kind 10050)
am relay remove <url> [--no-publish]        # Remove relay, then publish DM relay list
am relay publish                            # Publish configured relays as DM relay list (NIP-17 kind 10050)
am relay list                               # Show relays

am config show                              # Dump config
am config set <key> <value>                 # Set config value
```

## Output

JSON by default (for agents). Use `--format text` for human-readable output.

Exit codes: 0=success, 1=general, 2=args (bad user input, e.g. malformed npub/nsec), 3=network, 4=crypto (stored key / passphrase), 5=config.

## Data location

By default config lives at `$XDG_CONFIG_HOME/am/config.toml` and identities at `$XDG_DATA_HOME/am/identities/`. Set `AM_HOME=<dir>` (or pass the global `--home <dir>` flag) to keep everything in one directory instead: `<dir>/config.toml` and `<dir>/identities/`. `am-ingest` and `am-agent` honor `AM_HOME` too (`<dir>/messages.db`, `<dir>/am-agent.toml`). This is the easy way to run isolated agents side by side:

```bash
AM_HOME=~/agents/alice am identity generate
am --home ~/agents/bob identity generate
```

## Agent Harness

For autonomous agents that need to read and respond to messages, `am` provides a two-process harness:

```bash
# Terminal 1: Ingest messages into SQLite
am-ingest                          # Runs am listen internally, writes to $XDG_DATA_HOME/am/messages.db ($AM_HOME/messages.db if set)
am-ingest --db ~/msgs.db           # Custom db path
am-ingest --identity work          # Use specific am identity

# Terminal 2: Process messages with an agent
am-agent --once                    # Process pending messages and exit
am-agent                           # Poll loop (default: every 30s)
am-agent --config agent.toml       # Custom config file
am-agent --interval 60             # Custom poll interval
```

**How it works:**
1. `am-ingest` runs `am listen` as a subprocess, parses NDJSON (skipping `event` status lines), and writes messages into SQLite with conversation threading; each row also stores the message `id` as `rumor_id` (a nullable column added automatically to existing databases)
2. `am-agent` polls SQLite for unprocessed messages, groups by conversation, invokes a configurable agent CLI with strict context isolation, sends replies via `am send`

**Configuration** (`$XDG_CONFIG_HOME/am/am-agent.toml`, or `$AM_HOME/am-agent.toml` if set):
```toml
[agent]
command = "claude"           # Any CLI that accepts a prompt
args = ["-p", "{prompt}"]    # {prompt} replaced with assembled context
# stdin = true               # Pipe prompt via stdin instead

[general]
interval = 30                # Seconds between poll cycles
identity = "default"         # am identity for sending replies
system_prompt = "agent.md"   # Path to system prompt file
```

## Project Structure

```
agent-messenger/
├── Cargo.toml                     # Workspace
├── crates/
│   ├── am-core/                   # Library: identity, messaging, relay, config
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── client.rs          # Shared relay client, retry logic, RelayResult types
│   │       ├── identity.rs
│   │       ├── message.rs
│   │       ├── profile.rs
│   │       ├── relay.rs
│   │       ├── config.rs
│   │       ├── output.rs
│   │       └── error.rs
│   ├── am-cli/                    # Binary: thin CLI shell
│   │   ├── src/
│   │   │   ├── main.rs
│   │   │   └── commands/
│   │   │       ├── mod.rs
│   │   │       ├── identity.rs
│   │   │       ├── send.rs
│   │   │       ├── listen.rs
│   │   │       ├── profile.rs
│   │   │       ├── relay.rs
│   │   │       └── config.rs
│   │   └── tests/                 # Integration tests
│   │       ├── identity.rs
│   │       ├── relay.rs
│   │       └── messaging.rs
│   ├── am-ingest/                 # Message ingestion daemon
│   │   └── src/main.rs
│   └── am-agent/                  # Agent orchestrator
│       └── src/main.rs
```

## Development

```bash
make build        # Compile release binary (target/release/am)
make clean        # Remove release binary
make clean-all    # Full cargo clean

make test-unit    # Run unit + integration tests (no relay needed)
make test-e2e     # End-to-end test against a live relay (builds first)
make test         # Both

make clippy       # cargo clippy --all-targets
make fmt          # cargo fmt
make fmt-check    # cargo fmt --check

```

The e2e test (`scripts/e2e.sh`) generates three ephemeral identities (one passphrase-encrypted), sends messages through a live relay, and validates all features: profile publishing, 1:1 DMs, group messaging, and encrypt/decrypt round-trips.

## Listen output

Each message is one JSON line with a stable `id` (the NIP-17 rumor id, or the NIP-04 event id), `from`, `content`, `timestamp`, `participants`, `protocol`, `kind` (14 text, 15 file, 4 NIP-04), `tags`, `reply_to` (the parent `id`, or `null`) and, for kind 15, a `file` object. `am send` prints the same `id`, identical for every recipient of a group message. Only kind-14 and kind-15 rumors are emitted; other rumor kinds (reactions, etc.) are dropped.

`--since` filters on the message's own timestamp (inclusive), not the gift wrap's, which NIP-59 backdates by up to two days; `am` asks relays for the extra two days and drops older messages itself. To resume after a restart, pass the last `timestamp` you processed and dedup the boundary by `id`. `--limit` keeps the newest N messages by that timestamp in `--once` mode.

Streaming `am listen` also writes status lines, which always carry an `event` key (messages never do):

```json
{"event":"ready","relays":["wss://nos.lol","wss://relay.primal.net"]}
{"event":"relay","relay":"wss://nos.lol","status":"disconnected"}
{"event":"auth","relay":"wss://auth.nostr1.com","status":"ok"}
```

`ready` is printed once the subscription is open. If no relay connected, it is printed with an empty `relays` list and `am listen` exits with code 3 (network error), so supervisors such as `am-ingest` restart it; `am listen --once` exits 3 in the same case, so empty output always means "no messages". `relay` lines report status changes (`connected`, `disconnected`, `terminated`, `banned`, `sleeping`) and are de-duplicated per relay. `auth` lines report NIP-42 authentication (`ok`/`failed`); `am` answers AUTH challenges automatically.

## Protocol

Uses NIP-17 (Private Direct Messages) by default. Legacy NIP-04 (kind 4) is supported for interop only and is off by default: `am listen --nip04` receives it, `am send --reply-protocol nip04` answers in it (text only; file messages are NIP-17 only). NIP-04 exposes sender, recipient and timing to relays.

`am relay add`/`remove` publish the configured relays as the identity's NIP-17 DM relay list (kind 10050), which clients like Damus, 0xchat and Amethyst use to decide where to deliver gift wraps. Pass `--no-publish` to skip, or run `am relay publish` explicitly. Publish failures are reported in the output (`relay_list_error`) but do not fail the relay change.

- Identity: secp256k1 keypair stored as nsec (or NIP-49 ncryptsec) with 0600 permissions
- Transport: Nostr relay mesh
- Encryption: NIP-44 (via NIP-59 gift wrapping)

## License

GPL-3.0-or-later — same license as [nostr-commander-rs](https://github.com/8go/nostr-commander-rs).
