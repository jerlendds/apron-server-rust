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
    // Embed identity applies to every embed only on a server with some
    // `embed:*` capability (§4.8.2); `cap` asserts this one is advertised.
    peer.cap("embed:upload");
    assert!(
        items(&peer.hello["capabilities"])
            .iter()
            .any(|c| c.as_str().is_some_and(|c| c.starts_with("embed:")))
    );
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
    // An empty value clears a field, and `you` is complete (§3.3.1): this
    // server keeps a cleared avatar as "".
    let cleared = peer.call("me", json!({"avatar":""})).ok();
    assert_eq!(cleared["you"]["avatar"], "", "{cleared}");
    let again = peer.call("me", json!({})).ok();
    assert_eq!(again["you"]["avatar"], "", "{again}");
}

// --- helpers --------------------------------------------------------------

/// A streaming `PUT` to `write_url`, fed through stdin.
struct Writer {
    child: std::process::Child,
    input: Option<std::process::ChildStdin>,
}

impl Writer {
    fn start(write_url: &str) -> Self {
        let mut child = curl()
            .args(["--upload-file", "-", "--url", write_url])
            .args(["--write-out", "%{http_code}"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        Self { child, input }
    }

    fn write(&mut self, bytes: &[u8]) {
        let input = self.input.as_mut().unwrap();
        input.write_all(bytes).unwrap();
        input.flush().unwrap();
    }

    /// Ends the body and returns the HTTP status and the response text.
    fn finish(mut self) -> (u16, String) {
        drop(self.input.take());
        let out = self.child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let (body, code) = text.split_at(text.len().saturating_sub(3));
        (code.parse().unwrap_or(0), body.to_owned())
    }

    /// Drops the connection mid-body, as a crashed writer would.
    fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

/// Waits for the snapshot that adds the live `url` and returns it.
fn live(peer: &mut Peer, id: &Value) -> Value {
    peer.take("message", |p| {
        p["message_id"] == *id && p["body"]["embeds"][0]["url"].is_string()
    })
}

/// Waits for the snapshot that ends live text: the embed has no `url`.
fn ended(peer: &mut Peer, id: &Value, after: &Value) -> Value {
    let after = log_id(&after["log_id"]);
    peer.take("message", |p| {
        p["message_id"] == *id
            && log_id(&p["log_id"]) > after
            && !p["body"]["embeds"][0]["url"].is_string()
    })
}

/// A streaming `GET` of a live text URL, with the headers already read.
struct Reader {
    child: std::process::Child,
    output: std::process::ChildStdout,
}

impl Reader {
    fn open(url: &str, headers: &[&str]) -> (Self, String) {
        let mut command = curl();
        command.args(["--no-buffer", "--include", "--url", url]);
        for header in headers {
            command.args(["--header", header]);
        }
        let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
        let mut output = child.stdout.take().unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            output.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        (Self { child, output }, String::from_utf8(head).unwrap())
    }

    fn read(&mut self, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        self.output.read_exact(&mut bytes).unwrap();
        bytes
    }

    fn rest(mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        self.output.read_to_end(&mut bytes).unwrap();
        self.child.wait().unwrap();
        bytes
    }
}

/// §4.8.6, transcribed.
fn keep_last(output: &[u8], limit: usize) -> &[u8] {
    if output.len() <= limit {
        return output;
    }
    let tail = &output[output.len() - limit..];
    if let Some(i) = tail.iter().position(|&b| b < 0x20) {
        return &tail[i..];
    }
    let skip = tail
        .iter()
        .take_while(|&&b| (0x80..0xc0).contains(&b))
        .count();
    &tail[skip..]
}

fn open(caps: &[&str]) -> Peer {
    let mut peer = Peer::open();
    for cap in caps {
        peer.cap(cap);
    }
    peer.guest();
    peer
}

/// Uploads `bytes` to a new message and returns (message_id, embed_id,
/// completed snapshot).
fn uploaded(peer: &mut Peer, bytes: &[u8]) -> (Value, String, Value) {
    let (id, embed_id, write_url) = embed(peer, "upload");
    let response = http("PUT", &write_url, &[], Some(bytes));
    assert!((200..300).contains(&response.status), "{}", response.status);
    let completed = peer.take("message", |p| {
        p["message_id"] == id && p["body"]["embeds"][0]["url"].is_string()
    });
    (id, embed_id, completed)
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRfixture";

// --- live text (§4.8.5–4.8.6) ------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with embed:append, and curl"]
fn a_reader_that_connects_mid_stream_gets_the_text_from_the_start() {
    let mut peer = open(&["embed:append"]);
    let (id, _, write_url) = embed(&mut peer, "append");
    let mut writer = Writer::start(&write_url);
    writer.write(b"first ");
    let started = live(&mut peer, &id);
    let url = string(&started["body"]["embeds"][0]["url"]).to_owned();
    let (mut early, _) = Reader::open(&url, &[]);
    assert_eq!(early.read(6), b"first ");
    writer.write(b"second ");
    assert_eq!(early.read(7), b"second ");
    let (mut late, _) = Reader::open(&url, &[]);
    assert_eq!(
        late.read(13),
        b"first second ",
        "from the start of the text"
    );
    writer.write(b"third");
    assert_eq!(late.read(5), b"third");
    assert_eq!(writer.finish().0, 204);
    assert_eq!(early.rest(), b"third");
    assert!(late.rest().is_empty());
    let done = ended(&mut peer, &id, &started);
    assert_eq!(done["body"]["text"], "prefix:first second third");
}

#[test]
#[ignore = "requires APRON_TEST_WS with embed:append, and curl"]
fn opening_live_text_as_a_page_serves_plain_text() {
    let mut peer = open(&["embed:append"]);
    let (id, _, write_url) = embed(&mut peer, "append");
    let mut writer = Writer::start(&write_url);
    writer.write(b"page");
    let started = live(&mut peer, &id);
    let url = string(&started["body"]["embeds"][0]["url"]).to_owned();
    let (mut reader, head) = Reader::open(
        &url,
        &["Sec-Fetch-Mode: navigate", "Sec-Fetch-Dest: document"],
    );
    let head = head.to_ascii_lowercase();
    // This server does not redirect pages (§4.8.5 MAY), so it serves the
    // text itself, with the headers required for every reader.
    assert!(head.starts_with("http/1.1 200"), "{head}");
    for header in [
        "content-type: text/plain; charset=utf-8",
        "x-content-type-options: nosniff",
        "cache-control: no-transform",
        "access-control-allow-origin: *",
    ] {
        assert!(head.contains(header), "missing {header}: {head}");
    }
    assert_eq!(reader.read(4), b"page");
    writer.finish();
    reader.rest();
}

#[test]
#[ignore = "requires APRON_TEST_WS with embed:append, and curl"]
fn a_writer_that_disconnects_ends_the_text_with_what_was_kept() {
    let mut peer = open(&["embed:append"]);
    let (id, _, write_url) = embed(&mut peer, "append");
    let mut writer = Writer::start(&write_url);
    writer.write(b"partial \xc3");
    let started = live(&mut peer, &id);
    let url = string(&started["body"]["embeds"][0]["url"]).to_owned();
    let (mut reader, _) = Reader::open(&url, &[]);
    assert_eq!(reader.read(8), b"partial ");
    writer.kill();
    let done = ended(&mut peer, &id, &started);
    // The incomplete sequence at the cut decodes as one U+FFFD.
    assert_eq!(done["body"]["text"], "prefix:partial \u{fffd}");
    assert!(items(&done["body"]["embeds"]).is_empty());
}

#[test]
#[ignore = "requires APRON_TEST_WS with embed:append, edit, and curl"]
fn a_save_that_removes_the_append_embed_ends_the_text_and_wins() {
    let mut peer = open(&["embed:append", "edit"]);
    let (id, _, write_url) = embed(&mut peer, "append");
    let mut writer = Writer::start(&write_url);
    writer.write(b"streamed");
    let started = live(&mut peer, &id);
    let saved = peer.call(
        "message",
        json!({"room_id":started["room_id"],"message_id":id,
               "body":{"text":"prefix:what I meant"}}),
    );
    let snapshot = saved.before("message", |p| p["message_id"] == id).clone();
    saved.ok();
    assert_eq!(snapshot["body"]["text"], "prefix:what I meant");
    assert!(items(&snapshot["body"]["embeds"]).is_empty());
    // The writer is cut off, and the server publishes nothing after the save.
    let _ = writer.input.as_mut().map(|i| i.write_all(b" more"));
    let (status, _) = writer.finish();
    assert!(status == 0 || status >= 400, "status {status}");
    peer.fence();
    let saved_at = log_id(&snapshot["log_id"]);
    assert!(
        !peer.inbox.iter().any(|f| event(f, "message")
            && f["params"]["message_id"] == id
            && log_id(&f["params"]["log_id"]) > saved_at),
        "the save is the final snapshot"
    );
    let page = super::rooms::history(
        &mut peer,
        json!({"room_id":started["room_id"],"after":(saved_at + 1).to_string()}),
    );
    assert!(
        items(&page["messages"])
            .iter()
            .all(|m| m["message_id"] != id)
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS with embed:append, and curl"]
fn append_text_that_fills_the_message_limit_ends_on_a_whole_character() {
    let mut peer = open(&["embed:append"]);
    let (id, _, write_url) = embed(&mut peer, "append");
    let mut writer = Writer::start(&write_url);
    let sent = "é".repeat(150_000);
    writer.write("é".as_bytes());
    let started = live(&mut peer, &id);
    let input = writer.input.as_mut().unwrap();
    let _ = input.write_all(&sent.as_bytes()[2..]);
    let (status, reason) = writer.finish();
    assert_eq!(status, 413, "{reason}");
    assert!(!reason.is_empty(), "a plain-text reason");
    let done = ended(&mut peer, &id, &started);
    let text = string(&done["body"]["text"]);
    let appended = text.strip_prefix("prefix:").unwrap();
    assert!(!appended.is_empty() && appended.len() < sent.len());
    assert!(sent.starts_with(appended), "kept the first part");
    assert!(appended.chars().all(|c| c == 'é'), "no partial character");
    assert!(
        done.to_string().len() < 512 * 1024,
        "the final snapshot fits in a frame"
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS with embed:terminal, and curl"]
fn terminal_output_over_the_kept_size_keeps_the_last_part() {
    let mut peer = open(&["embed:terminal"]);
    let (id, embed_id, write_url) = embed(&mut peer, "terminal");
    // Long lines of two-byte characters, so a cut almost surely falls inside
    // a line and inside a character.
    let mut output = Vec::new();
    for line in 0..300 {
        output.extend_from_slice(format!("\x1b[32m{line:04}\x1b[0m ").as_bytes());
        output.extend_from_slice("é".repeat(500).as_bytes());
        output.extend_from_slice(b"\r\n");
    }
    assert!(output.len() > 256 * 1024);
    let mut writer = Writer::start(&write_url);
    writer.write(&output[..1]);
    let started = live(&mut peer, &id);
    writer.write(&output[1..]);
    assert_eq!(writer.finish().0, 204);
    let done = ended(&mut peer, &id, &started);
    let embed = &done["body"]["embeds"][0];
    assert_eq!(embed["embed_id"], embed_id);
    let kept = string(&embed["output"]);
    assert!(kept.len() < output.len(), "the output was cut");
    // The server's kept size isn't advertised. Whatever it is, the kept part
    // is `keep_last` of the output at that size: it starts at the first
    // control byte of the tail, so at most one line shorter than the size.
    assert!(output.ends_with(kept.as_bytes()));
    assert!(
        (kept.len()..=kept.len() + 1100).any(|limit| keep_last(&output, limit) == kept.as_bytes()),
        "not a keep_last cut: {:?}",
        &kept[..40]
    );
    assert!(kept.starts_with(['\x1b', '\r', '\n']), "{:?}", &kept[..8]);
}

#[test]
#[ignore = "requires APRON_TEST_WS with embed:append, embed:terminal, and curl"]
fn finished_text_is_decoded_as_utf8_without_bom() {
    let raw = b"\xef\xbb\xbfA\xffB\xed\xa0\x80C\xe2\x82";
    let expected = "\u{feff}A\u{fffd}B\u{fffd}\u{fffd}\u{fffd}C\u{fffd}";
    let mut peer = open(&["embed:append", "embed:terminal"]);
    for kind in ["append", "terminal"] {
        let (id, _, write_url) = embed(&mut peer, kind);
        let mut writer = Writer::start(&write_url);
        writer.write(raw);
        let started = live(&mut peer, &id);
        writer.finish();
        let done = ended(&mut peer, &id, &started);
        if kind == "append" {
            assert_eq!(done["body"]["text"], format!("prefix:{expected}"));
        } else {
            assert_eq!(done["body"]["embeds"][0]["output"], expected);
        }
    }
}

#[test]
#[ignore = "requires APRON_TEST_WS with embed:append, embed:terminal, and a fixture-controlled grant timeout"]
fn live_text_that_never_starts_removes_the_pending_embed() {
    let mut peer = open(&["embed:append", "embed:terminal"]);
    let pending = ["append", "terminal"].map(|kind| {
        let (id, embed_id, write_url) = embed(&mut peer, kind);
        (id, embed_id, write_url)
    });
    control("expire-upload", json!({"embed_id":pending[0].1}));
    for (id, _, write_url) in &pending {
        let expired = peer.take("message", |p| p["message_id"] == *id && p["log_id"] != *id);
        assert!(items(&expired["body"]["embeds"]).is_empty(), "{expired}");
        assert_eq!(expired["body"]["text"], "prefix:");
        let late = http("PUT", write_url, &[], Some(b"too late"));
        assert_eq!(late.status, 404);
    }
}

// --- uploads (§4.8.4) ------------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with embed:upload, edit, and curl"]
fn hosted_files_are_deleted_with_their_embed_or_message() {
    let mut peer = open(&["embed:upload", "edit"]);
    let (id, embed_id, completed) = uploaded(&mut peer, b"short-lived");
    let url = string(&completed["body"]["embeds"][0]["url"]).to_owned();
    assert!(!url.contains(&embed_id), "{url} exposes {embed_id}");
    let secret = url.rsplit('/').next().unwrap();
    assert!(secret.len() >= 16, "a random-looking segment: {url}");
    assert_eq!(http("GET", &url, &[], None).status, 200);
    peer.call(
        "message",
        json!({"room_id":completed["room_id"],"message_id":id,"body":{"text":"no file"}}),
    )
    .ok();
    assert_eq!(
        http("GET", &url, &[], None).status,
        404,
        "removed by a save"
    );

    let (id, _, completed) = uploaded(&mut peer, b"deleted with message");
    let url = string(&completed["body"]["embeds"][0]["url"]).to_owned();
    peer.call(
        "message",
        json!({"room_id":completed["room_id"],"message_id":id,"deleted":true}),
    )
    .ok();
    assert_eq!(
        http("GET", &url, &[], None).status,
        404,
        "removed by a delete"
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS with embed:upload, and curl"]
fn an_upload_over_the_limit_fails_with_a_reason_and_drops_the_embed() {
    let mut peer = open(&["embed:upload"]);
    let (id, _, write_url) = embed(&mut peer, "upload");
    let mut writer = Writer::start(&write_url);
    let chunk = vec![b'x'; 64 * 1024];
    for _ in 0..40 {
        if writer.input.as_mut().unwrap().write_all(&chunk).is_err() {
            break;
        }
    }
    let (status, reason) = writer.finish();
    assert_eq!(status, 413, "{reason}");
    assert!(!reason.trim().is_empty());
    let snapshot = peer.take("message", |p| p["message_id"] == id && p["log_id"] != id);
    assert!(items(&snapshot["body"]["embeds"]).is_empty(), "{snapshot}");
}

// --- avatars (§4.8.7) -------------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with command, embed:upload, and curl"]
fn avatar_command_sets_the_avatar_from_an_upload() {
    let mut peer = open(&["command", "embed:upload"]);
    let me = peer.call("me", json!({})).ok()["you"]["user_id"].clone();
    let mut observer = open(&[]);
    observer.fence();
    observer.inbox.clear();
    let result = peer
        .call(
            "command",
            json!({"body":{"text":"/avatar","embeds":[{"kind":"upload","title":"me.png"}]}}),
        )
        .ok();
    let grants = array(&result["embeds"]);
    assert_eq!(grants.len(), 1);
    let write_url = string(&grants[0]["write_url"]);

    let rejected = http("PUT", write_url, &[], Some(b"not an image"));
    assert_eq!(rejected.status, 415);
    assert!(!rejected.body.is_empty());

    let result = peer
        .call(
            "command",
            json!({"body":{"text":"/avatar","embeds":[{"kind":"upload"}]}}),
        )
        .ok();
    let write_url = string(&result["embeds"][0]["write_url"]).to_owned();
    assert_eq!(http("PUT", &write_url, &[], Some(PNG)).status, 204);
    let own = peer.take("user", |p| p["you"]["user_id"] == me);
    let avatar = string(&own["you"]["avatar"]).to_owned();
    let seen = observer.take("user", |p| p["new"]["user_id"] == me);
    assert_eq!(seen["new"]["avatar"], avatar.as_str());
    let image = http("GET", &avatar, &[], None);
    assert_eq!(image.status, 200);
    assert_eq!(image.header("content-type"), Some("image/png"));
    assert_eq!(image.body, PNG);

    peer.call(
        "command",
        json!({"body":{"text":"/avatar","embeds":[{"kind":"upload"},{"kind":"upload"}]}}),
    )
    .error(INVALID);
}

// --- embed identity (§4.8.2) -------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with embed:terminal, edit, and curl"]
fn a_save_keeps_a_finished_terminal_output_whatever_the_client_sends() {
    let mut peer = open(&["embed:terminal", "edit"]);
    let (id, embed_id, write_url) = embed(&mut peer, "terminal");
    let mut writer = Writer::start(&write_url);
    writer.write(b"real output\r\n");
    let started = live(&mut peer, &id);
    writer.finish();
    let done = ended(&mut peer, &id, &started);
    let saved = peer.call(
        "message",
        json!({"room_id":done["room_id"],"message_id":id,"body":{"text":"edited",
            "embeds":[{"kind":"terminal","embed_id":embed_id,"title":"renamed",
                       "output":"forged","url":"https://example.invalid/live"}]}}),
    );
    let snapshot = saved.before("message", |p| p["message_id"] == id).clone();
    assert!(
        saved.ok().get("embeds").is_none(),
        "a kept embed gets no grant"
    );
    let embed = &snapshot["body"]["embeds"][0];
    assert_eq!(embed["output"], "real output\r\n");
    assert!(embed.get("url").is_none());
    assert_eq!(embed["title"], "renamed", "client fields still change");
}

#[test]
#[ignore = "requires APRON_TEST_WS with embed:upload, edit, and curl"]
fn a_save_removes_only_the_embed_it_leaves_out() {
    let mut peer = open(&["embed:upload", "edit"]);
    let posted = peer.post(json!({"body":{"text":"three","embeds":[
        {"kind":"html","html":"<p>one</p>"},
        {"kind":"iframe","url":"https://example.invalid/two"},
        {"kind":"html","html":"<p>three</p>"}
    ]}}));
    let id = &posted["message_id"];
    let embeds = array(&posted["body"]["embeds"]).to_vec();
    let saved = peer.call(
        "message",
        json!({"room_id":posted["room_id"],"message_id":id,"body":{"text":"two",
            "embeds":[embeds[0].clone(), embeds[2].clone()]}}),
    );
    let snapshot = saved.before("message", |p| p["message_id"] == *id).clone();
    saved.ok();
    let kept = array(&snapshot["body"]["embeds"]);
    assert_eq!(kept, [embeds[0].clone(), embeds[2].clone()]);

    let all = peer.call(
        "message",
        json!({"room_id":posted["room_id"],"message_id":id,"body":{"text":"again",
            "embeds":kept}}),
    );
    let snapshot = all.before("message", |p| p["message_id"] == *id).clone();
    all.ok();
    assert_eq!(array(&snapshot["body"]["embeds"]), kept);
}
