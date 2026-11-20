use super::{rooms::history, support::*};
use serde_json::{Value, json};

#[test]
#[ignore = "requires APRON_TEST_WS with rooms, history, edit, reactions and ext"]
fn edits_moves_reactions_and_tombstones_share_one_ordered_log() {
    let mut writer = Peer::open();
    for cap in ["rooms", "history", "edit", "reactions", "ext"] {
        writer.cap(cap);
    }
    let author = writer.guest()["you"]["user_id"].clone();
    let source = writer.room(json!({"title":unique("source")}));
    let destination = writer.room(json!({"title":unique("destination")}));
    let mut observer = Peer::open();
    observer.guest();
    observer.join(&source);
    observer.join(&destination);

    let original = writer.post(
        json!({"room_id":source,"body":{"text":"original","format":"markdown",
        "mentions":[author]},"ext":{"test":{"nested":1},"preserved":{"v":2}}}),
    );
    let message = &original["message_id"];
    let react = writer.call(
        "reactions",
        json!({"message_id":message,"emojis":["👍","👍","❤️","❤"]}),
    );
    let reaction = react.before("reactions", |p| p["message_id"] == *message);
    assert_eq!(array(&reaction["reactions"][0]["emojis"]).len(), 2);
    react.ok();

    writer
        .call("me", json!({"name":unique("new-profile")}))
        .ok();
    let moved = writer.call(
        "message",
        json!({"room_id":destination,"message_id":message,
        "body":{"text":"moved"},"ext":{"test":{"new":1}}}),
    );
    let snapshot = moved
        .before("message", |p| p["message_id"] == *message)
        .clone();
    let moved_at = log_id(&snapshot["log_id"]);
    let moved_reaction = moved.before("reactions", |p| p["message_id"] == *message);
    assert!(log_id(&moved_reaction["log_id"]) > moved_at);
    assert_eq!(moved_reaction["room_id"], destination);
    assert_eq!(snapshot["prev_room_id"], source);
    assert_eq!(snapshot["room_id"], destination);
    assert_eq!(snapshot["from"], original["from"]);
    assert!(snapshot["body"]["format"].is_null() || snapshot["body"]["format"] == "plain");
    assert!(items(&snapshot["body"]["mentions"]).is_empty());
    assert_eq!(
        snapshot["ext"],
        json!({"test":{"new":1},"preserved":{"v":2}})
    );
    assert_eq!(moved.ok()["message_id"], *message);

    let received = observer.take("message", |p| p["log_id"] == snapshot["log_id"]);
    assert_eq!(received, snapshot);
    observer.fence();
    assert!(
        !observer
            .inbox
            .iter()
            .any(|f| f["params"]["log_id"] == snapshot["log_id"])
    );

    for room in [&source, &destination] {
        let page = history(
            &mut writer,
            json!({"room_id":room,
            "after":moved_at.to_string(),"before":moved_at.to_string()}),
        );
        assert_eq!(array(&page["messages"]).len(), 1);
        assert_eq!(page["messages"][0]["message_id"], *message);
    }
    // This server supports history without `room_id` (§4.3 SHOULD).
    let records = history(
        &mut writer,
        json!({"after":moved_at.to_string(),
        "before":moved_at.to_string()}),
    );
    assert_eq!(array(&records["messages"]).len(), 1, "{records}");
    assert_eq!(records["messages"][0]["log_id"], snapshot["log_id"]);

    let reset = writer.call("reactions", json!({"message_id":message,"emojis":[]}));
    let clear = reset.before("reactions", |p| p["message_id"] == *message);
    assert_eq!(clear["reactions"][0]["emojis"], json!([]));
    reset.ok();

    let deleted = writer.call(
        "message",
        json!({"room_id":destination,"message_id":message,
        "deleted":true,"body":{"text":"discarded"},"ext":{"discarded":true}}),
    );
    let tombstone = deleted.before("message", |p| p["message_id"] == *message);
    assert_eq!(tombstone["deleted"], true);
    assert!(tombstone.get("body").is_none() && tombstone.get("ext").is_none());
    assert!(log_id(&tombstone["log_id"]) > moved_at);
    deleted.ok();
}

