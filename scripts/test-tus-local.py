#!/usr/bin/env python3
"""Exercise the upload gateway against an isolated local Compose stack."""

from concurrent.futures import ThreadPoolExecutor
import hashlib
import http.client
import json
import os
import secrets
import socket
import subprocess
import time
from urllib.parse import urlsplit
import uuid

project = os.environ.get("COMPOSE_PROJECT_NAME", "")
if not project.startswith("cloud-tus-test-"):
    raise SystemExit("Run against a dedicated COMPOSE_PROJECT_NAME=cloud-tus-test-* stack.")

origin = urlsplit(f"http://127.0.0.1:{os.environ.get('WEB_PORT', '8080')}")
compose = ["docker", "compose"]
version = {"Tus-Resumable": "1.0.0"}
start_headers = {**version, "Upload-Length": "6"}
chunk_headers = {**version, "Content-Type": "application/offset+octet-stream"}


def command(*args, data=None):
    return subprocess.run(
        [*compose, *args], input=data, text=True, check=True, capture_output=True,
    ).stdout


def sql(statement):
    return command("exec", "-T", "db", "psql", "-XAt", "-v", "ON_ERROR_STOP=1",
                   "-U", "cloud_dev", "-d", "cloud_dev", data=statement)


def request(method, path, body=None, *, cookie="", headers=None, request_origin=None):
    request_headers = dict(headers or {})
    if method not in ("GET", "HEAD"):
        request_headers["Origin"] = request_origin or origin.geturl()
    if cookie:
        request_headers["Cookie"] = f"deepria-dev-session={cookie}"
    connection = http.client.HTTPConnection(origin.hostname, origin.port, timeout=15)
    try:
        connection.request(method, path, body=body, headers=request_headers)
        response = connection.getresponse()
        return response.status, response.headers, response.read()
    finally:
        connection.close()


def expect(actual, code):
    status, headers, body = actual
    assert status == code, (status, code, body[:512])
    return headers, body


def reserve(cookie, size):
    body = json.dumps({"request_key": str(uuid.uuid4()), "original_name": "fixture.bin", "size_bytes": size})
    _, result = expect(request("POST", "/api/files/uploads", body, cookie=cookie,
                               headers={"Content-Type": "application/json"}), 201)
    return "/uploads/" + json.loads(result)["upload_id"]


def start(path, cookie):
    headers, _ = expect(request("POST", path, cookie=cookie, headers=start_headers), 201)
    assert headers["Location"] == path, headers["Location"]


def offset(path, cookie, count):
    headers, _ = expect(request("HEAD", path, cookie=cookie, headers=version), 200)
    assert headers["Upload-Offset"] == str(count)
    assert headers["Upload-Length"] == "6"


def append(path, cookie, at, body, status=204):
    return expect(request("PATCH", path, body, cookie=cookie,
                          headers={**chunk_headers, "Upload-Offset": str(at)}), status)[0]


def denied(path, cookie, status):
    operations = [
        ("POST", None, start_headers),
        ("PATCH", b"xyz", {**chunk_headers, "Upload-Offset": "0"}),
        ("HEAD", None, version),
        ("DELETE", None, version),
    ]
    for method, body, headers in operations:
        result = request(method, path, body, cookie=cookie, headers=headers)
        assert result[0] == status, (method, result[0], status)


def interrupt_chunk(path, cookie):
    with socket.create_connection((origin.hostname, origin.port), timeout=10) as connection:
        connection.sendall((
            f"PATCH {path} HTTP/1.1\r\nHost: {origin.netloc}\r\n"
            f"Origin: {origin.geturl()}\r\nCookie: deepria-dev-session={cookie}\r\n"
            "Tus-Resumable: 1.0.0\r\nUpload-Offset: 0\r\n"
            "Content-Type: application/offset+octet-stream\r\n"
            "Content-Length: 6\r\nConnection: close\r\n\r\n"
        ).encode("ascii") + b"abc")
        upload_id = path.rsplit("/", 1)[-1]
        # Confirm the proxy is streaming before closing the incomplete body.
        for _ in range(50):
            if command("exec", "-T", "tusd", "cat", f"/srv/tusd-data/{upload_id}") == "abc":
                break
            time.sleep(0.1)
        else:
            raise AssertionError("The incomplete request was buffered")
        connection.shutdown(socket.SHUT_WR)
        try:
            connection.recv(4096)
        except (TimeoutError, ConnectionResetError):
            pass


