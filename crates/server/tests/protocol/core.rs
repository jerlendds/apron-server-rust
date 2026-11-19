use super::support::*;
use serde_json::json;
use std::env;

#[test]
#[ignore = "requires APRON_TEST_WS"]
fn greeting_ping_and_unauthenticated_requests() {
    let mut peer = Peer::open();
    if !peer.hello["ping"].is_null() {
        assert!(peer.hello["ping"].as_f64().unwrap() > 0.0);
        peer.send(json!({"method":"ping"}));
        peer.take("pong", |_| true);
    }
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
