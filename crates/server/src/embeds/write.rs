use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    extract::{Path, State as AxumState},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use store::{Asset, EmbedGrant, EmbedTarget, State, epoch_millis};

use crate::{
    auth::random_secret,
    embeds::{Live, avatar_media_type, stream, upload},
    service::{Coordinator, Server},
    wire::{Error, FRAME_LIMIT, Result},
};

const UPLOAD_LIMIT: usize = 2 * 1024 * 1024;
const STREAM_LIMIT: usize = 512 * 1024;
const KEPT_TEXT: usize = 64 * 1024;

type Failure = (StatusCode, &'static str);

fn missing() -> Failure {
    (
        StatusCode::NOT_FOUND,
        "Write grant is missing, expired, or already used",
    )
}

fn internal(_: Error) -> Failure {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Could not commit the embed",
    )
}

pub(crate) async fn claim(
    AxumState(server): AxumState<Arc<Server>>,
    Path(secret): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let media_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    // The worker owns the request body, so a writer disconnect still finalizes the grant.
    let result = tokio::spawn(write(server, secret, media_type, body)).await;
    let response = match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(failure)) => failure.into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Write worker failed").into_response(),
    };
    cors(response)
}

async fn write(
    server: Arc<Server>,
    secret: String,
    media_type: Option<String>,
    body: Body,
) -> std::result::Result<(), Failure> {
    let Started {
        grant,
        live,
        append_budget,
    } = start(&server, &secret).await?;
    let mut append = append_budget.map(stream::AppendBuffer::new);
    let limit = if grant.kind == "upload" {
        UPLOAD_LIMIT
    } else {
        STREAM_LIMIT
    };
    let deadline = tokio::time::sleep(Duration::from_secs(120));
    tokio::pin!(deadline);
    let mut bytes = Vec::new();
    let mut stream = body.into_data_stream();
    let mut failure = None;
    let mut received = 0;
    loop {
        if append.as_ref().is_some_and(|append| append.full) {
            failure = Some((
                StatusCode::PAYLOAD_TOO_LARGE,
                "Append text reached the server limit",
            ));
            break;
        }
        let next = tokio::select! {
            _ = server.shutdown.cancelled() => {
                failure = Some((StatusCode::SERVICE_UNAVAILABLE, "Server is shutting down"));
                break;
            },
            _ = &mut deadline => {
                failure = Some((StatusCode::REQUEST_TIMEOUT, "Write timed out"));
                break;
            },
            _ = wait_for_cancel(live.as_ref()) => return Err(missing()),
            next = stream.next() => next,
        };
        let Some(next) = next else { break };
        let chunk = match next {
            Ok(chunk) => chunk,
            Err(_) => {
                failure = Some((StatusCode::BAD_REQUEST, "Write transport ended early"));
                break;
            }
        };
        let allowed = chunk.len().min(limit - received);
        received += allowed;
        if let Some(append) = &mut append {
            let kept = append.accept(&chunk[..allowed], false);
            bytes.extend_from_slice(&kept);
            if let Some(live) = &live {
                live.push(&kept);
            }
        } else {
            bytes.extend_from_slice(&chunk[..allowed]);
            if let Some(live) = &live {
                live.push(&chunk[..allowed]);
            }
        }
        if allowed < chunk.len() {
            failure = Some((
                StatusCode::PAYLOAD_TOO_LARGE,
                "Write exceeds the server limit",
            ));
            break;
        }
    }
    if let Some(append) = &mut append {
        let kept = append.accept(&[], true);
        bytes.extend_from_slice(&kept);
        if let Some(live) = &live {
            live.push(&kept);
        }
    }
    if matches!(grant.target, EmbedTarget::Avatar(_)) && avatar_media_type(&bytes).is_none() {
        failure.get_or_insert((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Avatar must be PNG, JPEG, GIF, or WebP",
        ));
    }
    let payload = if failure.is_none() || grant.kind != "upload" {
        Some((bytes, media_type))
    } else {
        None
    };
    let mut coordinator = server.coordinator.lock().await;
    let finished = finish(&mut coordinator, &secret, payload)
        .await
        .map_err(internal)?;
    if !finished {
        return Err(missing());
    }
    failure.map_or(Ok(()), Err)
}

async fn wait_for_cancel(live: Option<&Arc<Live>>) {
    match live {
        Some(live) => live.cancelled.cancelled().await,
        None => std::future::pending().await,
    }
}

struct Started {
    grant: EmbedGrant,
    live: Option<Arc<Live>>,
    append_budget: Option<usize>,
}

