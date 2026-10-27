use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;

use clap::Parser;
use rusqlite::Connection;
use serde::Deserialize;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(
    name = "am-agent",
    about = "Agent orchestrator with conversation isolation"
)]
struct Cli {
    /// Path to config file
    #[arg(long)]
    config: Option<PathBuf>,

    /// Path to SQLite database (overrides config)
    #[arg(long)]
    db: Option<PathBuf>,

    /// Poll interval in seconds (overrides config)
    #[arg(long)]
    interval: Option<u64>,

    /// Process pending messages once and exit
    #[arg(long)]
    once: bool,
}

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default)]
    agent: AgentConfig,
    #[serde(default)]
    general: GeneralConfig,
}

#[derive(Debug, Deserialize)]
struct AgentConfig {
    #[serde(default = "default_command")]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    stdin: bool,
    #[serde(default)]
    env: HashMap<String, String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            command: default_command(),
            args: vec!["-p".to_string(), "{prompt}".to_string()],
            stdin: false,
            env: HashMap::new(),
        }
    }
}

fn default_command() -> String {
    "claude".to_string()
}

#[derive(Debug, Deserialize)]
struct GeneralConfig {
    #[serde(default)]
    db: String,
    #[serde(default = "default_interval")]
    interval: u64,
    #[serde(default = "default_identity")]
    identity: String,
    #[serde(default)]
    system_prompt: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            db: String::new(),
            interval: default_interval(),
            identity: default_identity(),
            system_prompt: String::new(),
        }
    }
}

fn default_interval() -> u64 {
    30
}

