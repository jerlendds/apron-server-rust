use serde_json::{Value, json};
use store::{Record, State};

use crate::{
    policy::{joined, visible},
    wire::{Error, Params, Result, string},
};

pub(crate) fn query(state: &State, user: &str, params: &Params) -> Result<Value> {
    let room = string(params, "room_id")?;
    if room.is_some_and(|room| !visible(state, user, room)) {
        return Err(Error::Invalid);
    }
    let after = bound(params, "after")?;
    let before = bound(params, "before")?;
    let limit = params
        .get("limit")
        .map(|value| {
            value
                .as_u64()
                .filter(|&value| value > 0)
                .ok_or(Error::Invalid)
        })
        .transpose()?
        .unwrap_or(100)
        .min(500) as usize;
    let (first, latest) = room.map_or_else(
        || (state.records.keys().next().copied(), state.watermark),
        |room| state.room_bounds(room),
    );
    let mut result = json!({
        "history_log_id": first.map(|id| id.to_string()),
        "latest_log_id": latest.to_string(),
        "more": false,
    });
    let start = after.unwrap_or(0);
    let end = before.unwrap_or(u64::MAX);
    if start > end {
        return Ok(result);
    }
    let records: Box<dyn DoubleEndedIterator<Item = (u64, &Record)>> = if let Some(room) = room {
        Box::new(state.room_records(room, start, end))
    } else {
        Box::new(
            state
                .records
                .range(start..=end)
                .filter_map(|(&id, record)| {
                    record
                        .rooms
                        .iter()
                        .any(|room| joined(state, user, room))
                        .then_some((id, record))
                }),
        )
    };
    let records = records.filter(|(_, record)| accessible_record(state, user, record));
    let mut page = if after.is_some() {
        records.take(limit + 1).collect::<Vec<_>>()
    } else {
        records.rev().take(limit + 1).collect()
    };
    result["more"] = json!(page.len() > limit);
    page.truncate(limit);
    if after.is_none() {
        page.reverse();
    }
    if let (Some((first, _)), Some((last, _))) = (page.first(), page.last()) {
        result["first_log_id"] = json!(first.to_string());
        result["last_log_id"] = json!(last.to_string());
    }
    for (_, record) in page {
        let kind = if record.kind == "message" {
            "messages"
        } else {
            &record.kind
        };
        let entries = result
            .as_object_mut()
            .ok_or(Error::Internal)?
            .entry(kind.to_owned())
            .or_insert_with(|| json!([]));
        entries
            .as_array_mut()
            .ok_or(Error::Internal)?
            .push(record.value.clone());
    }
    Ok(result)
}

fn accessible_record(state: &State, user: &str, record: &Record) -> bool {
    record.rooms.iter().all(|room| visible(state, user, room))
}

