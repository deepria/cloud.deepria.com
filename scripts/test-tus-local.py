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
        [*compose, *args], input=data, text=True, check=True, capture_output=True, timeout=60,
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


def start(path, cookie, size=6):
    headers, _ = expect(request("POST", path, cookie=cookie,
                                headers={**version, "Upload-Length": str(size)}), 201)
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


def verification(path):
    upload_id = path.rsplit("/", 1)[-1]
    result = sql(
        "SELECT status, encode(sha256, 'hex') FROM app.files "
        f"JOIN app.uploads ON file_id = files.id WHERE uploads.id = '{upload_id}';"
    )
    return tuple(result.strip().split("|"))


def wait_for_verification(path, status, payload=None):
    digest = hashlib.sha256(payload).hexdigest() if payload is not None else ""
    for _ in range(40):
        observed = verification(path)
        if observed == (status, digest):
            return
        time.sleep(0.25)
    raise AssertionError((observed, (status, digest)))


def internal_request(host, port, method, path, body, headers):
    raw_headers = {"Host": host, "Connection": "close", "Content-Length": str(len(body)), **headers}
    message = (
        f"{method} {path} HTTP/1.1\r\n"
        + "".join(f"{key}: {value}\r\n" for key, value in raw_headers.items())
        + "\r\n"
        + body
    )
    # BusyBox nc stops reading when stdin closes. Keep the pipe open for the reply.
    response = command("exec", "-T", "tusd", "sh", "-c",
                       '{ cat; sleep 1; } | nc -w 5 "$@"', "nc", host, str(port), data=message)
    status_line, _, _ = response.partition("\n")
    assert status_line.startswith("HTTP/1.1 "), (method, path, status_line)
    return int(status_line.split()[1])


def finish_on_tusd(path, token=None):
    headers = {**chunk_headers, "Upload-Offset": "3"}
    if token is not None:
        headers["X-Deepria-Tus-Token"] = token
    assert internal_request("127.0.0.1", 8080, "PATCH", path.replace("/uploads/", "/files/"),
                            "def", headers) == 204


def post_finish(path, token):
    payload = json.dumps({
        "Type": "post-finish",
        "Event": {"Upload": {
            "ID": path.rsplit("/", 1)[-1], "Offset": 6, "Size": 6, "SizeIsDeferred": False,
        }},
    })
    return internal_request("api", 3000, "POST", "/internal/tusd", payload,
                            {"Content-Type": "application/json", "X-Deepria-Tus-Token": token})


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
    wait_for_verification(path, "ready", b"abcdef")
    start(path, a)
    offset(path, a, 6)
    denied(path, b, 404)
    assert request("GET", path, cookie=a, headers=version)[0] == 405
    expect(request("DELETE", path, cookie=a, headers=version), 409)

    payload = b"abcdef" * 32768
    large = reserve(a, len(payload))
    start(large, a, len(payload))
    assert append(large, a, 0, payload)["Upload-Offset"] == str(len(payload))
    wait_for_verification(large, "ready", payload)

    empty = reserve(a, 0)
    start(empty, a, 0)
    wait_for_verification(empty, "ready", b"")

    token = command("exec", "-T", "api", "printenv", "TUS_INTERNAL_TOKEN").strip()
    command("stop", "verify-uploads")
    interrupted = reserve(a, 6)
    start(interrupted, a)
    interrupt_chunk(interrupted, a)
    offset(interrupted, a, 3)
    assert append(interrupted, a, 3, b"def")["Upload-Offset"] == "6"
    assert verification(interrupted) == ("verifying", "")
    expect(request("DELETE", interrupted, cookie=a, headers=version), 204)
    expect(request("DELETE", interrupted, cookie=a, headers=version), 204)
    interrupted_id = interrupted.rsplit("/", 1)[-1]
    for suffix in ("", ".info"):
        command("exec", "-T", "tusd", "test", "!", "-e", f"/srv/tusd-data/{interrupted_id}{suffix}")
    expect(request("HEAD", interrupted, cookie=a, headers=version), 404)
    expect(request("POST", interrupted, cookie=a, headers=start_headers), 404)
    append(interrupted, a, 6, b"xyz", status=404)
    assert post_finish(interrupted, token) == 200

    duplicate = reserve(a, 6)
    start(duplicate, a)
    append(duplicate, a, 0, b"abc")
    assert post_finish(duplicate, "invalid") == 403
    assert verification(duplicate) == ("uploading", "")
    # A real tusd hook is the only completion signal received by the API.
    finish_on_tusd(duplicate, token)
    wait_for_verification(duplicate, "verifying")
    assert post_finish(duplicate, token) == 200
    assert post_finish(duplicate, token) == 200
    assert verification(duplicate) == ("verifying", "")

    missed = reserve(a, 6)
    start(missed, a)
    append(missed, a, 0, b"abc")
    # No gateway PATCH response is observed, and tusd's hook has no token.
    finish_on_tusd(missed)
    assert verification(missed) == ("uploading", "")

    truncated = reserve(a, 6)
    start(truncated, a)
    append(truncated, a, 0, b"abcdef")
    assert verification(truncated) == ("verifying", "")
    truncated_id = truncated.rsplit("/", 1)[-1]
    command("exec", "-T", "tusd", "truncate", "-s", "5", f"/srv/tusd-data/{truncated_id}")

    incomplete = reserve(a, 6)
    start(incomplete, a)
    append(incomplete, a, 0, b"abc")

    unstarted = reserve(b, 6)
    expect(request("DELETE", unstarted, cookie=b, headers=version), 204)
    expect(request("POST", unstarted, cookie=b, headers=start_headers), 404)

    command("restart", "api", "tusd")
    command("start", "verify-uploads")
    # Recovery happens without a public HEAD or PATCH to either upload.
    wait_for_verification(duplicate, "ready", b"abcdef")
    wait_for_verification(missed, "ready", b"abcdef")
    wait_for_verification(truncated, "failed")
    assert verification(incomplete) == ("uploading", "")
    assert verification(interrupted) == ("cancelled", "")
    assert post_finish(duplicate, token) == 200
    assert post_finish(truncated, token) == 200
    assert verification(duplicate) == ("ready", hashlib.sha256(b"abcdef").hexdigest())
    assert verification(truncated) == ("failed", "")

    # Nginx can retain the old API address until its DNS cache expires.
    for _ in range(30):
        result = request("HEAD", missed, cookie=a, headers=version)
        if result[0] == 200:
            break
        time.sleep(0.5)
    headers, _ = expect(result, 200)
    assert headers["Upload-Offset"] == "6"

    print("tusd gateway OK: ownership, resume after restart/disconnect, and cancellation")
    print("Upload verification OK: SHA-256, empty files, truncated files, duplicate and missing "
          "completion signals after restart")
finally:
    ids = ", ".join(f"'{uid}'" for uid in users)
    sql(f"DELETE FROM app.users WHERE id IN ({ids});")
