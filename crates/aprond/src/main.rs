mod config;

use clap::Parser;
use server::{EmailConfig, PushConfig, ServerConfig};
use store::Store;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = config::Config::parse();
    let store = if config.memory {
        Store::memory()
    } else {
        Store::open(&config.db).await?
    };
    let listener = TcpListener::bind(config.listen).await?;
    let shutdown = CancellationToken::new();
    let signal = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        signal.cancel();
    });
    server::serve(
        listener,
        ServerConfig {
            core_only: config.core_only,
            title: config.title,
            welcome: config.welcome,
            ext_limit: Some(config.ext_limit),
            http_origin: config.http_origin,
            grant_ttl_ms: config.grant_ttl_ms,
            push: (config.push_relay || config.push_vapid_key.is_some()).then_some(PushConfig {
                vapid_key: config.push_vapid_key,
                private_origin: config.push_private_origin,
                private_ca: config.push_private_ca,
            }),
            external: config.external_auth,
            code_ttl_ms: config.code_ttl_ms,
            passkey: config.passkey_auth,
            passkey_ttl_ms: config.passkey_ttl_ms,
            email: config.mail_command.map(|command| EmailConfig {
                command,
                ttl_ms: config.email_ttl_ms,
            }),
        },
        store,
        shutdown,
    )
    .await
}
