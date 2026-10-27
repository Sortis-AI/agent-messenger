use clap::{Args, ValueEnum};

use am_core::error::AmResult;
use am_core::message::{FileMeta, Protocol, SendRequest};
use am_core::output::Format;
use am_core::{message, output};

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum ReplyProtocol {
    /// NIP-17 gift-wrapped private DM (default)
    #[default]
    Nip17,
    /// Legacy NIP-04 kind-4 DM, for clients that only speak NIP-04 (leaks metadata)
    Nip04,
}

impl From<ReplyProtocol> for Protocol {
    fn from(p: ReplyProtocol) -> Self {
        match p {
            ReplyProtocol::Nip17 => Protocol::Nip17,
            ReplyProtocol::Nip04 => Protocol::Nip04,
        }
    }
}

const FILE_REQUIRED: [&str; 4] = [
    "file_type",
    "decryption_key",
    "decryption_nonce",
    "file_hash",
];

/// NIP-17 kind-15 file message. `am` does not upload or encrypt: host the
/// encrypted file yourself and pass its URL, keys and hash.
#[derive(Debug, Args)]
#[command(next_help_heading = "File message (NIP-17 kind 15)")]
pub struct FileArgs {
    /// URL of the encrypted file; sends a kind-15 file message instead of text
    #[arg(long, value_name = "URL", requires_all = FILE_REQUIRED)]
    pub file_url: Option<String>,

    /// MIME type of the file before encryption (e.g. image/jpeg)
    #[arg(long, value_name = "MIME", requires = "file_url")]
    pub file_type: Option<String>,

    /// Key the recipient decrypts the file with
    #[arg(long, value_name = "KEY", requires = "file_url")]
    pub decryption_key: Option<String>,

    /// Nonce the recipient decrypts the file with
    #[arg(long, value_name = "NONCE", requires = "file_url")]
    pub decryption_nonce: Option<String>,

    /// SHA-256 (hex) of the encrypted file (`x` tag)
    #[arg(long, value_name = "SHA256", requires = "file_url")]
    pub file_hash: Option<String>,

    /// Encryption algorithm [default: aes-gcm]
    #[arg(long, value_name = "ALG", requires = "file_url")]
    pub encryption_algorithm: Option<String>,

    /// SHA-256 (hex) of the file before encryption (`ox` tag)
    #[arg(long, value_name = "SHA256", requires = "file_url")]
    pub file_original_hash: Option<String>,

    /// Encrypted file size in bytes
    #[arg(long, value_name = "BYTES", requires = "file_url")]
    pub file_size: Option<u64>,

    /// Image dimensions, <width>x<height>
    #[arg(long, value_name = "WxH", requires = "file_url")]
    pub file_dim: Option<String>,

    /// Blurhash placeholder
    #[arg(long, value_name = "HASH", requires = "file_url")]
    pub file_blurhash: Option<String>,

    /// Thumbhash placeholder
    #[arg(long, value_name = "HASH", requires = "file_url")]
    pub file_thumbhash: Option<String>,

    /// Thumbnail URL (encrypted with the same key and nonce)
    #[arg(long, value_name = "URL", requires = "file_url")]
    pub file_thumb: Option<String>,

    /// Fallback file URL (repeatable; same key and nonce)
    #[arg(long, value_name = "URL", requires = "file_url")]
    pub file_fallback: Vec<String>,
}

impl FileArgs {
    fn into_meta(self) -> Option<FileMeta> {
        let url = self.file_url?;
        Some(FileMeta {
            url,
            file_type: self.file_type,
            encryption_algorithm: Some(
                self.encryption_algorithm
                    .unwrap_or_else(|| "aes-gcm".to_string()),
            ),
            decryption_key: self.decryption_key,
            decryption_nonce: self.decryption_nonce,
            hash: self.file_hash,
            original_hash: self.file_original_hash,
            size: self.file_size,
            dim: self.file_dim,
            blurhash: self.file_blurhash,
            thumbhash: self.file_thumbhash,
            thumb: self.file_thumb,
            fallback: self.file_fallback,
        })
    }
}

#[derive(Debug, Args)]
pub struct SendArgs {
    /// Recipient npub (repeatable for group messages)
    #[arg(long, required = true)]
    pub to: Vec<String>,

    /// DM protocol to send with; use the `protocol` of the message you're answering
    #[arg(long, value_enum, default_value_t)]
    pub reply_protocol: ReplyProtocol,

    /// Reply to the message with this `id` (from `listen`); adds an `e` tag
    #[arg(long, value_name = "ID")]
    pub reply_to: Option<String>,

    #[command(flatten)]
    pub file: FileArgs,

    /// Message content (reads from stdin if omitted)
    #[arg(conflicts_with = "file_url")]
    pub message: Option<String>,
}

pub async fn run(
    args: SendArgs,
    identity: Option<&str>,
    passphrase: Option<&str>,
    format: Format,
    verbosity: u8,
) -> AmResult<()> {
    let request = SendRequest {
        to: &args.to,
        message: args.message.as_deref(),
        protocol: args.reply_protocol.into(),
        reply_to: args.reply_to.as_deref(),
        file: args.file.into_meta(),
    };
    let result = message::send(identity, request, passphrase, verbosity).await?;
    match format {
        Format::Json => output::print_json(&result)?,
        Format::Text => {
            let recipients = result.to.join(", ");
            println!("Sent message {} to {recipients}", result.id);
            if !result.failed.is_empty() {
                let failed = result.failed.join(", ");
                eprintln!("Failed to send to: {failed}");
            }
        }
    }
    Ok(())
}
