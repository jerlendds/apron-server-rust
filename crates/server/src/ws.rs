use std::sync::Arc;

use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, header},
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Notify, mpsc};

use crate::{
    service::Server,
    wire::{Error, FRAME_LIMIT, Request},
};

pub(crate) async fn upgrade(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    ws.max_message_size(2 * FRAME_LIMIT)
        .max_frame_size(2 * FRAME_LIMIT)
        .on_upgrade(move |socket| handle(server, socket, origin))
}

/// Frames queued for a connection's writer. A connection that lets the queue
/// fill up is closed (§3.5.3).
const OUTGOING: usize = 128;
/// Free queue slots a connection needs before its next request is read, so
/// that the replies and broadcasts of its own requests cannot overflow it.
const HEADROOM: usize = 32;

async fn handle(server: Arc<Server>, socket: WebSocket, origin: Option<String>) {
    let (mut outgoing, mut incoming) = socket.split();
    let (tx, mut rx) = mpsc::channel(OUTGOING);
    let own = tx.clone();
    let drained = Arc::new(Notify::new());
    let (id, closed) = server.connect(tx, origin).await;
    let writer_drained = drained.clone();
    let writer = async move {
        while let Some(frame) = rx.recv().await {
            writer_drained.notify_one();
            if outgoing
                .send(Message::Text(frame.to_string().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    };
    let reader = async {
        loop {
            // Backpressure: a client that sends faster than it reads waits for
            // its own output to drain, instead of being closed for it. This
            // only watches the queue: reserving slots would take them from
            // the deliveries that need them.
            while own.capacity() < HEADROOM {
                tokio::select! {
                    _ = closed.cancelled() => return,
                    _ = drained.notified() => {}
                }
            }
            let next = tokio::select! {
                _ = closed.cancelled() => break,
                next = incoming.next() => next,
            };
            let Some(Ok(message)) = next else { break };
            match message {
                Message::Text(text) => {
                    let request = Request::parse(&text);
                    if let Ok(request) = &request
                        && request.method == "push_register"
                        && text.len() <= FRAME_LIMIT
                        && let Some(request_id) = request.id.as_deref()
                    {
                        let push = {
                            let coordinator = server.coordinator.lock().await;
                            coordinator
                                .connections
                                .get(&id)
                                .and_then(|connection| connection.user.as_ref())
                                .and(coordinator.push.clone())
                        };
                        if let Some(push) = push {
                            let url = request.params["url"]
                                .as_str()
                                .ok_or(Error::Invalid)
                                .and_then(crate::push::checked_url);
                            let valid = match url {
                                Ok(url) => push.validate_endpoint(&url).await.is_ok(),
                                Err(_) => false,
                            };
                            if !valid {
                                server
                                    .coordinator
                                    .lock()
                                    .await
                                    .send(id, Error::Invalid.frame(Some(request_id)));
                                continue;
                            }
                        }
                    }
                    let mut coordinator = server.coordinator.lock().await;
                    match request {
                        Ok(request) => {
                            coordinator
                                .dispatch(id, request, text.len() > FRAME_LIMIT)
                                .await
                        }
                        Err(error) => coordinator.send(id, error.frame(None)),
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    };
    // Keep the reader alive through in-flight commits, even if the writer fails.
    let writer_closed = closed.clone();
    let writer = tokio::spawn(async move {
        tokio::select! {
            _ = writer_closed.cancelled() => {},
            _ = writer => {},
        }
        writer_closed.cancel();
    });
    reader.await;
    closed.cancel();
    server.disconnect(id).await;
    let _ = writer.await;
}