#[test]
#[ignore = "requires APRON_TEST_WS with ext and rooms"]
fn extension_values_merge_by_key_on_profile_and_room_writes() {
    let mut peer = Peer::open();
    peer.cap("ext");
    peer.cap("rooms");
    peer.guest();
    peer.call(
        "me",
        json!({"ext":{"kept":{"v":1},"replaced":{"old":true}}}),
    )
    .ok();
    let user = peer
        .call("me", json!({"ext":{"replaced":{"new":true}}}))
        .ok();
    assert_eq!(
        user["you"]["ext"],
        json!({"kept":{"v":1},"replaced":{"new":true}})
    );
    let user = peer.call("me", json!({"ext":{}})).ok();
    assert_eq!(
        user["you"]["ext"],
        json!({"kept":{"v":1},"replaced":{"new":true}})
    );
    let room = peer.room(json!({"title":unique("ext"),"ext":{"kept":1,"replaced":{"old":true}}}));
    let reply = peer.call(
        "room_set",
        json!({"room_id":room,"ext":{"replaced":{"new":true}}}),
    );
    let update = reply.before("room_update", |p| has_room(p, "updated", &room));
    let record = array(&update["updated"])
        .iter()
        .find(|r| r["room_id"] == room)
        .unwrap();
    assert_eq!(record["ext"], json!({"kept":1,"replaced":{"new":true}}));
    reply.ok();
}

#[test]
#[ignore = "requires APRON_TEST_WS with command and /help"]
fn command_help_is_private_transient_and_precedes_reply() {
    let mut peer = Peer::open();
    peer.cap("command");
    peer.guest();
    let mut other = Peer::open();
    other.guest();
    let reply = peer.call("command", json!({"body":{"text":"/help"}}));
    let notice = reply.before("message", |p| p["from"]["user_id"] == "~private");
    assert!(notice.get("message_id").is_none() && notice.get("log_id").is_none());
    let text = string(&notice["body"]["text"]).to_owned();
    reply.ok();
    other.fence();
    assert!(!other.inbox.iter().any(
        |f| f["params"]["from"]["user_id"] == "~private" && f["params"]["body"]["text"] == text
    ));
    peer.call("command", json!({"message_id":"1","body":{"text":"/help"}}))
        .error(INVALID);
    peer.call("command", json!({"deleted":true,"body":{"text":"/help"}}))
        .error(INVALID);
    let literal = peer.post(json!({"body":{"text":"/help"}}));
    assert_eq!(literal["body"]["text"], "/help");
}

#[test]
#[ignore = "requires APRON_TEST_WS with status, rooms and guest resumption"]
fn mutes_are_private_to_the_user_and_replayed_after_sign_in() {
    let mut owner = Peer::open();
    owner.cap("status");
    owner.cap("rooms");
    let auth = owner.guest();
    let mut observer = Peer::open();
    observer.guest();
    let room = owner.room(json!({"title":unique("presence")}));
    observer.join(&room);
    let muted = owner.call("status", json!({"room_id":room,"mute":true}));
    let change = muted.before("status", |p| p["room_id"] == room);
    assert_eq!(change["mute"], true);
    assert_eq!(muted.ok(), json!({}));
    observer.fence();
    assert!(
        !observer
            .inbox
            .iter()
            .any(|f| event(f, "status") && f["params"]["room_id"] == room)
    );

    let mut resume = Peer::open();
    let auth_reply = resume.auth(json!({"scheme":"token","token":string(&auth["token"])}));
    assert!(
        !auth_reply
            .before
            .iter()
            .any(|f| event(f, "status") && f["params"]["room_id"] == room)
    );
    assert_eq!(auth_reply.ok()["you"]["user_id"], auth["you"]["user_id"]);
    let replay = resume.take("status", |p| p["room_id"] == room);
    assert_eq!(replay["mute"], true);

    owner
        .call("status", json!({"room_id":room,"mute":false}))
        .ok();
    assert_eq!(
        resume.take("status", |p| p["room_id"] == room)["mute"],
        false
    );
    owner.call("status", json!({"idle":false})).ok();
    owner.call("status", json!({"idle":-1})).error(INVALID);
    let chosen = owner.call("me", json!({"status":""})).ok();
    assert_eq!(chosen["you"]["status"], "");
}

#[test]
#[ignore = "requires APRON_TEST_WS with room-scoped activity delivery enabled"]
fn activity_is_notification_only_and_absent_from_history() {
    let mut sender = Peer::open();
    for cap in ["rooms", "activity", "history"] {
        sender.cap(cap);
    }
    let user = sender.guest()["you"]["user_id"].clone();
    let room = sender.room(json!({"title":unique("activity")}));
    let mut observer = Peer::open();
    observer.guest();
    observer.join(&room);
    let snapshot = sender.post(json!({"room_id":room,"body":{"text":"read this"}}));
    sender.send(
        json!({"method":"activity","params":{"room_id":room,"typing":4,
        "read_message_id":snapshot["message_id"]}}),
    );
    let activity = observer.take("activity", |p| {
        p["room_id"] == room && p["from"]["user_id"] == user
    });
    assert_eq!(activity["typing"], 4);
    assert_eq!(activity["read_message_id"], snapshot["message_id"]);
    assert!(activity.get("log_id").is_none());
    let pos = log_id(&snapshot["log_id"]);
    let page = history(
        &mut observer,
        json!({"room_id":room,"after":(pos+1).to_string()}),
    );
    assert_eq!(page["more"], false);
    assert!(page.get("first_log_id").is_none());
}

