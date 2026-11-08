#!/usr/bin/env python3
"""Independent actual-MCP file-filter contract, loopback fixtures only."""
import importlib.util
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import signal
import subprocess
import sys
import threading
from urllib.parse import parse_qs, urlsplit

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('original_smoke', ROOT / 'scripts/smoke_stdio.py')
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)
PREFIX = '/company-proxy/2.0'
PR = PREFIX + '/repositories/acme/demo/pullrequests/'
REPO = PREFIX + '/repositories/forkspace/forkrepo/'
FILE = 'src/a + b[1]#&ü".rs'
OLD_PATH = json.dumps('a/' + FILE, ensure_ascii=False)
NEW_PATH = json.dumps('b/' + FILE, ensure_ascii=False)
PATCH = f'diff --git {OLD_PATH} {NEW_PATH}\n--- {OLD_PATH}\n+++ {NEW_PATH}\n@@ -1 +1 @@\n-old\n+SELECTED_FILE_PATCH\n'
MAX_DIFF_BYTES = 500_000
JSON_REPLY_EXPANSION = 13
PROTOCOL_MARGIN = 500_000
LINE = b'\x00' * 20 + b'\n'
TOOLS = {
    'list_pull_requests', 'get_pull_request', 'get_pull_request_diff',
    'list_pull_request_comments', 'create_pull_request', 'create_pull_request_comment',
    'approve_pull_request', 'withdraw_pull_request_approval', 'get_pull_request_approvals',
    'list_pull_request_files', 'get_pull_request_file_diff',
}
FIELDS = {'old_path', 'new_path', 'status', 'lines_added', 'lines_removed'}


