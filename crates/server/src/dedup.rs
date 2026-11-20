use serde_json::Value;
use store::{Retry, State, epoch_millis};

use crate::wire::{Error, Result};

const RETENTION_MS: u64 = 24 * 60 * 60 * 1000;
const MAX_PER_USER: usize = 512;

pub(crate) fn replay<'a>(
    state: &'a State,
    user: &str,
    id: &str,
    method: &str,
    params: &Value,
) -> Result<Option<&'a Retry>> {
    let cutoff = epoch_millis().saturating_sub(RETENTION_MS);
    let Some(retry) = state
        .retries
        .get(user)
        .and_then(|entries| entries.get(id))
        .filter(|entry| entry.committed_at >= cutoff)
    else {
        return Ok(None);
    };
    let fingerprint = retry.params.as_str().map_or_else(
        || &retry.params == params,
        |saved| saved == fingerprint(params),
    );
    if retry.method != method || !fingerprint {
        return Err(Error::Invalid);
    }
    Ok(Some(retry))
}

pub(crate) fn remember(
    state: &mut State,
    user: &str,
    id: String,
    method: String,
    params: Value,
    result: Value,
) {
    let now = epoch_millis();
    let cutoff = now.saturating_sub(RETENTION_MS);
    let entries = state.retries.entry(user.to_owned()).or_default();
    entries.retain(|_, retry| retry.committed_at >= cutoff);
    if entries.len() >= MAX_PER_USER {
        let oldest = entries
            .iter()
            .min_by_key(|(_, retry)| retry.committed_at)
            .map(|(id, _)| id.clone());
        if let Some(oldest) = oldest {
            entries.remove(&oldest);
        }
    }
    entries.insert(
        id,
        Retry {
            method,
            params: Value::String(fingerprint(&params)),
            result,
            committed_at: now,
        },
    );
}

fn fingerprint(params: &Value) -> String {
    // serde_json maps serialize keys in sorted order with the workspace's features.
    crate::auth::secret_hash(&params.to_string())
}

#[cfg(test)]
mod tests {
    //! §1.2: the key is `(user_id, id)`, params compare without regard to
    //! object key order, and remembered IDs survive a restart.
    use super::*;
    use serde_json::json;

    fn params(text: &str) -> Value {
        // Built from text so the key order in the source is the key order
        // the parser sees.
        serde_json::from_str(text).unwrap()
    }

    fn remembered(state: &mut State, user: &str, id: &str, method: &str, params: Value) {
        remember(
            state,
            user,
            id.into(),
            method.into(),
            params,
            json!({"message_id": "1"}),
        );
    }

    /// `Ok(true)` for a replay, `Ok(false)` for a new operation, or the wire
    /// error code of a conflict.
    fn check(
        state: &State,
        user: &str,
        id: &str,
        method: &str,
        params: &Value,
    ) -> std::result::Result<bool, i64> {
        match replay(state, user, id, method, params) {
            Ok(found) => Ok(found.is_some()),
            Err(error) => Err(error.frame(None)["error"]["code"].as_i64().unwrap()),
        }
    }

