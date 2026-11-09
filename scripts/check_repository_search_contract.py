#!/usr/bin/env python3
"""Verify repository-search schemas and TLS policy against a local self-signed HTTPS API."""

import base64
import json
import os
from pathlib import Path
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import smoke_stdio
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

EMAIL = "search-smoke@example.invalid"
TOKEN = "search-smoke-only-not-a-real-token"
AUTH = "Basic " + base64.b64encode(f"{EMAIL}:{TOKEN}".encode()).decode()
PREFIX = "/proxy/2.0"
WORKSPACE = PREFIX + "/repositories/acme"
TOOLS = {
    "search_repositories", "list_pull_requests", "get_pull_request",
    "get_pull_request_diff", "list_pull_request_files", "get_pull_request_file_diff",
    "list_pull_request_comments", "create_pull_request", "create_pull_request_comment",
    "approve_pull_request", "withdraw_pull_request_approval", "get_pull_request_approvals",
}
LITERAL_NAME = r'Release "A\B" [tool]+'
LITERAL_QUERY = r'"a\b" [TOOL]+'
LATER = {
    "uuid": "{later-id}", "name": "Later Match", "full_name": "acme/later-repo",
    "workspace": {"slug": "acme"}, "is_private": True,
    "links": {"html": {"href": "https://bitbucket.org/acme/later-repo"}},
}


class Fixture(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, address):
        super().__init__(address, Handler)
        self.requests = []
        self.accepted = 0
        self.raw_accepts = 0
        self.tls_context = None
        self.lock = threading.Lock()

    def get_request(self):
        request, address = self.socket.accept()
        with self.lock:
            self.raw_accepts += 1
        request.settimeout(3)
        if self.tls_context is not None:
            request = self.tls_context.wrap_socket(request, server_side=True)
        with self.lock:
            self.accepted += 1
        return request, address

    def handle_error(self, _request, _client_address):
        pass


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, *_args):
        pass

    def do_GET(self):
        with self.server.lock:
            self.server.requests.append((self.path, self.headers.get("Authorization")))
        path, _, raw_query = self.path.partition("?")
        params = {}
        for part in raw_query.split("&"):
            if "=" in part:
                key, value = part.split("=", 1)
                params[key] = value
        if path == WORKSPACE and params.get("pagelen") == "2" and params.get("page") == "opaque":
            value = {"values": [LATER]}
        elif path == WORKSPACE and "page" not in params:
            value = {"values": [
                {"uuid": "{public-id}", "name": LITERAL_NAME, "full_name": "acme/public-tool",
                 "workspace": {"slug": "acme"}, "is_private": False,
                 "links": {"html": {"href": "https://bitbucket.org/acme/public-tool"}}},
                {"uuid": "{private-id}", "name": "Private Project", "full_name": "acme/private-project",
                 "workspace": {"slug": "acme"}, "is_private": True,
                 "links": {"html": {"href": "https://bitbucket.org/acme/private-project"}}},
            ]}
            if params.get("pagelen") == "2":
                value = {"values": [], "next": f"https://127.0.0.1:{self.server.server_port}{WORKSPACE}?pagelen=2&page=opaque"}
        elif path == PREFIX + "/repositories/acme/demo/pullrequests":
            value = {"values": []}
        else:
            self.send_error(404)
            return
        body = json.dumps(value, separators=(",", ":")).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


Stdio = smoke_stdio.StdioClient

def structured(result):
    value = result.get("structuredContent")
    assert isinstance(value, dict), result
    return value


def tls_server(directory):
    key = Path(directory) / "fixture.key"
    cert = Path(directory) / "fixture.crt"
    subprocess.run([
        "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
        "-keyout", str(key), "-out", str(cert), "-subj", "/CN=localhost",
        "-addext", "subjectAltName=IP:127.0.0.1,DNS:localhost",
    ], check=True, capture_output=True, timeout=15)
    server = Fixture(("127.0.0.1", 0))
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(certfile=cert, keyfile=key)
    server.tls_context = context
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, thread


def init(client):
    result = client.request("initialize", {
        "protocolVersion": "2025-03-26", "capabilities": {},
        "clientInfo": {"name": "search-contract", "version": "1"},
    }, request_id=1)
    assert "tools" in result["capabilities"]
    client.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    tools = client.request("tools/list", {}, request_id=2)["tools"]
    assert {tool["name"] for tool in tools} == TOOLS
    search = next(tool for tool in tools if tool["name"] == "search_repositories")
    assert set(search["inputSchema"]["properties"]) == {"query", "page", "pagelen"}
    assert not (set(search["inputSchema"]["properties"]) & {"workspace", "q", "role", "fields", "sort", "url", "tls"})
    return tools


def fixture_env(tls_flag):
    return {"BITBUCKET_EMAIL": EMAIL, "BITBUCKET_API_TOKEN": TOKEN,
            "BITBUCKET_TLS_VERIFY": tls_flag}