pub(crate) fn bound(params: &Params, key: &str) -> Result<Option<u64>> {
    string(params, key)?
        .map(|id| {
            if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(Error::Invalid);
            }
            id.parse::<u64>()
                .ok()
                .filter(|&value| value < (1 << 53))
                .ok_or(Error::Invalid)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    //! §4.3.1–4.3.2: bounds, direction, limits, `more`, and room scoping.
    use super::*;

    const USER: &str = "ada";

    /// Public rooms `a` and `b` that `ada` joined, plus `hidden`, a private
    /// room that she did not.
    fn state() -> State {
        let mut state = State::default();
        for room in ["a", "b", "hidden"] {
            state.rooms.insert(room.into(), json!({"room_id": room}));
        }
        state.rooms.get_mut("hidden").unwrap()["private"] = json!(true);
        for room in ["a", "b"] {
            state
                .members
                .entry(room.into())
                .or_default()
                .insert(USER.into());
        }
        state
            .members
            .entry("hidden".into())
            .or_default()
            .insert("bob".into());
        state
    }

    fn append(state: &mut State, kind: &str, rooms: &[&str]) -> u64 {
        let id = state.next_log_id().unwrap();
        let rooms = rooms.iter().map(|room| room.to_string()).collect();
        state.log(
            id,
            kind,
            rooms,
            json!({"log_id": id.to_string(), "kind": kind}),
        );
        id
    }

    fn query_ok(state: &State, params: Value) -> Value {
        let Value::Object(params) = params else {
            panic!("params object")
        };
        match query(state, USER, &params) {
            Ok(result) => result,
            Err(error) => panic!("query failed: {}", error.frame(None)),
        }
    }

    fn error_code(state: &State, params: Value) -> i64 {
        let Value::Object(params) = params else {
            panic!("params object")
        };
        match query(state, USER, &params) {
            Ok(result) => panic!("expected an error, got {result}"),
            Err(error) => error.frame(None)["error"]["code"].as_i64().unwrap(),
        }
    }

    /// Every record in the page, across the per-kind arrays, by log_id.
    fn page(result: &Value) -> Vec<u64> {
        let mut ids: Vec<u64> = ["rooms", "messages", "reactions", "memberships"]
            .iter()
            .flat_map(|kind| result[*kind].as_array().cloned().unwrap_or_default())
            .map(|record| record["log_id"].as_str().unwrap().parse().unwrap())
            .collect();
        ids.sort_unstable();
        ids
    }

    fn bounds(result: &Value) -> (Option<u64>, Option<u64>) {
        let parse = |key: &str| result[key].as_str().map(|id| id.parse().unwrap());
        (parse("first_log_id"), parse("last_log_id"))
    }

    fn id(value: u64) -> String {
        value.to_string()
    }

    #[test]
    fn after_and_before_are_inclusive_and_either_can_be_omitted() {
        let mut state = state();
        let ids: Vec<u64> = (0..5)
            .map(|_| append(&mut state, "message", &["a"]))
            .collect();

        let both = query_ok(
            &state,
            json!({"room_id": "a", "after": id(ids[1]), "before": id(ids[3])}),
        );
        assert_eq!(page(&both), ids[1..=3]);
        let only_after = query_ok(&state, json!({"room_id": "a", "after": id(ids[3])}));
        assert_eq!(page(&only_after), ids[3..]);
        let only_before = query_ok(&state, json!({"room_id": "a", "before": id(ids[1])}));
        assert_eq!(page(&only_before), ids[..=1]);
        let neither = query_ok(&state, json!({"room_id": "a"}));
        assert_eq!(page(&neither), ids);
        let single = query_ok(
            &state,
            json!({"room_id": "a", "after": id(ids[2]), "before": id(ids[2])}),
        );
        assert_eq!(
            page(&single),
            [ids[2]],
            "after == before fetches one record (§4.3.1)"
        );
    }

    #[test]
    fn with_after_the_page_runs_forward_otherwise_it_ends_at_the_newest() {
        let mut state = state();
        let ids: Vec<u64> = (0..6)
            .map(|_| append(&mut state, "message", &["a"]))
            .collect();

        let forward = query_ok(
            &state,
            json!({"room_id": "a", "after": id(ids[0]), "limit": 2}),
        );
        assert_eq!(page(&forward), ids[..2]);
        assert_eq!(bounds(&forward), (Some(ids[0]), Some(ids[1])));
        let backward = query_ok(&state, json!({"room_id": "a", "limit": 2}));
        assert_eq!(page(&backward), ids[4..]);
        assert_eq!(bounds(&backward), (Some(ids[4]), Some(ids[5])));
        let backward_from = query_ok(
            &state,
            json!({"room_id": "a", "before": id(ids[3]), "limit": 2}),
        );
        assert_eq!(page(&backward_from), ids[2..=3]);
    }

    #[test]
    fn limit_counts_changes_of_every_kind() {
        let mut state = state();
        let ids = [
            append(&mut state, "rooms", &["a"]),
            append(&mut state, "message", &["a"]),
            append(&mut state, "reactions", &["a"]),
            append(&mut state, "memberships", &["a"]),
        ];
        let result = query_ok(&state, json!({"room_id": "a", "after": "0", "limit": 3}));
        assert_eq!(page(&result), ids[..3]);
        assert_eq!(result["rooms"].as_array().unwrap().len(), 1);
        assert_eq!(result["messages"].as_array().unwrap().len(), 1);
        assert_eq!(result["reactions"].as_array().unwrap().len(), 1);
        assert!(
            result.get("memberships").is_none(),
            "an empty array may be omitted"
        );
        assert_eq!(result["more"], true);
    }

    #[test]
    fn more_is_exact_in_both_directions() {
        let mut state = state();
        let ids: Vec<u64> = (0..4)
            .map(|_| append(&mut state, "message", &["a"]))
            .collect();
        let cases = [
            (
                json!({"room_id": "a", "after": id(ids[0]), "limit": 3}),
                true,
            ),
            (
                json!({"room_id": "a", "after": id(ids[0]), "limit": 4}),
                false,
            ),
            (
                json!({"room_id": "a", "after": id(ids[1]), "limit": 3}),
                false,
            ),
            (json!({"room_id": "a", "limit": 3}), true),
            (json!({"room_id": "a", "limit": 4}), false),
            (
                json!({"room_id": "a", "before": id(ids[2]), "limit": 3}),
                false,
            ),
            (
                json!({"room_id": "a", "before": id(ids[2]), "limit": 2}),
                true,
            ),
        ];
        for (params, more) in cases {
            assert_eq!(query_ok(&state, params.clone())["more"], more, "{params}");
        }
    }

    #[test]
    fn an_empty_slice_has_no_bounds_and_no_more() {
        let mut state = state();
        let last = append(&mut state, "message", &["a"]);
        for params in [
            json!({"room_id": "a", "after": id(last + 1)}),
            json!({"room_id": "a", "before": id(last - 1)}),
            json!({"room_id": "a", "after": id(last), "before": id(last - 1)}),
            json!({"room_id": "b"}),
        ] {
            let result = query_ok(&state, params.clone());
            assert_eq!(result["more"], false, "{params}");
            assert_eq!(bounds(&result), (None, None), "{params}");
            assert_eq!(page(&result), [] as [u64; 0], "{params}");
            assert_eq!(
                result["latest_log_id"],
                id(state.watermark).as_str(),
                "{params}"
            );
        }
    }

    #[test]
    fn continuation_from_the_bounds_visits_every_record_once() {
        let mut state = state();
        let ids: Vec<u64> = (0..7)
            .map(|n| {
                append(
                    &mut state,
                    ["message", "reactions", "memberships"][n % 3],
                    &["a"],
                )
            })
            .collect();

        let (mut seen, mut after) = (vec![], ids[0]);
        loop {
            let result = query_ok(
                &state,
                json!({"room_id": "a", "after": id(after), "limit": 3}),
            );
            seen.extend(page(&result));
            if result["more"] == false {
                break;
            }
            after = bounds(&result).1.unwrap() + 1;
        }
        assert_eq!(seen, ids);

        let (mut seen, mut before) = (vec![], None);
        loop {
            let mut params = json!({"room_id": "a", "limit": 3});
            if let Some(before) = before {
                params["before"] = json!(id(before));
            }
            let result = query_ok(&state, params);
            seen.splice(0..0, page(&result));
            if result["more"] == false {
                break;
            }
            before = Some(bounds(&result).0.unwrap() - 1);
        }
        assert_eq!(seen, ids);
    }

    #[test]
    fn each_record_array_is_in_ascending_log_id_order() {
        let mut state = state();
        for n in 0..9 {
            append(
                &mut state,
                ["message", "reactions", "memberships"][n % 3],
                &["a"],
            );
        }
        for params in [
            json!({"room_id": "a", "limit": 9}),
            json!({"room_id": "a", "after": "0"}),
        ] {
            let result = query_ok(&state, params);
            for kind in ["messages", "reactions", "memberships"] {
                let ids: Vec<u64> = result[kind]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|record| record["log_id"].as_str().unwrap().parse().unwrap())
                    .collect();
                assert_eq!(ids.len(), 3);
                assert!(
                    ids.windows(2).all(|pair| pair[0] < pair[1]),
                    "{kind}: {ids:?}"
                );
            }
        }
    }

    #[test]
    fn a_room_query_returns_only_its_records_including_moves() {
        let mut state = state();
        let in_a = append(&mut state, "message", &["a"]);
        let in_b = append(&mut state, "message", &["b"]);
        let moved = append(&mut state, "message", &["a", "b"]);
        let after_move = append(&mut state, "reactions", &["b"]);

        assert_eq!(
            page(&query_ok(&state, json!({"room_id": "a"}))),
            [in_a, moved]
        );
        assert_eq!(
            page(&query_ok(&state, json!({"room_id": "b"}))),
            [in_b, moved, after_move]
        );
        let source = query_ok(&state, json!({"room_id": "a"}));
        assert_eq!(source["history_log_id"], id(in_a).as_str());
        assert_eq!(source["latest_log_id"], id(moved).as_str());
    }

    #[test]
    fn a_query_without_room_covers_joined_rooms_only() {
        let mut state = state();
        let in_a = append(&mut state, "message", &["a"]);
        append(&mut state, "message", &["hidden"]);
        let in_b = append(&mut state, "message", &["b"]);
        assert_eq!(page(&query_ok(&state, json!({}))), [in_a, in_b]);
    }

    #[test]
    fn invalid_bounds_limits_and_hidden_rooms_are_invalid_params() {
        let mut state = state();
        append(&mut state, "message", &["a"]);
        for params in [
            json!({"room_id": "hidden"}),
            json!({"room_id": "missing"}),
            json!({"room_id": "a", "after": ""}),
            json!({"room_id": "a", "after": "-1"}),
            json!({"room_id": "a", "after": "1e3"}),
            json!({"room_id": "a", "after": 5}),
            json!({"room_id": "a", "before": id(1 << 53)}),
            json!({"room_id": "a", "limit": 0}),
            json!({"room_id": "a", "limit": -1}),
            json!({"room_id": "a", "limit": "10"}),
        ] {
            assert_eq!(error_code(&state, params.clone()), -32602, "{params}");
        }
    }
}
