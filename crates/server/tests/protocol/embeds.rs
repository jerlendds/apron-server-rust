use super::support::*;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    process::Stdio,
};

fn embed(peer: &mut Peer, kind: &str) -> (Value, String, String) {
    let x = peer.call("message", json!({"body":{"text":"prefix:","embeds":[
        {"kind":kind,"title":"test.txt","embed_id":"client-id","url":"https://example.invalid/client",
         "output":"client data"}
    ]}}));
    let id = x.frame["result"]["message_id"].clone();
    let pending = x.before("message", |p| p["message_id"] == id);
    let public = &pending["body"]["embeds"][0];
    let embed_id = opaque(&public["embed_id"]).to_owned();
    assert_ne!(embed_id, "client-id");
    for field in ["url", "output", "write_url"] {
        assert!(public.get(field).is_none());
    }
    let result = x.ok();
    assert_eq!(result["embeds"][0]["embed_id"], embed_id);
    assert_eq!(result["embeds"][0]["kind"], kind);
    let write_url = string(&result["embeds"][0]["write_url"]).to_owned();
    (id, embed_id, write_url)
}

#[test]
#[ignore = "requires APRON_TEST_WS with upload and edit, and curl"]
fn upload_grants_are_private_single_use_and_completed_by_snapshot() {
    let mut peer = Peer::open();
    peer.cap("embed:upload");
    peer.cap("edit");
    peer.guest();
    let (id, embed_id, write_url) = embed(&mut peer, "upload");
    let origin = "Origin: https://fixture-client.invalid";
    let preflight = http(
        "OPTIONS",
        &write_url,
        &[
            origin,
            "Access-Control-Request-Method: PUT",
            "Access-Control-Request-Headers: content-type",
        ],
        None,
    );
    assert!((200..300).contains(&preflight.status));
    assert!(matches!(
        preflight.header("Access-Control-Allow-Origin"),
        Some("*" | "https://fixture-client.invalid")
    ));
    let methods = preflight.header("Access-Control-Allow-Methods").unwrap();
    assert!(
        methods
            .split(',')
            .any(|method| matches!(method.trim(), "*" | "PUT"))
    );
    let response = http(
        "PUT",
        &write_url,
        &[origin, "Content-Type: text/plain"],
        Some(b"apron fixture\n"),
    );
    assert!((200..300).contains(&response.status));
    assert!(matches!(
        response.header("Access-Control-Allow-Origin"),
        Some("*" | "https://fixture-client.invalid")
    ));
    let completed = peer.take("message", |p| {
        p["message_id"] == id && p["body"]["embeds"][0]["url"].is_string()
    });
    assert!(log_id(&completed["log_id"]) > log_id(&id));
    let public = &completed["body"]["embeds"][0];
    assert_eq!(public["embed_id"], embed_id);
    assert!(public.get("write_url").is_none());
    let asset = string(&public["url"]);
    let file = http("GET", asset, &[], None);
    assert_eq!(file.status, 200);
    assert_eq!(file.body, b"apron fixture\n");
    let reuse = http("PUT", &write_url, &[origin], Some(b"replacement"));
    assert!((400..500).contains(&reuse.status));
    assert!(!reuse.body.is_empty());

    let save = peer.call("message", json!({"room_id":completed["room_id"],"message_id":id,"body":{
        "text":"updated","embeds":[{"kind":"upload","embed_id":embed_id,"url":"https://example.invalid/override"}]
    }}));
    let snapshot = save.before("message", |p| p["message_id"] == id);
    assert_eq!(snapshot["body"]["embeds"][0]["url"], asset);
    save.ok();
    peer.call(
        "message",
        json!({"room_id":completed["room_id"],"message_id":id,
        "body":{"text":"remove attachment"}}),
    )
    .ok();
    peer.call(
        "message",
        json!({"room_id":completed["room_id"],"message_id":id,"body":{
            "text":"stale","embeds":[{"kind":"upload","embed_id":embed_id}]
        }}),
    )
    .error(INVALID);
}