async fn start(server: &Server, secret: &str) -> std::result::Result<Started, Failure> {
    let mut coordinator = server.coordinator.lock().await;
    let mut proposed = coordinator.state.clone();
    let grant = proposed.embed_grants.get_mut(secret).ok_or_else(missing)?;
    if grant.claimed || grant.expires <= epoch_millis() {
        return Err(missing());
    }
    grant.claimed = true;
    grant.expires = epoch_millis().saturating_add(125_000);
    let grant = grant.clone();
    let mut update = None;
    let mut live = None;
    let mut append_budget = None;
    if grant.kind != "upload" {
        let EmbedTarget::Message(message) = &grant.target else {
            return Err(missing());
        };
        let mut snapshot = proposed
            .messages
            .get(message)
            .cloned()
            .ok_or_else(missing)?;
        let index = embed_index(&snapshot, &grant.embed_id).ok_or_else(missing)?;
        let key = random_secret().map_err(internal)?;
        snapshot["body"]["embeds"][index]["url"] =
            json!(format!("{}/live/{key}", coordinator.origin));
        if grant.kind == "append" {
            append_budget = Some(text_budget(&snapshot));
        }
        update = Some(save(&mut proposed, message, snapshot).map_err(internal)?);
        live = Some((key, Arc::new(Live::new())));
    }
    coordinator.commit(proposed).await.map_err(internal)?;
    let live = live.map(|(key, live)| {
        coordinator.live.insert(key, live.clone());
        live
    });
    if let Some((room, update)) = update {
        coordinator.room_event("message", &[room], &update);
    }
    Ok(Started {
        grant,
        live,
        append_budget,
    })
}

pub(crate) async fn finish(
    coordinator: &mut Coordinator,
    secret: &str,
    payload: Option<(Vec<u8>, Option<String>)>,
) -> Result<bool> {
    let mut proposed = coordinator.state.clone();
    let Some(grant) = proposed.embed_grants.remove(secret) else {
        return Ok(false);
    };
    let mut message_update = None;
    let mut user_update = None;
    let mut live_key = None;
    match &grant.target {
        EmbedTarget::Message(message) => {
            if let Some(mut snapshot) = proposed.messages.get(message).cloned()
                && let Some(index) = embed_index(&snapshot, &grant.embed_id)
            {
                live_key = snapshot["body"]["embeds"][index]["url"]
                    .as_str()
                    .and_then(|url| url.rsplit_once("/live/"))
                    .map(|(_, key)| key.to_owned());
                match (grant.kind.as_str(), payload) {
                    ("upload", Some((bytes, media_type))) => {
                        let title = snapshot["body"]["embeds"][index]["title"]
                            .as_str()
                            .map(str::to_owned);
                        let url = put_asset(&mut proposed, &coordinator.origin, bytes, media_type)?;
                        snapshot["body"]["embeds"][index]["url"] = json!(url);
                        if snapshot["body"]["embeds"][index].get("og").is_none()
                            && let Some(title) = title
                        {
                            snapshot["body"]["embeds"][index]["og"] = json!({"title": title});
                        }
                    }
                    ("terminal", Some((bytes, _))) => {
                        let limit = text_budget(&snapshot).min(KEPT_TEXT);
                        let embed = snapshot["body"]["embeds"][index]
                            .as_object_mut()
                            .ok_or(Error::Internal)?;
                        embed.remove("url");
                        embed.insert(
                            "output".into(),
                            json!(stream::terminal_output(&bytes, limit)),
                        );
                    }
                    (kind, payload) => {
                        if let ("append", Some((bytes, _))) = (kind, payload) {
                            let base = snapshot["body"]["text"].as_str().unwrap_or_default();
                            snapshot["body"]["text"] =
                                json!(stream::append_text(base, &bytes, text_budget(&snapshot)));
                        }
                        snapshot["body"]["embeds"]
                            .as_array_mut()
                            .ok_or(Error::Internal)?
                            .remove(index);
                    }
                }
                message_update = Some(save(&mut proposed, message, snapshot)?);
            }
        }
        EmbedTarget::Avatar(user) => {
            if let Some((bytes, media_type)) = payload {
                let url = put_asset(&mut proposed, &coordinator.origin, bytes, media_type)?;
                let profile = proposed.users.get_mut(user).ok_or(Error::Internal)?;
                profile["avatar"] = json!(url);
                user_update = Some((user.clone(), profile.clone()));
                let key = super::asset_key(&url, &coordinator.origin).ok_or(Error::Internal)?;
                if let Some(previous) = proposed
                    .avatar_assets
                    .insert(user.to_owned(), key.to_owned())
                {
                    proposed.assets.remove(&previous);
                }
            }
        }
    }
    coordinator.commit(proposed).await?;
    if let Some(key) = live_key {
        coordinator.live.remove(&key);
    }
    if let Some((room, update)) = message_update {
        coordinator.room_event("message", &[room], &update);
    }
    if let Some((user, update)) = user_update {
        coordinator.user_event(&user, &update);
    }
    Ok(true)
}

