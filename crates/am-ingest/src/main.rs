use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use clap::Parser;
use rusqlite::Connection;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "am-ingest", about = "Ingest NIP-17 messages into SQLite")]
struct Cli {
    /// Path to SQLite database
    #[arg(long)]
    db: Option<PathBuf>,

    /// am identity to use for listening
    #[arg(long, default_value = "default")]
    identity: String,
}

#[derive(Debug, Deserialize)]
struct ReceivedMessage {
    from: String,
    content: String,
    timestamp: u64,
    participants: Vec<String>,
    /// Rumor id (NIP-17) / event id (NIP-04). Absent from older `am` builds.
    #[serde(default)]
    id: Option<String>,
}

/// One stdout line of `am listen`.
#[derive(Debug)]
enum ListenLine {
    Message(ReceivedMessage),
    /// A status line (`{"event": "ready" | "relay" | "auth", ...}`), not a message
    Event(String),
}

fn parse_line(line: &str) -> serde_json::Result<ListenLine> {
    let value: serde_json::Value = serde_json::from_str(line)?;
    if let Some(event) = value.get("event") {
        let name = event.as_str().unwrap_or_default().to_string();
        return Ok(ListenLine::Event(name));
    }
    serde_json::from_value(value).map(ListenLine::Message)
}

