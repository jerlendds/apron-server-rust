use std::{cmp::Reverse, collections::BTreeSet};

use serde_json::{Value, json};
use store::State;

use crate::{
    extensions,
    policy::{joined, visible},
    wire::{Error, Params, Result, boolean, string},
};

pub(crate) struct Change {
    pub room: String,
    pub record_changed: bool,
    pub membership: Option<Value>,
}

pub(crate) fn set(
    state: &mut State,
    actor: &str,
    params: &Params,
    ext_limit: Option<usize>,
) -> Result<Change> {
    let existing = string(params, "room_id")?;
    let parent = string(params, "parent_room_id")?;
    let private = params
        .contains_key("private")
        .then(|| boolean(params, "private"))
        .transpose()?;
    let mut room = if let Some(id) = existing {
        if !visible(state, actor, id) {
            return Err(Error::Invalid);
        }
        if !joined(state, actor, id) {
            return Err(Error::Denied);
        }
        let room = state.rooms.get(id).ok_or(Error::Invalid)?.clone();
        if parent.is_some_and(|parent| room["parent_room_id"] != parent)
            || private.is_some_and(|private| (room["private"] == true) != private)
        {
            return Err(Error::Invalid);
        }
        room
    } else {
        let mut room = json!({});
        if let Some(parent) = parent {
            if !visible(state, actor, parent) {
                return Err(Error::Invalid);
            }
            room["parent_room_id"] = json!(parent);
        }
        if private.unwrap_or_else(|| parent.is_some_and(|id| state.rooms[id]["private"] == true)) {
            room["private"] = json!(true);
        }
        room
    };
    for field in ["title", "description"] {
        if let Some(value) = string(params, field)? {
            if value.is_empty() {
                room.as_object_mut().ok_or(Error::Internal)?.remove(field);
            } else {
                room[field] = json!(value);
            }
        }
    }
    extensions::merge(&mut room, params, ext_limit)?;
    let id = state.next_log_id().ok_or(Error::Internal)?;
    let room_id = existing.map_or_else(|| id.to_string(), str::to_owned);
    if room["parent_room_id"].is_string() && room["title"].is_null() {
        room["title"] = json!("Thread");
    }
    if let Some(previous) = room.get("log_id") {
        room["prev_log_id"] = previous.clone();
    }
    room["room_id"] = json!(room_id);
    room["log_id"] = json!(id.to_string());
    state.rooms.insert(room_id.clone(), room.clone());
    state.log(id, "rooms", vec![room_id.clone()], room);
    let membership = if existing.is_none() {
        change_membership(state, &room_id, actor, true)?
    } else {
        None
    };
    Ok(Change {
        room: room_id,
        record_changed: true,
        membership,
    })
}

pub(crate) fn membership(
    state: &mut State,
    actor: &str,
    params: &Params,
    join: bool,
) -> Result<Vec<Change>> {
    let room = string(params, "room_id")?.ok_or(Error::Invalid)?;
    let target = string(params, "user_id")?.unwrap_or(actor);
    if !visible(state, actor, room) || !state.users.contains_key(target) {
        return Err(Error::Invalid);
    }
    if target != actor && !joined(state, actor, room) {
        return Err(Error::Denied);
    }
    if join && !ancestors_visible(state, target, &state.rooms[room]) {
        return Err(Error::Invalid);
    }
    let Some(membership) = change_membership(state, room, target, join)? else {
        return Ok(Vec::new());
    };
    let mut changes = vec![Change {
        room: room.to_owned(),
        record_changed: false,
        membership: Some(membership),
    }];
    if !join {
        // Losing access to a private ancestor also ends the hidden subscriptions.
        let hidden = state
            .members
            .iter()
            .filter(|(room, members)| members.contains(target) && !visible(state, target, room))
            .map(|(room, _)| room.clone())
            .collect::<Vec<_>>();
        for room in hidden {
            let membership = change_membership(state, &room, target, false)?;
            changes.push(Change {
                room,
                record_changed: false,
                membership,
            });
        }
    }
    Ok(changes)
}

fn ancestors_visible(state: &State, user: &str, room: &Value) -> bool {
    room["parent_room_id"]
        .as_str()
        .is_none_or(|parent| visible(state, user, parent))
}

pub(crate) fn change_membership(
    state: &mut State,
    room: &str,
    user: &str,
    join: bool,
) -> Result<Option<Value>> {
    let members = state.members.entry(room.to_owned()).or_default();
    let changed = if join {
        members.insert(user.to_owned())
    } else {
        members.remove(user)
    };
    if !changed {
        return Ok(None);
    }
    let id = state.next_log_id().ok_or(Error::Internal)?;
    let record = json!({
        "room_id": room, "log_id": id.to_string(),
        "members": [{"user": crate::users::recorded(state, user)?, "joined": join}]
    });
    state.log(id, "memberships", vec![room.to_owned()], record.clone());
    Ok(Some(record))
}

pub(crate) fn project(state: &State, room: &str, members: bool, only: Option<&str>) -> Value {
    let mut record = state.rooms[room].clone();
    let (first, last) = state.room_bounds(room);
    record["latest_log_id"] = json!(last.to_string());
    record["history_log_id"] = json!(first.map(|id| id.to_string()));
    if members {
        record["members"] = json!(
            state
                .members
                .get(room)
                .into_iter()
                .flatten()
                .filter(|user| only.is_none_or(|target| *user == target))
                .filter(|user| visible(state, user, room))
                .map(|id| json!({"user_id": id}))
                .collect::<Vec<_>>()
        );
    }
    record
}

pub(crate) fn list(state: &State, actor: &str, params: &Params) -> Result<Value> {
    let filter = string(params, "filter")?.unwrap_or("all");
    let (include_joined, include_other) = match filter {
        "all" => (true, true),
        "joined" => (true, false),
        "not_joined" => (false, true),
        _ => return Err(Error::Invalid),
    };
    let specific = string(params, "room_id")?;
    let parent = if specific.is_none() {
        string(params, "parent_room_id")?
    } else {
        None
    };
    if specific
        .or(parent)
        .is_some_and(|id| !visible(state, actor, id))
    {
        return Err(Error::Invalid);
    }
    let members = boolean(params, "members")?;
    let only = if members {
        string(params, "user_id")?
    } else {
        None
    };
    let mut own = Vec::new();
    let mut other = Vec::new();
    let mut users = BTreeSet::new();
    for (id, room) in &state.rooms {
        if !visible(state, actor, id)
            || specific.is_some_and(|specific| id != specific)
            || parent.is_some_and(|parent| room["parent_room_id"] != parent)
        {
            continue;
        }
        let output = if joined(state, actor, id) && include_joined {
            &mut own
        } else if !joined(state, actor, id)
            && include_other
            && (specific.is_some() || parent.is_some() || room["parent_room_id"].is_null())
        {
            &mut other
        } else {
            continue;
        };
        let projected = project(state, id, members, only);
        users.extend(
            projected["members"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| entry["user_id"].as_str())
                .map(str::to_owned),
        );
        output.push(projected);
    }
    let mut result = json!({});
    for (include, key, mut records) in [
        (include_joined, "joined", own),
        (include_other, "not_joined", other),
    ] {
        if include {
            records.sort_by_key(|room| {
                Reverse(
                    room["latest_log_id"]
                        .as_str()
                        .and_then(|id| id.parse::<u64>().ok()),
                )
            });
            result[key] = json!(records);
        }
    }
    if members {
        result["users"] = json!(
            users
                .into_iter()
                .filter_map(|id| state.users.get(&id))
                .collect::<Vec<_>>()
        );
    }
    Ok(result)
}
