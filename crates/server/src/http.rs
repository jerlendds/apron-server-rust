use std::sync::Arc;

use axum::{
    Router,
    routing::{get, post, put},
};

use crate::{
    auth::{email, external, passkey_page},
    embeds,
    service::Server,
    ws,
};

pub(crate) fn router(server: Arc<Server>, core_only: bool) -> Router {
    let routes = Router::new()
        .route("/ws", get(ws::upgrade))
        .route("/health", get(|| async { "Apron is ready" }));
    let routes = if core_only {
        routes
    } else {
        routes
            .route("/login", get(email::login_page))
            .route("/login.js", get(email::login_script))
            .route("/passkey", get(passkey_page::show))
            .route("/passkey/begin", post(passkey_page::begin))
            .route("/passkey/finish", post(passkey_page::finish))
            .route("/passkey.js", get(passkey_page::script))
            .route(
                "/oauth/authorize",
                get(external::show).post(external::consent),
            )
            .route(
                "/w/{secret}",
                put(embeds::claim).options(embeds::write_options),
            )
            .route("/live/{key}", get(embeds::read_live))
            .route("/f/{key}", get(embeds::read_asset))
    };
    routes.with_state(server)
}
