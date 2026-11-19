use serde_json::{Value, json};
use std::{
    env,
    io::{ErrorKind, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tungstenite::{Message, WebSocket, client::IntoClientRequest, http::header::ORIGIN};

pub const DENIED: i64 = -32001;
pub const INVALID: i64 = -32602;
pub const UNSUPPORTED: i64 = -32601;
const TIMEOUT: Duration = Duration::from_secs(8);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn unique(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{label}-{:x}-{nanos:x}-{sequence:x}", std::process::id())
}

pub struct Peer {
    ws: WebSocket<TcpStream>,
    pub hello: Value,
    pub inbox: Vec<Value>,
}

pub struct Exchange {
    pub frame: Value,
    pub before: Vec<Value>,
}

impl Exchange {
    pub fn ok(self) -> Value {
        assert!(
            self.frame.get("error").is_none(),
            "unexpected error: {}",
            self.frame
        );
        let result = &self.frame["result"];
        assert!(result.is_object(), "missing result object: {}", self.frame);
        result.clone()
    }

    pub fn error(self, code: i64) {
        assert!(
            self.frame.get("result").is_none(),
            "unexpected result: {}",
            self.frame
        );
        assert_eq!(self.frame["error"]["code"], code, "{}", self.frame);
        string(&self.frame["error"]["message"]);
    }

    pub fn before(&self, method: &str, matches: impl Fn(&Value) -> bool) -> &Value {
        self.before
            .iter()
            .find(|f| event(f, method) && matches(&f["params"]))
            .unwrap_or_else(|| panic!("no {method} before reply: {:?}", self.before))
            .get("params")
            .unwrap()
    }
}

pub fn event(frame: &Value, method: &str) -> bool {
    frame["method"] == method && frame.get("id").is_none()
}

fn loopback(uri: &tungstenite::http::Uri) -> SocketAddr {
    assert_eq!(uri.scheme_str(), Some("ws"), "use a local ws:// fixture");
    let host = uri
        .host()
        .expect("WS URI has no host")
        .trim_matches(['[', ']']);
    let addr = (host, uri.port_u16().unwrap_or(80))
        .to_socket_addrs()
        .unwrap()
        .next()
        .expect("WS URI did not resolve");
    assert!(
        addr.ip().is_loopback(),
        "fixture must listen on loopback: {addr}"
    );
    addr
}

impl Peer {
    pub fn open() -> Self {
        Self::at(&env::var("APRON_TEST_WS").expect("set APRON_TEST_WS"), None)
    }

    pub fn at(url: &str, origin: Option<&str>) -> Self {
        let mut request = url.into_client_request().expect("invalid WS URL");
        let socket = TcpStream::connect_timeout(&loopback(request.uri()), TIMEOUT).unwrap();
        socket.set_read_timeout(Some(TIMEOUT)).unwrap();
        socket.set_write_timeout(Some(TIMEOUT)).unwrap();
        socket.set_nodelay(true).unwrap();
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert(ORIGIN, origin.parse().unwrap());
        }
        let (ws, _) = tungstenite::client(request, socket).expect("WebSocket handshake failed");
        let mut peer = Self {
            ws,
            hello: Value::Null,
            inbox: Vec::new(),
        };
        let greeting = peer.next();
        assert!(
            event(&greeting, "server"),
            "expected immediate greeting: {greeting}"
        );
        peer.hello = greeting["params"].clone();
        assert_eq!(peer.hello["apron"], 10);
        assert!(!array(&peer.hello["auth"]).is_empty());
        for scheme in array(&peer.hello["auth"]) {
            string(scheme);
        }
        peer
    }

    pub fn cap(&self, expected: &str) {
        assert!(
            self.has_cap(expected),
            "fixture must advertise {expected}: {}",
            self.hello
        );
    }

    pub fn has_cap(&self, expected: &str) -> bool {
        items(&self.hello["capabilities"])
            .iter()
            .any(|name| name == expected)
    }

    pub fn scheme(&self, expected: &str) {
        assert!(
            items(&self.hello["auth"])
                .iter()
                .chain(items(&self.hello["signup"]))
                .any(|name| name == expected),
            "fixture must offer {expected}: {}",
            self.hello
        );
    }

    pub fn close(mut self) {
        self.ws.close(None).expect("close frame");
    }

    pub fn send(&mut self, frame: Value) {
        self.send_text(frame.to_string());
    }

    pub fn send_text(&mut self, text: String) {
        self.ws.send(Message::Text(text.into())).unwrap();
    }

    pub fn next(&mut self) -> Value {
        loop {
            match self.ws.read() {
                Ok(Message::Text(text)) => {
                    let frame: Value = serde_json::from_str(&text).expect("invalid server JSON");
                    assert!(frame.is_object(), "expected one object per frame: {frame}");
                    assert!(
                        frame.get("jsonrpc").is_none(),
                        "unexpected JSON-RPC version: {frame}"
                    );
                    if let Some(id) = frame.get("id") {
                        assert!(id.is_string(), "non-string reply ID: {frame}");
                    }
                    return frame;
                }
                Ok(Message::Ping(_)) => self.ws.flush().unwrap(),
                Ok(Message::Pong(_)) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) =>
                {
                    panic!(
                        "timed out waiting for frame; recent inbox: {:?}",
                        self.inbox
                    )
                }
                frame => panic!("unexpected WebSocket input: {frame:?}"),
            }
        }
    }

    pub fn reply(&mut self, id: &str) -> Exchange {
        let mut before = Vec::new();
        for _ in 0..256 {
            let frame = self.next();
            if frame["id"] == id {
                return Exchange { frame, before };
            }
            before.push(frame.clone());
            self.inbox.push(frame);
        }
        panic!("too many frames awaiting {id}");
    }

    pub fn call_id(&mut self, id: &str, method: &str, params: Value) -> Exchange {
        self.send(json!({"id": id, "method": method, "params": params}));
        self.reply(id)
    }

    pub fn call(&mut self, method: &str, params: Value) -> Exchange {
        self.call_id(&unique("req"), method, params)
    }

    pub fn auth(&mut self, params: Value) -> Exchange {
        self.call("auth", params)
    }

    pub fn guest(&mut self) -> Value {
        self.scheme("guest");
        let result = self
            .auth(json!({"scheme":"guest","name":unique("Guest")}))
            .ok();
        opaque(&result["you"]["user_id"]);
        result
    }

    pub fn take(&mut self, method: &str, matches: impl Fn(&Value) -> bool) -> Value {
        if let Some(i) = self
            .inbox
            .iter()
            .position(|f| event(f, method) && matches(&f["params"]))
        {
            return self.inbox.remove(i)["params"].clone();
        }
        for _ in 0..256 {
            let frame = self.next();
            if event(&frame, method) && matches(&frame["params"]) {
                return frame["params"].clone();
            }
            self.inbox.push(frame);
        }
        panic!("too many frames awaiting {method}");
    }

    // Fence delivery already caused by a completed sender operation with a reply
    // on this connection. No sleep is needed for checking ordered delivery.
    pub fn fence(&mut self) {
        self.call("ext:conformance-fence", json!({}))
            .error(UNSUPPORTED);
    }

    pub fn room(&mut self, params: Value) -> String {
        let exchange = self.call("room_set", params);
        let id = opaque(&exchange.frame["result"]["room_id"]).to_owned();
        let update = exchange.before("room_update", |p| has_room(p, "joined", &id));
        let joined = items(&update["joined"])
            .iter()
            .find(|r| r["room_id"] == id)
            .unwrap();
        array(&joined["members"]);
        assert!(
            items(&update["memberships"])
                .iter()
                .any(|r| r["room_id"] == id)
        );
        exchange.ok();
        id
    }

    pub fn join(&mut self, room: &str) {
        let x = self.call("room_join", json!({"room_id":room}));
        x.before("room_update", |p| has_room(p, "joined", room));
        assert_eq!(x.ok(), json!({}));
    }

    pub fn post(&mut self, params: Value) -> Value {
        let x = self.call("message", params);
        let id = string(&x.frame["result"]["message_id"]);
        let snapshot = x.before("message", |p| p["message_id"] == id).clone();
        let sequence = log_id(&snapshot["log_id"]);
        assert_eq!(sequence, log_id(&snapshot["message_id"]));
        opaque(&snapshot["room_id"]);
        opaque(&snapshot["from"]["user_id"]);
        assert!(snapshot["body"].is_object());
        x.ok();
        snapshot
    }
}