class Fixture(BaseHTTPRequestHandler):
    calls = []
    failures = []
    attacker = ''
    direct_release = threading.Event()
    stream_release = threading.Event()

    def log_message(self, *_):
        pass

    def reply(self, value, status=200, location=None):
        body = value if isinstance(value, bytes) else json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Length', str(len(body)))
        if location:
            self.send_header('Location', location)
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def do_GET(self):
        try:
            u = urlsplit(self.path)
            q = parse_qs(u.query, keep_blank_values=True)
            type(self).calls.append(('GET', u.path, q))
            assert self.headers.get('Authorization') == smoke.AUTH, 'wrong Basic auth'
            if u.path.startswith(PR):
                ident, operation = u.path[len(PR):].split('/')
                assert not q, 'undocumented PR query forwarding'
                if operation == 'diff':
                    target = REPO + 'diff/source..dest?topic=true&path=wrong.txt'
                    if ident == '43':
                        target = self.attacker + '/exfiltrate'
                    elif ident == '44':
                        target = '/outside-api-prefix'
                    elif ident == '45':
                        self.send_response(200)
                        self.send_header('Content-Length', '128')
                        self.end_headers()
                        self.direct_release.wait(30)
                        try:
                            self.wfile.write(b'UNFILTERED_WHOLE_PR'.ljust(128))
                        except (BrokenPipeError, ConnectionResetError):
                            pass
                        return
                    elif ident == '46':
                        target = PREFIX + '/user'
                    elif ident == '47':
                        target = REPO + 'diff/huge..dest?topic=true'
                    elif ident == '48':
                        self.reply(b'', 302)
                        return
                    elif ident == '49':
                        target = REPO + 'diff/loop..dest?topic=true'
                    elif ident == '59':
                        target = REPO + 'diff/source-conflict..dest?topic=true'
                    self.reply(b'', 302, target)
                    return
                if operation == 'diffstat':
                    modifiers = '' if ident == '60' else '?topic=true'
                    suffix = 'acme/demo:111111111111%0D222222222222' if ident == '61' else f'{ident}..dest'
                    comparison = REPO + 'diffstat/' + suffix
                    self.reply(b'', 302, comparison + ('?from_pullrequest_id=61&topic=true' if ident == '61' else modifiers))
                    return
            if u.path.startswith(REPO + 'diff/'):
                assert q.get('path') == [FILE], 'repository diff requested without exact single path'
                assert q.get('topic') == ['true'], 'comparison modifier lost'
                if u.path.endswith('source..dest'):
                    self.reply(b'', 302, REPO + 'diff/resolved..dest?topic=true&path=another.txt')
                elif u.path.endswith('source-conflict..dest'):
                    self.reply(b'', 302, REPO + 'diff/resolved..dest?topic=false&path=another.txt')
                elif u.path.endswith('loop..dest'):
                    self.reply(b'', 302, REPO + 'diff/loop..dest?topic=true')
                elif u.path.endswith('huge..dest'):
                    self.send_response(200)
                    self.send_header('Content-Length', '20000000')
                    self.end_headers()
                    try:
                        self.wfile.write(LINE * 30_000)
                        self.wfile.flush()
                        self.stream_release.wait(30)
                    except (BrokenPipeError, ConnectionResetError):
                        pass
                else:
                    self.reply(PATCH.encode())
                return
            if u.path == REPO + 'diffstat/acme/demo:111111111111%0D222222222222':
                assert q.get('pagelen') == ['5'], 'bounded page size not applied on Cloud comparison'
                assert q.get('topic') == ['true'] and q.get('from_pullrequest_id') == ['61'], 'Cloud comparison modifiers changed'
                if q.get('page') == ['2']:
                    self.reply({'values': []})
                else:
                    nxt = f'http://127.0.0.1:{self.server.server_port}{u.path}?from_pullrequest_id=61&topic=true&pagelen=5&page=2'
                    self.reply({'values': [], 'next': nxt})
                return
            if u.path.startswith(REPO + 'diffstat/'):
                assert q.get('pagelen') == ['5'], 'bounded page size not applied on repository URL'
                wanted_topic = [] if '/60..dest' in u.path else ['true']
                assert q.get('topic', []) == wanted_topic, 'diffstat comparison changed'
                if q.get('page') == ['2']:
                    self.reply({'values': []})
                    return
                row = {'status': 'renamed', 'old': {'path': 'old/name.rs'},
                       'new': {'path': FILE}, 'lines_added': 3, 'lines_removed': 2,
                       'unexpected': {smoke.TOKEN: smoke.TOKEN}}
                values = [row, {'status': 'removed', 'old': {'path': 'deleted.txt'}, 'new': None},
                          {'status': 'added', 'old': None,
                           'new': {'path': 'new.bin', 'attributes': 'binary'}}]
                nxt = f'http://127.0.0.1:{self.server.server_port}{u.path}?topic=true&pagelen=5&page=2'
                if '/51..dest' in u.path:
                    nxt = self.attacker + '/bad?page=2'
                elif '/52..dest' in u.path:
                    values[0]['lines_added'] = -1
                elif '/53..dest' in u.path:
                    values = [{'status': 'modified', 'old': None, 'new': None}]
                elif '/54..dest' in u.path:
                    nxt = nxt.replace('/54..dest?', '/changed..dest?')
                elif '/55..dest' in u.path:
                    values[0]['lines_added'] = '3'
                elif '/56..dest' in u.path:
                    nxt = 17
                elif '/57..dest' in u.path:
                    nxt = nxt.replace('topic=true', 'topic=false')
                elif '/58..dest' in u.path:
                    nxt = nxt.replace('topic=true&', '')
                elif '/60..dest' in u.path:
                    nxt = nxt.replace('topic=true', 'topic=false')
                self.reply({'values': values, 'next': nxt})
                return
            raise AssertionError('unexpected endpoint: ' + u.path)
        except Exception as exc:
            type(self).failures.append(str(exc))
            self.reply({'fixture_error': str(exc)}, 599)

    def do_POST(self):
        type(self).failures.append('unexpected mutation')
        self.reply(b'', 599)

    do_DELETE = do_POST


class Attacker(BaseHTTPRequestHandler):
    hits = 0

    def log_message(self, *_):
        pass

    def do_GET(self):
        type(self).hits += 1
        self.send_response(200)
        self.end_headers()


