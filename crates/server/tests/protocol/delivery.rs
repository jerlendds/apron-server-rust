use super::{rooms::history, support::*};
use serde_json::json;

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
    let aggregate = writer.call(
        "history",
        json!({"after":moved_at.to_string(),
        "before":moved_at.to_string()}),
    );
    if aggregate.frame.get("error").is_some() {
        aggregate.error(UNSUPPORTED);
    } else {
        let records = aggregate.ok();
        assert_eq!(array(&records["messages"]).len(), 1);
        assert_eq!(records["messages"][0]["log_id"], snapshot["log_id"]);
    }

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
    let reports = [
        ("dnd", "dnd"),
        ("invisible", "offline"),
        ("", ""),
        ("online", "online"),
    ];
    for (chosen, shown) in reports {
        if !["", "online"].contains(&chosen) && !optional.iter().any(|v| v == chosen) {
            continue;
        }
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
