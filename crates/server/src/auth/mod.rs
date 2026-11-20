pub(crate) mod email;
pub(crate) mod external;
pub(crate) mod passkey_page;
mod tokens;
pub(crate) mod webauthn;

use tokens::{bearer_user, token_result, token_result_bound};
pub(crate) use tokens::{migrate_tokens, random_secret, secret_hash};

use serde_json::{Value, json};
use store::State;

use crate::wire::{Error, Params, Result, string};

pub(crate) fn auth(
    state: &mut State,
    params: &Params,
    origin: Option<&str>,
) -> Result<(String, Value, bool)> {
    match string(params, "scheme")?.ok_or(Error::Invalid)? {
        "guest" => guest(state, params),
        "token" => {
            let token = string(params, "token")?.ok_or(Error::Invalid)?;
            let user_id = bearer_user(state, token, origin)?;
            let user = state.users.get(user_id).ok_or(Error::Denied)?;
            Ok((user_id.to_owned(), json!({"you": user}), false))
        }
        _ => Err(Error::Unsupported),
    }
}

fn guest(state: &mut State, params: &Params) -> Result<(String, Value, bool)> {
    let user = create_account(state, params, "guest")?;
    let result = token_result(state, &user)?;
    Ok((user, result, true))
}

pub(super) fn create_account(state: &mut State, params: &Params, prefix: &str) -> Result<String> {
    let name = string(params, "name")?;
    let requested_id = string(params, "user_id")?;
    let user_id = if let Some(id) = requested_id.filter(|id| {
        !id.is_empty()
            && !id.starts_with('~')
            && id.len() <= 128
            && !id
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == '@' || c == ':')
            && !state.users.contains_key(*id)
    }) {
        id.to_owned()
    } else {
        loop {
            state.next_guest = state.next_guest.checked_add(1).ok_or(Error::Internal)?;
            let id = format!("{prefix}_{}", state.next_guest);
            if !state.users.contains_key(&id) {
                break id;
            }
        }
    };
    let name = name.filter(|name| !name.is_empty()).unwrap_or(&user_id);
    if name.len() > 512 {
        return Err(Error::TooLarge);
    }
    let user = json!({"user_id": user_id, "name": name});
    state.users.insert(user_id.clone(), user);
    Ok(user_id)
}

#[cfg(test)]
mod tests {
    //! §2.4, §3.2, A.3: which requested `user_id`s an account may take.
    use super::*;

    /// The `user_id` assigned when `requested` is asked for on a fresh server
    /// that already has `taken`.
    fn assigned(requested: &str) -> String {
        let mut state = State::default();
        state
            .users
            .insert("taken".into(), json!({"user_id": "taken"}));
        let Value::Object(params) = json!({"user_id": requested, "name": "Ada"}) else {
            unreachable!()
        };
        match create_account(&mut state, &params, "guest") {
            Ok(user) => user,
            Err(error) => panic!("{requested:?}: {}", error.frame(None)),
        }
    }

    fn is_fallback(id: &str) -> bool {
        id.strip_prefix("guest_")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    }

    #[test]
    fn ordinary_requested_ids_are_granted() {
        for id in ["ada", "guest_1234x", "a.b-c_d", "Ada99", "é"] {
            assert_eq!(assigned(id), id);
        }
    }

    #[test]
    fn unusable_requested_ids_fall_back_to_an_assigned_id() {
        for id in [
            "",
            "two words",
            "tab\there",
            "line\nbreak",
            "nbsp\u{a0}here",
            "\u{3000}",
            "nul\0",
            "esc\u{1b}",
            "del\u{7f}",
            "c1\u{85}",
            "~server",
            "~private",
            "carol@b.example",
            "irc:ada",
            "taken",
            &"x".repeat(129),
        ] {
            let got = assigned(id);
            assert!(is_fallback(&got), "{id:?} was granted as {got:?}");
        }
        assert_eq!(
            assigned(&"x".repeat(128)),
            "x".repeat(128),
            "128 bytes is allowed"
        );
    }

    #[test]
    fn assigned_ids_are_fresh_and_never_reused() {
        let mut state = State::default();
        state
            .users
            .insert("guest_1".into(), json!({"user_id": "guest_1"}));
        let params = Params::new();
        let first = create_account(&mut state, &params, "guest")
            .unwrap_or_else(|e| panic!("{}", e.frame(None)));
        let second = create_account(&mut state, &params, "guest")
            .unwrap_or_else(|e| panic!("{}", e.frame(None)));
        assert_eq!((first.as_str(), second.as_str()), ("guest_2", "guest_3"));
        assert_eq!(
            state.users[&first]["name"], first,
            "an absent name shows the user_id"
        );
    }
}
