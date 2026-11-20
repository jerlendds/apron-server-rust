"""Independent HTTPS receiver, encrypted record decoder, and VAPID verifier."""

from datetime import datetime, timedelta, timezone
import hashlib
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import secrets
import ssl
import threading
import time
from urllib.parse import urlsplit

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.asymmetric.utils import encode_dss_signature
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.x509.oid import NameOID

from protocol_browser import b64decode, b64encode


def public_bytes(key):
    return key.public_key().public_bytes(
        serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint,
    )


def certificate(directory):
    now = datetime.now(timezone.utc)
    ca_key = ec.generate_private_key(ec.SECP256R1())
    leaf_key = ec.generate_private_key(ec.SECP256R1())
    ca_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Apron test CA")])
    leaf_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "localhost")])

    def builder(subject, issuer, key):
        return x509.CertificateBuilder().subject_name(subject).issuer_name(issuer).public_key(
            key.public_key(),
        ).serial_number(x509.random_serial_number()).not_valid_before(
            now - timedelta(minutes=1),
        ).not_valid_after(now + timedelta(hours=1))

    ca = builder(ca_name, ca_name, ca_key).add_extension(
        x509.BasicConstraints(ca=True, path_length=0), critical=True,
    ).sign(ca_key, hashes.SHA256())
    leaf = builder(leaf_name, ca_name, leaf_key).add_extension(
        x509.SubjectAlternativeName([x509.DNSName("localhost")]), critical=False,
    ).sign(ca_key, hashes.SHA256())
    ca_path, cert_path, key_path = (directory / name for name in (
        "push-ca.pem", "push-tls.pem", "push-tls-key.pem",
    ))
    ca_path.write_bytes(ca.public_bytes(serialization.Encoding.PEM))
    cert_path.write_bytes(leaf.public_bytes(serialization.Encoding.PEM))
    key_path.write_bytes(private_bytes(leaf_key))
    return ca_path, cert_path, key_path


def private_bytes(key):
    return key.private_bytes(
        serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    )


def read_key(path):
    return serialization.load_pem_private_key(path.read_bytes(), password=None)


class Receiver(ThreadingHTTPServer):
    def __init__(self, directory):
        super().__init__(("127.0.0.1", 0), Capture)
        self.directory = directory
        self.origin = f"https://localhost:{self.server_address[1]}"
        ca, cert, key = certificate(directory)
        self.ca = ca
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(cert, key)
        self.socket = context.wrap_socket(self.socket, server_side=True)
        self.vapid = directory / "vapid.pem"
        self.vapid.write_bytes(private_bytes(ec.generate_private_key(ec.SECP256R1())))
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.shutdown()
        self.server_close()
        self.thread.join(timeout=5)


