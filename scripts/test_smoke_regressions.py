import importlib.util
import json
import os
import sys
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent


def load_script(name):
    spec = importlib.util.spec_from_file_location(name, HERE / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


stdio = load_script("smoke_stdio")
consent = load_script("smoke_pi_consent")


class StdioRegressionTests(unittest.TestCase):
    def test_approval_result_requires_structured_content(self):
        self.assertEqual(stdio.structured_data({
            "isError": False, "structuredContent": {"count": 0, "approvers": []},
        }), {"count": 0, "approvers": []})
        with self.assertRaisesRegex(AssertionError, "canonical structured content"):
            stdio.structured_data({
                "isError": False,
                "content": [{"type": "text", "text": '{"count":0,"approvers":[]}'}],
            })

    def test_approval_report_requires_canonical_fields_and_identity(self):
        stdio.verify_approval_report({
            "approvers": [{"uuid": stdio.BOB["uuid"],
                           "account_id": stdio.BOB["account_id"],
                           "display_name": "Bob Approved"}],
            "count": 1,
        })
        for invalid in (
            {"approvers": [{"uuid": stdio.BOB["uuid"], "display_name": "Bob Approved"}], "count": 1},
            {"approved_users": [{"uuid": stdio.BOB["uuid"], "account_id": stdio.BOB["account_id"],
                                 "display_name": "Bob Approved"}], "count": 1},
            {"approvers": [{"uuid": stdio.BOB["uuid"], "account_id": "wrong",
                            "display_name": "Bob Approved"}], "count": 1},
            {"approvers": [{"uuid": stdio.BOB["uuid"], "account_id": stdio.BOB["account_id"],
                            "display_name": "Bob Approved"}], "approval_count": 1},
        ):
            with self.subTest(invalid=invalid), self.assertRaises(AssertionError):
                stdio.verify_approval_report(invalid)

    def test_workflow_writes_require_exact_payloads_and_no_retries(self):
        reads = [("GET", path, True, None) for path in (
            stdio.PR_PATH, stdio.PR_PATH + "/42", stdio.COMMENTS_PATH,
            stdio.PR_PATH + "/42/diff", stdio.FILES_PATH,
            stdio.ROOT + "/repositories/acme/demo/diffstat/abc..def",
            stdio.ROOT + "/repositories/acme/demo/diff/abc..def")]
        writes = [
            ("POST", stdio.PR_PATH, True, {
                "title": "Smoke-created PR", "source": {"branch": {"name": "feature/smoke"}},
                "destination": {"branch": {"name": "main"}}, "description": "Created locally",
                "reviewers": [{"uuid": stdio.BOB["uuid"]}],
            }),
            ("POST", stdio.COMMENTS_PATH, True, {"content": {"raw": "General smoke comment"}}),
            ("POST", stdio.COMMENTS_PATH, True,
             {"content": {"raw": "Reply smoke comment"}, "parent": {"id": 7}}),
            ("POST", stdio.COMMENTS_PATH, True,
             {"content": {"raw": "Old-line smoke comment"},
              "inline": {"path": "src/lib.rs", "from": 2}}),
            ("POST", stdio.COMMENTS_PATH, True,
             {"content": {"raw": "New-line smoke comment"},
              "inline": {"path": "src/lib.rs", "to": 3}}),
            ("POST", stdio.APPROVE_42_PATH, True, None),
            ("DELETE", stdio.APPROVE_42_PATH, True, None),
            ("POST", stdio.APPROVE_PATH, True, None),
        ]
        stdio.verify_workflow_requests(reads + writes)
        wrong_inline = list(writes)
        wrong_inline[3] = (*wrong_inline[3][:3], {
            "content": {"raw": "Old-line smoke comment"},
            "inline": {"path": "src/lib.rs", "to": 2},
        })
        duplicate = writes + [writes[-1]]
        wrong_auth = list(writes)
        wrong_auth[-1] = (*wrong_auth[-1][:2], False, wrong_auth[-1][3])
        for invalid in (reads + wrong_inline, reads + duplicate, reads + wrong_auth):
            with self.subTest(invalid=invalid), self.assertRaises(AssertionError):
                stdio.verify_workflow_requests(invalid)

    def test_mutation_requires_exact_endpoint_and_basic_auth(self):
        stdio.verify_mutation_requests([("POST", stdio.APPROVE_PATH, True)])
        for request in (
            [("POST", stdio.APPROVE_PATH + "-wrong", True)],
            [("POST", stdio.APPROVE_PATH, False)],
            [("POST", stdio.APPROVE_PATH, True), ("POST", stdio.APPROVE_PATH, True)],
            [("POST", stdio.ROOT + "/unknown", True)],
            [],
        ):
            with self.subTest(request=request), self.assertRaises(AssertionError):
                stdio.verify_mutation_requests(request)

    def test_only_canonical_structured_unknown_outcome_is_accepted(self):
        self.assertTrue(stdio.is_outcome_unknown({
            "isError": True,
            "structuredContent": {"error": {"code": "outcome_unknown", "message": "disconnected"}},
        }))
        for result in (
            {"isError": False,
             "structuredContent": {"error": {"code": "outcome_unknown"}}},
            {"error": {"code": "outcome_unknown"}},
            {"structuredContent": {"error": {"kind": "outcome_unknown"}}},
            {"structuredContent": {"error": {"code": "unknown_endpoint"}}},
            {"content": [{"type": "text", "text": "mutation outcome uncertain after disconnect"}]},
            {"content": [{"type": "text", "text": "unknown endpoint"}]},
        ):
            with self.subTest(result=result):
                self.assertFalse(stdio.is_outcome_unknown(result))

    def test_plain_basic_and_raw_encoded_credentials_are_rejected(self):
        self.assert_no_leak({"stdout": "clean", "stderr": "clean"})
        token_key = json.dumps({stdio.TOKEN: "fixture-only-untrusted-value"})
        for value in (stdio.TOKEN, stdio.AUTH, stdio.ENCODED_AUTH, token_key):
            for channel in ("stdout", "stderr"):
                with self.subTest(value_kind="encoded" if value == stdio.ENCODED_AUTH else "credential",
                                  channel=channel):
                    with self.assertRaisesRegex(AssertionError, f"credential leaked to {channel}"):
                        stdio.assert_no_credentials({channel: value})

    def test_raw_encoded_credential_leaks_at_shutdown_are_rejected_in_both_channels(self):
        for channel in ("stdout", "stderr"):
            with self.subTest(channel=channel), tempfile.TemporaryDirectory() as directory:
                executable = Path(directory) / "leaker"
                output = "print(" + repr(stdio.ENCODED_AUTH) + (", file=sys.stderr)\n" if channel == "stderr"
                         else ")\n")
                executable.write_text("#!/usr/bin/env python3\nimport sys\nsys.stdin.read()\n" + output)
                executable.chmod(0o700)
                client = stdio.StdioClient(executable, "http://127.0.0.1/unused")
                with self.assertRaisesRegex(AssertionError, f"credential leaked to {channel}"):
                    client.close()

    def test_capture_overflow_fails_waiter_and_bounds_capture_and_queue(self):
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "noisy"
            executable.write_text(
                "#!/usr/bin/env python3\n"
                "import sys\n"
                "sys.stdin.buffer.readline()\n"
                "sys.stdout.buffer.write(b'x' * 2000000)\n"
                "sys.stdout.flush()\n"
                "sys.stderr.buffer.write(b'e' * 2000000)\n"
                "sys.stderr.flush()\n"
                "sys.stdout.buffer.write(b'\\n' * 50000)\n"
            )
            executable.chmod(0o700)
            client = stdio.StdioClient(executable, "http://127.0.0.1/unused")
            with self.assertRaisesRegex(AssertionError, "bounded capture"):
                client.request("tools/list", {})
            try:
                client.close()
            except AssertionError as error:
                self.assertIn("bounded capture", str(error))
            self.assertLessEqual(client.responses.qsize(), client.RESPONSE_QUEUE_LIMIT)
            self.assertTrue(all(size <= client.CAPTURE_LIMIT for size in client._sizes.values()))
            self.assertTrue(all(sum(map(len, chunks)) <= client.CAPTURE_LIMIT
                                for chunks in client._captures.values()))

    def test_client_env_overrides_replace_and_unset_fixture_values(self):
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "environment"
            executable.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, sys\n"
                "sys.stdin.read()\n"
                "print(json.dumps({'email_set': bool(os.environ.get('BITBUCKET_EMAIL')), "
                "'token_set': bool(os.environ.get('BITBUCKET_API_TOKEN')), "
                "'tls_set': 'BITBUCKET_TLS_VERIFY' in os.environ}))\n"
            )
            executable.chmod(0o700)
            client = stdio.StdioClient(executable, "http://127.0.0.1/unused", env_overrides={
                "BITBUCKET_EMAIL": "fixture@example.invalid",
                "BITBUCKET_API_TOKEN": "fixture-only-token",
                "BITBUCKET_TLS_VERIFY": None,
            })
            client.close()
            self.assertEqual(json.loads(client._captured()["stdout"]), {
                "email_set": True, "token_set": True, "tls_set": False,
            })
            self.assertIn("fixture-only-token", client.secrets)

    def test_broken_pipe_while_closing_stdin_still_kills_process_group(self):
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "waiting"
            ready = Path(directory) / "ready"
            executable.write_text(
                "#!/usr/bin/env python3\n"
                "import time\nfrom pathlib import Path\n"
                f"Path({str(ready)!r}).write_text('ready')\n"
                "time.sleep(30)\n"
            )
            executable.chmod(0o700)
            client = stdio.StdioClient(executable, "http://127.0.0.1/unused")
            try:
                deadline = time.monotonic() + 3
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(ready.exists(), "fixture did not become ready")
                self.assertIsNone(client.process.poll(), "fixture exited before failure injection")
                with patch.object(client.process.stdin, "close", side_effect=BrokenPipeError("controlled close")):
                    with self.assertRaisesRegex(BrokenPipeError, "controlled close"):
                        client.close()
                self.assertIsNotNone(client.process.poll())
                self.assertTrue(all(not reader.is_alive() for reader in client._readers))
            finally:
                if client.process.poll() is None:
                    client.process.kill()
                client.process.wait(timeout=3)
                client.process.stdin.close()
                for reader in client._readers:
                    reader.join(timeout=3)

    def test_exited_parent_descendant_holding_pipes_is_killed_and_readers_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "term-seen"
            ready = Path(directory) / "ready"
            child = Path(directory) / "child.py"
            executable = Path(directory) / "parent"
            child.write_text(
                "import signal, time\n"
                f"from pathlib import Path\nmarker = Path({str(marker)!r})\nready = Path({str(ready)!r})\n"
                "def stopped(*_):\n    marker.write_text('term')\n    raise SystemExit(0)\n"
                "signal.signal(signal.SIGTERM, stopped)\nready.write_text('ready')\n"
                "time.sleep(30)\n"
            )
            executable.write_text(
                "#!/usr/bin/env python3\n"
                "import subprocess, sys\n"
                "import time\n"
                f"subprocess.Popen([sys.executable, {str(child)!r}])\n"
                f"deadline = time.monotonic() + 3\nwhile not __import__('pathlib').Path({str(ready)!r}).exists() and time.monotonic() < deadline: time.sleep(0.01)\n"
            )
            executable.chmod(0o700)
            client = stdio.StdioClient(executable, "http://127.0.0.1/unused")
            client.process.wait(timeout=3)
            self.assertIsNotNone(client.process.poll(), "test parent did not exit before cleanup")
            client.close()
            self.assertTrue(marker.exists(), "process-group TERM did not reach pipe-holding descendant")
            self.assertTrue(all(not reader.is_alive() for reader in client._readers))
            self.assertLessEqual(sum(client._sizes.values()), 2 * client.CAPTURE_LIMIT)

    def test_finite_stdout_and_stderr_are_captured_within_bounds(self):
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "finite"
            executable.write_text(
                "#!/usr/bin/env python3\n"
                "import sys\n"
                "sys.stdin.read()\n"
                "print('finite stdout')\n"
                "print('finite stderr', file=sys.stderr)\n"
            )
            executable.chmod(0o700)
            client = stdio.StdioClient(executable, "http://127.0.0.1/unused")
            client.close()
            captured = client._captured()
            self.assertEqual(captured["stdout"], "finite stdout\n")
            self.assertEqual(captured["stderr"], "finite stderr\n")
            self.assertTrue(all(size <= client.CAPTURE_LIMIT for size in client._sizes.values()))

    def test_child_environment_excludes_unrelated_parent_secrets(self):
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "environment"
            executable.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, sys\n"
                "sys.stdin.read()\n"
                "print(json.dumps({'unrelated': os.environ.get('UNRELATED_ACCOUNT_KEY'), "
                "'email_set': bool(os.environ.get('BITBUCKET_EMAIL')), "
                "'token_set': bool(os.environ.get('BITBUCKET_API_TOKEN')), "
                "'workspace': os.environ.get('BITBUCKET_WORKSPACE'), "
                "'tls_verify': os.environ.get('BITBUCKET_TLS_VERIFY'), "
                "'no_proxy': os.environ.get('NO_PROXY')}))\n"
            )
            executable.chmod(0o700)
            with patch.dict(os.environ, {"UNRELATED_ACCOUNT_KEY": "parent-only-secret"}):
                client = stdio.StdioClient(executable, "http://127.0.0.1/unused")
                client.close()
            environment = json.loads(client._captured()["stdout"])
            self.assertIsNone(environment["unrelated"])
            self.assertTrue(environment["email_set"])
            self.assertTrue(environment["token_set"])
            self.assertEqual(environment["workspace"], "acme")
            self.assertEqual(environment["tls_verify"], "true")
            self.assertEqual(environment["no_proxy"], "127.0.0.1,localhost")

    def assert_no_leak(self, output):
        stdio.assert_no_credentials(output)


