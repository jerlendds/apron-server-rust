//! §4.9 push: wake scopes, payloads and registrations. Each registration
//! gets its own HTTPS receiver from the fixture (`push-sink`), so a test can
//! tell which registration a message woke.
use super::support::*;
use serde_json::{Value, json};

/// Provisions a receiver and registers it for `peer`. `wake: None` leaves
/// `wake` out, so the server's defaults apply.
fn register(peer: &mut Peer, kind: &str, wake: Option<Value>) -> String {
    let sink = control(
        "push-sink",
        json!({"kind": if kind == "relay-encrypted" { "relay" } else { kind }}),
    );
    let url = string(&sink["url"]).to_owned();
    let mut params = json!({"kind": if kind == "webpush" { "webpush" } else { "relay" },
                            "url": url});
    if let Some(wake) = wake {
        params["wake"] = wake;
    }
    for key in ["keys", "token"] {
        if let Some(value) = sink.get(key) {
            params[key] = value.clone();
        }
    }
    peer.call("push_register", params).ok();
    url
}

/// One captured push, as (urgency, payload), or None after `wait_ms`.
fn take(url: &str, wait_ms: u64) -> Option<(String, Value)> {
    let captured = control("take-push", json!({"url":url,"wait_ms":wait_ms}));
    if captured.is_null() {
        return None;
    }
    let urgency = string(&captured["headers"]["urgency"]).to_owned();
    let payload = captured["plaintext"]
        .as_str()
        .map(|text| {
            assert!(
                text.len() <= 2048,
                "payload over 2048 bytes: {}",
                text.len()
            );
            serde_json::from_str(text).unwrap()
        })
        .unwrap_or(Value::Null);
    Some((urgency, payload))
}