fn check_stream(kind: &str, payload: &[u8], assert_complete: impl FnOnce(&Value, &str)) {
    let mut peer = Peer::open();
    peer.cap(&format!("embed:{kind}"));
    peer.guest();
    let (id, embed_id, grant) = embed(&mut peer, kind);
    let mut writer = curl()
        .args(["--fail", "--upload-file", "-", "--url", &grant])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = writer.stdin.take().unwrap();
    input.write_all(payload).unwrap();
    input.flush().unwrap();
    let live = peer.take("message", |p| {
        p["message_id"] == id && p["body"]["embeds"][0]["url"].is_string()
    });
    let live_id = log_id(&live["log_id"]);
    let live_url = string(&live["body"]["embeds"][0]["url"]);
    let mut reader = curl()
        .args(["--no-buffer", "--include", "--url", live_url])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = reader.stdout.take().unwrap();
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        assert!(headers.len() < 16 * 1024, "oversized live text headers");
        let mut byte = [0];
        output.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
    }
    let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
    assert!(headers.contains(" 200 "));
    for header in [
        "content-type: text/plain; charset=utf-8",
        "x-content-type-options: nosniff",
        "cache-control: no-transform",
        "access-control-allow-origin: *",
    ] {
        assert!(headers.contains(header), "missing {header}: {headers}");
    }
    let mut live_body = vec![0; payload.len()];
    output.read_exact(&mut live_body).unwrap();
    assert_eq!(live_body, payload);
    drop(input);
    assert!(writer.wait().unwrap().success());
    let mut tail = Vec::new();
    output.read_to_end(&mut tail).unwrap();
    assert!(tail.is_empty());
    assert!(reader.wait().unwrap().success());
    let finished = peer.take("message", |p| {
        p["message_id"] == id && log_id(&p["log_id"]) > live_id
    });
    assert_complete(&finished, &embed_id);
}

#[test]
#[ignore = "requires APRON_TEST_WS with embed:append, and curl"]
fn append_stream_folds_edits_into_body_and_drops_embed() {
    check_stream("append", b"old\x0cHellx\x08o", |snapshot, _| {
        assert_eq!(snapshot["body"]["text"], "prefix:Hello");
        assert!(items(&snapshot["body"]["embeds"]).is_empty());
    });
    let mut peer = Peer::open();
    peer.guest();
    peer.call(
        "message",
        json!({"body":{"text":"invalid","embeds":[
        {"kind":"append"},{"kind":"append"}]}}),
    )
    .error(INVALID);
}

#[test]
#[ignore = "requires APRON_TEST_WS with embed:terminal, and curl"]
fn terminal_stream_finishes_as_output_without_live_url() {
    check_stream(
        "terminal",
        b"\x1b[32mOK\x1b[0m\r\n",
        |snapshot, embed_id| {
            assert_eq!(snapshot["body"]["text"], "prefix:");
            let embed = &snapshot["body"]["embeds"][0];
            assert_eq!(embed["embed_id"], embed_id);
            assert_eq!(embed["output"], "\x1b[32mOK\x1b[0m\r\n");
            assert!(embed.get("url").is_none());
        },
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS with upload and a fixture-controlled grant timeout"]
fn unused_upload_expires_and_removes_the_pending_embed() {
    let mut peer = Peer::open();
    peer.cap("embed:upload");
    peer.guest();
    let (message, id, url) = embed(&mut peer, "upload");
    control("expire-upload", json!({"embed_id":id}));
    let expired = peer.take("message", |p| {
        p["message_id"] == message && p["log_id"] != message
    });
    assert!(items(&expired["body"]["embeds"]).is_empty());
    let result = http("PUT", &url, &[], Some(b"expired"));
    assert!((400..500).contains(&result.status));
}

#[test]
#[ignore = "requires APRON_TEST_WS with embed identity support"]
fn inline_embeds_preserve_display_order_and_can_be_the_entire_message() {
    let mut peer = Peer::open();
    peer.cap("embed:upload");
    peer.guest();
    let snapshot = peer.post(json!({"body":{"embeds":[
        {"kind":"html","title":"Inline test","html":"<p>hello</p>"},
        {"kind":"iframe","title":"Frame test","url":"https://example.invalid/frame","height":320,
         "og":{"title":"preview"}}
    ]}}));
    let embeds = array(&snapshot["body"]["embeds"]);
    assert_eq!(embeds.len(), 2);
    assert_eq!(embeds[0]["kind"], "html");
    assert_eq!(embeds[1]["kind"], "iframe");
    assert_ne!(
        opaque(&embeds[0]["embed_id"]),
        opaque(&embeds[1]["embed_id"])
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS whose profile policy accepts HTTPS avatars"]
fn user_avatar_is_set_and_cleared_via_profile_update() {
    let mut peer = Peer::open();
    let identity = peer.guest()["you"]["user_id"].clone();
    let url = "https://example.invalid/avatar.png";
    let user = peer.call("me", json!({"avatar":url})).ok();
    assert_eq!(user["you"]["user_id"], identity);
    assert_eq!(user["you"]["avatar"], url);
    let cleared = peer.call("me", json!({"avatar":""})).ok();
    assert!(cleared["you"].get("avatar").is_none() || cleared["you"]["avatar"] == "");
}
