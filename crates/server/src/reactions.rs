use std::collections::BTreeSet;

use serde_json::{Value, json};
use store::State;

use crate::{
    policy::visible,
    users,
    wire::{Error, Params, Result, string},
};

pub(crate) fn set(state: &mut State, user: &str, params: &Params) -> Result<Value> {
    let message = string(params, "message_id")?.ok_or(Error::Invalid)?;
    let snapshot = state.messages.get(message).ok_or(Error::Invalid)?;
    let room = snapshot["room_id"]
        .as_str()
        .ok_or(Error::Internal)?
        .to_owned();
    if !visible(state, user, &room) || snapshot["deleted"] == true {
        return Err(Error::Invalid);
    }
    let entries = params
        .get("emojis")
        .and_then(Value::as_array)
        .ok_or(Error::Invalid)?;
    let mut normalized = BTreeSet::new();
    let mut emojis = Vec::new();
    for entry in entries {
        let emoji = entry.as_str().ok_or(Error::Invalid)?;
        if emoji.is_empty() || emoji.len() > 128 || emoji.chars().any(char::is_control) {
            return Err(Error::Invalid);
        }
        let key = emoji.replace('\u{fe0f}', "");
        if key.is_empty() {
            return Err(Error::Invalid);
        }
        if normalized.insert(key) {
            emojis.push(emoji);
        }
    }
    if emojis.len() > 32 {
        return Err(Error::Denied);
    }
    let set = json!({"from": users::recorded(state, user)?, "emojis": emojis});
    state
        .reactions
        .entry(message.to_owned())
        .or_default()
        .insert(user.to_owned(), set.clone());
    log(state, message, &room, vec![set])
}

pub(crate) fn moved(state: &mut State, message: &str, room: &str) -> Result<Option<Value>> {
    let sets = state
        .reactions
        .get(message)
        .into_iter()
        .flat_map(|sets| sets.values())
        .filter(|set| {
            set["emojis"]
                .as_array()
                .is_some_and(|emojis| !emojis.is_empty())
        })
        .cloned()
        .collect::<Vec<_>>();
    if sets.is_empty() {
        Ok(None)
    } else {
        log(state, message, room, sets).map(Some)
    }
}

fn log(state: &mut State, message: &str, room: &str, sets: Vec<Value>) -> Result<Value> {
    let id = state.next_log_id().ok_or(Error::Internal)?;
    let record = json!({
        "log_id": id.to_string(), "message_id": message, "room_id": room, "reactions": sets,
    });
    state.log(id, "reactions", vec![room.to_owned()], record.clone());
    Ok(record)
}

#[cfg(test)]
mod tests {
    //! §4.7: how a reaction set is normalized before it is stored.
    use super::*;

    fn state() -> State {
        let mut state = State::default();
        state
            .users
            .insert("ada".into(), json!({"user_id": "ada", "name": "Ada"}));
        state
            .rooms
            .insert("general".into(), json!({"room_id": "general"}));
        state.messages.insert(
            "1".into(),
            json!({"message_id": "1", "room_id": "general", "body": {"text": "hi"}}),
        );
        state
    }

    /// The stored emoji list after `ada` sets `emojis` on message 1.
    fn stored(emojis: &[&str]) -> Vec<String> {
        let mut state = state();
        let Value::Object(params) = json!({"message_id": "1", "emojis": emojis}) else {
            unreachable!()
        };
        let record = match set(&mut state, "ada", &params) {
            Ok(record) => record,
            Err(error) => panic!("{emojis:?}: {}", error.frame(None)),
        };
        assert_eq!(
            record["reactions"][0]["emojis"],
            state.reactions["1"]["ada"]["emojis"]
        );
        serde_json::from_value(record["reactions"][0]["emojis"].clone()).unwrap()
    }

    fn rejected(emojis: Value) -> i64 {
        let mut state = state();
        let Value::Object(params) = json!({"message_id": "1", "emojis": emojis}) else {
            unreachable!()
        };
        match set(&mut state, "ada", &params) {
            Ok(record) => panic!("accepted: {record}"),
            Err(error) => error.frame(None)["error"]["code"].as_i64().unwrap(),
        }
    }

    #[test]
    fn entries_that_differ_only_in_fe0f_are_one_entry() {
        assert_eq!(stored(&["❤️", "❤"]), ["❤️"]);
        assert_eq!(stored(&["☺", "☺️", "☺\u{fe0f}\u{fe0f}"]).len(), 1);
        assert_eq!(stored(&["🏳️‍🌈", "🏳‍🌈"]).len(), 1);
    }

    #[test]
    fn duplicates_collapse_keeping_the_order_of_first_appearance() {
        assert_eq!(stored(&["🎉", "👍", "🎉", "👀", "👍"]), ["🎉", "👍", "👀"]);
        assert_eq!(
            stored(&["👍🏽", "👍", "👍🏽"]),
            ["👍🏽", "👍"],
            "skin tones are distinct"
        );
        assert_eq!(
            stored(&[]),
            [] as [&str; 0],
            "an empty set clears the reaction"
        );
    }

    #[test]
    fn custom_keys_are_kept_as_written() {
        assert_eq!(stored(&[":partyparrot:", "👍"]), [":partyparrot:", "👍"]);
    }

    #[test]
    #[ignore = "feature: the server stores entries as sent, not fully-qualified (§4.7 SHOULD); see .tasks/03"]
    fn emoji_sequences_are_stored_fully_qualified() {
        // emoji-test.txt lists each of these as fully-qualified only with FE0F.
        assert_eq!(stored(&["❤"]), ["❤️"]);
        assert_eq!(stored(&["☺"]), ["☺️"]);
        assert_eq!(stored(&["🏳‍🌈"]), ["🏳️‍🌈"]);
        assert_eq!(stored(&["👍"]), ["👍"], "already fully-qualified");
    }

    #[test]
    fn malformed_entries_are_invalid_params() {
        for emojis in [
            json!([""]),
            json!(["\u{fe0f}"]),
            json!(["\n"]),
            json!([1]),
            json!("👍"),
            json!(["x".repeat(129)]),
        ] {
            assert_eq!(rejected(emojis.clone()), -32602, "{emojis}");
        }
        let many: Vec<String> = (0..33).map(|n| format!(":e{n}:")).collect();
        assert_eq!(rejected(json!(many)), -32001, "a count limit is denied");
    }
}