#[test]
#[ignore = "requires APRON_TEST_WS with status and room membership"]
fn chosen_status_and_observer_status_are_distinct() {
    let mut owner = Peer::open();
    owner.cap("status");
    owner.cap("rooms");
    let identity = owner.guest()["you"]["user_id"].clone();
    let room = owner.room(json!({"title":unique("shown-status")}));
    let mut observer = Peer::open();
    observer.guest();
    observer.join(&room);
    let optional = items(&owner.hello["status"]).to_vec();
    assert!(!optional.iter().any(|v| *v == "online" || *v == ""));
    for value in ["dnd", "invisible"] {
        assert!(
            optional.iter().any(|v| v == value),
            "the fixture advertises {value}: {optional:?}"
        );
    }
    let reports = [
        ("dnd", "dnd"),
        ("invisible", "offline"),
        ("", ""),
        ("online", "online"),
    ];
    for (chosen, shown) in reports {
        let result = owner.call("me", json!({"status":chosen})).ok();
        assert_eq!(result["you"]["status"], chosen);
        let listing = observer
            .call("room_list", json!({"filter":"joined","members":true}))
            .ok();
        let shown_user = array(&listing["users"])
            .iter()
            .find(|u| u["user_id"] == identity)
            .unwrap();
        assert_eq!(shown_user["status"], shown);
        if shown != "offline" {
            assert!(shown_user.get("seen").is_none());
        }
        assert!(shown_user.get("idle").is_none());
    }
}

#[test]
#[ignore = "requires APRON_TEST_WS with two push kinds, rooms, and an HTTPS push sink"]
fn push_relay_and_webpush_deliver_bounded_message_payloads_and_can_unregister() {
    for kind in ["relay", "relay-encrypted", "webpush"] {
        let mut receiver = Peer::open();
        let advertised = if kind == "relay-encrypted" {
            "relay"
        } else {
            kind
        };
        assert!(receiver.hello["push"][advertised].is_object());
        if advertised == "webpush" {
            string(&receiver.hello["push"]["webpush"]["key"]);
        }
        assert!(
            array(&receiver.hello["push"]["wake"])
                .iter()
                .any(|v| v == "mentions")
        );
        let auth = receiver.guest();
        let recipient = &auth["you"]["user_id"];
        let room = receiver.room(json!({"title":unique("push")}));
        let sink = control("push-sink", json!({"kind":kind}));
        let url = string(&sink["url"]);
        assert!(url.starts_with("https://"));
        let push_id = unique("push");
        let registration = |id: &str| {
            let mut params = json!({"kind":advertised,"url":url,"push_id":id,"wake":["mentions"]});
            for key in ["keys", "token"] {
                if let Some(value) = sink.get(key) {
                    params[key] = value.clone();
                }
            }
            params
        };
        receiver
            .call("push_register", registration("replaced"))
            .ok();
        receiver.call("push_register", registration(&push_id)).ok();

        let mut sender = Peer::open();
        let author = sender.guest()["you"]["user_id"].clone();
        sender.join(&room);
        receiver.close();
        sender.post(json!({"room_id":room,"body":{"text":format!("@{}", string(recipient))}}));
        let message = sender
            .post(json!({"room_id":room,"body":{"text":"é".repeat(1600),"mentions":[recipient]}}));
        let captured = control("take-push", json!({"url":url,"wait_ms":5000}));
        assert_eq!(captured["method"], "POST");
        let headers = &captured["headers"];
        assert!(string(&headers["ttl"]).parse::<u64>().is_ok());
        assert_eq!(headers["urgency"], "high");
        if kind == "relay" {
            assert!(string(&headers["content-type"]).starts_with("application/json"));
            if let Some(token) = sink["token"].as_str() {
                assert_eq!(headers["authorization"], format!("Bearer {token}"));
            }
        } else {
            assert!(string(&headers["content-type"]).starts_with("application/octet-stream"));
            assert_eq!(headers["content-encoding"], "aes128gcm");
            assert_eq!(captured["decrypted"], true);
        }
        if kind == "webpush" {
            assert_eq!(captured["vapid_verified"], true);
        }
        let payload = string(&captured["plaintext"]);
        assert!(payload.len() <= 2048);
        let payload: serde_json::Value = serde_json::from_str(payload).unwrap();
        assert_eq!(payload["push_id"], push_id);
        let item = &payload["message"];
        assert_eq!(item["message_id"], message["message_id"]);
        assert_eq!(item["room_id"], room);
        assert_eq!(item["from"]["user_id"], author);
        assert!(item.get("log_id").is_none());
        assert!(
            control("take-push", json!({"url":url,"wait_ms":500})).is_null(),
            "registration replacement or unmentioned text produced an extra delivery"
        );

        let mut receiver = Peer::open();
        let token = string(&auth["token"]);
        assert_eq!(
            receiver.auth(json!({"scheme":"token","token":token})).ok()["you"]["user_id"],
            *recipient
        );
        for _ in 0..2 {
            receiver.call("push_unregister", json!({"url":url})).ok();
        }
        receiver.close();
        sender.post(
            json!({"room_id":room,"body":{"text":"after unregister","mentions":[recipient]}}),
        );
        assert!(control("take-push", json!({"url":url,"wait_ms":500})).is_null());
    }
}

