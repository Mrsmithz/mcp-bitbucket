#!/usr/bin/env python3
"""Exercise the built stdio server against local HTTP only; no account needed."""

import base64
import json
import os
from pathlib import Path
import queue
import re
import signal
import socket
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = "/company-proxy/2.0"
EMAIL = "smoke@example.invalid"
TOKEN = "smoke-only-not-a-real-atlassian-token"
ENCODED_AUTH = base64.b64encode(f"{EMAIL}:{TOKEN}".encode()).decode()
AUTH = "Basic " + ENCODED_AUTH
TOOLS = {
    "list_pull_requests", "get_pull_request", "get_pull_request_diff",
    "list_pull_request_comments", "create_pull_request",
    "create_pull_request_comment", "approve_pull_request",
    "withdraw_pull_request_approval", "get_pull_request_approvals",
}
TARGET = {"repo_slug": "demo", "pull_request_id": 42}
PR_PATH = ROOT + "/repositories/acme/demo/pullrequests"
COMMENTS_PATH = PR_PATH + "/42/comments"
APPROVE_42_PATH = PR_PATH + "/42/approve"
APPROVE_PATH = PR_PATH + "/45/approve"
DIFF = """diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 context
-old_line
+new_line
 tail
"""


def user(name, identifier):
    return {"type": "user", "display_name": name, "nickname": identifier,
            "uuid": "{" + identifier + "}", "account_id": identifier}


BOB = user("Bob Approved", "11111111-1111-4111-8111-111111111111")
ALICE = user("Alice Reviewer", "22222222-2222-4222-8222-222222222222")
CHARLIE = user("Charlie Changes", "33333333-3333-4333-8333-333333333333")
PR = {
    "type": "pullrequest", "id": 42, "title": "Smoke PR", "state": "OPEN",
    "description": "Local fixture", "author": ALICE, "draft": False,
    "source": {"branch": {"name": "feature"}, "repository": {"full_name": "acme/demo"}},
    "destination": {"branch": {"name": "main"}, "repository": {"full_name": "acme/demo"}},
    "reviewers": [ALICE],
    "participants": [
        {"type": "participant", "role": "REVIEWER", "user": BOB,
         "approved": True, "state": "approved"},
        {"type": "participant", "role": "REVIEWER", "user": ALICE,
         "approved": False, "state": None},
        {"type": "participant", "role": "PARTICIPANT", "user": CHARLIE,
         "approved": False, "state": "changes_requested"},
    ],
    "links": {"html": {"href": "https://bitbucket.org/acme/demo/pull-requests/42"}},
}


class Fixture(BaseHTTPRequestHandler):
    requests = []
    attacker_url = ""

    def log_message(self, *_args):
        pass

    def _record(self, method):
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length)) if length else None
        type(self).requests.append((method, self.path, self.headers.get("Authorization") == AUTH, body))
        return body

    def _send_json(self, status, value):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self._record("GET")
        path = self.path.split("?", 1)[0]
        pr_root = PR_PATH + "/"
        if path == PR_PATH:
            self._send_json(200, {"values": [PR], "pagelen": 1})
        elif path == pr_root + "42":
            self._send_json(200, PR)
        elif path == COMMENTS_PATH:
            self._send_json(200, {"values": [{"id": 7, "content": {"raw": "Existing comment"}}]})
        elif path == pr_root + "42/diff":
            self.send_response(302)
            self.send_header("Location", ROOT + "/repositories/acme/demo/diff/abc..def")
            self.send_header("Content-Length", "0")
            self.end_headers()
        elif path == ROOT + "/repositories/acme/demo/diff/abc..def":
            body = DIFF.encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        elif path == pr_root + "99/diff":
            self.send_response(302)
            self.send_header("Location", self.attacker_url + "/steal")
            self.send_header("Content-Length", "0")
            self.end_headers()
        elif path == pr_root + "98/diff":
            self.send_response(302)
            self.send_header("Location", "/outside-configured-prefix")
            self.send_header("Content-Length", "0")
            self.end_headers()
        else:
            self._send_json(404, {"error": {"message": "unexpected local fixture request"}})

    def do_POST(self):
        body = self._record("POST")
        if self.path == APPROVE_PATH:
            self.connection.shutdown(socket.SHUT_RDWR)
            self.connection.close()
        elif self.path == PR_PATH:
            self._send_json(201, {**PR, "id": 43, "title": body["title"],
                                  "description": body.get("description"),
                                  "source": body["source"], "destination": body["destination"]})
        elif self.path == COMMENTS_PATH:
            self._send_json(201, {"id": 8, "content": body.get("content"),
                                  "parent": body.get("parent"), "inline": body.get("inline")})
        elif self.path == APPROVE_42_PATH:
            self._send_json(200, {"type": "participant", "user": BOB, "role": "REVIEWER",
                                  "approved": True, "state": "approved",
                                  TOKEN: "fixture-only-untrusted-value", "future_field": "ignored"})
        else:
            self._send_json(404, {"error": {"message": "unexpected local fixture request"}})

    def do_DELETE(self):
        self._record("DELETE")
        if self.path == APPROVE_42_PATH:
            self.send_response(204)
            self.send_header("Content-Length", "0")
            self.end_headers()
        else:
            self._send_json(404, {"error": {"message": "unexpected local fixture request"}})


