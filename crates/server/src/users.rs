use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use store::State;

use crate::{
    extensions,
    wire::{Error, Params, Result, string},
};

pub(crate) fn me(
    state: &mut State,
    user_id: &str,
    params: &Params,
    ext_limit: Option<usize>,
    status: bool,
) -> Result<(Value, Option<Value>)> {
    let user = state.users.get_mut(user_id).ok_or(Error::Internal)?;
    let previous = user.clone();
    if let Some(name) = string(params, "name")? {
        if name.len() > 512 {
            return Err(Error::TooLarge);
        }
        user["name"] = json!(name);
    }
    if let Some(avatar) = string(params, "avatar")? {
        if avatar.len() > 16 * 1024 {
            return Err(Error::TooLarge);
        }
        if !avatar.is_empty() && !valid_avatar(avatar) {
            return Err(Error::Invalid);
        }
        user["avatar"] = json!(avatar);
    }
    if status && let Some(chosen) = string(params, "status")? {
        let chosen = if matches!(chosen, "" | "online") || crate::status::OPTIONAL.contains(&chosen)
        {
            chosen
        } else {
            ""
        };
        user["status"] = json!(chosen);
    }
    extensions::merge(user, params, ext_limit)?;
    let changed = if previous == *user {
        None
    } else {
        let mut update = user.clone();
        if previous.get("ext").is_some() {
            let mut ext = update["ext"].as_object().cloned().unwrap_or_default();
            if let Some(before) = previous["ext"].as_object() {
                for key in before.keys() {
                    ext.entry(key.clone()).or_insert_with(|| json!(""));
                }
            }
            update["ext"] = json!(ext);
        }
        Some(update)
    };
    let result = json!({"you": user});
    if params.contains_key("avatar") {
        state.embed_grants.retain(|_, grant| !matches!(&grant.target, store::EmbedTarget::Avatar(owner) if owner == user_id));
        if previous["avatar"] != result["you"]["avatar"]
            && let Some(asset) = state.avatar_assets.remove(user_id)
        {
            state.assets.remove(&asset);
        }
    }
    Ok((result, changed))
}

pub(crate) fn recorded(state: &State, user: &str) -> Result<Value> {
    let mut profile = state.users.get(user).ok_or(Error::Internal)?.clone();
    if let Some(fields) = profile.as_object_mut() {
        fields.remove("status");
        fields.remove("seen");
    }
    Ok(profile)
}

fn valid_avatar(avatar: &str) -> bool {
    if let Some(data) = avatar.strip_prefix("data:image/") {
        return data.split_once(";base64,").is_some_and(|(kind, bytes)| {
            matches!(kind, "png" | "jpeg" | "gif" | "webp")
                && STANDARD.decode(bytes).is_ok_and(|bytes| {
                    crate::embeds::avatar_media_type(&bytes)
                        .is_some_and(|detected| detected.strip_prefix("image/") == Some(kind))
                })
        });
    }
    url::Url::parse(avatar).is_ok_and(|url| {
        url.scheme() == "https"
            && url.has_host()
            && url.username().is_empty()
            && url.password().is_none()
    })
}
