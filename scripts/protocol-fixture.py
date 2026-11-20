#!/usr/bin/env python3
"""Run the loopback protocol tests against disposable Apron daemons."""

import hashlib
import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

import protocol_browser
import protocol_push


def local_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def launch(command, port, log):
    with log.open("ab") as output:
        process = subprocess.Popen(
            command, start_new_session=True, stdin=subprocess.DEVNULL,
            stdout=output, stderr=output,
            env=os.environ | {"APRON_FIXTURE_INBOX": str(log.parent / "mail")},
        )
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"aprond exited with status {process.returncode}: {log.read_text()}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return process
        except OSError:
            time.sleep(0.05)
    stop(process.pid)
    raise RuntimeError(f"aprond did not start on port {port}")


def stop(pid):
    try:
        os.kill(pid, signal.SIGINT)
    except ProcessLookupError:
        pass


def restart():
    fixture_path = Path(os.environ["APRON_FIXTURE_STATE"])
    fixture = json.loads(fixture_path.read_text())
    old_pid = fixture["pid"]
    stop(old_pid)
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", fixture["port"]), timeout=0.2):
                time.sleep(0.05)
        except ConnectionRefusedError:
            break
    else:
        raise RuntimeError("previous aprond did not close its listener")
    new_process = launch(fixture["command"], fixture["port"], fixture_path.with_name("server.log"))
    fixture["pid"] = new_process.pid
    fixture_path.write_text(json.dumps(fixture))
    print("{}")


def record_mail(address):
    message = sys.stdin.read()
    link = next(line.removeprefix("Open ").removesuffix(" to continue.")
                for line in message.splitlines() if line.startswith("Open "))
    code = next(line.split()[3].removesuffix(".")
                for line in message.splitlines() if line.startswith("Your code is "))
    fields = parse_qs(urlsplit(link).fragment, strict_parsing=True)
    inbox = Path(os.environ["APRON_FIXTURE_INBOX"])
    inbox.mkdir(exist_ok=True)
    key = mail_key(address, fields.get("state", [""])[0])
    result = {"link": link, "link_token": fields["token"][0], "code": code,
              "state": fields.get("state", [""])[0], "server": fields["server"][0],
              "text": message}
    temporary = inbox / f"{key}.tmp"
    temporary.write_text(json.dumps(result))
    temporary.replace(inbox / key)


def mail_key(address, state):
    return hashlib.sha256(json.dumps([address, state]).encode()).hexdigest()


def read_mail():
    request = json.load(sys.stdin)
    inbox = Path(os.environ["APRON_FIXTURE_STATE"]).parent / "mail"
    mail = inbox / mail_key(request["email"], request["state"])
    deadline = time.monotonic() + 6
    while time.monotonic() < deadline:
        if mail.is_file():
            print(mail.read_text())
            return
        time.sleep(0.05)
    raise RuntimeError("mail transport did not deliver the proposal")


def run(test_command):
    root = Path(__file__).resolve().parent.parent
    executable = root / "target" / "debug" / "aprond"
    with tempfile.TemporaryDirectory(prefix="apron-protocol-") as directory:
        fixture_path = Path(directory) / "fixture.json"
        port = local_port()
        push = protocol_push.Receiver(Path(directory))
        command = [
            str(executable), "--listen", f"127.0.0.1:{port}",
            "--db", str(Path(directory) / "apron.db"),
            "--welcome", "Welcome to Apron", "--ext-limit", "512", "--grant-ttl-ms", "3000",
            "--mail-command", str(Path(__file__).resolve()), "--email-ttl-ms", "8000",
            "--external-auth", "--passkey-auth",
            "--http-origin", f"http://localhost:{port}",
            "--code-ttl-ms", "3500", "--passkey-ttl-ms", "12000",
            "--push-relay", "--push-vapid-key", str(push.vapid),
            "--push-private-origin", push.origin, "--push-private-ca", str(push.ca),
        ]
        daemon = launch(command, port, fixture_path.with_name("server.log"))
        fixture_path.write_text(json.dumps({"pid": daemon.pid, "port": port, "command": command}))
        core = None
        try:
            core_port = local_port()
            core = launch(
                [str(executable), "--memory", "--core-only", "--listen", f"127.0.0.1:{core_port}"],
                core_port, fixture_path.with_name("core.log"),
            )
            env = os.environ | {
                "APRON_FIXTURE_STATE": str(fixture_path),
                "APRON_TEST_CONTROL": str(Path(__file__).resolve()),
                "APRON_TEST_WS": f"ws://127.0.0.1:{port}/ws",
                "APRON_TEST_CORE_WS": f"ws://127.0.0.1:{core_port}/ws",
                "APRON_TEST_EXT_LIMIT": "512",
                "APRON_TEST_MAIL_DOMAIN": "fixture.example",
                "APRON_TEST_ORIGIN": f"http://localhost:{port}",
                "APRON_TEST_PUBLIC_WS": f"ws://localhost:{port}/ws",
                "APRON_FIXTURE_PUSH_ORIGIN": push.origin,
            }
            return subprocess.call(test_command, cwd=root, env=env)
        finally:
            fixture = json.loads(fixture_path.read_text())
            stop(fixture["pid"])
            daemon.wait(timeout=10)
            if core is not None:
                stop(core.pid)
                core.wait(timeout=10)
            push.close()


if __name__ == "__main__":
    if sys.argv[1:2] == ["--"] and len(sys.argv) == 3:
        record_mail(sys.argv[2])
    elif sys.argv[1:] == ["read-mail"]:
        read_mail()
    elif sys.argv[1:] == ["open-email-link"]:
        print(json.dumps(protocol_browser.open_email_link(json.load(sys.stdin))))
    elif sys.argv[1:] == ["passkey-credential"]:
        directory = Path(os.environ["APRON_FIXTURE_STATE"]).parent
        print(json.dumps(protocol_browser.passkey_credential(json.load(sys.stdin), directory)))
    elif sys.argv[1:] in (["push-sink"], ["take-push"], ["push-gone"]):
        directory = Path(os.environ["APRON_FIXTURE_STATE"]).parent
        action = {
            "push-sink": protocol_push.provision,
            "take-push": protocol_push.take,
            "push-gone": protocol_push.gone,
        }[sys.argv[1]]
        print(json.dumps(action(json.load(sys.stdin), directory)))
    elif sys.argv[1:] == ["authorize"]:
        print(json.dumps(protocol_browser.authorize(json.load(sys.stdin))))
    elif sys.argv[1:] == ["expire-passkey"]:
        json.load(sys.stdin)
        time.sleep(12.1)
        print("{}")
    elif sys.argv[1:] == ["restart"]:
        restart()
    elif sys.argv[1:] == ["expire-email"]:
        json.load(sys.stdin)
        time.sleep(8.1)
        print("{}")
    elif sys.argv[1:] == ["expire-upload"]:
        json.load(sys.stdin)
        time.sleep(3.1)
        print("{}")
    elif sys.argv[1:2] == ["run"] and len(sys.argv) > 2:
        sys.exit(run(sys.argv[2:]))
    else:
        sys.exit("Usage: protocol-fixture.py run TEST_COMMAND [ARGS ...]")