class Attacker(BaseHTTPRequestHandler):
    requests = 0

    def log_message(self, *_args):
        pass

    def do_GET(self):
        type(self).requests += 1
        self.send_response(200)
        self.end_headers()


class StdioClient:
    CAPTURE_LIMIT = 1_000_000
    LINE_LIMIT = 64_000
    RESPONSE_QUEUE_LIMIT = 32

    def __init__(self, executable, base_url):
        env = {key: os.environ[key] for key in ("PATH", "LANG", "LC_ALL", "SHELL")
               if key in os.environ}
        env.update(BITBUCKET_EMAIL=EMAIL, BITBUCKET_API_TOKEN=TOKEN,
                   BITBUCKET_WORKSPACE="acme", BITBUCKET_BASE_URL=base_url,
                   NO_PROXY="127.0.0.1,localhost",
                   no_proxy="127.0.0.1,localhost")
        self.process = subprocess.Popen([str(executable)], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        env=env, start_new_session=True)
        self.responses = queue.Queue(maxsize=self.RESPONSE_QUEUE_LIMIT)
        self.sequence = 0
        self._lock = threading.Lock()
        self._captures = {"stdout": [], "stderr": []}
        self._sizes = {"stdout": 0, "stderr": 0}
        self._overflow = threading.Event()
        self._readers = [
            threading.Thread(target=self._drain, args=("stdout", self.process.stdout), daemon=True),
            threading.Thread(target=self._drain, args=("stderr", self.process.stderr), daemon=True),
        ]
        for reader in self._readers:
            reader.start()

    def _drain(self, name, stream):
        while True:
            line = stream.readline(self.LINE_LIMIT + 1)
            if not line:
                return
            if self._overflow.is_set():
                continue
            with self._lock:
                remaining = self.CAPTURE_LIMIT - self._sizes[name]
                captured = line[:max(remaining, 0)]
                if captured:
                    self._captures[name].append(captured)
                    self._sizes[name] += len(captured)
                if len(line) > max(remaining, 0) or len(line) > self.LINE_LIMIT:
                    self._overflow.set()
            if name == "stdout" and not self._overflow.is_set():
                try:
                    self.responses.put_nowait(line.decode(errors="replace"))
                except queue.Full:
                    self._overflow.set()

    def send(self, message):
        self.process.stdin.write((json.dumps(message) + "\n").encode())
        self.process.stdin.flush()

    def request(self, method, params):
        self.sequence += 1
        request_id = self.sequence
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        deadline = time.monotonic() + 15
        while True:
            assert not self._overflow.is_set(), "MCP output exceeded bounded capture"
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise queue.Empty
            try:
                line = self.responses.get(timeout=min(remaining, 0.1))
            except queue.Empty:
                if self.process.poll() is not None:
                    for reader in self._readers:
                        reader.join(timeout=0.1)
                    assert not self._overflow.is_set(), "MCP output exceeded bounded capture"
                    raise AssertionError("MCP process exited before replying")
                continue
            assert not self._overflow.is_set(), "MCP output exceeded bounded capture"
            message = json.loads(line)
            if message.get("id") == request_id:
                assert "error" not in message, "MCP protocol error"
                return message["result"]

    def call(self, name, target):
        return self.request("tools/call", {"name": name, "arguments": target})

    def _captured(self):
        with self._lock:
            return {name: b"".join(parts).decode(errors="replace")
                    for name, parts in self._captures.items()}

    def _check_secrets(self):
        assert not self._overflow.is_set(), "MCP output exceeded bounded capture"
        assert_no_credentials(self._captured())

    def close(self):
        try:
            if self.process.stdin and not self.process.stdin.closed:
                self.process.stdin.close()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass
            try:
                os.killpg(self.process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            self.process.wait(timeout=2)
            for reader in self._readers:
                reader.join(timeout=2)
            self._check_secrets()
        finally:
            for stream in (self.process.stdout, self.process.stderr):
                if stream and not stream.closed:
                    stream.close()


def assert_no_credentials(outputs):
    for channel, output in outputs.items():
        if any(secret in output for secret in (TOKEN, AUTH, ENCODED_AUTH)):
            raise AssertionError(f"credential leaked to {channel}")


def data(result):
    assert not result.get("isError", False), "tool returned an error"
    if "structuredContent" in result:
        return result["structuredContent"]
    texts = [item["text"] for item in result.get("content", []) if item.get("type") == "text"]
    assert len(texts) == 1, "expected one structured result"
    return json.loads(texts[0])


def structured_data(result):
    assert not result.get("isError", False), "tool returned an error"
    value = result.get("structuredContent")
    assert isinstance(value, dict), "expected canonical structured content"
    return value


def approver_collection(approvals):
    return approvals.get("approvers")


def normalized_id(value):
    return value.strip("{}") if isinstance(value, str) else value


def is_outcome_unknown(result):
    structured = result.get("structuredContent")
    error = structured.get("error") if isinstance(structured, dict) else None
    return (result.get("isError") is True and isinstance(error, dict)
            and error.get("code") == "outcome_unknown")


def verify_approval_report(approvals):
    people = approver_collection(approvals)
    assert isinstance(people, list), "approvers must be a list"
    assert len(people) == 1, "expected exactly one approver"
    person = people[0]
    assert isinstance(person, dict), "approver must be an object"
    assert person.get("display_name") == "Bob Approved", "wrong approver identity"
    assert normalized_id(person.get("uuid")) == normalized_id(BOB["uuid"]), "wrong approver UUID"
    assert person.get("account_id") == BOB["account_id"], "wrong approver account ID"
    assert approvals.get("count") == 1, "approval report count must equal approvers"


def verify_mutation_requests(requests):
    posts = [entry for entry in requests if entry[0] == "POST" and entry[1] == APPROVE_PATH]
    assert len(posts) == 1, "uncertain mutation must be sent exactly once"
    assert posts[0][2], "mutation request omitted correct Basic authentication"


def verify_workflow_requests(requests):
    writes = [entry for entry in requests if entry[0] in {"POST", "DELETE"}]
    expected = [
        ("POST", PR_PATH, {
            "title": "Smoke-created PR", "source": {"branch": {"name": "feature/smoke"}},
            "destination": {"branch": {"name": "main"}}, "description": "Created locally",
            "reviewers": [{"uuid": BOB["uuid"]}],
        }),
        ("POST", COMMENTS_PATH, {"content": {"raw": "General smoke comment"}}),
        ("POST", COMMENTS_PATH, {"content": {"raw": "Reply smoke comment"}, "parent": {"id": 7}}),
        ("POST", COMMENTS_PATH, {"content": {"raw": "Old-line smoke comment"},
                                  "inline": {"path": "src/lib.rs", "from": 2}}),
        ("POST", COMMENTS_PATH, {"content": {"raw": "New-line smoke comment"},
                                  "inline": {"path": "src/lib.rs", "to": 3}}),
        ("POST", APPROVE_42_PATH, None),
        ("DELETE", APPROVE_42_PATH, None),
        ("POST", APPROVE_PATH, None),
    ]
    assert [(method, path, body) for method, path, _, body in writes] == expected, (
        "workflow used an unexpected write path, payload, or retry")
    assert all(valid_auth for _, _, valid_auth, _ in writes), "workflow write omitted Basic authentication"
    reads = [(method, path.split("?", 1)[0]) for method, path, _, _ in requests if method == "GET"]
    expected_reads = {("GET", PR_PATH), ("GET", PR_PATH + "/42"),
                      ("GET", COMMENTS_PATH), ("GET", PR_PATH + "/42/diff")}
    assert expected_reads.issubset(set(reads)), "one or more read tools did not reach the local fixture"


def main():
    executable = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/mcp-bitbucket").resolve()
    assert executable.is_file(), "build first with cargo build"
    fixture = ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    attacker = None
    servers = [fixture]
    started_servers = []
    client = None
    errors = []
    try:
        threading.Thread(target=fixture.serve_forever, daemon=True).start()
        started_servers.append(fixture)
        attacker = ThreadingHTTPServer(("127.0.0.1", 0), Attacker)
        servers.append(attacker)
        Fixture.attacker_url = f"http://127.0.0.1:{attacker.server_port}"
        threading.Thread(target=attacker.serve_forever, daemon=True).start()
        started_servers.append(attacker)
        client = StdioClient(executable, f"http://127.0.0.1:{fixture.server_port}" + ROOT)
        initialized = client.request("initialize", {
            "protocolVersion": "2025-03-26", "capabilities": {},
            "clientInfo": {"name": "independent-smoke", "version": "1.0"}})
        assert "tools" in initialized["capabilities"]
        client.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        tools = client.request("tools/list", {})["tools"]
        assert {tool["name"] for tool in tools} == TOOLS, "unexpected public tool surface"
        for tool in tools:
            if tool["name"] in {"approve_pull_request", "withdraw_pull_request_approval"}:
                assert "confirm" not in tool["inputSchema"].get("properties", {})

        completed_tools = set()

        def call_tool(name, arguments):
            result = client.call(name, arguments)
            assert not result.get("isError", False), f"{name} returned an error"
            completed_tools.add(name)
            return result

        approvals = structured_data(call_tool("get_pull_request_approvals", TARGET))
        verify_approval_report(approvals)
        call_tool("list_pull_requests", {"repo_slug": "demo"})
        call_tool("get_pull_request", TARGET)
        call_tool("list_pull_request_comments", {"repo_slug": "demo",
                                                   "pull_request_id": 42})
        call_tool("create_pull_request", {
            "repo_slug": "demo", "title": "Smoke-created PR",
            "source_branch": "feature/smoke", "destination_branch": "main",
            "description": "Created locally", "reviewers": [BOB["uuid"]],
        })
        comments = (
            {"kind": "general", "body": "General smoke comment"},
            {"kind": "reply", "body": "Reply smoke comment", "parent_id": 7},
            {"kind": "inline", "body": "Old-line smoke comment", "path": "src/lib.rs",
             "side": "old", "line": 2},
            {"kind": "inline", "body": "New-line smoke comment", "path": "src/lib.rs",
             "side": "new", "line": 3},
        )
        for comment in comments:
            call_tool("create_pull_request_comment", {**TARGET, "comment": comment})
        call_tool("approve_pull_request", TARGET)
        call_tool("withdraw_pull_request_approval", TARGET)

        diff = data(call_tool("get_pull_request_diff", TARGET))
        assert completed_tools == TOOLS, "smoke did not call every public tool"
        assert "new_line" in json.dumps(diff), "documented diff redirect did not resolve"
        for pr_id in (98, 99):
            denied = client.call("get_pull_request_diff", {**TARGET, "pull_request_id": pr_id})
            assert denied.get("isError"), "unsafe redirect was accepted"
        assert Attacker.requests == 0, "cross-origin redirect received a credentialed request"

        uncertain = client.call("approve_pull_request", {**TARGET, "pull_request_id": 45})
        assert uncertain.get("isError"), "dropped mutation response must be an error"
        assert is_outcome_unknown(uncertain), "uncertain write outcome was not reported semantically"
        verify_mutation_requests(Fixture.requests)
        verify_workflow_requests(Fixture.requests)
        assert all(path.startswith(ROOT + "/") for _, path, _, _ in Fixture.requests), "request escaped API prefix"
        assert all(valid_auth for _, _, valid_auth, _ in Fixture.requests), "API authentication failed"
        client._check_secrets()
    except BaseException as error:
        errors.append(error)
    finally:
        for resource in (client,):
            if resource:
                try:
                    resource.close()
                except BaseException as error:
                    errors.append(error)
        for server in reversed(servers):
            if server in started_servers:
                try:
                    server.shutdown()
                except BaseException as error:
                    errors.append(error)
            try:
                server.server_close()
            except BaseException as error:
                errors.append(error)
    if errors:
        raise errors[0]
    print("PASS: all nine tools, PR/comment writes, approval identities, API auth/prefix, safe redirects, no retry")


if __name__ == "__main__":
    main()