users = [str(uuid.uuid4()) for _ in range(2)]
cookies = [secrets.token_urlsafe(32) for _ in users]
sql("BEGIN; " + " ".join(
    f"INSERT INTO app.users (id, display_name) VALUES ('{uid}', 'tus test'); "
    f"INSERT INTO app.sessions (token_hash, user_id, expires_at) "
    f"VALUES (decode('{hashlib.sha256(token.encode()).hexdigest()}', 'hex'), "
    f"'{uid}', now() + interval '1 hour');"
    for uid, token in zip(users, cookies)
) + " COMMIT;")

try:
    a, b = cookies
    path = reserve(a, 6)
    upload_id = path.rsplit("/", 1)[-1]
    denied(path, "", 401)
    denied(path, b, 404)
    expect(request("POST", path, cookie=a, headers=start_headers,
                   request_origin="https://unrelated.example.invalid"), 403)
    expect(request("POST", "/internal/tusd", b"{}", headers={"Content-Type": "application/json"}), 404)
    expect(request("POST", path, cookie=a, headers={**version, "Upload-Length": "7"}), 409)

    with ThreadPoolExecutor(max_workers=2) as parallel:
        results = [parallel.submit(start, path, a) for _ in range(2)]
        for result in results:
            result.result()
    offset(path, a, 0)
    assert append(path, a, 0, b"abc")["Upload-Offset"] == "3"
    start(path, a)
    offset(path, a, 3)
    denied(path, "", 401)
    denied(path, b, 404)
    append(path, a, 0, b"xyz", status=409)

    command("restart", "tusd")
    for attempt in range(10):
        result = request("HEAD", path, cookie=a, headers=version)
        if result[0] == 200:
            assert result[1]["Upload-Offset"] == "3"
            break
        if attempt == 9:
            expect(result, 200)
        time.sleep(0.5)

    assert append(path, a, 3, b"def")["Upload-Offset"] == "6"
    offset(path, a, 6)
    assert command("exec", "-T", "tusd", "cat", f"/srv/tusd-data/{upload_id}") == "abcdef"
    assert sql(f"SELECT status FROM app.files JOIN app.uploads ON file_id = files.id WHERE uploads.id = '{upload_id}';").strip() == "verifying"
    assert request("GET", path, cookie=a, headers=version)[0] == 405

    expect(request("DELETE", path, cookie=a, headers=version), 204)
    expect(request("DELETE", path, cookie=a, headers=version), 204)
    command("exec", "-T", "tusd", "test", "!", "-e", f"/srv/tusd-data/{upload_id}")
    command("exec", "-T", "tusd", "test", "!", "-e", f"/srv/tusd-data/{upload_id}.info")
    expect(request("HEAD", path, cookie=a, headers=version), 404)
    expect(request("POST", path, cookie=a, headers=start_headers), 404)
    append(path, a, 6, b"xyz", status=404)
    assert sql(f"SELECT status FROM app.files JOIN app.uploads ON file_id = files.id WHERE uploads.id = '{upload_id}';").strip() == "cancelled"

    interrupted = reserve(a, 6)
    start(interrupted, a)
    interrupt_chunk(interrupted, a)
    offset(interrupted, a, 3)
    assert append(interrupted, a, 3, b"def")["Upload-Offset"] == "6"
    expect(request("DELETE", interrupted, cookie=a, headers=version), 204)

    unstarted = reserve(b, 6)
    expect(request("DELETE", unstarted, cookie=b, headers=version), 204)
    expect(request("POST", unstarted, cookie=b, headers=start_headers), 404)

    print("tusd gateway OK: owner-only POST/PATCH/HEAD/DELETE, 401/404, resume after restart/disconnect, stored bytes and cancellation")
finally:
    ids = ", ".join(f"'{uid}'" for uid in users)
    sql(f"DELETE FROM app.users WHERE id IN ({ids});")