#[test]
#[ignore = "requires APRON_TEST_WS with a relay and badge wake, and an HTTPS push sink"]
fn muted_mentions_only_update_the_badge() {
    let mut receiver = Peer::open();
    assert!(
        array(&receiver.hello["push"]["wake"])
            .iter()
            .any(|v| v == "badge")
    );
    let user = receiver.guest()["you"]["user_id"].clone();
    let room = receiver.room(json!({"title":unique("badge")}));
    let sink = control("push-sink", json!({"kind":"relay"}));
    let url = string(&sink["url"]);
    receiver
        .call(
            "push_register",
            json!({"kind":"relay","url":url,"wake":["badge","mentions"]}),
        )
        .ok();
    receiver.call("status", json!({"mute":true})).ok();
    receiver.close();
    let mut sender = Peer::open();
    sender.guest();
    sender.join(&room);
    sender.post(json!({"room_id":room,"body":{"text":"badge only","mentions":[user]}}));
    let delivery = control("take-push", json!({"url":url,"wait_ms":5000}));
    assert_eq!(delivery["headers"]["urgency"], "low");
    let payload: serde_json::Value = serde_json::from_str(string(&delivery["plaintext"])).unwrap();
    assert!(payload.get("message").is_none());
    assert!(payload["unread"].as_u64().unwrap() > 0);
}

#[test]
#[ignore = "requires APRON_TEST_WS with status, idle tracking, rooms and guest resumption"]
fn user_is_idle_only_after_every_connection_is_unattended() {
    let mut first = Peer::open();
    first.cap("status");
    first.cap("rooms");
    let auth = first.guest();
    let user = &auth["you"]["user_id"];
    let room = first.room(json!({"title":unique("idle")}));
    let mut other = Peer::open();
    other.guest();
    other.join(&room);
    let mut second = Peer::open();
    second
        .auth(json!({"scheme":"token","token":string(&auth["token"])}))
        .ok();
    let shown = |peer: &mut Peer| {
        let listing = peer
            .call("room_list", json!({"filter":"joined","members":true}))
            .ok();
        array(&listing["users"])
            .iter()
            .find(|u| u["user_id"] == *user)
            .unwrap()["status"]
            .clone()
    };
    first.call("status", json!({"idle":300})).ok();
    assert_eq!(shown(&mut other), "online");
    second.call("status", json!({"idle":300})).ok();
    assert_eq!(shown(&mut other), "idle");
    first.call("status", json!({"idle":false})).ok();
    assert_eq!(shown(&mut other), "online");
}

#[test]
#[ignore = "requires APRON_TEST_WS with relay push and HTTPS public endpoint validation"]
fn push_rejects_invalid_and_internal_endpoints() {
    let mut peer = Peer::open();
    assert!(peer.hello["push"]["relay"].is_object());
    peer.guest();
    for url in [
        "not-a-url",
        "http://example.invalid/push",
        "https://127.0.0.1/push",
        "https://[::1]/push",
    ] {
        peer.call("push_register", json!({"kind":"relay","url":url}))
            .error(INVALID);
    }
}

#[test]
#[ignore = "requires APRON_TEST_WS with ext and APRON_TEST_EXT_LIMIT below the frame limit"]
fn extension_size_limit_applies_to_the_merge_and_rejection_is_atomic() {
    let mut peer = Peer::open();
    peer.cap("ext");
    peer.guest();
    let limit: usize = std::env::var("APRON_TEST_EXT_LIMIT")
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        (512..=128 * 1024).contains(&limit),
        "use a dedicated extension limit"
    );
    let value = "a".repeat(limit * 2 / 3);
    let first = peer.call("me", json!({"ext":{"one":value}})).ok();
    assert_eq!(first["you"]["ext"], json!({"one":value}));
    peer.call("me", json!({"ext":{"two":value}})).error(-32003);
    let unchanged = peer.call("me", json!({})).ok();
    assert_eq!(unchanged["you"]["ext"], first["you"]["ext"]);
}

