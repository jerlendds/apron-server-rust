use std::{net::SocketAddr, path::PathBuf};

use clap::Parser;

#[derive(Parser)]
#[command(version, about = "Run an Apron v10 chat server")]
pub struct Config {
    #[arg(long, env = "APRON_LISTEN", default_value = "127.0.0.1:9000")]
    pub listen: SocketAddr,

    #[arg(long, env = "APRON_DB", default_value = "apron.db")]
    pub db: String,

    #[arg(long, env = "APRON_MEMORY")]
    pub memory: bool,

    #[arg(long, env = "APRON_CORE_ONLY")]
    pub core_only: bool,

    #[arg(long, env = "APRON_TITLE")]
    pub title: Option<String>,

    #[arg(long, env = "APRON_WELCOME")]
    pub welcome: Option<String>,

    #[arg(long, env = "APRON_EXT_LIMIT", default_value_t = 16384)]
    pub ext_limit: usize,

    #[arg(long, env = "APRON_HTTP_ORIGIN")]
    pub http_origin: Option<String>,

    #[arg(long, env = "APRON_GRANT_TTL_MS", default_value_t = 300_000, value_parser = clap::value_parser!(u64).range(1..))]
    pub grant_ttl_ms: u64,

    /// Executable that accepts -- and the recipient, and reads an RFC 5322 message on stdin.
    #[arg(long, env = "APRON_MAIL_COMMAND")]
    pub mail_command: Option<PathBuf>,

    #[arg(long, env = "APRON_EMAIL_TTL_MS", default_value_t = 300_000, value_parser = clap::value_parser!(u64).range(1..=600_000))]
    pub email_ttl_ms: u64,
    #[arg(long, env = "APRON_EXTERNAL_AUTH")]
    pub external_auth: bool,

    #[arg(long, env = "APRON_CODE_TTL_MS", default_value_t = 180_000, value_parser = clap::value_parser!(u64).range(1..=600_000))]
    pub code_ttl_ms: u64,
    #[arg(long, env = "APRON_PASSKEY_AUTH")]
    pub passkey_auth: bool,

    #[arg(long, env = "APRON_PASSKEY_TTL_MS", default_value_t = 180_000, value_parser = clap::value_parser!(u64).range(1..=600_000))]
    pub passkey_ttl_ms: u64,
    #[arg(long, env = "APRON_PUSH_RELAY")]
    pub push_relay: bool,

    #[arg(long, env = "APRON_PUSH_VAPID_KEY")]
    pub push_vapid_key: Option<PathBuf>,

    #[arg(long, env = "APRON_PUSH_PRIVATE_ORIGIN", requires = "push_private_ca")]
    pub push_private_origin: Option<String>,

    #[arg(long, env = "APRON_PUSH_PRIVATE_CA", requires = "push_private_origin")]
    pub push_private_ca: Option<PathBuf>,
}
