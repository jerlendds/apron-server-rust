use serde_json::{Value, json};
use store::{EmbedTarget, State};

use crate::{
    embeds,
    policy::{DEFAULT_ROOM, visible},
    wire::{Error, Params, Result, object, string},
};

pub(crate) fn run(
    state: &mut State,
    user: &str,
    params: &Params,
    origin: &str,
    ttl_ms: u64,
) -> Result<(Value, Option<Value>)> {
    if params.contains_key("message_id") || params.contains_key("deleted") {
        return Err(Error::Invalid);
    }
    let room = string(params, "room_id")?.unwrap_or(DEFAULT_ROOM);
    if !visible(state, user, room) {
        return Err(Error::Invalid);
    }
    if let Some(reply) = object(params, "reply_to")? {
        let message = string(reply, "message_id")?.ok_or(Error::Invalid)?;
        let snapshot = state.messages.get(message).ok_or(Error::Invalid)?;
        let room = snapshot["room_id"].as_str().ok_or(Error::Internal)?;
        if !visible(state, user, room) {
            return Err(Error::Invalid);
        }
    }
    let body = object(params, "body")?.ok_or(Error::Invalid)?;
    match string(body, "text")?.map(str::trim) {
        Some("/help") => Ok((
            json!({}),
            Some(json!({
                "room_id": room,
                "from": {"user_id": "~private", "name": "Only you"},
                "body": {
                    "text": "- `/help` shows the available commands.\n- `/avatar` with one image upload sets your avatar.",
                    "format": "markdown"
                }
            })),
        )),
        Some("/avatar") => {
            let entries = body
                .get("embeds")
                .and_then(Value::as_array)
                .ok_or(Error::Invalid)?;
            if entries.len() != 1 || entries[0]["kind"] != "upload" {
                return Err(Error::Invalid);
            }
            // Keep only the most recent pending avatar write for this account.
            let target = EmbedTarget::Avatar(user.to_owned());
            state.embed_grants.retain(|_, grant| grant.target != target);
            let mut upload = [json!({"kind": "upload"})];
            let grants = embeds::prepare(state, target, None, &mut upload, origin, ttl_ms)?;
            Ok((json!({"embeds": grants}), None))
        }
        _ => Err(Error::Invalid),
    }
}