// --- helpers --------------------------------------------------------------

fn quiet(peer: &mut Peer) {
    peer.fence();
    peer.inbox.clear();
}

/// Every frame this peer received up to a fence.
fn drained(peer: &mut Peer) -> Vec<Value> {
    peer.fence();
    std::mem::take(&mut peer.inbox)
}

/// Sends an `activity` notification and waits until the server has handled
/// it. Requests on one connection are handled in order, so a fence after it
/// follows the activity.
fn activity(peer: &mut Peer, params: Value) {
    peer.send(json!({"method":"activity","params":params}));
    peer.fence();
}

fn about<'a>(frames: &'a [Value], user: &'a Value) -> impl Iterator<Item = &'a Value> + 'a {
    frames
        .iter()
        .filter(move |f| event(f, "user") && f["params"]["new"]["user_id"] == *user)
        .map(|f| &f["params"]["new"])
}

/// Three users who share a fresh room, with `owner` as its creator.
fn trio(label: &str) -> (Peer, Value, Peer, Peer, String) {
    let mut owner = Peer::open();
    for cap in [
        "rooms",
        "status",
        "reactions",
        "activity",
        "history",
        "edit",
    ] {
        owner.cap(cap);
    }
    let auth = owner.guest();
    let room = owner.room(json!({"title":unique(label)}));
    let mut second = Peer::open();
    second.guest();
    second.join(&room);
    let mut third = Peer::open();
    third.guest();
    third.join(&room);
    (owner, auth, second, third, room)
}

fn reacting(peer: &mut Peer, message: &Value, emojis: Value) -> Value {
    let exchange = peer.call("reactions", json!({"message_id":message,"emojis":emojis}));
    let record = exchange
        .before("reactions", |p| p["message_id"] == *message)
        .clone();
    assert_eq!(exchange.ok(), json!({}));
    record
}