def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/mcp-bitbucket").resolve()
    assert binary.is_file(), "build the Rust binary first"
    with tempfile.TemporaryDirectory(prefix="mcp-bitbucket-search-tls-") as directory:
        server, server_thread = tls_server(directory)
        base = f"https://127.0.0.1:{server.server_port}{PREFIX}"
        try:
            verified_attempts = []
            for tls_flag in (None, "true"):
                before_verified = server.raw_accepts
                client = Stdio(binary, base, env_overrides=fixture_env(tls_flag))
                try:
                    init(client)
                    denied = client.call("search_repositories", {"query": "anything"}, request_id=3)
                    assert denied.get("isError") is True, denied
                    with server.lock:
                        assert server.raw_accepts > before_verified, "verified TLS did not attempt a TCP connection"
                        verified_attempts.append(server.raw_accepts - before_verified)
                        assert not server.requests, "verified TLS unexpectedly accepted the self-signed fixture"
                finally:
                    client.close()
                client._check_secrets()

            for invalid_flag in ("", "FALSE", "TRUE", " false", "false ", "yes", "ＦＡＬＳＥ"):
                before_invalid = server.raw_accepts
                invalid = Stdio(binary, base, env_overrides=fixture_env(invalid_flag))
                try:
                    invalid.process.stdin.close()
                    status = invalid.process.wait(timeout=5)
                    for reader in invalid._readers:
                        reader.join(timeout=2)
                    assert status != 0
                    assert "BITBUCKET_TLS_VERIFY" in invalid._captured()["stderr"]
                    assert invalid._captured()["stdout"] == "", invalid._captured()["stdout"]
                    assert server.raw_accepts == before_invalid, "invalid TLS flag reached the fixture"
                finally:
                    invalid.close()
                invalid._check_secrets()

            before_insecure = server.raw_accepts
            client = Stdio(binary, base, env_overrides=fixture_env("false"))
            try:
                init(client)
                matched = structured(client.call("search_repositories", {
                    "query": LITERAL_QUERY,
                }, request_id=3))
                assert [item["name"] for item in matched["values"]] == [LITERAL_NAME]
                assert matched["values"][0] == {
                    "uuid": "{public-id}", "name": LITERAL_NAME, "slug": "public-tool",
                    "workspace": "acme", "url": "https://bitbucket.org/acme/public-tool",
                    "is_private": False,
                }
                empty = structured(client.call("search_repositories", {
                    "query": "Later", "pagelen": 2,
                }, request_id=4))
                assert empty["values"] == [] and isinstance(empty["next_page"], str)
                token = empty["next_page"]
                before = len(server.requests)
                mismatch = client.call("search_repositories", {
                    "query": "not Later", "pagelen": 2, "page": token,
                }, request_id=5)
                assert mismatch.get("isError") is True
                assert len(server.requests) == before, "cursor mismatch reached HTTPS fixture"
                for identifier in (6, 7):
                    later = structured(client.call("search_repositories", {
                        "query": "Later", "pagelen": 2, "page": token,
                    }, request_id=identifier))
                    assert later["values"] == [{
                        "uuid": "{later-id}", "name": "Later Match", "slug": "later-repo",
                        "workspace": "acme", "url": "https://bitbucket.org/acme/later-repo",
                        "is_private": True,
                    }]
                    assert later["next_page"] is None
                old_tool = structured(client.call("list_pull_requests", {"repo_slug": "demo"}, request_id=8))
                assert old_tool["values"] == []
                with server.lock:
                    requests = list(server.requests)
                assert requests == [
                    (WORKSPACE + "?pagelen=50", AUTH),
                    (WORKSPACE + "?pagelen=2", AUTH),
                    (WORKSPACE + "?pagelen=2&page=opaque", AUTH),
                    (WORKSPACE + "?pagelen=2&page=opaque", AUTH),
                    (PREFIX + "/repositories/acme/demo/pullrequests?pagelen=50", AUTH),
                ], requests
                assert "WARNING: BITBUCKET_TLS_VERIFY=false" in client._captured()["stderr"]
                assert "WARNING" not in client._captured()["stdout"]
            finally:
                client.close()
            client._check_secrets()
            assert server.raw_accepts - before_insecure == len(requests), "HTTPS connection count differs from authenticated requests"
        finally:
            server.shutdown()
            server.server_close()
            server_thread.join(timeout=3)
            assert not server_thread.is_alive(), "HTTPS fixture thread did not stop"
    print(
        "PASS: 12-tool stdio schema, literal search/cursors, TLS verify/opt-out, shared-client old tool; "
        f"self-signed TLS raw TCP attempts: default={verified_attempts[0]}, explicit_true={verified_attempts[1]}, "
        f"explicit_false={server.raw_accepts - before_insecure}; verified HTTP requests=0, "
        f"insecure authenticated HTTP requests={len(requests)}"
    )


if __name__ == "__main__":
    main()