    #[test]
    fn the_key_is_user_and_request_id() {
        let mut state = State::default();
        let body = params(r#"{"room_id":"general","body":{"text":"hi"}}"#);
        remembered(&mut state, "ada", "c1", "message", body.clone());

        assert_eq!(check(&state, "ada", "c1", "message", &body), Ok(true));
        assert_eq!(
            check(&state, "bob", "c1", "message", &body),
            Ok(false),
            "another user's c1 is new"
        );
        assert_eq!(check(&state, "ada", "c2", "message", &body), Ok(false));

        remembered(&mut state, "bob", "c1", "me", params(r#"{"name":"Bob"}"#));
        assert_eq!(
            check(&state, "ada", "c1", "message", &body),
            Ok(true),
            "bob's entry is separate"
        );
    }

    #[test]
    fn params_compare_without_object_key_order_at_any_depth() {
        let mut state = State::default();
        remembered(
            &mut state,
            "ada",
            "c1",
            "message",
            params(
                r#"{"room_id":"a","body":{"text":"hi","format":"plain","embeds":[{"kind":"upload","title":"x"}]}}"#,
            ),
        );
        let reordered = params(
            r#"{"body":{"embeds":[{"title":"x","kind":"upload"}],"format":"plain","text":"hi"},"room_id":"a"}"#,
        );
        assert_eq!(check(&state, "ada", "c1", "message", &reordered), Ok(true));
    }

    #[test]
    fn a_different_method_or_params_under_the_same_id_is_invalid_params() {
        let mut state = State::default();
        let original = params(r#"{"room_id":"a","body":{"text":"hi"}}"#);
        remembered(&mut state, "ada", "c1", "message", original.clone());

        assert_eq!(
            check(&state, "ada", "c1", "command", &original),
            Err(-32602)
        );
        for changed in [
            r#"{"room_id":"a","body":{"text":"hi!"}}"#,
            r#"{"room_id":"b","body":{"text":"hi"}}"#,
            r#"{"room_id":"a","body":{"text":"hi","format":"plain"}}"#,
            r#"{"room_id":"a"}"#,
        ] {
            assert_eq!(
                check(&state, "ada", "c1", "message", &params(changed)),
                Err(-32602),
                "{changed}"
            );
        }
        // Array order is part of the value, unlike object key order.
        remembered(
            &mut state,
            "ada",
            "c2",
            "reactions",
            params(r#"{"message_id":"1","emojis":["a","b"]}"#),
        );
        assert_eq!(
            check(
                &state,
                "ada",
                "c2",
                "reactions",
                &params(r#"{"message_id":"1","emojis":["b","a"]}"#)
            ),
            Err(-32602)
        );
    }

    #[test]
    fn entries_past_retention_are_forgotten() {
        let mut state = State::default();
        let body = params(r#"{"text":"hi"}"#);
        remembered(&mut state, "ada", "c1", "message", body.clone());
        state
            .retries
            .get_mut("ada")
            .unwrap()
            .get_mut("c1")
            .unwrap()
            .committed_at = epoch_millis() - RETENTION_MS - 1_000;
        assert_eq!(check(&state, "ada", "c1", "message", &body), Ok(false));
    }

    #[test]
    fn each_user_keeps_at_most_the_newest_entries() {
        let mut state = State::default();
        for n in 0..MAX_PER_USER + 10 {
            remembered(
                &mut state,
                "ada",
                &format!("c{n}"),
                "message",
                json!({"n": n}),
            );
            // Distinct, increasing commit times so "oldest" is well defined.
            state
                .retries
                .get_mut("ada")
                .unwrap()
                .get_mut(&format!("c{n}"))
                .unwrap()
                .committed_at = epoch_millis() - 100_000 + n as u64;
        }
        let entries = &state.retries["ada"];
        assert_eq!(entries.len(), MAX_PER_USER);
        assert!(
            !entries.contains_key("c0"),
            "the oldest entry is evicted first"
        );
        assert!(entries.contains_key(&format!("c{}", MAX_PER_USER + 9)));
    }

    #[tokio::test]
    async fn remembered_ids_survive_a_reopen() {
        let dir = std::env::temp_dir().join(format!(
            "apron-dedup-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("apron.db");
        let db = db.to_str().unwrap();
        let body = params(r#"{"room_id":"a","body":{"text":"hi"}}"#);
        {
            let mut store = store::Store::open(db).await.unwrap();
            let mut state = store.load().await.unwrap();
            remembered(&mut state, "ada", "c1", "message", body.clone());
            store.commit(&state).await.unwrap();
        }
        let mut store = store::Store::open(db).await.unwrap();
        let state = store.load().await.unwrap();
        let reordered = params(r#"{"body":{"text":"hi"},"room_id":"a"}"#);
        let outcome = check(&state, "ada", "c1", "message", &reordered);
        let conflict = check(
            &state,
            "ada",
            "c1",
            "message",
            &params(r#"{"room_id":"b"}"#),
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(outcome, Ok(true));
        assert_eq!(conflict, Err(-32602));
        assert_eq!(
            state.retries["ada"]["c1"].result,
            json!({"message_id": "1"})
        );
    }
}
