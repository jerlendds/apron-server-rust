use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicU64},
};

use serde_json::{Value, json};
use store::{State, Store};
use tokio::{
    net::TcpListener,
    sync::{Mutex, mpsc},
};
use tokio_util::sync::CancellationToken;

use crate::{
    auth::{email, external, passkey_page, webauthn},
    embeds, http,
    policy::DEFAULT_ROOM,
    push,
};

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub core_only: bool,
    pub title: Option<String>,
    pub welcome: Option<String>,
    pub ext_limit: Option<usize>,
    pub http_origin: Option<String>,
    pub grant_ttl_ms: u64,
    pub email: Option<email::EmailConfig>,
    pub external: bool,
    pub code_ttl_ms: u64,
    pub passkey: bool,
    pub passkey_ttl_ms: u64,
    pub push: Option<push::PushConfig>,
}

pub(crate) type ConnectionId = u64;

pub(crate) struct Connection {
    pub user: Option<String>,
    pub idle: bool,
    pub pending_email: Option<String>,
    pub origin: Option<String>,
    pub ceremony: Option<webauthn::Ceremony>,
    pub tx: mpsc::Sender<Value>,
    pub closed: CancellationToken,
}

pub(crate) struct Coordinator {
    pub state: State,
    pub store: Store,
    pub connections: BTreeMap<ConnectionId, Connection>,
    pub config: ServerConfig,
    pub origin: String,
    pub mail: Option<mpsc::Sender<email::Delivery>>,
    pub passkeys: Option<webauthn::Passkeys>,
    pub passkey_pages: BTreeMap<String, passkey_page::Page>,
    pub passkey_additions: BTreeMap<String, passkey_page::Addition>,
    pub browser_pages: BTreeMap<String, external::PendingPage>,
    pub live: BTreeMap<String, Arc<embeds::Live>>,
    pub push: Option<Arc<push::Push>>,
    pub push_queue: Option<mpsc::Sender<push::Signal>>,
}

pub(crate) struct Server {
    pub coordinator: Mutex<Coordinator>,
    pub(crate) next_connection: AtomicU64,
    pub(crate) shutdown: CancellationToken,
}

pub async fn serve(
    listener: TcpListener,
    config: ServerConfig,
    mut store: Store,
    shutdown: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut state = store.load().await?;
    let mut migrate = crate::auth::migrate_tokens(&mut state);
    if !state
        .rooms
        .get(DEFAULT_ROOM)
        .is_some_and(|room| room["log_id"].is_string())
    {
        let id = state.next_log_id().ok_or("log ID exhausted")?;
        let room = json!({"room_id": DEFAULT_ROOM, "title": "General", "log_id": id.to_string()});
        state.rooms.insert(DEFAULT_ROOM.into(), room.clone());
        state.log(id, "rooms", vec![DEFAULT_ROOM.into()], room);
        migrate = true;
    }
    if migrate {
        store.commit(&state).await?;
    }
    let origin = match &config.http_origin {
        Some(origin) => origin.trim_end_matches('/').to_owned(),
        None => format!("http://{}", listener.local_addr()?),
    };
    let mut parsed = url::Url::parse(&origin)?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.has_host()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("public HTTP origin must be an HTTP or HTTPS origin".into());
    }
    if (config.external || config.passkey)
        && !config.core_only
        && parsed.scheme() != "https"
        && !external::loopback(&parsed)
    {
        return Err("browser sign-in requires an HTTPS origin or loopback HTTP".into());
    }
    let passkeys = if config.passkey && !config.core_only {
        Some(webauthn::Passkeys::new(
            &parsed,
            config.passkey_ttl_ms,
            config.title.as_deref().unwrap_or("Apron"),
        )?)
    } else {
        None
    };
    parsed.set_path("");
    let origin = parsed.as_str().trim_end_matches('/').to_owned();
    let (push, push_queue, push_rx) =
        if let Some(config) = config.push.as_ref().filter(|_| !config.core_only) {
            let push = Arc::new(push::Push::open(config).await?);
            let (tx, rx) = mpsc::channel(256);
            (Some(push), Some(tx), Some(rx))
        } else {
            (None, None, None)
        };
    let mut coordinator = Coordinator {
        state,
        store,
        connections: BTreeMap::new(),
        config,
        origin,
        mail: None,
        passkeys,
        passkey_pages: BTreeMap::new(),
        passkey_additions: BTreeMap::new(),
        browser_pages: BTreeMap::new(),
        live: BTreeMap::new(),
        push: push.clone(),
        push_queue,
    };
    if !coordinator.config.core_only {
        embeds::expire(&mut coordinator, true)
            .await
            .map_err(|_| "could not recover embed grants")?;
    }
    let mail_worker = coordinator
        .config
        .email
        .as_ref()
        .filter(|_| !coordinator.config.core_only)
        .map(|config| {
            let (sender, worker) = email::start(config.command.clone(), shutdown.clone());
            coordinator.mail = Some(sender);
            worker
        });
    let server = Arc::new(Server {
        coordinator: Mutex::new(coordinator),
        next_connection: AtomicU64::new(1),
        shutdown: shutdown.clone(),
    });
    let push_worker = push_rx.zip(push).map(|(signals, push)| {
        tokio::spawn(push::worker(
            signals,
            server.clone(),
            push,
            shutdown.clone(),
        ))
    });
    let timer_server = server.clone();
    let timer_token = shutdown.clone();
    let timer = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = timer_token.cancelled() => break,
                _ = interval.tick() => {
                    let mut coordinator = timer_server.coordinator.lock().await;
                    if !coordinator.config.core_only {
                        coordinator.expire_mutes().await;
                        let _ = embeds::expire(&mut coordinator, false).await;
                    }
                }
            }
        }
    });
    let core_only = server.coordinator.lock().await.config.core_only;
    let routes = http::router(server, core_only);
    let result = axum::serve(listener, routes)
        .with_graceful_shutdown(shutdown.clone().cancelled_owned())
        .await;
    shutdown.cancel();
    let _ = timer.await;
    if let Some(worker) = push_worker {
        let _ = worker.await;
    }
    if let Some(worker) = mail_worker {
        let _ = worker.await;
    }
    result?;
    Ok(())
}