class Client(smoke.StdioClient):
    LINE_LIMIT = MAX_DIFF_BYTES * JSON_REPLY_EXPANSION + PROTOCOL_MARGIN
    CAPTURE_LIMIT = LINE_LIMIT + 2_000_000

    def close(self):
        errors = []
        try:
            if self.process.stdin and not self.process.stdin.closed:
                self.process.stdin.close()
        except (BrokenPipeError, OSError):
            pass
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                import os
                os.killpg(self.process.pid, sig)
            except ProcessLookupError:
                pass
            except OSError as exc:
                errors.append(exc)
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                if sig == signal.SIGKILL:
                    errors.append(AssertionError('MCP process did not exit'))
        for reader in self._readers:
            reader.join(timeout=2)
        try:
            self._check_secrets()
        except Exception as exc:
            errors.append(exc)
        for stream, reader in zip((self.process.stdout, self.process.stderr), self._readers):
            if stream and not stream.closed and not reader.is_alive():
                stream.close()
        if errors:
            raise errors[0]


def contains_workspace(value):
    if isinstance(value, dict):
        if 'workspace' in value.get('properties', {}):
            return True
        return any(contains_workspace(item) for item in value.values())
    return isinstance(value, list) and any(contains_workspace(item) for item in value)


def expected(path, query=None):
    return ('GET', path, query or {})


def assert_requests(actual, required):
    assert actual == required, ('unexpected request sequence', actual, required)


