use super::support::*;
use serde_json::{Value, json};
use std::env;

#[test]
#[ignore = "requires APRON_TEST_WS"]
fn greeting_ping_and_unauthenticated_requests() {
    let mut peer = Peer::open();
    let ping = peer.hello["ping"].as_f64();
    assert!(
        ping.is_some_and(|ping| ping > 0.0),
        "the fixture advertises ping: {}",
        peer.hello
    );
    peer.send(json!({"method":"ping"}));
    peer.take("pong", |_| true);
    peer.call("message", json!({"body":{"text":"unauthenticated"}}))
        .error(DENIED);
    peer.auth(json!({"scheme":"ext:unoffered"}))
        .error(UNSUPPORTED);
}

#[test]
#[ignore = "requires APRON_TEST_WS"]
fn auth_barrier_and_default_room() {
    let mut peer = Peer::open();
    peer.scheme("guest");
    let auth = unique("auth");
    let post = unique("post");
    let text = unique("pipelined");
    peer.send(json!({"id":auth,"method":"auth","params":{"scheme":"guest"}}));
    peer.send(json!({"id":post,"method":"message","params":{"body":{"text":text}}}));
    let exchange = peer.reply(&auth);
    assert!(!exchange.before.iter().any(|f| f["id"] == post));
    let user = exchange.ok()["you"]["user_id"].clone();
    opaque(&user);
    let exchange = peer.reply(&post);
    let id = exchange.frame["result"]["message_id"].clone();
    let snapshot = exchange.before("message", |p| p["message_id"] == id);
    assert_eq!(snapshot["from"]["user_id"], user);
    assert_eq!(snapshot["body"]["text"], text);
    assert_eq!(log_id(&snapshot["log_id"]), log_id(&id));
    opaque(&snapshot["room_id"]);
    exchange.ok();
}

#[test]
#[ignore = "requires APRON_TEST_WS with guest and token sign-in"]
fn bearer_reconnect_executes_auth_even_with_repeated_request_id() {
    let mut first = Peer::open();
    first.scheme("token");
    let guest = first.guest();
    let key = unique("same-auth-id");
    let token = string(&guest["token"]);
    let result = first
        .call_id(&key, "auth", json!({"scheme":"token","token":token}))
        .ok();
    let latest = result["token"].as_str().unwrap_or(token);
    let mut second = Peer::open();
    second
        .call_id(
            &key,
            "auth",
            json!({"scheme":"token","token":unique("invalid")}),
        )
        .error(DENIED);
    second.call("me", json!({})).error(DENIED);
    let result = second
        .call_id(&key, "auth", json!({"scheme":"token","token":latest}))
        .ok();
    assert_eq!(result["you"]["user_id"], guest["you"]["user_id"]);
    second
        .auth(json!({"scheme":"token","token":unique("invalid")}))
        .error(DENIED);
    let snapshot = second.post(json!({"body":{"text":unique("still-signed-in")}}));
    assert_eq!(snapshot["from"]["user_id"], guest["you"]["user_id"]);
}