/// `$AM_HOME` if set (matching the `am` CLI), else `<platform dir>/am`.
fn am_home() -> Option<PathBuf> {
    std::env::var_os("AM_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn default_db_path() -> PathBuf {
    if let Some(home) = am_home() {
        return home.join("messages.db");
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("am")
        .join("messages.db")
}

fn init_db(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS messages (
            id          INTEGER PRIMARY KEY,
            message_id  TEXT UNIQUE NOT NULL,
            conv_id     TEXT NOT NULL,
            sender      TEXT NOT NULL,
            content     TEXT NOT NULL,
            timestamp   INTEGER NOT NULL,
            processed   INTEGER NOT NULL DEFAULT 0,
            created_at  INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE INDEX IF NOT EXISTS idx_conv_unprocessed
            ON messages(conv_id, processed, timestamp);

        CREATE INDEX IF NOT EXISTS idx_unprocessed
            ON messages(processed, timestamp);

        CREATE TABLE IF NOT EXISTS conversations (
            conv_id       TEXT PRIMARY KEY,
            participants  TEXT NOT NULL,
            conv_type     TEXT NOT NULL,
            last_message  INTEGER NOT NULL DEFAULT 0,
            metadata      TEXT
        );",
    )?;
    migrate_rumor_id(conn)
}

/// Add the nullable `rumor_id` column to databases created before it existed.
/// `message_id` stays the dedup key: existing rows were keyed by it, and
/// re-keying would re-ingest history as unprocessed.
fn migrate_rumor_id(conn: &Connection) -> rusqlite::Result<()> {
    let has_column = conn
        .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name = 'rumor_id'")?
        .exists([])?;
    if !has_column {
        conn.execute_batch("ALTER TABLE messages ADD COLUMN rumor_id TEXT;")?;
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_rumor_id ON messages(rumor_id);")
}

fn derive_conv_id(participants: &[String]) -> String {
    let mut sorted = participants.to_vec();
    sorted.sort();
    sorted.dedup();
    let joined = sorted.join(",");
    let hash = Sha256::digest(joined.as_bytes());
    format!("{hash:x}")
}

fn derive_message_id(from: &str, timestamp: u64, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(from.as_bytes());
    hasher.update(timestamp.to_le_bytes());
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn insert_message(conn: &Connection, msg: &ReceivedMessage) -> rusqlite::Result<bool> {
    let conv_id = derive_conv_id(&msg.participants);
    let message_id = derive_message_id(&msg.from, msg.timestamp, &msg.content);
    let ts = msg.timestamp as i64;

    let inserted = conn.execute(
        "INSERT OR IGNORE INTO messages (message_id, conv_id, sender, content, timestamp, rumor_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![message_id, conv_id, msg.from, msg.content, ts, msg.id],
    )?;

    if inserted == 0 && msg.id.is_some() {
        // Backfill rows ingested before `am listen` reported ids.
        conn.execute(
            "UPDATE messages SET rumor_id = ?1 WHERE message_id = ?2 AND rumor_id IS NULL",
            rusqlite::params![msg.id, message_id],
        )?;
    }

    if inserted > 0 {
        let participants_json = serde_json::to_string(&msg.participants).unwrap_or_default();
        let conv_type = if msg.participants.len() <= 2 {
            "dm"
        } else {
            "group"
        };

        conn.execute(
            "INSERT INTO conversations (conv_id, participants, conv_type, last_message)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(conv_id) DO UPDATE SET last_message = MAX(last_message, excluded.last_message)",
            rusqlite::params![conv_id, participants_json, conv_type, ts],
        )?;
    }

    Ok(inserted > 0)
}

async fn run_ingest(db_path: &PathBuf, identity: &str) -> Result<(), Box<dyn std::error::Error>> {
    let db_dir = db_path.parent().unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(db_dir)?;

    let conn = Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    init_db(&conn)?;

    info!("database ready at {}", db_path.display());

    let max_backoff = Duration::from_secs(60);
    let mut backoff = Duration::from_secs(1);

    loop {
        info!("spawning am listen --identity {identity}");

        let mut child = Command::new("am")
            .args(["--identity", identity, "listen"])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdout = child.stdout.take().expect("stdout piped");
        let mut reader = BufReader::new(stdout).lines();
        let mut received_any = false;

        loop {
            tokio::select! {
                line = reader.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            if line.trim().is_empty() {
                                continue;
                            }
                            match parse_line(&line) {
                                Ok(ListenLine::Event(name)) => {
                                    tracing::debug!("am listen status line ({name}): {line}");
                                }
                                Ok(ListenLine::Message(msg)) => {
                                    received_any = true;
                                    match insert_message(&conn, &msg) {
                                        Ok(true) => {
                                            info!(
                                                "ingested message from {} in conv {}",
                                                &msg.from[..12.min(msg.from.len())],
                                                &derive_conv_id(&msg.participants)[..8]
                                            );
                                        }
                                        Ok(false) => {
                                            tracing::debug!("duplicate message, skipped");
                                        }
                                        Err(e) => {
                                            error!("sqlite insert error: {e}");
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!("malformed NDJSON line, skipping: {e}");
                                }
                            }
                        }
                        Ok(None) => {
                            info!("am listen stdout closed");
                            break;
                        }
                        Err(e) => {
                            error!("reading stdout: {e}");
                            break;
                        }
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    info!("received shutdown signal");
                    let _ = child.kill().await;
                    return Ok(());
                }
            }
        }

        let status = child.wait().await?;
        let code = status.code().unwrap_or(1);

        if code == 3 {
            // Reset backoff if we successfully received messages before disconnecting
            if received_any {
                backoff = Duration::from_secs(1);
            }
            warn!("am listen exited with code 3 (network error), retrying in {backoff:?}");
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(max_backoff);
        } else {
            error!("am listen exited with code {code}");
            return Err(format!("am listen exited with code {code}").into());
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(default_db_path);

    if let Err(e) = run_ingest(&db_path, &cli.identity).await {
        error!("{e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_conv_id_deterministic() {
        let p1 = vec!["npub1abc".to_string(), "npub1def".to_string()];
        let p2 = vec!["npub1def".to_string(), "npub1abc".to_string()];
        assert_eq!(derive_conv_id(&p1), derive_conv_id(&p2));
    }

    #[test]
    fn test_derive_conv_id_different_participants() {
        let p1 = vec!["npub1abc".to_string(), "npub1def".to_string()];
        let p2 = vec!["npub1abc".to_string(), "npub1ghi".to_string()];
        assert_ne!(derive_conv_id(&p1), derive_conv_id(&p2));
    }

    #[test]
    fn test_derive_conv_id_deduplicates() {
        let p1 = vec![
            "npub1abc".to_string(),
            "npub1abc".to_string(),
            "npub1def".to_string(),
        ];
        let p2 = vec!["npub1abc".to_string(), "npub1def".to_string()];
        assert_eq!(derive_conv_id(&p1), derive_conv_id(&p2));
    }

    #[test]
    fn test_derive_message_id_deterministic() {
        let id1 = derive_message_id("npub1abc", 1000, "hello");
        let id2 = derive_message_id("npub1abc", 1000, "hello");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_derive_message_id_different_content() {
        let id1 = derive_message_id("npub1abc", 1000, "hello");
        let id2 = derive_message_id("npub1abc", 1000, "world");
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_ndjson_parsing_valid() {
        let json = r#"{"from":"npub1abc","content":"hello","timestamp":1000,"participants":["npub1abc","npub1def"]}"#;
        let msg: ReceivedMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.from, "npub1abc");
        assert_eq!(msg.content, "hello");
        assert_eq!(msg.timestamp, 1000);
        assert_eq!(msg.participants, vec!["npub1abc", "npub1def"]);
    }

    #[test]
    fn test_ndjson_parsing_malformed() {
        let json = r#"{"bad": true}"#;
        assert!(serde_json::from_str::<ReceivedMessage>(json).is_err());
    }

    #[test]
    fn test_parse_line_skips_event_lines() {
        for line in [
            r#"{"event":"ready","relays":["wss://nos.lol"]}"#,
            r#"{"event":"relay","relay":"wss://nos.lol","status":"disconnected"}"#,
            r#"{"event":"auth","relay":"wss://nos.lol","status":"ok"}"#,
        ] {
            assert!(matches!(parse_line(line).unwrap(), ListenLine::Event(_)));
        }
    }

    #[test]
    fn test_parse_line_message_with_new_fields() {
        let line = r#"{"id":"ab12","from":"npub1abc","content":"https://x/f","timestamp":1000,"participants":["npub1abc","npub1def"],"protocol":"nip17","kind":15,"tags":[["p","00"],["x","ff"]],"reply_to":null,"file":{"url":"https://x/f","file_type":"image/png"}}"#;
        match parse_line(line).unwrap() {
            ListenLine::Message(msg) => {
                assert_eq!(msg.id.as_deref(), Some("ab12"));
                assert_eq!(msg.content, "https://x/f");
            }
            other => panic!("expected message, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_line_legacy_message_without_id() {
        let line =
            r#"{"from":"npub1abc","content":"hi","timestamp":1,"participants":["npub1abc"]}"#;
        match parse_line(line).unwrap() {
            ListenLine::Message(msg) => assert!(msg.id.is_none()),
            other => panic!("expected message, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_line_malformed() {
        assert!(parse_line(r#"{"bad": true}"#).is_err());
        assert!(parse_line("not json").is_err());
    }

    #[test]
    fn test_migrate_legacy_db_adds_rumor_id() {
        let conn = Connection::open_in_memory().unwrap();
        // Schema as created by earlier am-ingest versions, with a row in it.
        conn.execute_batch(
            "CREATE TABLE messages (
                id INTEGER PRIMARY KEY, message_id TEXT UNIQUE NOT NULL,
                conv_id TEXT NOT NULL, sender TEXT NOT NULL, content TEXT NOT NULL,
                timestamp INTEGER NOT NULL, processed INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL DEFAULT (unixepoch()));
             INSERT INTO messages (message_id, conv_id, sender, content, timestamp, processed)
                VALUES ('old', 'c', 'npub1abc', 'hello', 1000, 1);",
        )
        .unwrap();
        init_db(&conn).unwrap();
        init_db(&conn).unwrap(); // idempotent on a migrated DB

        let old: Option<String> = conn
            .query_row(
                "SELECT rumor_id FROM messages WHERE message_id = 'old'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(old.is_none());
    }

    #[test]
    fn test_insert_stores_and_backfills_rumor_id() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let mut msg = ReceivedMessage {
            from: "npub1abc".to_string(),
            content: "hello".to_string(),
            timestamp: 1000,
            participants: vec!["npub1abc".to_string(), "npub1def".to_string()],
            id: None,
        };
        assert!(insert_message(&conn, &msg).unwrap());
        // Same message again, now with an id: still a duplicate, id backfilled.
        msg.id = Some("deadbeef".to_string());
        assert!(!insert_message(&conn, &msg).unwrap());
        let (count, rumor_id): (i64, Option<String>) = conn
            .query_row("SELECT COUNT(*), MAX(rumor_id) FROM messages", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(rumor_id.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn test_ndjson_parsing_empty() {
        assert!(serde_json::from_str::<ReceivedMessage>("").is_err());
    }

    #[test]
    fn test_init_db_fresh() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // Verify tables exist
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('messages', 'conversations')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_init_db_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        init_db(&conn).unwrap(); // should not error
    }

    #[test]
    fn test_insert_message_new() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let msg = ReceivedMessage {
            from: "npub1abc".to_string(),
            content: "hello".to_string(),
            timestamp: 1000,
            participants: vec!["npub1abc".to_string(), "npub1def".to_string()],
            id: None,
        };

        assert!(insert_message(&conn, &msg).unwrap());

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_insert_message_duplicate() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let msg = ReceivedMessage {
            from: "npub1abc".to_string(),
            content: "hello".to_string(),
            timestamp: 1000,
            participants: vec!["npub1abc".to_string(), "npub1def".to_string()],
            id: None,
        };

        assert!(insert_message(&conn, &msg).unwrap());
        assert!(!insert_message(&conn, &msg).unwrap()); // duplicate
    }

    #[test]
    fn test_insert_message_creates_conversation() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let msg = ReceivedMessage {
            from: "npub1abc".to_string(),
            content: "hello".to_string(),
            timestamp: 1000,
            participants: vec!["npub1abc".to_string(), "npub1def".to_string()],
            id: None,
        };

        insert_message(&conn, &msg).unwrap();

        let conv_type: String = conn
            .query_row("SELECT conv_type FROM conversations LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(conv_type, "dm");
    }

    #[test]
    fn test_insert_group_message_conv_type() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let msg = ReceivedMessage {
            from: "npub1abc".to_string(),
            content: "hello group".to_string(),
            timestamp: 1000,
            participants: vec![
                "npub1abc".to_string(),
                "npub1def".to_string(),
                "npub1ghi".to_string(),
            ],
            id: None,
        };

        insert_message(&conn, &msg).unwrap();

        let conv_type: String = conn
            .query_row("SELECT conv_type FROM conversations LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(conv_type, "group");
    }

    #[test]
    fn test_same_conversation_same_conv_id() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let msg1 = ReceivedMessage {
            from: "npub1abc".to_string(),
            content: "hello".to_string(),
            timestamp: 1000,
            participants: vec!["npub1abc".to_string(), "npub1def".to_string()],
            id: None,
        };
        let msg2 = ReceivedMessage {
            from: "npub1def".to_string(),
            content: "world".to_string(),
            timestamp: 1001,
            participants: vec!["npub1abc".to_string(), "npub1def".to_string()],
            id: None,
        };

        insert_message(&conn, &msg1).unwrap();
        insert_message(&conn, &msg2).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let msg_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(msg_count, 2);
    }

    #[test]
    fn test_conversation_last_message_updates() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let msg1 = ReceivedMessage {
            from: "npub1abc".to_string(),
            content: "hello".to_string(),
            timestamp: 1000,
            participants: vec!["npub1abc".to_string(), "npub1def".to_string()],
            id: None,
        };
        let msg2 = ReceivedMessage {
            from: "npub1def".to_string(),
            content: "world".to_string(),
            timestamp: 2000,
            participants: vec!["npub1abc".to_string(), "npub1def".to_string()],
            id: None,
        };

        insert_message(&conn, &msg1).unwrap();
        insert_message(&conn, &msg2).unwrap();

        let last: i64 = conn
            .query_row(
                "SELECT last_message FROM conversations LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(last, 2000);
    }
}