class PiConsentRegressionTests(unittest.TestCase):
    def setUp(self):
        self.request = consent.SmokeRequest(0, "nonce-a", "approve_pull_request")

    def test_stale_result_marker_cannot_match_current_request(self):
        stale = consent.result_marker("nonce-old", "approve_pull_request", "allow_once")
        current = consent.result_marker("nonce-a", "approve_pull_request", "allow_once")
        self.assertFalse(consent.matches_result(stale, self.request, "allow_once"))
        self.assertTrue(consent.matches_result(current, self.request, "allow_once"))
        self.assertFalse(consent.matches_result(current, self.request, "deny"))

    def test_exact_pr_number_and_action_labels_are_required(self):
        self.assertTrue(consent.has_exact_pr_id("Pull request: #42"))
        self.assertFalse(consent.has_exact_pr_id("Pull request: #142"))
        self.assertFalse(consent.has_exact_pr_id("Other text 42"))
        self.assertTrue(consent.displayed_action("Action: Approve pull request", "approve_pull_request"))
        self.assertFalse(consent.displayed_action("Action: Withdraw approval", "approve_pull_request"))
        self.assertTrue(consent.displayed_action("Action: Withdraw approval",
                                                 "withdraw_pull_request_approval"))
        self.assertFalse(consent.displayed_action(
            "Action: Approve pull request\nAction: Withdraw approval", "approve_pull_request"))

    def test_prompt_requires_exact_pair_pr_action_and_server_alias(self):
        valid = ("Action: Approve pull request\nServer: renamed-bitbucket-smoke\n"
                 "Repository: demo\nPull request: #42")
        consent.validate_prompt(valid, "approve_pull_request")
        invalid = (
            "Action: Approve pull request\nServer: renamed-bitbucket-smoke\n"
            "Repository: other\nPull request: #42",
            "Action: Approve pull request\nServer: renamed-bitbucket-smoke\n"
            "Repository: other\nRepository: demo\nPull request: #42",
            "Action: Approve pull request\nServer: renamed-bitbucket-smoke\n"
            "Repository: demo-extra\nPull request: #42",
            "Action: Approve pull request\nServer: renamed-bitbucket-smoke\n"
            "Repository: demo\nPull request: #142",
            "Action: Approve pull request\nServer: renamed-bitbucket-smoke\n"
            "Repository: demo\nPull request 42",
            "Action: Approve pull request\nServer: renamed-bitbucket-smoke-extra\n"
            "Repository: demo\nPull request: #42",
            "Action: Withdraw approval\nServer: renamed-bitbucket-smoke\n"
            "Repository: demo\nPull request: #42",
        )
        for screen in invalid:
            with self.subTest(screen=screen), self.assertRaises(AssertionError):
                consent.validate_prompt(screen, "approve_pull_request")

    def test_previous_approval_result_does_not_override_current_withdrawal_action(self):
        previous = consent.result_marker("nonce-old", "approve_pull_request", "allow_once")
        current = (previous + "\nAction: Withdraw approval\nServer: renamed-bitbucket-smoke\n"
                   "Repository: demo\nPull request: #42")
        self.assertTrue(consent.prompt_is_valid(current, "withdraw_pull_request_approval"))
        wrong_current = current.replace("Action: Withdraw approval", "Action: Approve pull request")
        self.assertFalse(consent.prompt_is_valid(wrong_current, "withdraw_pull_request_approval"))


if __name__ == "__main__":
    unittest.main()