// --- reactions (§4.7) -----------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with rooms and reactions"]
fn a_reaction_set_keeps_its_order_and_accepts_keys() {
    let (mut owner, _, mut other, _, room) = trio("reaction-order");
    let message = owner.post(json!({"room_id":room,"body":{"text":"react"}}))["message_id"].clone();
    let record = reacting(&mut other, &message, json!(["❤️", "👍", ":partyparrot:"]));
    assert_eq!(record["room_id"], room.as_str());
    let sets = array(&record["reactions"]);
    assert_eq!(sets.len(), 1, "a live broadcast carries one set: {record}");
    // Policy here: any non-empty, non-control string up to 128 bytes is a
    // key, so a custom-emoji key is accepted as sent.
    assert_eq!(sets[0]["emojis"], json!(["❤️", "👍", ":partyparrot:"]));
    let seen = owner.take("reactions", |p| p["message_id"] == message);
    assert_eq!(seen, record);
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms, reactions and history"]
fn more_than_32_distinct_emoji_is_denied_and_logs_nothing() {
    let (mut owner, _, mut other, _, room) = trio("reaction-limit");
    let message = owner.post(json!({"room_id":room,"body":{"text":"lots"}}))["message_id"].clone();
    let keys = |n: usize| json!((0..n).map(|i| format!(":k{i}:")).collect::<Vec<_>>());
    let full = reacting(&mut other, &message, keys(32));
    assert_eq!(array(&full["reactions"][0]["emojis"]).len(), 32);
    quiet(&mut owner);
    other
        .call("reactions", json!({"message_id":message,"emojis":keys(33)}))
        .error(DENIED);
    // Duplicates collapse before the limit: 33 entries, 32 distinct.
    let mut doubled = array(&keys(32)).to_vec();
    doubled.push(json!(":k0:\u{fe0f}"));
    reacting(&mut other, &message, json!(doubled));
    let frames = drained(&mut owner);
    let records = frames
        .iter()
        .filter(|f| event(f, "reactions"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1, "only the accepted request is logged");
    let page = history(&mut owner, json!({"room_id":room,"after":full["log_id"]}));
    assert_eq!(items(&page["reactions"]).len(), 2, "{page}");
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms and reactions"]
fn an_unchanged_reaction_set_is_logged_again_with_a_new_log_id() {
    let (mut owner, _, mut other, _, room) = trio("reaction-repeat");
    let message = owner.post(json!({"room_id":room,"body":{"text":"again"}}))["message_id"].clone();
    let first = reacting(&mut other, &message, json!(["👍"]));
    let second = reacting(&mut other, &message, json!(["👍"]));
    // §4.7 lets a server log nothing here; this one logs an identical record.
    assert!(log_id(&first["log_id"]) < log_id(&second["log_id"]));
    let strip = |record: &Value| {
        let mut record = record.clone();
        record.as_object_mut().unwrap().remove("log_id");
        record
    };
    assert_eq!(strip(&first), strip(&second));
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms, reactions, edit and private rooms"]
fn reactions_on_hidden_or_deleted_messages_are_invalid() {
    let (mut owner, _, mut other, _, room) = trio("reaction-invalid");
    let mut outsider = Peer::open();
    outsider.guest();
    let private = owner.room(json!({"title":unique("reaction-private"),"private":true}));
    let hidden =
        owner.post(json!({"room_id":private,"body":{"text":"secret"}}))["message_id"].clone();
    outsider
        .call("reactions", json!({"message_id":hidden,"emojis":["👍"]}))
        .error(INVALID);

    let doomed = owner.post(json!({"room_id":room,"body":{"text":"doomed"}}))["message_id"].clone();
    owner
        .call(
            "message",
            json!({"room_id":room,"message_id":doomed,"deleted":true}),
        )
        .ok();
    quiet(&mut owner);
    other
        .call("reactions", json!({"message_id":doomed,"emojis":["👍"]}))
        .error(INVALID);
    assert!(!drained(&mut owner).iter().any(|f| event(f, "reactions")));
}

// --- status (§4.5) --------------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with status and rooms"]
fn a_last_disconnect_shows_offline_and_seen_only_ever_comes_with_offline() {
    let (mut owner, auth, mut observer, _, _) = trio("seen");
    let user = auth["you"]["user_id"].clone();
    let mut extra = Peer::open();
    extra
        .auth(json!({"scheme":"token","token":string(&auth["token"])}))
        .ok();
    quiet(&mut observer);
    extra.close();
    owner.fence();
    assert_eq!(
        about(&drained(&mut observer), &user).count(),
        0,
        "the user still has a connection"
    );
    owner.close();
    let change = observer.take("user", |p| p["new"]["user_id"] == user);
    assert_eq!(change["new"]["status"], "offline");
    // `seen` is optional (§4.5.2) and this server does not send it. When it
    // does, it must be a log_id.
    if let Some(seen) = change["new"].get("seen") {
        log_id(seen);
    }

    // Invisible: connecting and disconnecting changes nothing others see.
    let mut back = Peer::open();
    back.auth(json!({"scheme":"token","token":string(&auth["token"])}))
        .ok();
    back.call("me", json!({"status":"invisible"})).ok();
    back.close();
    let mut invisible = Peer::open();
    invisible
        .auth(json!({"scheme":"token","token":string(&auth["token"])}))
        .ok();
    quiet(&mut observer);
    invisible.call("me", json!({"name":"Still hidden"})).ok();
    invisible.close();
    let frames = drained(&mut observer);
    for new in about(&frames, &user) {
        assert_ne!(new["status"], "online", "{new}");
        assert_ne!(new["status"], "invisible", "{new}");
        if new.get("seen").is_some() {
            assert_eq!(new["status"], "offline", "{new}");
        }
    }
    assert!(
        !about(&frames, &user).any(|n| n.get("seen").is_some()),
        "an invisible user's connections must not move seen"
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS with status and rooms"]
fn an_invisible_user_appears_offline_in_live_updates_and_invisible_to_themselves() {
    let (mut owner, auth, mut observer, _, _) = trio("invisible-live");
    let user = auth["you"]["user_id"].clone();
    let mut sibling = Peer::open();
    sibling
        .auth(json!({"scheme":"token","token":string(&auth["token"])}))
        .ok();
    quiet(&mut observer);
    quiet(&mut sibling);
    let chosen = owner.call("me", json!({"status":"invisible"})).ok();
    assert_eq!(chosen["you"]["status"], "invisible");
    let own = sibling.take("user", |p| p["you"]["user_id"] == user);
    assert_eq!(own["you"]["status"], "invisible");
    let shown = observer.take("user", |p| p["new"]["user_id"] == user);
    assert_eq!(shown["new"]["status"], "offline");

    owner.call("status", json!({"idle":30})).ok();
    owner.call("status", json!({"idle":false})).ok();
    owner.call("me", json!({"name":"Hidden"})).ok();
    for frame in drained(&mut observer) {
        assert!(
            !frame.to_string().contains("invisible"),
            "chosen status leaked: {frame}"
        );
        if event(&frame, "user") && frame["params"]["new"]["user_id"] == user {
            let status = &frame["params"]["new"]["status"];
            assert!(status.is_null() || status == "offline", "{frame}");
        }
    }
    owner.call("me", json!({"status":"online"})).ok();
}

#[test]
#[ignore = "requires APRON_TEST_WS with status"]
fn an_unlisted_chosen_status_becomes_no_status() {
    let mut peer = Peer::open();
    peer.cap("status");
    peer.guest();
    let listed = items(&peer.hello["status"]).to_vec();
    assert!(!listed.iter().any(|s| s == "away"));
    // §4.5.1: servers set "" when they do not accept a value.
    let result = peer.call("me", json!({"status":"away"})).ok();
    assert_eq!(result["you"]["status"], "");
}

#[test]
#[ignore = "requires APRON_TEST_WS with status, rooms and guest resumption"]
fn sign_in_announces_each_present_room_sharer_once() {
    let (mut owner, auth, mut online, mut dnd, room) = trio("sign-in-statuses");
    let online_id = online.call("me", json!({})).ok()["you"]["user_id"].clone();
    let dnd_id = dnd.call("me", json!({"status":"dnd"})).ok()["you"]["user_id"].clone();
    let mut hidden = Peer::open();
    let hidden_id = hidden.guest()["you"]["user_id"].clone();
    hidden.join(&room);
    hidden.call("me", json!({"status":"invisible"})).ok();
    let mut opted_out = Peer::open();
    let opted_out_id = opted_out.guest()["you"]["user_id"].clone();
    opted_out.join(&room);
    opted_out.call("me", json!({"status":""})).ok();
    let mut gone = Peer::open();
    let gone_id = gone.guest()["you"]["user_id"].clone();
    gone.join(&room);
    gone.close();
    owner.fence();

    let mut resumed = Peer::open();
    resumed
        .auth(json!({"scheme":"token","token":string(&auth["token"])}))
        .ok();
    let frames = drained(&mut resumed);
    let announced = frames
        .iter()
        .filter(|f| event(f, "user") && f["params"].get("new").is_some())
        .map(|f| &f["params"]["new"])
        .collect::<Vec<_>>();
    let count = |id: &Value| announced.iter().filter(|n| n["user_id"] == *id).count();
    assert_eq!(count(&online_id), 1, "{announced:?}");
    assert_eq!(count(&dnd_id), 1, "{announced:?}");
    for absent in [&hidden_id, &opted_out_id, &gone_id] {
        assert_eq!(
            count(absent),
            0,
            "{absent} is offline or \"\": {announced:?}"
        );
    }
    for new in &announced {
        assert!(
            !matches!(new["status"].as_str(), Some("offline" | "")),
            "{new}"
        );
        assert_eq!(count(&new["user_id"]), 1, "one notification per user");
    }
    let status =
        |id: &Value| announced.iter().find(|n| n["user_id"] == *id).unwrap()["status"].clone();
    assert_eq!(status(&online_id), "online");
    assert_eq!(status(&dnd_id), "dnd");
}

#[test]
#[ignore = "requires APRON_TEST_WS with status, rooms and guest resumption"]
fn timed_and_global_mutes_are_replayed_and_expire_with_mute_false() {
    let mut owner = Peer::open();
    owner.cap("status");
    let auth = owner.guest();
    owner.call("status", json!({"mute":3600})).ok();
    let mut resumed = Peer::open();
    resumed
        .auth(json!({"scheme":"token","token":string(&auth["token"])}))
        .ok();
    let replay = resumed.take("status", |p| p.get("room_id").is_none());
    let left = replay["mute"].as_u64().expect("seconds left, not true");
    assert!((3590..=3600).contains(&left), "{replay}");

    owner.call("status", json!({"mute":true})).ok();
    let mut again = Peer::open();
    again
        .auth(json!({"scheme":"token","token":string(&auth["token"])}))
        .ok();
    assert_eq!(
        again.take("status", |p| p.get("room_id").is_none())["mute"],
        true
    );

    // Expiry: the server ends a 1-second mute on its own and says so.
    owner.call("status", json!({"mute":1})).ok();
    quiet(&mut again);
    let ended = again.take("status", |p| p["mute"] == false);
    assert!(ended.get("room_id").is_none(), "{ended}");
    let mut after = Peer::open();
    after
        .auth(json!({"scheme":"token","token":string(&auth["token"])}))
        .ok();
    assert!(
        !drained(&mut after).iter().any(|f| event(f, "status")),
        "an expired mute is not replayed"
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS with status, rooms, private rooms and guest resumption"]
fn a_rejected_status_request_changes_nothing() {
    let (mut owner, auth, mut observer, _, _) = trio("status-error");
    let user = auth["you"]["user_id"].clone();
    let mut stranger = Peer::open();
    stranger.guest();
    let hidden = stranger.room(json!({"title":unique("status-hidden"),"private":true}));
    quiet(&mut observer);
    // A valid `idle` with an invalid `mute`: neither applies.
    owner
        .call("status", json!({"idle":300,"mute":-5}))
        .error(INVALID);
    owner
        .call("status", json!({"idle":300,"room_id":hidden,"mute":true}))
        .error(INVALID);
    owner.fence();
    let frames = drained(&mut observer);
    assert_eq!(about(&frames, &user).count(), 0, "idle was not applied");
    let listing = observer
        .call("room_list", json!({"filter":"joined","members":true}))
        .ok();
    let shown = array(&listing["users"])
        .iter()
        .find(|u| u["user_id"] == user)
        .unwrap();
    assert_eq!(shown["status"], "online");
    let mut resumed = Peer::open();
    resumed
        .auth(json!({"scheme":"token","token":string(&auth["token"])}))
        .ok();
    assert!(
        !drained(&mut resumed).iter().any(|f| event(f, "status")),
        "no mute was stored"
    );
    assert!(!drained(&mut owner).iter().any(|f| event(f, "status")));
}

// --- activity (§4.6) ------------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with room-scoped activity delivery enabled"]
fn typing_zero_is_relayed_and_activity_with_an_id_is_a_notification() {
    let (mut sender, auth, mut observer, _, room) = trio("typing-zero");
    let user = auth["you"]["user_id"].clone();
    quiet(&mut observer);
    activity(&mut sender, json!({"room_id":room,"typing":5}));
    activity(&mut sender, json!({"room_id":room,"typing":0}));
    let frames = drained(&mut observer);
    let typing = frames
        .iter()
        .filter(|f| event(f, "activity") && f["params"]["from"]["user_id"] == user)
        .map(|f| f["params"]["typing"].clone())
        .collect::<Vec<_>>();
    assert_eq!(typing, [json!(5), json!(0)]);

    sender.send(json!({"method":"activity","id":"act-1","params":{"room_id":room,"typing":2}}));
    let frames = drained(&mut sender);
    assert!(
        !frames.iter().any(|f| f["id"] == "act-1"),
        "§1.1: activity gets no reply even with an id: {frames:?}"
    );
    let relayed = observer.take("activity", |p| p["from"]["user_id"] == user);
    assert_eq!(relayed["typing"], 2);
}

#[test]
#[ignore = "requires APRON_TEST_WS with room-scoped activity delivery and guest resumption"]
fn read_cursors_only_move_forward_and_are_replayed_after_listing() {
    let (mut reader, auth, mut observer, _, room) = trio("read-cursor");
    let user = auth["you"]["user_id"].clone();
    let early = reader.post(json!({"room_id":room,"body":{"text":"one"}}))["message_id"].clone();
    let late = reader.post(json!({"room_id":room,"body":{"text":"two"}}))["message_id"].clone();
    activity(&mut reader, json!({"room_id":room,"read_message_id":late}));
    let forward = observer.take("activity", |p| p["from"]["user_id"] == user);
    assert_eq!(forward["read_message_id"], late);
    quiet(&mut observer);

    activity(&mut reader, json!({"room_id":room,"read_message_id":early}));
    activity(
        &mut reader,
        json!({"room_id":room,"read_message_id":early,"typing":3}),
    );
    let frames = drained(&mut observer);
    let mine = frames
        .iter()
        .filter(|f| event(f, "activity") && f["params"]["from"]["user_id"] == user)
        .collect::<Vec<_>>();
    assert_eq!(
        mine.len(),
        1,
        "a backward cursor alone is not sent: {mine:?}"
    );
    assert_eq!(mine[0]["params"]["typing"], 3);
    assert!(mine[0]["params"].get("read_message_id").is_none());

    let mut other = Peer::open();
    other
        .auth(json!({"scheme":"token","token":string(&auth["token"])}))
        .ok();
    quiet(&mut other);
    other.call("room_list", json!({"room_id":room})).ok();
    let replay = other.take("activity", |p| p["room_id"] == room);
    assert_eq!(replay["from"]["user_id"], user);
    assert_eq!(
        replay["read_message_id"], late,
        "the cursor did not move back"
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS with activity and private rooms"]
fn activity_for_a_hidden_room_is_dropped_silently() {
    let (mut owner, _, _, _, _) = trio("activity-hidden");
    let private = owner.room(json!({"title":unique("activity-private"),"private":true}));
    let mut outsider = Peer::open();
    outsider.guest();
    quiet(&mut owner);
    activity(&mut outsider, json!({"room_id":private,"typing":5}));
    let frames = drained(&mut outsider);
    assert!(
        !frames
            .iter()
            .any(|f| f.get("error").is_some() || event(f, "activity")),
        "{frames:?}"
    );
    assert!(!drained(&mut owner).iter().any(|f| event(f, "activity")));
}
