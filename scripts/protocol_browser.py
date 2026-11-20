"""Chromium and virtual-authenticator controls for the protocol fixture."""

from contextlib import contextmanager
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
from urllib.parse import parse_qsl, urlencode, urlsplit
from urllib.request import Request, urlopen


def b64decode(value):
    return base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))


def b64encode(value):
    return base64.urlsafe_b64encode(value).decode().rstrip("=")


class Browser:
    def __init__(self, port, profile):
        chrome = {
            "args": [
                "--headless=new", "--no-sandbox", "--disable-dev-shm-usage",
                "--disable-background-networking", "--disable-component-update",
                "--no-first-run", "--no-proxy-server", f"--user-data-dir={profile}",
            ],
        }
        if binary := os.environ.get("APRON_TEST_CHROME"):
            chrome["binary"] = binary
        self.url = f"http://127.0.0.1:{port}"
        self.session = None
        result = self.call("POST", "/session", {
            "capabilities": {"alwaysMatch": {
                "browserName": "chrome", "goog:chromeOptions": chrome,
                "goog:loggingPrefs": {"performance": "ALL"},
                "timeouts": {"implicit": 0, "pageLoad": 10000, "script": 10000},
            }}
        })
        self.session = result["sessionId"]
        self.cdp("WebAuthn.enable")
        self.authenticator = self.cdp("WebAuthn.addVirtualAuthenticator", {
            "options": {
                "protocol": "ctap2", "transport": "internal", "hasResidentKey": True,
                "hasUserVerification": True, "isUserVerified": True,
                "automaticPresenceSimulation": True,
            }
        })["authenticatorId"]

    def call(self, method, path, data=None):
        url = self.url + (f"/session/{self.session}" if self.session else "") + path
        body = None if data is None else json.dumps(data).encode()
        request = Request(url, method=method, data=body, headers={"Content-Type": "application/json"})
        with urlopen(request, timeout=15) as response:
            return json.load(response)["value"]

    def cdp(self, command, params=None):
        return self.call("POST", "/goog/cdp/execute", {"cmd": command, "params": params or {}})

    def navigate(self, url):
        self.call("POST", "/url", {"url": url})

    def script(self, script, *args, asynchronous=False):
        path = "async" if asynchronous else "sync"
        return self.call("POST", f"/execute/{path}", {"script": script, "args": args})

    def credentials(self):
        return self.cdp("WebAuthn.getCredentials", {
            "authenticatorId": self.authenticator,
        })["credentials"]

    def restore(self, credentials):
        for credential in credentials:
            self.cdp("WebAuthn.addCredential", {
                "authenticatorId": self.authenticator, "credential": credential,
            })

    def redirect(self):
        deadline = time.monotonic() + 6
        while time.monotonic() < deadline:
            for entry in self.call("POST", "/log", {"type": "performance"}):
                event = json.loads(entry["message"])["message"]
                if event["method"] != "Network.responseReceivedExtraInfo":
                    continue
                headers = {key.lower(): value for key, value in event["params"]["headers"].items()}
                if "location" not in headers:
                    continue
                fields = dict(parse_qsl(urlsplit(headers["location"]).query))
                if "state" in fields and ("code" in fields or "error" in fields):
                    return fields
            time.sleep(0.05)
        raise RuntimeError("authorization page did not redirect: " +
                           str(self.script("return document.body.innerText") or "")[:400])


@contextmanager
def browser():
    driver = os.environ.get("APRON_TEST_CHROMEDRIVER") or shutil.which("chromedriver")
    if driver is None:
        raise RuntimeError("install Chromium and a matching chromedriver, "
                           "or set APRON_TEST_CHROMEDRIVER")
    with tempfile.TemporaryDirectory(prefix="apron-chromium-") as directory:
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        log = Path(directory) / "driver.log"
        with log.open("w") as output:
            process = subprocess.Popen([driver, f"--port={port}"], stdout=output, stderr=output)
        client = None
        try:
            deadline = time.monotonic() + 6
            while time.monotonic() < deadline:
                try:
                    with urlopen(f"http://127.0.0.1:{port}/status", timeout=0.5):
                        break
                except OSError:
                    if process.poll() is not None:
                        raise RuntimeError(log.read_text())
                    time.sleep(0.05)
            else:
                raise RuntimeError("chromedriver did not start")
            client = Browser(port, Path(directory) / "profile")
            yield client
        finally:
            if client is not None:
                try:
                    client.call("DELETE", "")
                except OSError:
                    pass
            process.terminate()
            process.wait(timeout=10)


GET_CREDENTIAL = """
const [action, options, finish] = arguments;
(async () => {
    const register = action === 'register';
    const publicKey = register
        ? PublicKeyCredential.parseCreationOptionsFromJSON(options)
        : PublicKeyCredential.parseRequestOptionsFromJSON(options);
    const credential = register
        ? await navigator.credentials.create({publicKey})
        : await navigator.credentials.get({publicKey});
    finish(credential.toJSON());
})().catch(error => finish({error: String(error)}));
"""


