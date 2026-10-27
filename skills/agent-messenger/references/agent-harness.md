# Agent Harness: am-ingest + am-agent

Two companion daemons that turn `am` into an autonomous message-processing platform. They operate independently of each other and of `am` itself — communicating only through SQLite and the `am` CLI.

## Architecture

```
Nostr relays
    |
    v
am listen (subprocess)  -->  NDJSON
    |
    v
am-ingest  -->  SQLite (messages.db)
    |
    v
am-agent (polls SQLite)
    |
    +-- invokes agent CLI per conversation
    +-- sends reply via: am send --to <participants>
    +-- marks messages processed
```

## am-ingest — Ingestion Daemon

Spawns `am listen` as a subprocess, parses NDJSON output, and stores messages in SQLite with conversation threading and deduplication.

### CLI

```bash
am-ingest [--db <path>] [--identity <name>]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--db` | `$AM_HOME/messages.db` if set, else `$XDG_DATA_HOME/am/messages.db` | SQLite database path |
| `--identity` | `default` | Which am identity to listen as |

### Behavior

- Creates database with WAL mode on first run
- Derives `conversation_id` from SHA-256 of sorted participant npubs joined by comma (deterministic, stable)
- Derives `message_id` from SHA-256 of (sender + timestamp + content) for deduplication. This stays the dedup key: databases already hold these ids, and re-keying would re-ingest history as unprocessed
- Stores the `id` from `am listen` (NIP-17 rumor id / NIP-04 event id) in the nullable `rumor_id` column. Databases created before this column existed are migrated on startup (`ALTER TABLE ... ADD COLUMN`); old rows keep `rumor_id = NULL` unless the same message is seen again, which backfills it
- Skips `am listen` status lines (any line with an `event` key: `ready`, `relay`, `auth`) with a debug log; only lines that are neither messages nor status lines are logged as "malformed NDJSON line"
- Runs `am listen` without `--since`, so each restart re-reads what relays still hold; dedup by `message_id` makes that harmless
- Classifies conversations as `dm` (2 participants) or `group` (3+)
- On network error (exit code 3) — including `am listen` finding no connected relay at startup — reconnects with exponential backoff (1s → 2s → 4s → ... max 60s)
- On other errors: exits immediately

### SQLite Schema

**messages table:**

```sql
CREATE TABLE messages (
    id          INTEGER PRIMARY KEY,
    message_id  TEXT UNIQUE NOT NULL,    -- SHA-256(from + timestamp + content); the dedup key
    conv_id     TEXT NOT NULL,           -- SHA-256(sorted participants)
    sender      TEXT NOT NULL,           -- sender npub
    content     TEXT NOT NULL,           -- decrypted message (the file URL for kind-15 file messages)
    timestamp   INTEGER NOT NULL,        -- the message's own Unix timestamp
    processed   INTEGER NOT NULL DEFAULT 0, -- 0 = pending, 1 = processed by am-agent
    created_at  INTEGER NOT NULL DEFAULT (unixepoch()),
    rumor_id    TEXT                     -- `id` from am listen (NIP-17 rumor id); NULL for rows ingested before it existed
);
```

**conversations table:**

```sql
CREATE TABLE conversations (
    conv_id       TEXT PRIMARY KEY,
    participants  TEXT NOT NULL,          -- JSON array of npubs
    conv_type     TEXT NOT NULL,          -- "dm" or "group"
    last_message  INTEGER NOT NULL DEFAULT 0, -- Unix timestamp of latest message
    metadata      TEXT                    -- conversation summary (NULL until the first reply)
);
```

### Useful Queries

```bash
# Count unprocessed messages
sqlite3 messages.db "SELECT COUNT(*) FROM messages WHERE processed = 0;"

# View recent messages
sqlite3 -json messages.db "SELECT * FROM messages ORDER BY timestamp DESC LIMIT 10;"

# List conversations
sqlite3 -json messages.db "SELECT conv_id, conv_type, participants, last_message FROM conversations ORDER BY last_message DESC;"

# Check a specific conversation
sqlite3 -json messages.db "SELECT * FROM messages WHERE conv_id = '<id>' ORDER BY timestamp;"
```

## am-agent — Agent Orchestrator

Polls SQLite for unprocessed messages, groups them by conversation, invokes a configurable agent CLI with per-conversation context isolation, sends replies via `am send`, and maintains rolling summaries.

### CLI

