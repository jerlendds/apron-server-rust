use std::sync::atomic::Ordering;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    service::{Connection, ConnectionId, Coordinator, Server},
    wire::notification,
};

impl Server {
    pub async fn connect(
        &self,
        tx: mpsc::Sender<Value>,
        origin: Option<String>,
    ) -> (ConnectionId, CancellationToken) {
        let id = self.next_connection.fetch_add(1, Ordering::Relaxed);
        let closed = self.shutdown.child_token();
        let mut coordinator = self.coordinator.lock().await;
        coordinator.connections.insert(
            id,
            Connection {
                user: None,
                idle: false,
                pending_email: None,
                origin,
                ceremony: None,
                tx,
                closed: closed.clone(),
            },
        );
        coordinator.send(id, notification("server", coordinator.greeting()));
        if let Some(welcome) = &coordinator.config.welcome {
            coordinator.send(
                id,
                notification(
                    "message",
                    json!({
                        "from": {"user_id": "~private"},
                        "body": {"text": welcome, "format": "markdown"}
                    }),
                ),
            );
        }
        (id, closed)
    }

    pub async fn disconnect(&self, id: ConnectionId) {
        let mut coordinator = self.coordinator.lock().await;
        let user = coordinator
            .connections
            .get(&id)
            .and_then(|connection| connection.user.clone());
        let before = user.as_deref().map(|user| coordinator.shown(user));
        coordinator.connections.remove(&id);
        if let Some((user, before)) = user.as_deref().zip(before) {
            coordinator.presence_change(user, before);
        }
    }
}

impl Coordinator {
    pub(crate) fn greeting(&self) -> Value {
        let mut capabilities = Vec::new();
        if !self.config.core_only {
            capabilities.extend([
                "rooms",
                "history",
                "edit",
                "reactions",
                "status",
                "activity",
                "command",
                "embed:upload",
                "embed:append",
                "embed:terminal",
            ]);
            if self.config.ext_limit.is_some() {
                capabilities.push("ext");
            }
        }
        let mut hello = json!({
            "apron": 10, "agent": concat!("aprond/", env!("CARGO_PKG_VERSION")),
            "auth": ["token"], "signup": ["guest"], "capabilities": capabilities, "ping": 30
        });
        if let Some(push) = &self.push {
            hello["push"] = push.greeting();
        }
        if self.mail.is_some() {
            hello["auth"] = json!(["email", "token"]);
            hello["signup"] = json!(["guest", "email"]);
        }
        if self.config.external && !self.config.core_only {
            hello["auth"]
                .as_array_mut()
                .expect("auth is an array")
                .insert(0, json!("external"));
            hello["signup"]
                .as_array_mut()
                .expect("signup is an array")
                .push(json!("external"));
        }
        if self.passkeys.is_some() {
            hello["auth"]
                .as_array_mut()
                .expect("auth is an array")
                .insert(0, json!("webauthn"));
            hello["signup"]
                .as_array_mut()
                .expect("signup is an array")
                .push(json!("webauthn"));
        }
        if !self.config.core_only {
            hello["status"] = json!(crate::status::OPTIONAL);
        }
        if let Some(title) = &self.config.title {
            hello["title"] = json!(title);
        }
        if let Some(welcome) = &self.config.welcome {
            hello["welcome"] = json!(welcome);
        }
        hello
    }
}