def passkey_credential(request, directory):
    origin = request["origin"]
    options = request["public_key"]
    action = request["action"]
    path = directory / ("key-" + hashlib.sha256(request["key"].encode()).hexdigest())
    with browser() as client:
        client.navigate(origin + "/health")
        if action == "login":
            client.restore(json.loads(path.read_text()))
        credential = client.script(GET_CREDENTIAL, action, options, asynchronous=True)
        if "error" in credential:
            raise RuntimeError(credential["error"])
        if action == "register":
            path.write_text(json.dumps(client.credentials()))
    response = credential["response"]
    variant = request["variant"]
    if variant == "wrong-origin":
        data = json.loads(b64decode(response["clientDataJSON"]))
        data["origin"] = "https://wrong-origin.invalid"
        response["clientDataJSON"] = b64encode(json.dumps(data).encode())
    elif variant in ("cross-origin", "top-origin"):
        # Client data as a browser reports it for a ceremony run in a frame.
        data = json.loads(b64decode(response["clientDataJSON"]))
        if variant == "cross-origin":
            data["crossOrigin"] = True
        else:
            data["topOrigin"] = "https://framing-site.invalid"
        response["clientDataJSON"] = b64encode(json.dumps(data).encode())
    elif variant in ("wrong-rp", "no-user-verification"):
        attestation = bytearray(b64decode(response["attestationObject"]))
        rp_hash = hashlib.sha256(options["rp"]["id"].encode()).digest()
        index = attestation.index(rp_hash)
        if variant == "wrong-rp":
            attestation[index] ^= 1
        else:
            attestation[index + len(rp_hash)] &= ~0x04
        response["attestationObject"] = b64encode(attestation)
    elif variant == "bad-signature":
        signature = bytearray(b64decode(response["signature"]))
        signature[-1] ^= 1
        response["signature"] = b64encode(signature)
    elif variant != "valid":
        raise ValueError(variant)
    return credential


BROWSER_REGISTRATION = """
const [url, done] = arguments;
(async () => {
    const ws = new WebSocket(url);
    await new Promise((resolve, reject) => {
        ws.addEventListener('open', resolve, {once: true});
        ws.addEventListener('error', reject, {once: true});
    });
    async function call(id, params) {
        const received = new Promise(resolve => {
            function reply(event) {
                const frame = JSON.parse(event.data);
                if (frame.id === id) {
                    ws.removeEventListener('message', reply);
                    resolve(frame);
                }
            }
            ws.addEventListener('message', reply);
        });
        ws.send(JSON.stringify({method: 'auth', id, params: {scheme: 'webauthn', action: 'register', ...params}}));
        const frame = await received;
        if (!frame.result) throw new Error(JSON.stringify(frame.error));
        return frame.result;
    }
    const options = await call('1', {step: 'begin'});
    const publicKey = PublicKeyCredential.parseCreationOptionsFromJSON(options.public_key);
    const credential = await navigator.credentials.create({publicKey});
    await call('2', {step: 'finish', challenge_id: options.challenge_id, credential: credential.toJSON()});
    ws.close();
    done(true);
})().catch(error => done(String(error)));
"""


def authorize(request):
    url = request["url"]
    fields = {key: request[key] for key in (
        "client_id", "redirect_uri", "code_challenge", "code_challenge_method", "state", "resource",
    )}
    query = urlencode(fields | {"response_type": "code"})
    with browser() as client:
        if request["variant"] == "passkey-login":
            client.navigate(request["resource"] + "/health")
            result = client.script(BROWSER_REGISTRATION, os.environ["APRON_TEST_PUBLIC_WS"], asynchronous=True)
            if result is not True:
                raise RuntimeError(f"passkey fixture registration failed: {result}")
        client.navigate(url + ("&" if "?" in url else "?") + query)
        if request["decision"] == "decline":
            client.script("document.querySelector('button[value=decline]').click()")
        else:
            account_token = request.get("account_token")
            if account_token is not None:
                client.script("document.querySelector('#token, input[name=token]').value = arguments[0]",
                              account_token)
            client.script("document.querySelector('#allow, button[value=allow]').click()")
            if account_token is not None and request["variant"].startswith("passkey"):
                wait_for(client, "document.getElementById('account').textContent.length > 0")
                client.script("document.getElementById('allow').click()")
        result = client.redirect()
    if request["variant"] == "expired":
        time.sleep(3.6)
    return result


def open_email_link(request):
    with browser() as client:
        client.navigate(request["link"])
        client.script("document.querySelector('#login button').click()")
        wait_for(client, "!document.getElementById('show-token').hidden")
        return client.script("""
            document.getElementById('show-token').click();
            return {token: document.getElementById('resume-token').value};
        """)


def wait_for(client, condition):
    deadline = time.monotonic() + 6
    while time.monotonic() < deadline:
        if client.script("return " + condition):
            return
        time.sleep(0.05)
    raise RuntimeError("expected browser state was absent: " +
                       str(client.script("return document.body.innerText") or "")[:400])