fn put_asset(
    state: &mut State,
    origin: &str,
    bytes: Vec<u8>,
    media_type: Option<String>,
) -> Result<String> {
    let key = random_secret()?;
    let media_type = upload::safe_media_type(media_type.as_deref(), &bytes);
    state
        .assets
        .insert(key.clone(), Asset { bytes, media_type });
    Ok(format!("{origin}/f/{key}"))
}

fn save(state: &mut State, message: &str, mut snapshot: Value) -> Result<(String, Value)> {
    let room = snapshot["room_id"]
        .as_str()
        .ok_or(Error::Internal)?
        .to_owned();
    let id = state.next_log_id().ok_or(Error::Internal)?;
    snapshot["prev_log_id"] = snapshot["log_id"].clone();
    snapshot["log_id"] = json!(id.to_string());
    snapshot
        .as_object_mut()
        .ok_or(Error::Internal)?
        .remove("prev_room_id");
    state.messages.insert(message.to_owned(), snapshot.clone());
    state.log(id, "message", vec![room.clone()], snapshot.clone());
    Ok((room, snapshot))
}

fn text_budget(snapshot: &Value) -> usize {
    // Reserve framing, completion metadata and the longest JSON escape per retained byte.
    (FRAME_LIMIT - 4096).saturating_sub(snapshot.to_string().len()) / 6
}

fn embed_index(snapshot: &Value, id: &str) -> Option<usize> {
    snapshot["body"]["embeds"]
        .as_array()?
        .iter()
        .position(|embed| embed["embed_id"] == id)
}

pub(crate) async fn expire(coordinator: &mut Coordinator, recover: bool) -> Result<()> {
    let now = epoch_millis();
    let expired = coordinator
        .state
        .embed_grants
        .iter()
        .filter(|(_, grant)| (grant.claimed && recover) || grant.expires <= now)
        .map(|(secret, _)| secret.clone())
        .collect::<Vec<_>>();
    for secret in expired {
        finish(coordinator, &secret, None).await?;
    }
    Ok(())
}

pub(crate) async fn read_live(
    AxumState(server): AxumState<Arc<Server>>,
    Path(key): Path<String>,
) -> Response {
    let coordinator = server.coordinator.lock().await;
    let Some(live) = coordinator.live.get(&key) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut response = live.body().into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-transform"),
    );
    cors(response)
}

pub(crate) async fn read_asset(
    AxumState(server): AxumState<Arc<Server>>,
    Path(key): Path<String>,
) -> Response {
    let coordinator = server.coordinator.lock().await;
    let Some(asset) = coordinator.state.assets.get(&key) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut response = asset.bytes.clone().into_response();
    let media_type = HeaderValue::from_str(&asset.media_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, media_type);
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    cors(response)
}

pub(crate) async fn write_options() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("PUT, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    cors(response)
}

fn cors(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

#[cfg(test)]
mod tests {
    //! §4.8.5: the text that an append keeps fits with `body.text` in a frame.
    use super::{FRAME_LIMIT, stream, text_budget};
    use serde_json::json;

    fn snapshot(base: &str) -> serde_json::Value {
        json!({
            "method": "message",
            "params": {
                "message_id": "1724803600000", "log_id": "1724803600000", "room_id": "general",
                "from": {"user_id": "planner", "name": "Planner"},
                "body": {"text": base, "format": "markdown",
                         "embeds": [{"embed_id": "embed_1300", "kind": "append", "url": "https://chat.example/a/x"}]}
            }
        })
    }

    #[test]
    fn worst_case_escaping_of_the_kept_text_stays_within_the_frame_limit() {
        for base in [
            "",
            "Here is the plan:\n\n",
            &"\"".repeat(20_000),
            &"x".repeat(200_000),
        ] {
            let mut finished = snapshot(base);
            let budget = text_budget(&finished["params"]);
            // U+0001 is one byte that JSON escapes to six (\u0001).
            let raw = vec![0x01u8; budget + 1_000];
            let text = stream::append_text(base, &raw, budget);
            assert_eq!(text.len(), base.len() + budget, "the budget is filled");
            finished["params"]["body"]["text"] = json!(text);
            finished["params"]["body"]
                .as_object_mut()
                .unwrap()
                .remove("embeds");
            finished["params"]["log_id"] = json!("9007199254740991");
            finished["params"]["prev_log_id"] = json!("9007199254740990");
            let size = finished.to_string().len();
            assert!(
                size <= FRAME_LIMIT,
                "base {} bytes: frame is {size}",
                base.len()
            );
        }
    }

    #[test]
    fn a_body_that_already_fills_the_frame_leaves_no_budget() {
        let full = snapshot(&"x".repeat(FRAME_LIMIT));
        assert_eq!(text_budget(&full["params"]), 0);
    }
}