```bash
am-agent [--config <path>] [--db <path>] [--interval <seconds>] [--once]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--config` | `$AM_HOME/am-agent.toml` if set, else `$XDG_CONFIG_HOME/am/am-agent.toml` | TOML config file |
| `--db` | `$AM_HOME/messages.db` if set, else `$XDG_DATA_HOME/am/messages.db` | Override database path |
| `--interval` | 30 | Poll interval in seconds |
| `--once` | false | Process pending messages and exit |

### Configuration

Full `am-agent.toml` reference:

```toml
[agent]
command = "llm"                      # Agent CLI executable
args = ["-s", "{prompt}"]            # Args; {prompt} replaced with assembled context
stdin = true                         # If true, pipe prompt via stdin (not arg replacement)
env = { "OLLAMA_HOST" = "localhost" } # Extra environment variables for agent process

[general]
db = ""                              # Database path (empty = $AM_HOME or default XDG location)
interval = 30                        # Poll interval in seconds
identity = "default"                 # am identity for sending replies
system_prompt = "persona.md"         # Path to system prompt file (optional)
```

### Prompt Assembly

For each conversation with pending messages, am-agent assembles a prompt:

```
[Contents of system_prompt file, if configured]

## Conversation context
[Previous summary from conversation metadata, if exists]

## New messages
[2024-01-15 10:30:00] npub1abc...: Hello, how are you?
[2024-01-15 10:31:00] npub1def...: I have a question about the project.

## Instructions
Respond to the new messages above. Then output a conversation summary
on a line starting with "SUMMARY:" that captures key context for future reference.
```

### SUMMARY Protocol

The agent CLI outputs a reply, optionally followed by a rolling summary:

```
Here's my response to the conversation.

SUMMARY: Alice asked about project status. I provided an update on the backend migration. Bob raised concerns about the timeline.
```

- Everything before `\nSUMMARY:` is the reply (sent to all participants)
- Everything after `SUMMARY:` is stored in conversation metadata for the next cycle
- If no `SUMMARY:` line, the full output is treated as the reply and no summary is stored
- Summaries roll forward — each cycle's summary replaces the previous one

### Conversation Isolation

Each conversation is processed in its own agent CLI invocation:

- No shared state between conversations
- Agent receives only messages from one conversation
- Previous context comes exclusively from the rolling summary
- A failure in one conversation does not affect others
- Messages are marked processed only after: agent succeeds AND reply is sent

### Agent CLI Examples

**With llm (Simon Willison) + Ollama:**

```toml
[agent]
command = "llm"
args = ["-m", "ollama/llama3", "-s", "{prompt}"]
stdin = true
```

**With aichat + Ollama backend:**

```toml
[agent]
command = "aichat"
args = ["--model", "ollama:llama3", "--prompt", "{prompt}"]
stdin = true
```

**With a custom script:**

```toml
[agent]
command = "python3"
args = ["my_agent.py"]
stdin = true
```

The custom script reads the assembled prompt from stdin and writes its reply to stdout.

## End-to-End Setup

Complete setup from scratch:

```bash
# 1. Install
cargo install agent-messenger

# 2. Create identity and add relays
am identity generate --name myagent
am relay add wss://relay.damus.io
am relay add wss://nos.lol

# 3. Share npub with peers
am identity show --name myagent | jq -r '.npub'

# 4. Create a persona file (optional)
cat > ~/.config/am/persona.md << 'EOF'
You are a helpful assistant that responds to messages concisely.
EOF

# 5. Configure the agent
cat > ~/.config/am/am-agent.toml << 'EOF'
[agent]
command = "llm"
args = ["-m", "ollama/llama3"]
stdin = true

[general]
identity = "myagent"
interval = 30
system_prompt = "~/.config/am/persona.md"
EOF

# 6. Start ingestion (Terminal 1)
am-ingest --identity myagent

# 7. Start agent (Terminal 2)
am-agent
```

## Troubleshooting

**No messages being ingested:**
- Verify `am listen --identity <name> --once` returns messages manually
- Check that relays are configured: `am relay list`
- Inspect database: `sqlite3 messages.db "SELECT COUNT(*) FROM messages;"`

**Agent not processing:**
- Check for unprocessed messages: `sqlite3 messages.db "SELECT COUNT(*) FROM messages WHERE processed = 0;"`
- Test agent CLI manually: `echo "Hello" | <your-command>`
- Run `am-agent --once` to see single-cycle output

**Reply not sending:**
- Verify identity can send: `am send --identity <name> --to <npub> "test"`
- Check relay connectivity: `am -v send --identity <name> --to <npub> "test"`
