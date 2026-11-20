use serde_json::{Value, json};
use store::{EmbedTarget, State};

use crate::{
    embeds, extensions,
    policy::{DEFAULT_ROOM, user_visible, visible},
    wire::{Error, Params, Result, boolean, object, string},
};

pub(crate) struct MessageWrite {
    pub id: String,
    pub snapshot: Value,
    pub rooms: Vec<String>,
    pub grants: Vec<Value>,
}

pub(crate) fn write(
    state: &mut State,
    user_id: &str,
    params: &Params,
    config: &crate::service::ServerConfig,
    origin: &str,
) -> Result<MessageWrite> {
    if params.contains_key("message_id") && config.core_only {
        return Err(Error::Unsupported);
    }
    let existing_id = string(params, "message_id")?;
    let room = string(params, "room_id")?.unwrap_or(DEFAULT_ROOM);
    if (config.core_only && room != DEFAULT_ROOM) || !visible(state, user_id, room) {
        return Err(Error::Invalid);
    }
    let old = existing_id
        .map(|id| {
            let snapshot = state.messages.get(id).ok_or(Error::Invalid)?;
            let source = snapshot["room_id"].as_str().ok_or(Error::Internal)?;
            if !visible(state, user_id, source) {
                return Err(Error::Invalid);
            }
            if snapshot["from"]["user_id"] != user_id {
                return Err(Error::Denied);
            }
            Ok(snapshot.clone())
        })
        .transpose()?;
    let deleted = boolean(params, "deleted")?;
    if deleted && old.is_none() {
        return Err(Error::Invalid);
    }
    let mut snapshot = json!({"room_id": room});
    let mut rooms = vec![room.to_owned()];
    if let Some(old) = &old {
        snapshot["from"] = old["from"].clone();
        if !config.core_only {
            snapshot["prev_log_id"] = old["log_id"].clone();
        }
        let source = old["room_id"].as_str().ok_or(Error::Internal)?;
        if source != room {
            snapshot["prev_room_id"] = json!(source);
            rooms.push(source.to_owned());
        }
    } else {
        snapshot["from"] = crate::users::recorded(state, user_id)?;
    }
    if let Some(reply) = object(params, "reply_to")? {
        let target = string(reply, "message_id")?.ok_or(Error::Invalid)?;
        let message = state.messages.get(target).ok_or(Error::Invalid)?;
        let target_room = message["room_id"].as_str().ok_or(Error::Internal)?;
        if Some(target) == existing_id || !visible(state, user_id, target_room) {
            return Err(Error::Invalid);
        }
        snapshot["reply_to"] = json!({"message_id": target});
    }
    if deleted {
        snapshot["deleted"] = json!(true);
    } else {
        if let Some(params) = object(params, "body")? {
            snapshot["body"] = body(state, user_id, params, !config.core_only, old.is_none())?;
        } else if old.is_none() {
            return Err(Error::Invalid);
        }
        if let Some(ext) = old.as_ref().and_then(|old| old.get("ext")) {
            snapshot["ext"] = ext.clone();
        }
        extensions::merge(
            &mut snapshot,
            params,
            config.ext_limit.filter(|_| !config.core_only),
        )?;
    }

    let log_id = state.next_log_id().ok_or(Error::Internal)?;
    let id = existing_id.map_or_else(|| log_id.to_string(), str::to_owned);
    let grants = if let Some(embeds) = snapshot
        .get_mut("body")
        .and_then(|body| body.get_mut("embeds"))
        .and_then(Value::as_array_mut)
        && !config.core_only
    {
        embeds::prepare(
            state,
            EmbedTarget::Message(id.clone()),
            old.as_ref(),
            embeds,
            origin,
            config.grant_ttl_ms,
        )?
    } else {
        Vec::new()
    };
    if let Some(old) = &old {
        embeds::prune_message(state, &id, old, &snapshot, origin);
    }
    snapshot["message_id"] = json!(id);
    snapshot["log_id"] = json!(log_id.to_string());
    state.messages.insert(id.clone(), snapshot.clone());
    state.log(log_id, "message", rooms.clone(), snapshot.clone());
    Ok(MessageWrite {
        id,
        snapshot,
        rooms,
        grants,
    })
}

fn body(
    state: &State,
    actor: &str,
    params: &Params,
    writes: bool,
    creating: bool,
) -> Result<Value> {
    let text = string(params, "text")?.unwrap_or_default();
    let format = string(params, "format")?;
    if format.is_some_and(|format| !matches!(format, "plain" | "markdown")) {
        return Err(Error::Invalid);
    }
    let mut body = json!({"text": text});
    if let Some(format) = format {
        body["format"] = json!(format);
    }
    if let Some(mentions) = params.get("mentions") {
        let mentions = mentions.as_array().ok_or(Error::Invalid)?;
        for mention in mentions {
            let id = mention.as_str().ok_or(Error::Invalid)?;
            if !user_visible(state, actor, id) {
                return Err(Error::Invalid);
            }
        }
        body["mentions"] = json!(mentions);
    }
    let mut has_embeds = false;
    if let Some(embeds) = params.get("embeds") {
        let embeds = embeds.as_array().ok_or(Error::Invalid)?;
        if embeds.len() > 32 {
            return Err(Error::Denied);
        }
        let embeds = embeds
            .iter()
            .map(|embed| {
                let embed = embed.as_object().ok_or(Error::Invalid)?;
                let kind = string(embed, "kind")?.ok_or(Error::Invalid)?;
                if !matches!(kind, "html" | "iframe")
                    && !kind.starts_with("ext:")
                    && !(writes && embeds::KINDS.contains(&kind))
                {
                    return Err(Error::Invalid);
                }
                let mut public = json!({"kind": kind});
                for field in ["title", "html"] {
                    if let Some(value) = string(embed, field)? {
                        public[field] = json!(value);
                    }
                }
                if !embeds::KINDS.contains(&kind)
                    && let Some(url) = string(embed, "url")?
                {
                    public["url"] = json!(url);
                }
                if let Some(id) = embed.get("embed_id") {
                    public["embed_id"] = id.clone();
                }
                if let Some(og) = object(embed, "og")? {
                    public["og"] = json!(og);
                }
                if let Some(height) = embed.get("height") {
                    public["height"] = json!(height.as_f64().ok_or(Error::Invalid)?);
                }
                Ok(public)
            })
            .collect::<Result<Vec<_>>>()?;
        has_embeds = !embeds.is_empty();
        body["embeds"] = json!(embeds);
    }
    if creating && text.is_empty() && !has_embeds {
        return Err(Error::Invalid);
    }
    Ok(body)
}