fn default_identity() -> String {
    "default".to_string()
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

fn default_config_path() -> PathBuf {
    if let Some(home) = am_home() {
        return home.join("am-agent.toml");
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("am")
        .join("am-agent.toml")
}

fn load_config(path: Option<&PathBuf>) -> Config {
    let config_path = path.cloned().unwrap_or_else(default_config_path);

    match std::fs::read_to_string(&config_path) {
        Ok(contents) => match toml::from_str(&contents) {
            Ok(config) => config,
            Err(e) => {
                error!("invalid config at {}: {e}", config_path.display());
                std::process::exit(1);
            }
        },
        Err(_) => {
            info!("no config at {}, using defaults", config_path.display());
            Config {
                agent: AgentConfig::default(),
                general: GeneralConfig::default(),
            }
        }
    }
}

struct UnprocessedMessage {
    from: String,
    content: String,
    timestamp: i64,
}

struct ConversationInfo {
    participants: Vec<String>,
    metadata: Option<String>,
}

fn get_pending_conversations(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT DISTINCT conv_id FROM messages WHERE processed = 0 ORDER BY timestamp")?;
    let conv_ids = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(conv_ids)
}

fn get_conversation_info(conn: &Connection, conv_id: &str) -> rusqlite::Result<ConversationInfo> {
    conn.query_row(
        "SELECT participants, metadata FROM conversations WHERE conv_id = ?1",
        [conv_id],
        |row| {
            let participants_json: String = row.get(0)?;
            let metadata: Option<String> = row.get(1)?;
            let participants: Vec<String> =
                serde_json::from_str(&participants_json).unwrap_or_default();
            Ok(ConversationInfo {
                participants,
                metadata,
            })
        },
    )
}

fn get_unprocessed_messages(
    conn: &Connection,
    conv_id: &str,
) -> rusqlite::Result<Vec<UnprocessedMessage>> {
    let mut stmt = conn.prepare(
        "SELECT sender, content, timestamp FROM messages
         WHERE conv_id = ?1 AND processed = 0
         ORDER BY timestamp",
    )?;
    let messages = stmt
        .query_map([conv_id], |row| {
            Ok(UnprocessedMessage {
                from: row.get(0)?,
                content: row.get(1)?,
                timestamp: row.get(2)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(messages)
}

fn mark_processed(conn: &Connection, conv_id: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE messages SET processed = 1 WHERE conv_id = ?1 AND processed = 0",
        [conv_id],
    )
}

fn update_conversation_metadata(
    conn: &Connection,
    conv_id: &str,
    metadata: &str,
    last_message: i64,
) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE conversations SET metadata = ?1, last_message = ?2 WHERE conv_id = ?3",
        rusqlite::params![metadata, last_message, conv_id],
    )
}

fn build_prompt(
    system_prompt: &str,
    summary: Option<&str>,
    messages: &[UnprocessedMessage],
) -> String {
    let mut prompt = String::new();

    if !system_prompt.is_empty() {
        prompt.push_str(system_prompt);
        prompt.push_str("\n\n");
    }

    if let Some(s) = summary {
        if !s.is_empty() {
            prompt.push_str("## Conversation context\n");
            prompt.push_str(s);
            prompt.push_str("\n\n");
        }
    }

    prompt.push_str("## New messages\n");
    for msg in messages {
        prompt.push_str(&format!(
            "[{}] {}: {}\n",
            msg.timestamp, msg.from, msg.content
        ));
    }

    prompt.push_str(
        "\n## Instructions\n\
         Respond to the new messages above. Then output a conversation summary\n\
         on a line starting with \"SUMMARY:\" that captures key context for future reference.\n",
    );

    prompt
}

fn parse_response(response: &str) -> (String, Option<String>) {
    if let Some(idx) = response.find("\nSUMMARY:") {
        let reply = response[..idx].trim().to_string();
        let summary = response[idx + 9..].trim().to_string(); // skip "\nSUMMARY:"
        (reply, Some(summary))
    } else if let Some(stripped) = response.strip_prefix("SUMMARY:") {
        // Entire response is a summary with no reply text
        (String::new(), Some(stripped.trim().to_string()))
    } else {
        (response.trim().to_string(), None)
    }
}

async fn invoke_agent(
    config: &AgentConfig,
    prompt: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut cmd = tokio::process::Command::new(&config.command);

    for (k, v) in &config.env {
        cmd.env(k, v);
    }

    if config.stdin {
        cmd.args(&config.args);
        cmd.stdin(Stdio::piped());
    } else {
        for arg in &config.args {
            if arg.contains("{prompt}") {
                cmd.arg(arg.replace("{prompt}", prompt));
            } else {
                cmd.arg(arg);
            }
        }
        cmd.stdin(Stdio::null());
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());

    let mut child = cmd.spawn()?;

    if config.stdin {
        use tokio::io::AsyncWriteExt;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes()).await?;
            stdin.shutdown().await?;
        }
    }

    let output = child.wait_with_output().await?;

    if !output.status.success() {
        return Err(format!(
            "agent exited with code {}",
            output.status.code().unwrap_or(-1)
        )
        .into());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

async fn send_reply(
    identity: &str,
    participants: &[String],
    reply: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = vec![
        "--identity".to_string(),
        identity.to_string(),
        "send".to_string(),
    ];

    for p in participants {
        args.push("--to".to_string());
        args.push(p.clone());
    }

    args.push(reply.to_string());

    let output = tokio::process::Command::new("am")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .await?;

    if !output.status.success() {
        return Err(format!(
            "am send failed with code {}",
            output.status.code().unwrap_or(-1)
        )
        .into());
    }

    Ok(())
}

async fn process_cycle(
    conn: &Connection,
    config: &Config,
    system_prompt: &str,
) -> Result<usize, Box<dyn std::error::Error>> {
    let conv_ids = get_pending_conversations(conn)?;

    if conv_ids.is_empty() {
        return Ok(0);
    }

    info!("processing {} conversations", conv_ids.len());
    let mut processed_count = 0;

    for conv_id in &conv_ids {
        let info = match get_conversation_info(conn, conv_id) {
            Ok(info) => info,
            Err(e) => {
                warn!("failed to load conversation {}: {e}", &conv_id[..8]);
                continue;
            }
        };

        let messages = get_unprocessed_messages(conn, conv_id)?;
        if messages.is_empty() {
            continue;
        }

        let last_ts = messages.last().map(|m| m.timestamp).unwrap_or(0);

        info!(
            "conversation {} ({} participants): {} new messages",
            &conv_id[..8],
            info.participants.len(),
            messages.len()
        );

        let prompt = build_prompt(system_prompt, info.metadata.as_deref(), &messages);

        match invoke_agent(&config.agent, &prompt).await {
            Ok(response) => {
                let (reply, summary) = parse_response(&response);

                if !reply.is_empty() {
                    match send_reply(&config.general.identity, &info.participants, &reply).await {
                        Ok(()) => {
                            info!("sent reply to conversation {}", &conv_id[..8]);
                        }
                        Err(e) => {
                            error!("failed to send reply for {}: {e}", &conv_id[..8]);
                            // Don't mark as processed — retry next cycle
                            continue;
                        }
                    }
                }

                mark_processed(conn, conv_id)?;

                if let Some(ref s) = summary {
                    update_conversation_metadata(conn, conv_id, s, last_ts)?;
                }

                processed_count += 1;
            }
            Err(e) => {
                error!("agent failed for conversation {}: {e}", &conv_id[..8]);
                // Don't mark as processed — retry next cycle
            }
        }
    }

    Ok(processed_count)
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config(cli.config.as_ref());

    let db_path = cli.db.unwrap_or_else(|| {
        if config.general.db.is_empty() {
            default_db_path()
        } else {
            PathBuf::from(&config.general.db)
        }
    });

    let interval = cli.interval.unwrap_or(config.general.interval);

    let conn = Connection::open(&db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;

    info!("using database at {}", db_path.display());

    // Load system prompt from file if configured
    let system_prompt = if config.general.system_prompt.is_empty() {
        String::new()
    } else {
        match std::fs::read_to_string(&config.general.system_prompt) {
            Ok(content) => content,
            Err(e) => {
                warn!(
                    "failed to read system prompt from {}: {e}",
                    config.general.system_prompt
                );
                String::new()
            }
        }
    };

    if cli.once {
        let count = process_cycle(&conn, &config, &system_prompt).await?;
        info!("processed {count} conversations");
        return Ok(());
    }

    info!("starting agent loop, polling every {interval}s");

    loop {
        tokio::select! {
            result = process_cycle(&conn, &config, &system_prompt) => {
                match result {
                    Ok(count) if count > 0 => {
                        info!("processed {count} conversations");
                    }
                    Ok(_) => {}
                    Err(e) => {
                        error!("cycle error: {e}");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("received shutdown signal");
                return Ok(());
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
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

    if let Err(e) = run(cli).await {
        error!("{e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_prompt_with_all_parts() {
        let messages = vec![
            UnprocessedMessage {
                from: "npub1abc".to_string(),
                content: "hello".to_string(),
                timestamp: 1000,
            },
            UnprocessedMessage {
                from: "npub1def".to_string(),
                content: "world".to_string(),
                timestamp: 1001,
            },
        ];

        let prompt = build_prompt("You are helpful.", Some("Previous context here"), &messages);

        assert!(prompt.contains("You are helpful."));
        assert!(prompt.contains("Previous context here"));
        assert!(prompt.contains("[1000] npub1abc: hello"));
        assert!(prompt.contains("[1001] npub1def: world"));
        assert!(prompt.contains("SUMMARY:"));
    }

    #[test]
    fn test_build_prompt_no_summary() {
        let messages = vec![UnprocessedMessage {
            from: "npub1abc".to_string(),
            content: "hello".to_string(),
            timestamp: 1000,
        }];

        let prompt = build_prompt("System.", None, &messages);

        assert!(prompt.contains("System."));
        assert!(!prompt.contains("Conversation context"));
        assert!(prompt.contains("[1000] npub1abc: hello"));
    }

    #[test]
    fn test_build_prompt_no_system_prompt() {
        let messages = vec![UnprocessedMessage {
            from: "npub1abc".to_string(),
            content: "hello".to_string(),
            timestamp: 1000,
        }];

        let prompt = build_prompt("", Some("summary"), &messages);

        assert!(prompt.starts_with("## Conversation context"));
    }

    #[test]
    fn test_build_prompt_messages_ordered() {
        let messages = vec![
            UnprocessedMessage {
                from: "a".to_string(),
                content: "first".to_string(),
                timestamp: 100,
            },
            UnprocessedMessage {
                from: "b".to_string(),
                content: "second".to_string(),
                timestamp: 200,
            },
            UnprocessedMessage {
                from: "c".to_string(),
                content: "third".to_string(),
                timestamp: 300,
            },
        ];

        let prompt = build_prompt("", None, &messages);
        let first_pos = prompt.find("first").unwrap();
        let second_pos = prompt.find("second").unwrap();
        let third_pos = prompt.find("third").unwrap();
        assert!(first_pos < second_pos);
        assert!(second_pos < third_pos);
    }

    #[test]
    fn test_parse_response_with_summary() {
        let response =
            "Here is my reply to your question.\nSUMMARY: User asked about X, I explained Y.";
        let (reply, summary) = parse_response(response);
        assert_eq!(reply, "Here is my reply to your question.");
        assert_eq!(summary.unwrap(), "User asked about X, I explained Y.");
    }

    #[test]
    fn test_parse_response_without_summary() {
        let response = "Just a plain reply.";
        let (reply, summary) = parse_response(response);
        assert_eq!(reply, "Just a plain reply.");
        assert!(summary.is_none());
    }

    #[test]
    fn test_parse_response_empty() {
        let (reply, summary) = parse_response("");
        assert_eq!(reply, "");
        assert!(summary.is_none());
    }

    #[test]
    fn test_parse_response_summary_only() {
        let response = "SUMMARY: Just context, no reply needed.";
        let (reply, summary) = parse_response(response);
        assert_eq!(reply, "");
        assert_eq!(summary.unwrap(), "Just context, no reply needed.");
    }

    #[test]
    fn test_parse_response_multiline_reply() {
        let response = "Line 1\nLine 2\nLine 3\nSUMMARY: Multi-line conversation about topics.";
        let (reply, summary) = parse_response(response);
        assert_eq!(reply, "Line 1\nLine 2\nLine 3");
        assert_eq!(summary.unwrap(), "Multi-line conversation about topics.");
    }

    #[test]
    fn test_config_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.agent.command, "claude");
        assert_eq!(config.general.interval, 30);
        assert_eq!(config.general.identity, "default");
        assert!(!config.agent.stdin);
    }

    #[test]
    fn test_config_custom() {
        let toml_str = r#"
[agent]
command = "gemini"
args = ["{prompt}"]
stdin = false

[general]
interval = 60
identity = "work"
system_prompt = "agent.md"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.agent.command, "gemini");
        assert_eq!(config.general.interval, 60);
        assert_eq!(config.general.identity, "work");
        assert_eq!(config.general.system_prompt, "agent.md");
    }

    #[test]
    fn test_config_invalid_toml() {
        let result: Result<Config, _> = toml::from_str("not valid {{{{ toml");
        assert!(result.is_err());
    }

    #[test]
    fn test_db_operations() {
        let conn = Connection::open_in_memory().unwrap();
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
            CREATE TABLE IF NOT EXISTS conversations (
                conv_id       TEXT PRIMARY KEY,
                participants  TEXT NOT NULL,
                conv_type     TEXT NOT NULL,
                last_message  INTEGER NOT NULL DEFAULT 0,
                metadata      TEXT
            );",
        )
        .unwrap();

        // Insert test data for two conversations
        conn.execute(
            "INSERT INTO messages (message_id, conv_id, sender, content, timestamp)
             VALUES ('msg1', 'conv_a', 'alice', 'hello', 1000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (message_id, conv_id, sender, content, timestamp)
             VALUES ('msg2', 'conv_a', 'bob', 'hi', 1001)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (message_id, conv_id, sender, content, timestamp)
             VALUES ('msg3', 'conv_b', 'carol', 'hey', 1002)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (conv_id, participants, conv_type, last_message)
             VALUES ('conv_a', '[\"alice\",\"bob\"]', 'dm', 1001)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (conv_id, participants, conv_type, last_message)
             VALUES ('conv_b', '[\"carol\",\"dave\"]', 'dm', 1002)",
            [],
        )
        .unwrap();

        // Test get_pending_conversations
        let pending = get_pending_conversations(&conn).unwrap();
        assert_eq!(pending.len(), 2);

        // Test get_conversation_info
        let info = get_conversation_info(&conn, "conv_a").unwrap();
        assert_eq!(info.participants, vec!["alice", "bob"]);
        assert!(info.metadata.is_none());

        // Test get_unprocessed_messages
        let msgs = get_unprocessed_messages(&conn, "conv_a").unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].from, "alice");
        assert_eq!(msgs[1].from, "bob");

        // Test mark_processed
        mark_processed(&conn, "conv_a").unwrap();
        let msgs_after = get_unprocessed_messages(&conn, "conv_a").unwrap();
        assert!(msgs_after.is_empty());

        // conv_b should still be unprocessed
        let pending_after = get_pending_conversations(&conn).unwrap();
        assert_eq!(pending_after.len(), 1);
        assert_eq!(pending_after[0], "conv_b");

        // Test update_conversation_metadata
        update_conversation_metadata(&conn, "conv_a", "summary text", 1001).unwrap();
        let info_after = get_conversation_info(&conn, "conv_a").unwrap();
        assert_eq!(info_after.metadata.unwrap(), "summary text");
    }

    #[test]
    fn test_conversation_isolation() {
        // Verify that processing one conversation doesn't affect another
        let conn = Connection::open_in_memory().unwrap();
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
            CREATE TABLE IF NOT EXISTS conversations (
                conv_id       TEXT PRIMARY KEY,
                participants  TEXT NOT NULL,
                conv_type     TEXT NOT NULL,
                last_message  INTEGER NOT NULL DEFAULT 0,
                metadata      TEXT
            );",
        )
        .unwrap();

        // Two conversations
        conn.execute(
            "INSERT INTO messages (message_id, conv_id, sender, content, timestamp)
             VALUES ('msg1', 'conv_a', 'alice', 'secret_a', 1000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (message_id, conv_id, sender, content, timestamp)
             VALUES ('msg2', 'conv_b', 'bob', 'secret_b', 1001)",
            [],
        )
        .unwrap();

        // Get messages for conv_a — should NOT contain conv_b content
        let msgs_a = get_unprocessed_messages(&conn, "conv_a").unwrap();
        assert_eq!(msgs_a.len(), 1);
        assert_eq!(msgs_a[0].content, "secret_a");
        assert!(!msgs_a.iter().any(|m| m.content == "secret_b"));

        // Get messages for conv_b — should NOT contain conv_a content
        let msgs_b = get_unprocessed_messages(&conn, "conv_b").unwrap();
        assert_eq!(msgs_b.len(), 1);
        assert_eq!(msgs_b[0].content, "secret_b");
        assert!(!msgs_b.iter().any(|m| m.content == "secret_a"));

        // Mark conv_a processed — conv_b should be unaffected
        mark_processed(&conn, "conv_a").unwrap();
        let msgs_b_after = get_unprocessed_messages(&conn, "conv_b").unwrap();
        assert_eq!(msgs_b_after.len(), 1);
    }
}
