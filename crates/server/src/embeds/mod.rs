mod stream;
mod upload;
mod write;

pub(crate) use stream::Live;
pub(crate) use upload::avatar_media_type;
pub(crate) use write::{claim, expire, read_asset, read_live, write_options};

use std::collections::BTreeSet;

use serde_json::{Value, json};
use store::{EmbedGrant, EmbedTarget, State, epoch_millis};

use crate::{
    auth::random_secret,
    wire::{Error, Result},
};

pub(crate) const KINDS: &[&str] = &["upload", "append", "terminal"];

pub(crate) fn prepare(
    state: &mut State,
    target: EmbedTarget,
    previous: Option<&Value>,
    embeds: &mut [Value],
    origin: &str,
    ttl_ms: u64,
) -> Result<Vec<Value>> {
    let mut grants = Vec::new();
    let mut seen = BTreeSet::new();
    let mut append = false;
    for embed in embeds {
        let kind = embed["kind"].as_str().ok_or(Error::Invalid)?.to_owned();
        if kind == "append" && std::mem::replace(&mut append, true) {
            return Err(Error::Invalid);
        }
        let fields = embed.as_object_mut().ok_or(Error::Invalid)?;
        fields.remove("write_url");
        fields.remove("output");
        if KINDS.contains(&kind.as_str()) {
            fields.remove("url");
        }
        if let Some((old, id)) = previous.zip(fields.get("embed_id")) {
            let id = id.as_str().ok_or(Error::Invalid)?;
            if !seen.insert(id.to_owned()) {
                return Err(Error::Invalid);
            }
            let old = old["body"]["embeds"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|old| old["embed_id"] == id && old["kind"] == kind)
                .ok_or(Error::Invalid)?;
            if KINDS.contains(&kind.as_str()) {
                for key in ["url", "output"] {
                    if let Some(value) = old.get(key) {
                        fields.insert(key.into(), value.clone());
                    }
                }
                if !fields.contains_key("og")
                    && let Some(value) = old.get("og")
                {
                    fields.insert("og".into(), value.clone());
                }
            }
            continue;
        }
        let id = format!("embed_{}", random_secret()?);
        fields.insert("embed_id".into(), json!(id));
        if KINDS.contains(&kind.as_str()) {
            let secret = random_secret()?;
            state.embed_grants.insert(
                secret.clone(),
                EmbedGrant {
                    embed_id: id.clone(),
                    kind: kind.clone(),
                    target: target.clone(),
                    expires: epoch_millis().saturating_add(ttl_ms),
                    claimed: false,
                },
            );
            grants.push(
                json!({"embed_id": id, "kind": kind, "write_url": format!("{origin}/w/{secret}")}),
            );
        }
    }
    Ok(grants)
}

pub(crate) fn prune_message(
    state: &mut State,
    id: &str,
    old: &Value,
    current: &Value,
    origin: &str,
) {
    let retained = current["body"]["embeds"].as_array();
    for embed in old["body"]["embeds"].as_array().into_iter().flatten() {
        if retained.is_some_and(|embeds| {
            embeds
                .iter()
                .any(|new| new["embed_id"] == embed["embed_id"])
        }) {
            continue;
        }
        let Some(id) = embed["embed_id"].as_str() else {
            continue;
        };
        state.embed_grants.retain(|_, grant| {
            grant.embed_id != id
                || grant.target
                    != EmbedTarget::Message(old["message_id"].as_str().unwrap_or_default().into())
        });
        if embed["kind"] == "upload"
            && let Some(key) = asset_key(embed["url"].as_str().unwrap_or_default(), origin)
        {
            state.assets.remove(key);
        }
    }
    debug_assert_eq!(old["message_id"], id);
}

pub(crate) fn asset_key<'a>(url: &'a str, origin: &str) -> Option<&'a str> {
    url.strip_prefix(origin)?.strip_prefix("/f/")
}

pub(crate) fn retry_result(state: &State, mut result: Value) -> Value {
    if let Some(embeds) = result.get_mut("embeds").and_then(Value::as_array_mut) {
        let now = epoch_millis();
        embeds.retain(|embed| {
            embed["write_url"]
                .as_str()
                .and_then(|url| url.rsplit('/').next())
                .and_then(|key| state.embed_grants.get(key))
                .is_some_and(|grant| !grant.claimed && grant.expires > now)
        });
        if embeds.is_empty() {
            result
                .as_object_mut()
                .expect("a result is an object")
                .remove("embeds");
        }
    }
    result
}