#[test]
#[ignore = "requires APRON_TEST_WS"]
fn unknown_keys_notifications_and_request_id_correlation() {
    let mut peer = Peer::open();
    peer.guest();
    let unknown = unique("unknown");
    let known = unique("known");
    let text = unique("markdown");
    peer.send(json!({"method":"ext:unknown-notification","params":{"any":true}}));
    peer.send(json!({"id":unknown,"method":"ext:unknown-request","params":{}}));
    peer.send(
        json!({"id":known,"method":"message","future":true,"params":{
            "some_future_field":true,"body":{"format":"markdown","text":text}
        }}),
    );
    let mut unknown_count = 0;
    let mut known_count = 0;
    let mut preceding_snapshot = None;
    while unknown_count == 0 || known_count == 0 {
        let frame = peer.next();
        if event(&frame, "message") && frame["params"]["body"]["text"] == text {
            preceding_snapshot = Some(frame["params"].clone());
        } else if frame["id"] == unknown {
            unknown_count += 1;
            Exchange {
                frame,
                before: vec![],
            }
            .error(UNSUPPORTED);
        } else if frame["id"] == known {
            known_count += 1;
            let reply = Exchange {
                frame,
                before: vec![],
            }
            .ok();
            let snapshot = preceding_snapshot
                .as_ref()
                .expect("broadcast must precede reply");
            assert_eq!(reply["message_id"], snapshot["message_id"]);
            assert_eq!(snapshot["body"]["format"], "markdown");
        } else {
            assert!(
                frame.get("error").is_none(),
                "notification produced an error: {frame}"
            );
        }
    }
    assert_eq!((unknown_count, known_count), (1, 1));
    peer.fence();
    assert!(
        !peer
            .inbox
            .iter()
            .any(|f| f["id"] == unknown || f["id"] == known || f.get("error").is_some())
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS"]
fn messages_validate_references_and_preserve_server_identity() {
    let mut peer = Peer::open();
    let guest = peer.guest();
    peer.call("message", json!({})).error(INVALID);
    peer.call("message", json!({"body":{"text":"","embeds":[]}}))
        .error(INVALID);
    peer.call("message", json!({"deleted":true,"body":{"text":"invalid"}}))
        .error(INVALID);
    peer.call(
        "message",
        json!({"room_id":unique("missing"),"body":{"text":"invalid"}}),
    )
    .error(INVALID);
    peer.call(
        "message",
        json!({"body":{"text":"invalid"},"reply_to":{"message_id":"1"}}),
    )
    .error(INVALID);
    let name = unique("profile");
    let change = peer
        .call("me", json!({"name":name, "avatar":"", "user_id":"~server"}))
        .ok();
    assert_eq!(change["you"]["user_id"], guest["you"]["user_id"]);
    assert_eq!(change["you"]["name"], name);

    let first = peer.post(json!({"from":{"user_id":"~server"},"log_id":"1",
        "body":{"text":"@just_prose is not an explicit mention"}}));
    assert_eq!(first["from"]["user_id"], guest["you"]["user_id"]);
    assert!(first["body"]["mentions"].is_null() || first["body"]["mentions"] == json!([]));

    let second = peer.post(json!({
        "body":{"text":"explicit mention","mentions":[guest["you"]["user_id"]]},
        "reply_to":{"message_id":first["message_id"]}
    }));
    assert_eq!(second["reply_to"]["message_id"], first["message_id"]);
    assert_eq!(second["body"]["mentions"], json!([guest["you"]["user_id"]]));
    assert!(log_id(&second["log_id"]) > log_id(&first["log_id"]));

    if peer.has_cap("edit") {
        peer.call(
            "message",
            json!({"message_id":first["message_id"],"body":{"text":"self reply"},
            "reply_to":{"message_id":first["message_id"]}}),
        )
        .error(INVALID);
    }
}

#[test]
#[ignore = "requires APRON_TEST_CORE_WS with guest auth and no optional features"]
fn core_only_works_without_capabilities_and_rejects_optional_operations() {
    let mut peer = Peer::at(
        &env::var("APRON_TEST_CORE_WS").expect("set APRON_TEST_CORE_WS"),
        None,
    );
    assert!(items(&peer.hello["capabilities"]).is_empty());
    peer.guest();
    let snapshot = peer.post(json!({"body":{"text":unique("core-only")}}));
    peer.call(
        "message",
        json!({"message_id":snapshot["message_id"],"body":{"text":"edit"}}),
    )
    .error(UNSUPPORTED);
    for method in [
        "command",
        "room_list",
        "history",
        "status",
        "reactions",
        "push_register",
    ] {
        peer.call(method, json!({})).error(UNSUPPORTED);
    }
}

#[test]
#[ignore = "requires APRON_TEST_WS implementing retry deduplication"]
fn retries_ignore_object_key_order_and_do_not_broadcast_twice() {
    let mut sender = Peer::open();
    let guest = sender.guest();
    let mut observer = Peer::open();
    observer.guest();
    let id = unique("dedup");
    let text = unique("once");
    let a = format!(
        r#"{{"id":"{id}","method":"message","params":{{"body":{{"text":"{text}","format":"plain"}}}}}}"#
    );
    let b = format!(
        r#"{{"id":"{id}","method":"message","params":{{"body":{{"format":"plain","text":"{text}"}}}}}}"#
    );
    sender.send_text(a);
    let first = sender.reply(&id);
    let message = first.frame["result"]["message_id"].clone();
    first.before("message", |p| p["message_id"] == message);
    first.ok();
    let observed = observer.take("message", |p| p["message_id"] == message);
    assert_eq!(observed["body"]["text"], text);

    let mut retry = Peer::open();
    retry
        .auth(json!({"scheme":"token","token":string(&guest["token"])}))
        .ok();
    retry.send_text(b);
    let second = retry.reply(&id);
    assert!(
        !second
            .before
            .iter()
            .any(|f| f["params"]["message_id"] == message)
    );
    assert_eq!(second.ok()["message_id"], message);
    retry
        .call_id(&id, "message", json!({"body":{"text":"different"}}))
        .error(INVALID);
    retry.call_id(&id, "me", json!({})).error(INVALID);
    observer.fence();
    assert!(
        !observer
            .inbox
            .iter()
            .any(|f| f["params"]["message_id"] == message)
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS with deduplication and resumable guests"]
fn concurrent_retries_commit_only_once_per_user() {
    let mut first = Peer::open();
    let guest = first.guest();
    let mut second = Peer::open();
    second
        .auth(json!({"scheme":"token","token":string(&guest["token"])}))
        .ok();
    let id = unique("concurrent");
    let text = unique("at-most-once");
    let request = json!({"id":id,"method":"message","params":{"body":{"text":text}}});
    first.send(request.clone());
    second.send(request.clone());
    let a = first.reply(&id).ok();
    let b = second.reply(&id).ok();
    assert_eq!(a, b);
    for peer in [&mut first, &mut second] {
        peer.take("message", |p| p["message_id"] == a["message_id"]);
        peer.fence();
        assert!(
            !peer
                .inbox
                .iter()
                .any(|f| f["params"]["message_id"] == a["message_id"])
        );
    }
    let mut independent = Peer::open();
    independent.guest();
    independent.send(request);
    let distinct = independent.reply(&id).ok();
    assert_ne!(distinct["message_id"], a["message_id"]);
}

#[test]
#[ignore = "requires APRON_TEST_WS configured to retain 256 KiB frames"]
fn transport_accepts_the_recommended_frame_size() {
    let mut peer = Peer::open();
    peer.guest();
    let id = unique("large-frame");
    let prefix = format!(r#"{{"id":"{id}","method":"me","params":{{}},"unknown":""#);
    let suffix = "\"}";
    let text = format!(
        "{prefix}{}{suffix}",
        "x".repeat(256 * 1024 - prefix.len() - suffix.len())
    );
    assert_eq!(text.len(), 256 * 1024);
    peer.send_text(text);
    assert!(peer.reply(&id).ok()["you"]["user_id"].is_string());
}

#[test]
#[ignore = "requires a restartable APRON_TEST_WS backed by persisted state"]
fn restart_preserves_history_log_watermark_identity_and_committed_retry() {
    let mut before = Peer::open();
    for cap in ["rooms", "history"] {
        before.cap(cap);
    }
    let auth = before.guest();
    let room = before.room(json!({"title":unique("durable")}));
    let key = unique("retry-after-restart");
    let params = json!({"room_id":room,"body":{"text":"persisted"}});
    let committed = before.call_id(&key, "message", params.clone()).ok();
    let id = log_id(&committed["message_id"]);
    before.close();
    control("restart", json!({}));

    let mut after = Peer::open();
    let result = after
        .auth(json!({"scheme":"token","token":string(&auth["token"])}))
        .ok();
    assert_eq!(result["you"]["user_id"], auth["you"]["user_id"]);
    let replay = after.call_id(&key, "message", params);
    assert!(
        !replay
            .before
            .iter()
            .any(|f| f["params"]["message_id"] == committed["message_id"])
    );
    assert_eq!(replay.ok(), committed);
    let page = super::rooms::history(&mut after, json!({"room_id":room,"after":id.to_string()}));
    assert_eq!(array(&page["messages"]).len(), 1);
    assert_eq!(page["messages"][0]["body"]["text"], "persisted");
    let next = after.post(json!({"room_id":room,"body":{"text":"new after restart"}}));
    assert!(log_id(&next["log_id"]) > id);
}

#[test]
#[ignore = "requires APRON_TEST_WS"]
fn malformed_json_and_envelopes_use_distinct_error_codes() {
    for (payload, code) in [("{", -32700), ("[]", -32600), (r#"{"method":42}"#, -32600)] {
        let mut peer = Peer::open();
        peer.guest();
        peer.send_text(payload.into());
        let error = (0..256)
            .map(|_| peer.next())
            .find(|frame| frame.get("error").is_some())
            .expect("malformed input did not produce an error");
        assert!(error.get("id").is_none());
        Exchange {
            frame: error,
            before: vec![],
        }
        .error(code);
    }
}

#[test]
#[ignore = "requires APRON_TEST_WS configured with a welcome before auth"]
fn welcome_before_auth_is_transient_and_scoped() {
    let mut peer = Peer::open();
    let welcome = peer.take("message", |p| p["from"]["user_id"] == "~private");
    assert!(welcome.get("message_id").is_none());
    assert!(welcome.get("log_id").is_none());
    peer.guest();
    peer.post(json!({"body":{"text":"hello after welcome"}}));
}

// --- user notifications (§3.3.3) ----------------------------------------

fn resume(token: &Value) -> Peer {
    let mut peer = Peer::open();
    peer.auth(json!({"scheme":"token","token":string(token)}))
        .ok();
    peer
}

/// Drops everything delivered so far, so that the next frames are caused by
/// what the test does next.
fn quiet(peer: &mut Peer) {
    peer.fence();
    peer.inbox.clear();
}

fn user_frames_about<'a>(peer: &'a Peer, user: &'a Value) -> impl Iterator<Item = &'a Value> {
    peer.inbox.iter().filter(move |f| {
        event(f, "user")
            && (f["params"]["new"]["user_id"] == *user
                || f["params"]["you"]["user_id"] == *user
                || f["params"]["old"]["user_id"] == *user)
    })
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms, guest sign-up and resumable tokens"]
fn profile_changes_reach_own_connections_and_room_sharers_only() {
    let mut editor = Peer::open();
    editor.cap("rooms");
    let ada = editor.guest();
    let user = ada["you"]["user_id"].clone();
    let mut other_device = resume(&ada["token"]);
    let mut sharer = Peer::open();
    sharer.guest();
    let mut stranger = Peer::open();
    stranger.guest();
    stranger
        .call("room_leave", json!({"room_id":"general"}))
        .ok();
    for peer in [&mut other_device, &mut sharer, &mut stranger] {
        quiet(peer);
    }

    let name = unique("Ada L");
    editor.call("me", json!({"name":name})).ok();

    let own = other_device.take("user", |p| p["you"]["user_id"] == user);
    assert_eq!(own["you"]["name"], name);
    assert!(own.get("new").is_none() && own.get("old").is_none());
    let seen = sharer.take("user", |p| p["new"]["user_id"] == user);
    assert_eq!(seen["new"]["name"], name);
    assert!(seen.get("you").is_none() && seen.get("old").is_none());

    stranger.fence();
    assert_eq!(
        user_frames_about(&stranger, &user).count(),
        0,
        "a user who shares no room hears nothing: {:?}",
        stranger.inbox
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS with guest sign-up"]
fn a_cleared_profile_field_is_sent_as_its_empty_value() {
    let mut editor = Peer::open();
    let user = editor.guest()["you"]["user_id"].clone();
    let mut sharer = Peer::open();
    sharer.guest();

    quiet(&mut sharer);
    let avatar = "https://avatars.example/ada.png";
    editor.call("me", json!({"avatar":avatar})).ok();
    let set = sharer.take("user", |p| p["new"]["user_id"] == user);
    assert_eq!(set["new"]["avatar"], avatar);

    let cleared = editor.call("me", json!({"avatar":""})).ok();
    assert_eq!(
        cleared["you"]["avatar"], "",
        "the complete object carries the cleared value"
    );
    let update = sharer.take("user", |p| p["new"]["user_id"] == user);
    assert_eq!(
        update["new"].get("avatar"),
        Some(&json!("")),
        "a cleared field is its empty value, not absent: {update}"
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS with status, guest sign-up and resumable tokens"]
fn signing_in_to_another_account_announces_the_previous_identity_left_without_old() {
    let mut owner = Peer::open();
    owner.cap("status");
    let account = owner.guest();
    owner.close();

    let mut switcher = Peer::open();
    let guest = switcher.guest()["you"]["user_id"].clone();
    let mut observer = Peer::open();
    observer.guest();
    quiet(&mut observer);

    switcher
        .auth(json!({"scheme":"token","token":string(&account["token"])}))
        .ok();
    let left = observer.take("user", |p| p["new"]["user_id"] == guest);
    assert_eq!(left["new"]["status"], "offline", "{left}");
    observer.fence();
    assert!(
        !observer
            .inbox
            .iter()
            .any(|f| event(f, "user") && f["params"].get("old").is_some()),
        "a sign-in to an existing account never sends old: {:?}",
        observer.inbox
    );
}

// --- order (§1, §3.2.2) ---------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with status, guest sign-up and resumable tokens"]
fn notifications_caused_by_a_sign_in_follow_its_auth_result() {
    let mut first = Peer::open();
    first.cap("status");
    let ada = first.guest();
    first.call("status", json!({"mute":true})).ok();
    let mut online = Peer::open();
    let bob = online.guest()["you"]["user_id"].clone();

    let mut later = Peer::open();
    let exchange = later.auth(json!({"scheme":"token","token":string(&ada["token"])}));
    for frame in &exchange.before {
        assert!(
            !(event(frame, "status") || event(frame, "user") || event(frame, "room_update")),
            "sign-in notification before the auth result: {frame}"
        );
    }
    assert_eq!(exchange.ok()["you"]["user_id"], ada["you"]["user_id"]);
    assert_eq!(
        later.take("status", |p| p.get("room_id").is_none())["mute"],
        true
    );
    let status = later.take("user", |p| p["new"]["user_id"] == bob);
    assert_eq!(status["new"]["status"], "online");
}

#[test]
#[ignore = "requires APRON_TEST_WS with guest sign-up and resumable tokens"]
fn a_pipelined_request_runs_as_the_identity_of_the_auth_before_it() {
    let mut owner = Peer::open();
    let account = owner.guest();
    let mut peer = Peer::open();
    let guest = peer.guest();
    assert_ne!(guest["you"]["user_id"], account["you"]["user_id"]);

    let auth = unique("switch");
    let me = unique("whoami");
    peer.send(
        json!({"id":auth,"method":"auth","params":{"scheme":"token","token":account["token"]}}),
    );
    peer.send(json!({"id":me,"method":"me","params":{}}));
    peer.reply(&auth).ok();
    let result = peer.reply(&me).ok();
    assert_eq!(result["you"]["user_id"], account["you"]["user_id"]);
}

#[test]
#[ignore = "requires APRON_TEST_WS with guest, external and webauthn sign-in"]
fn a_failed_auth_with_any_scheme_keeps_the_current_identity() {
    let mut peer = Peer::open();
    let user = peer.guest()["you"]["user_id"].clone();
    let failures = [
        (json!({"scheme":"ext:unoffered"}), UNSUPPORTED),
        (json!({"scheme":"token","token":unique("forged")}), DENIED),
        (
            json!({"scheme":"external","code":unique("forged"),"code_verifier":"x".repeat(43),
                   "redirect_uri":"https://client.example/cb","client_id":"https://client.example/"}),
            DENIED,
        ),
        (
            json!({"scheme":"webauthn","action":"login","step":"finish",
                   "challenge_id":unique("forged"),"credential":{}}),
            DENIED,
        ),
    ];
    for (params, code) in failures {
        if let Some(scheme) = params["scheme"].as_str().filter(|s| !s.starts_with("ext:")) {
            peer.scheme(scheme);
        }
        peer.auth(params).error(code);
        assert_eq!(peer.call("me", json!({})).ok()["you"]["user_id"], user);
    }
    let snapshot = peer.post(json!({"body":{"text":unique("still-me")}}));
    assert_eq!(snapshot["from"]["user_id"], user);
}

// --- log_id (§2.1) --------------------------------------------------------

/// Sends `count` posts without waiting, and returns the snapshots in
/// delivery order after every reply has arrived.
fn burst(peer: &mut Peer, tag: &str, count: usize) -> Vec<Value> {
    let ids: Vec<String> = (0..count).map(|n| format!("{tag}-{n}")).collect();
    for (n, id) in ids.iter().enumerate() {
        peer.send(
            json!({"id":id,"method":"message","params":{"body":{"text":format!("{tag} {n}")}}}),
        );
    }
    let mut replies = Vec::new();
    for id in &ids {
        replies.push(peer.reply(id).ok()["message_id"].clone());
    }
    let snapshots: Vec<Value> = peer
        .inbox
        .drain(..)
        .filter(|f| {
            event(f, "message")
                && f["params"]["body"]["text"]
                    .as_str()
                    .is_some_and(|t| t.starts_with(tag))
        })
        .map(|f| f["params"].clone())
        .collect();
    let delivered: Vec<&Value> = snapshots.iter().map(|s| &s["message_id"]).collect();
    assert_eq!(
        delivered,
        replies.iter().collect::<Vec<_>>(),
        "one broadcast per reply, in order"
    );
    snapshots
}

fn strictly_increasing(snapshots: &[Value]) {
    let ids: Vec<u64> = snapshots.iter().map(|s| log_id(&s["log_id"])).collect();
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
}

#[test]
#[ignore = "requires APRON_TEST_WS with guest sign-up"]
fn a_pipelined_burst_gets_strictly_increasing_log_ids() {
    let mut peer = Peer::open();
    peer.guest();
    let snapshots = burst(&mut peer, &unique("burst"), 50);
    assert_eq!(snapshots.len(), 50);
    strictly_increasing(&snapshots);
    for snapshot in &snapshots {
        assert_eq!(snapshot["log_id"], snapshot["message_id"]);
    }
}

#[test]
#[ignore = "requires APRON_TEST_WS with guest sign-up"]
fn concurrent_posters_give_an_observer_unique_increasing_ids() {
    let mut observer = Peer::open();
    observer.guest();
    let mut a = Peer::open();
    a.guest();
    let mut b = Peer::open();
    b.guest();
    let tag = unique("race");
    for n in 0..25 {
        for (peer, side) in [(&mut a, "a"), (&mut b, "b")] {
            peer.send(json!({"id":format!("{tag}-{side}-{n}"),"method":"message",
                "params":{"body":{"text":format!("{tag} {side}{n}")}}}));
        }
    }
    for n in 0..25 {
        a.reply(&format!("{tag}-a-{n}")).ok();
        b.reply(&format!("{tag}-b-{n}")).ok();
    }
    let seen: Vec<Value> = (0..50)
        .map(|_| {
            observer.take("message", |p| {
                p["body"]["text"]
                    .as_str()
                    .is_some_and(|t| t.starts_with(&tag))
            })
        })
        .collect();
    strictly_increasing(&seen);
}

// --- errors (§1.1) --------------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with email sign-in and a test mail inbox"]
fn a_rate_limited_request_carries_a_numeric_retry_after() {
    let mut peer = Peer::open();
    peer.scheme("email");
    let email = super::auth::email_address();
    let mut limited = None;
    for attempt in 0..32 {
        let exchange = peer.auth(json!({"scheme":"email","email":email,"state":unique("state")}));
        if exchange.frame.get("error").is_some() {
            limited = Some((attempt, exchange.frame));
            break;
        }
        exchange.ok();
    }
    let (attempt, frame) = limited.expect("the server never rate limited email proposals");
    assert!(attempt > 0, "the first proposal must succeed");
    assert_eq!(frame["error"]["code"], -32002, "{frame}");
    let delay = &frame["error"]["data"]["retry_after"];
    assert!(
        delay.as_f64().is_some_and(|d| d > 0.0),
        "retry_after must be a positive number: {frame}"
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS with guest sign-up"]
fn a_frame_over_the_limit_is_rejected_and_not_applied() {
    let mut peer = Peer::open();
    let before = peer.guest()["you"]["name"].clone();
    let id = unique("too-large");
    let prefix = format!(r#"{{"id":"{id}","method":"me","params":{{"name":"renamed"}},"pad":""#);
    let suffix = "\"}";
    let text = format!(
        "{prefix}{}{suffix}",
        "x".repeat(256 * 1024 + 1 - prefix.len() - suffix.len())
    );
    assert_eq!(text.len(), 256 * 1024 + 1);
    peer.send_text(text);
    let exchange = peer.reply(&id);
    assert_eq!(exchange.frame["id"], id.as_str());
    exchange.error(-32003);
    assert_eq!(
        peer.call("me", json!({})).ok()["you"]["name"],
        before,
        "not applied"
    );

    let id = unique("far-too-large");
    let huge = format!(
        r#"{{"id":"{id}","method":"me","params":{{}},"pad":"{}"}}"#,
        "x".repeat(1024 * 1024)
    );
    peer.expect_closed_after(&id, huge);
}

// --- identity (§3.2, A.3) -------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with guest sign-up"]
fn requested_system_user_ids_are_never_granted() {
    for requested in ["~server", "~room", "~private", "~anything"] {
        let mut peer = Peer::open();
        let result = peer
            .auth(json!({"scheme":"guest","user_id":requested}))
            .ok();
        let granted = opaque(&result["you"]["user_id"]);
        assert!(
            !granted.starts_with('~'),
            "{requested} granted as {granted}"
        );
    }
}

#[test]
#[ignore = "requires a restartable APRON_TEST_WS backed by persisted state"]
fn guest_ids_are_not_reused_after_a_restart() {
    let mut before = Peer::open();
    let first = before.guest()["you"]["user_id"].clone();
    before.close();
    control("restart", json!({}));
    let mut after = Peer::open();
    let second = after.guest()["you"]["user_id"].clone();
    assert_ne!(first, second);
}

// --- bearer tokens (§3.2.1) -----------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with guest, token and email sign-in and a test mail inbox"]
fn a_new_token_from_another_sign_in_leaves_earlier_tokens_valid() {
    // Configured policy: this server never rotates or replaces bearer
    // tokens, so every token it issued for an account keeps working.
    let mut guest = Peer::open();
    guest.scheme("email");
    let account = guest.guest();
    let user = account["you"]["user_id"].clone();
    let email = super::auth::email_address();
    let state = unique("add");
    let mail = super::auth::propose(&mut guest, &email, &state);
    let added = guest
        .auth(super::auth::approve(&mail, &state, "email"))
        .ok();
    assert_eq!(added["you"]["user_id"], user);

    let mut device = Peer::open();
    let state = unique("sign-in");
    let mail = super::auth::propose(&mut device, &email, &state);
    let signed_in = device
        .auth(super::auth::approve(&mail, &state, "email"))
        .ok();
    assert_eq!(signed_in["you"]["user_id"], user);
    let fresh = string(&signed_in["token"]);
    assert_ne!(fresh, string(&account["token"]));

    for token in [&account["token"], &signed_in["token"]] {
        let mut peer = Peer::open();
        let result = peer.auth(json!({"scheme":"token","token":token})).ok();
        assert_eq!(result["you"]["user_id"], user);
    }
}