pub fn has_room(params: &Value, key: &str, id: &str) -> bool {
    items(&params[key]).iter().any(|r| r["room_id"] == id)
}

pub fn string(value: &Value) -> &str {
    value
        .as_str()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| panic!("expected nonempty string: {value}"))
}

pub fn opaque(value: &Value) -> &str {
    let value = string(value);
    assert!(
        !value.chars().any(|c| c.is_whitespace() || c.is_control()),
        "invalid opaque ID: {value:?}"
    );
    value
}

pub fn log_id(value: &Value) -> u64 {
    let id = string(value);
    assert!(
        id.bytes().all(|c| c.is_ascii_digit()),
        "non-decimal log ID: {id}"
    );
    let id = id.parse().expect("invalid log ID");
    assert!((1..(1 << 53)).contains(&id), "out-of-range log ID: {id}");
    id
}

pub fn array(value: &Value) -> &[Value] {
    value
        .as_array()
        .unwrap_or_else(|| panic!("expected array: {value}"))
}

pub fn items(value: &Value) -> &[Value] {
    if value.is_null() { &[] } else { array(value) }
}

// Provider orchestration is fixture-specific. Pass JSON on stdin and receive
// only observations (mail, browser redirects, received push, etc.) on stdout.
pub fn control(action: &str, input: Value) -> Value {
    let program =
        env::var_os("APRON_TEST_CONTROL").expect("set APRON_TEST_CONTROL for provider tests");
    let mut child = Command::new(program)
        .arg(action)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let response = child.wait_with_output().unwrap();
    assert!(
        response.status.success(),
        "fixture action {action} failed: {}",
        response.status
    );
    serde_json::from_slice(&response.stdout)
        .unwrap_or_else(|e| panic!("invalid {action} response: {e}"))
}