class Capture(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        identifier = urlsplit(self.path).path.removeprefix("/")
        if len(identifier) != 32 or any(char not in "0123456789abcdef" for char in identifier):
            self.send_error(404)
            return
        directory = self.server.directory / identifier
        if not directory.is_dir():
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length", "0"))
        if not 0 < length < 8192:
            self.send_error(413)
            return
        raw = self.rfile.read(length)
        headers = {key.lower(): value for key, value in self.headers.items()}
        result = {"method": "POST", "headers": headers}
        gone = directory / "gone"
        if gone.exists():
            # A push service that no longer accepts this subscription. Record
            # the attempt, so tests can tell that the server stopped sending.
            status = int(gone.read_text())
            result["answered"] = status
            name = str(time.monotonic_ns())
            (directory / f"{name}.tmp").write_text(json.dumps(result))
            (directory / f"{name}.tmp").replace(directory / f"{name}.json")
            self.send_error(status)
            return
        try:
            keyfile = directory / "key.pem"
            if keyfile.exists():
                plaintext = decrypt(raw, read_key(keyfile), (directory / "auth").read_bytes())
                result.update(plaintext=plaintext.decode(), decrypted=True)
            else:
                result["plaintext"] = raw.decode()
            if (directory / "webpush").exists():
                verify_vapid(headers["authorization"], self.server.origin, self.server.vapid)
                result["vapid_verified"] = True
        except Exception as error:
            result["fixture_error"] = str(error) or type(error).__name__
        name = str(time.monotonic_ns())
        temporary = directory / f"{name}.tmp"
        temporary.write_text(json.dumps(result))
        temporary.replace(directory / f"{name}.json")
        self.send_response(201)
        self.end_headers()


def extract(salt, material):
    return hmac.new(salt, material, hashlib.sha256).digest()


def expand(key, info, length):
    return hmac.new(key, info + b"\x01", hashlib.sha256).digest()[:length]


def decrypt(body, receiver, auth):
    salt, record_size, key_len = body[:16], int.from_bytes(body[16:20], "big"), body[20]
    if key_len != 65 or record_size < len(body) - 21 - key_len:
        raise ValueError("expected one aes128gcm record with an uncompressed P-256 key")
    sender = body[21:21 + key_len]
    encrypted = body[21 + key_len:]
    peer = ec.EllipticCurvePublicKey.from_encoded_point(ec.SECP256R1(), sender)
    shared = receiver.exchange(ec.ECDH(), peer)
    input_key = expand(extract(auth, shared), b"WebPush: info\0" + public_bytes(receiver) + sender, 32)
    key = extract(salt, input_key)
    content_key = expand(key, b"Content-Encoding: aes128gcm\0", 16)
    nonce = expand(key, b"Content-Encoding: nonce\0", 12)
    plaintext = AESGCM(content_key).decrypt(nonce, encrypted, None).rstrip(b"\0")
    if not plaintext.endswith(b"\x02"):
        raise ValueError("invalid final-record delimiter")
    return plaintext[:-1]


def verify_vapid(header, origin, vapid):
    scheme, _, parameters = header.partition(" ")
    if scheme.lower() != "vapid":
        raise ValueError("invalid VAPID authorization type")
    fields = dict(parameter.strip().split("=", 1) for parameter in parameters.split(","))
    private = read_key(vapid)
    if b64decode(fields["k"]) != public_bytes(private):
        raise ValueError("wrong VAPID public key")
    metadata, claims, signature = fields["t"].split(".")
    now = int(time.time())
    values = json.loads(b64decode(claims))
    if json.loads(b64decode(metadata))["alg"] != "ES256" or values["aud"] != origin:
        raise ValueError("invalid VAPID algorithm or audience")
    if not now < values["exp"] <= now + 86_400:
        raise ValueError("invalid VAPID expiry")
    raw_signature = b64decode(signature)
    if len(raw_signature) != 64:
        raise ValueError("invalid ES256 signature length")
    r = int.from_bytes(raw_signature[:32], "big")
    s = int.from_bytes(raw_signature[32:], "big")
    private.public_key().verify(encode_dss_signature(r, s), f"{metadata}.{claims}".encode(),
                                ec.ECDSA(hashes.SHA256()))


def provision(request, directory):
    kind = request["kind"]
    identifier = secrets.token_hex(16)
    sink = directory / identifier
    sink.mkdir()
    result = {"url": f'{os.environ["APRON_FIXTURE_PUSH_ORIGIN"]}/{identifier}'}
    if kind != "webpush":
        result["token"] = secrets.token_urlsafe(20)
    if kind != "relay":
        key = ec.generate_private_key(ec.SECP256R1())
        secret = secrets.token_bytes(16)
        (sink / "key.pem").write_bytes(private_bytes(key))
        (sink / "auth").write_bytes(secret)
        result["keys"] = {"p256dh": b64encode(public_bytes(key)), "auth": b64encode(secret)}
    if kind == "webpush":
        (sink / "webpush").touch()
    return result


def gone(request, directory):
    """Makes a sink answer every later push with `status` (404 or 410)."""
    identifier = urlsplit(request["url"]).path.removeprefix("/")
    status = int(request["status"])
    if status not in (404, 410):
        raise ValueError("push-gone status must be 404 or 410")
    (directory / identifier / "gone").write_text(str(status))
    return {}


def take(request, directory):
    identifier = urlsplit(request["url"]).path.removeprefix("/")
    sink = directory / identifier
    deadline = time.monotonic() + request["wait_ms"] / 1000
    while time.monotonic() < deadline:
        for entry in sorted(sink.glob("*.json")):
            result = json.loads(entry.read_text())
            entry.unlink()
            if "fixture_error" in result:
                raise RuntimeError(result["fixture_error"])
            return result
        time.sleep(0.025)
    return None