def main():
    binary = Path(sys.argv[1]).resolve()
    Fixture.calls = []
    Fixture.failures = []
    Fixture.direct_release.clear()
    Fixture.stream_release.clear()
    Attacker.hits = 0
    servers = []
    client = None
    try:
        for handler in (Attacker, Fixture):
            server = ThreadingHTTPServer(('127.0.0.1', 0), handler)
            servers.append(server)
            threading.Thread(target=server.serve_forever, daemon=True).start()
        Fixture.attacker = f'http://127.0.0.1:{servers[0].server_port}'
        client = Client(binary, f'http://127.0.0.1:{servers[1].server_port}{PREFIX}')
        client.request('initialize', {'protocolVersion': '2025-03-26', 'capabilities': {},
                                    'clientInfo': {'name': 'independent-file-review', 'version': '1'}})
        client.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
        tools = client.request('tools/list', {})['tools']
        assert {tool['name'] for tool in tools} == TOOLS
        assert not any(contains_workspace(tool['inputSchema']) for tool in tools)
        target = {'repo_slug': 'demo', 'pull_request_id': 42}
        listing = 'list_pull_request_files'
        fetching = 'get_pull_request_file_diff'
        file_q = {'topic': ['true'], 'path': [FILE]}

        def invoke(name, args, requests):
            start = len(Fixture.calls)
            result = client.call(name, args)
            assert_requests(Fixture.calls[start:], requests)
            return result

        def list_requests(ident=42, page=None):
            q = {'pagelen': ['5']}
            if ident != 60:
                q['topic'] = ['true']
            if page:
                q['page'] = [page]
            return [expected(PR + f'{ident}/diffstat'), expected(REPO + f'diffstat/{ident}..dest', q)]

        cloud_requests = [expected(PR + '61/diffstat'), expected(REPO + 'diffstat/acme/demo:111111111111%0D222222222222', {'from_pullrequest_id': ['61'], 'topic': ['true'], 'pagelen': ['5']})]
        cloud = smoke.data(invoke(listing, {**target, 'pull_request_id': 61, 'pagelen': 5}, cloud_requests))
        assert cloud['next_page'] == '2' and cloud['values'] == []
        cloud_second = smoke.data(invoke(listing, {**target, 'pull_request_id': 61, 'pagelen': 5, 'page': '2'}, [expected(PR + '61/diffstat'), expected(REPO + 'diffstat/acme/demo:111111111111%0D222222222222', {'from_pullrequest_id': ['61'], 'topic': ['true'], 'pagelen': ['5'], 'page': ['2']})]))
        assert cloud_second['values'] == [] and cloud_second['next_page'] is None
        page = smoke.data(invoke(listing, {**target, 'pagelen': 5}, list_requests()))
        assert page['next_page'] == '2' and page['truncated'] is False
        rows = page['values']
        wanted = [dict(old_path='old/name.rs', new_path=FILE, status='renamed', lines_added=3, lines_removed=2),
                  dict(old_path='deleted.txt', new_path=None, status='removed', lines_added=None, lines_removed=None),
                  dict(old_path=None, new_path='new.bin', status='added', lines_added=None, lines_removed=None)]
        assert len(rows) == len(wanted)
        assert all(set(row) <= FIELDS for row in rows), 'wire metadata leaked'
        assert [{key: row.get(key) for key in FIELDS} for row in rows] == wanted
        smoke.assert_no_credentials({'metadata': json.dumps(page)})
        second = smoke.data(invoke(listing, {**target, 'pagelen': 5, 'page': '2'}, list_requests(page='2')))
        assert second['values'] == [] and second['next_page'] is None
        inherited = smoke.data(invoke(listing, {**target, 'pull_request_id': 58, 'pagelen': 5}, list_requests(58)))
        assert inherited['next_page'] == '2' and inherited['values'] == rows
        inherited_second = smoke.data(invoke(listing, {**target, 'pull_request_id': 58, 'pagelen': 5, 'page': '2'}, list_requests(58, '2')))
        assert inherited_second['values'] == [] and inherited_second['next_page'] is None
        result = smoke.data(invoke(fetching, {**target, 'path': FILE}, [
            expected(PR + '42/diff'), expected(REPO + 'diff/source..dest', file_q),
            expected(REPO + 'diff/resolved..dest', file_q)]))
        assert result == {'path': FILE, 'diff': PATCH, 'truncated': False}
        conflict_redirect = invoke(fetching, {**target, 'pull_request_id': 59, 'path': FILE}, [
            expected(PR + '59/diff'), expected(REPO + 'diff/source-conflict..dest', file_q)])
        assert conflict_redirect.get('isError'), 'redirect changed comparison topic'
        for ident in (43, 44, 45, 46, 48, 49):
            requests = [expected(PR + f'{ident}/diff')]
            if ident == 49:
                requests += [expected(REPO + 'diff/loop..dest', file_q)] * 3
            error = invoke(fetching, {**target, 'pull_request_id': ident, 'path': FILE}, requests)
            if ident == 45:
                Fixture.direct_release.set()
            assert error.get('isError'), (ident, error)
            assert 'UNFILTERED_WHOLE_PR' not in json.dumps(error)
        for ident in (51, 52, 53, 54, 55, 56, 57, 60):
            error = invoke(listing, {**target, 'pull_request_id': ident, 'pagelen': 5}, list_requests(ident))
            assert error.get('isError'), (ident, error)
        for name, args in [(fetching, {**target, 'workspace': 'wrong', 'path': FILE}),
                           (listing, {**target, 'workspace': 'wrong'})] + [
            (fetching, {**target, 'path': path})
            for path in ('', '/absolute', '../outside', 'a/../b', 'a\nb', 'a\x00b')]:
            before = len(Fixture.calls)
            try:
                error = client.call(name, args)
                assert error.get('isError'), 'invalid input was accepted'
            except AssertionError as exc:
                assert str(exc) == 'MCP protocol error', exc
            assert_requests(Fixture.calls[before:], [])
        huge = smoke.data(invoke(fetching, {**target, 'pull_request_id': 47, 'path': FILE}, [
            expected(PR + '47/diff'), expected(REPO + 'diff/huge..dest', file_q)]))
        Fixture.stream_release.set()
        assert huge['truncated'] is True and huge['diff'].encode() == LINE * (MAX_DIFF_BYTES // len(LINE))
        assert huge['path'] == FILE
        assert not Fixture.failures, Fixture.failures
        assert Attacker.hits == 0, 'credential-bearing cross-origin request'
    finally:
        Fixture.direct_release.set()
        Fixture.stream_release.set()
        try:
            if client:
                client.close()
        finally:
            for server in reversed(servers):
                server.shutdown()
                server.server_close()
    print('PASS independent file review:11tools, exact authenticated sequences, diffstat metadata/paging, encoded server filter, fork context, navigation/input failures, header-only rejection, retained streaming prefix')


if __name__ == '__main__':
    main()