/// Every push to `url` until none arrives for `quiet_ms`, as
/// (message_id or "", urgency).
fn drain(url: &str, quiet_ms: u64) -> Vec<(String, String)> {
    let mut pushes = Vec::new();
    while let Some((urgency, payload)) = take(url, quiet_ms) {
        assert!(
            payload["unread"].is_u64(),
            "badge server sends unread: {payload}"
        );
        let id = payload["message"]["message_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        pushes.push((id, urgency));
    }
    pushes
}

fn pushed(pairs: &[(&Value, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(message, urgency)| {
            (
                string(&message["message_id"]).to_owned(),
                (*urgency).to_owned(),
            )
        })
        .collect()
}

fn people() -> (Peer, Value, Peer, String) {
    let mut user = Peer::open();
    assert!(user.hello["push"]["relay"].is_object());
    user.cap("rooms");
    let user_id = user.guest()["you"]["user_id"].clone();
    let room = user.room(json!({"title":unique("push-room")}));
    let mut sender = Peer::open();
    sender.guest();
    sender.join(&room);
    (user, user_id, sender, room)
}

// --- wake scopes (§4.9.4) ---------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with relay push, private rooms and an HTTPS push sink"]
fn each_wake_scope_selects_its_messages_and_urgency() {
    let (mut user, user_id, mut sender, room) = people();
    let sender_id = sender.call("me", json!({})).ok()["you"]["user_id"].clone();
    let private = user.room(json!({"title":unique("push-private"),"private":true}));
    user.call("room_join", json!({"room_id":private,"user_id":sender_id}))
        .ok();
    let wake = array(&user.hello["push"]["wake"]).to_vec();
    for scope in ["mentions", "replies", "private", "joined"] {
        assert!(wake.iter().any(|w| w == scope), "{scope} not advertised");
    }

    let replies = register(&mut user, "relay", Some(json!(["replies"])));
    let private_only = register(&mut user, "relay", Some(json!(["private"])));
    let joined = register(&mut user, "relay", Some(json!(["joined"])));
    let default = register(&mut user, "relay", None);
    let nothing = register(&mut user, "relay", Some(json!([])));
    let unknown = register(
        &mut user,
        "relay",
        Some(json!(["mentions", "no-such-scope"])),
    );

    let own = user.post(json!({"room_id":room,"body":{"text":"my own message"}}));
    let plain = sender.post(json!({"room_id":room,"body":{"text":"plain"}}));
    let reply = sender.post(json!({"room_id":room,"body":{"text":"reply to you"},
                                   "reply_to":{"message_id":own["message_id"]}}));
    let other_reply = sender.post(json!({"room_id":room,"body":{"text":"reply to me"},
                                         "reply_to":{"message_id":plain["message_id"]}}));
    let in_private = sender.post(json!({"room_id":private,"body":{"text":"secret"}}));
    let mention = sender.post(json!({"room_id":room,"body":{"text":"hey","mentions":[user_id]}}));

    // `joined` gets the last message, so every earlier delivery is done.
    assert_eq!(
        drain(&joined, 3000),
        pushed(&[
            (&plain, "normal"),
            (&reply, "normal"),
            (&other_reply, "normal"),
            (&in_private, "normal"),
            (&mention, "normal"),
        ]),
        "joined: every message in a joined room but the user's own, at normal"
    );
    assert_eq!(drain(&replies, 500), pushed(&[(&reply, "high")]));
    assert_eq!(drain(&private_only, 500), pushed(&[(&in_private, "high")]));
    assert_eq!(
        drain(&default, 500),
        pushed(&[(&reply, "high"), (&mention, "high")]),
        "defaults are mentions and replies"
    );
    assert_eq!(drain(&nothing, 500), [], "an empty wake wakes for nothing");
    assert_eq!(
        drain(&unknown, 500),
        pushed(&[(&mention, "high")]),
        "unknown scopes are ignored"
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS with relay push, status, rooms and an HTTPS push sink"]
fn dnd_and_room_mutes_leave_only_badge_pushes() {
    let (mut user, user_id, mut sender, room) = people();
    user.cap("status");
    let badge = register(&mut user, "relay", Some(json!(["badge", "mentions"])));
    let mentions = register(&mut user, "relay", Some(json!(["mentions"])));
    let mention = |sender: &mut Peer, text: &str| {
        sender.post(json!({"room_id":room,"body":{"text":text,"mentions":[user_id]}}))
    };

    user.call("me", json!({"status":"dnd"})).ok();
    mention(&mut sender, "while dnd");
    let (urgency, payload) = take(&badge, 3000).expect("a badge push");
    assert_eq!(urgency, "low");
    assert!(payload.get("message").is_none(), "{payload}");
    assert!(take(&mentions, 500).is_none(), "dnd silences mentions");

    // A room mute with `room_id`: the suggested convention lets mentions
    // through, but this server silences them like any other message.
    user.call("me", json!({"status":"online"})).ok();
    user.call("status", json!({"room_id":room,"mute":true}))
        .ok();
    mention(&mut sender, "while the room is muted");
    let (urgency, payload) = take(&badge, 3000).expect("a badge push");
    assert_eq!(urgency, "low");
    assert!(payload.get("message").is_none(), "{payload}");
    assert!(take(&mentions, 500).is_none());

    user.call("status", json!({"room_id":room,"mute":false}))
        .ok();
    let woke = mention(&mut sender, "after unmuting");
    let (urgency, payload) = take(&mentions, 3000).expect("a mention push");
    assert_eq!(urgency, "high");
    assert_eq!(payload["message"]["message_id"], woke["message_id"]);
}

#[test]
#[ignore = "requires APRON_TEST_WS with relay and webpush push, and an HTTPS push sink"]
fn badge_is_ignored_for_webpush() {
    let (mut user, _, mut sender, room) = people();
    let relay = register(&mut user, "relay", Some(json!(["badge"])));
    let webpush = register(&mut user, "webpush", Some(json!(["badge"])));
    sender.post(json!({"room_id":room,"body":{"text":"unread goes up"}}));
    let (urgency, payload) = take(&relay, 3000).expect("a relay badge push");
    assert_eq!(urgency, "low");
    assert!(payload["unread"].as_u64().unwrap() > 0);
    assert!(take(&webpush, 500).is_none(), "webpush ignores badge");
}

#[test]
#[ignore = "requires APRON_TEST_WS with relay push, command, guest resumption and an HTTPS push sink"]
fn transient_notices_are_never_pushed() {
    let mut user = Peer::open();
    user.cap("command");
    let auth = user.guest();
    let url = register(
        &mut user,
        "relay",
        Some(json!(["joined", "badge", "mentions"])),
    );
    // The welcome notice at sign-in, and /help, are transient notices.
    let mut again = Peer::open();
    again
        .auth(json!({"scheme":"token","token":string(&auth["token"])}))
        .ok();
    again.take("message", |p| p["from"]["user_id"] == "~private");
    let help = again.call("command", json!({"body":{"text":"/help"}}));
    help.before("message", |p| p["from"]["user_id"] == "~private");
    help.ok();
    assert_eq!(drain(&url, 1000), []);
}

// --- payload (§4.9.3) ---------------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with relay push and an HTTPS push sink"]
fn a_long_message_is_cut_to_fit_and_keeps_its_ids() {
    let (mut user, user_id, mut sender, room) = people();
    let sender_id = sender.call("me", json!({})).ok()["you"]["user_id"].clone();
    let push_id = "A".repeat(32) + &"z9-_".repeat(8);
    assert_eq!(push_id.len(), 64);
    let sink = control("push-sink", json!({"kind":"relay"}));
    let url = string(&sink["url"]).to_owned();
    user.call(
        "push_register",
        json!({"kind":"relay","url":url,"token":sink["token"],"push_id":push_id}),
    )
    .ok();
    let texts = [
        "a".repeat(10_000),
        "\u{1}".repeat(1500) + "tail",
        "\"\\".repeat(2000),
        "😀".repeat(1000),
    ];
    for text in texts {
        let message = sender.post(
            json!({"room_id":room,"body":{"text":text,"format":"markdown","mentions":[user_id]}}),
        );
        let (_, payload) = take(&url, 3000).expect("a push");
        assert_eq!(payload["push_id"], push_id.as_str());
        let item = &payload["message"];
        assert_eq!(item["message_id"], message["message_id"]);
        assert_eq!(item["room_id"], room.as_str());
        assert_eq!(item["from"]["user_id"], sender_id);
        assert!(item.get("log_id").is_none());
        assert!(item["body"].get("format").is_none(), "format is omitted");
        let cut = item["body"]["text"].as_str().unwrap_or_default();
        assert!(text.starts_with(cut), "the text is cut, not changed");
        assert!(cut.len() < text.len());
        assert!(
            serde_json::to_vec(&payload).unwrap().len() > 1024,
            "the cut uses most of the room: {payload}"
        );
    }
}

// --- registration (§4.9.1) ------------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with relay push"]
fn push_id_must_be_1_to_64_url_safe_characters() {
    let mut peer = Peer::open();
    // This server does not refuse guests (§4.9.1 MAY).
    peer.guest();
    let sink = control("push-sink", json!({"kind":"relay"}));
    let url = string(&sink["url"]);
    let register = |peer: &mut Peer, push_id: &str| {
        peer.call(
            "push_register",
            json!({"kind":"relay","url":url,"push_id":push_id}),
        )
    };
    for bad in ["", &"a".repeat(65), "has space", "é", "a.b", "a/b", "a+b"] {
        register(&mut peer, bad).error(INVALID);
    }
    for good in ["a", &"Z".repeat(64), "AZaz09-_"] {
        register(&mut peer, good).ok();
    }
}

#[test]
#[ignore = "requires APRON_TEST_WS with relay push and an HTTPS push sink with push-gone"]
fn an_endpoint_that_answers_404_or_410_is_removed() {
    for status in [404, 410] {
        let (mut user, user_id, mut sender, room) = people();
        let url = register(&mut user, "relay", Some(json!(["mentions"])));
        control("push-gone", json!({"url":url,"status":status}));
        sender.post(json!({"room_id":room,"body":{"text":"first","mentions":[user_id]}}));
        let first = control("take-push", json!({"url":url,"wait_ms":3000}));
        assert_eq!(first["answered"], status, "the server tried once: {first}");
        sender.post(json!({"room_id":room,"body":{"text":"second","mentions":[user_id]}}));
        assert!(
            control("take-push", json!({"url":url,"wait_ms":1500})).is_null(),
            "a {status} endpoint gets nothing more"
        );
    }
}

#[test]
#[ignore = "requires a restartable APRON_TEST_WS with relay push and an HTTPS push sink"]
fn a_registration_survives_its_connection_and_a_restart() {
    let (mut user, user_id, mut sender, room) = people();
    let url = register(&mut user, "relay", Some(json!(["mentions"])));
    user.close();
    sender.post(json!({"room_id":room,"body":{"text":"closed","mentions":[user_id]}}));
    assert!(take(&url, 3000).is_some(), "after the connection closed");
    sender.close();

    control("restart", json!({}));
    let mut sender = Peer::open();
    sender.guest();
    sender.join(&room);
    let message =
        sender.post(json!({"room_id":room,"body":{"text":"restarted","mentions":[user_id]}}));
    let (_, payload) = take(&url, 3000).expect("after a restart");
    assert_eq!(payload["message"]["message_id"], message["message_id"]);
}

#[test]
#[ignore = "requires APRON_TEST_WS with relay push, private rooms and an HTTPS push sink"]
fn private_wakes_for_threads_of_private_rooms_the_user_is_in() {
    let (mut user, user_id, mut sender, _) = people();
    let sender_id = sender.call("me", json!({})).ok()["you"]["user_id"].clone();
    let private = user.room(json!({"title":unique("push-private-parent"),"private":true}));
    user.call("room_join", json!({"room_id":private,"user_id":sender_id}))
        .ok();
    let url = register(&mut user, "relay", Some(json!(["private"])));
    // A thread of a private room is private by default here (§4.2.4), so
    // the user receives it only once added.
    let thread =
        sender.room(json!({"title":unique("push-private-thread"),"parent_room_id":private}));
    sender
        .call("room_join", json!({"room_id":thread,"user_id":user_id}))
        .ok();
    let message = sender.post(json!({"room_id":thread,"body":{"text":"in the thread"}}));
    assert_eq!(drain(&url, 2000), pushed(&[(&message, "high")]));
}