pub struct Http {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Http {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub fn curl() -> Command {
    let mut command = Command::new("curl");
    command.args([
        "--silent",
        "--show-error",
        "--max-time",
        "8",
        "--noproxy",
        "*",
        "--proto",
        "=http,https",
        "--header",
        "Expect:",
    ]);
    command
}

pub fn http(method: &str, url: &str, headers: &[&str], body: Option<&[u8]>) -> Http {
    let mut command = curl();
    command.args(["--include", "--request", method]);
    for header in headers {
        command.args(["--header", header]);
    }
    if body.is_some() {
        command.args(["--data-binary", "@-"]).stdin(Stdio::piped());
    }
    let mut child = command
        .args(["--url", url])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(body) = body {
        child.stdin.take().unwrap().write_all(body).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "curl {method} failed: {}", out.status);
    let mut rest = &out.stdout[..];
    loop {
        let end = rest
            .windows(4)
            .position(|chunk| chunk == b"\r\n\r\n")
            .expect("missing HTTP headers");
        let headers = std::str::from_utf8(&rest[..end]).unwrap();
        let mut lines = headers.lines();
        let status = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        rest = &rest[end + 4..];
        if !(100..200).contains(&status) {
            return Http {
                status,
                headers: lines
                    .map(|line| {
                        let (key, value) = line.split_once(':').unwrap();
                        (key.to_owned(), value.trim().to_owned())
                    })
                    .collect(),
                body: rest.to_vec(),
            };
        }
    }
}
