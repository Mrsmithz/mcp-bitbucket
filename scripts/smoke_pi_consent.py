#!/usr/bin/env python3
"""Drive real Pi consent dialogs in a PTY, without a model or Bitbucket request.

Requires POSIX, Pi 1.5.0 on PATH, and npm ci in this repository. All Pi state is
isolated in a temporary directory. The adapter broker is the installed source,
not a replacement event implementation.
"""

from dataclasses import dataclass
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
ANSI = re.compile(r"\x1b\][^\x07]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]|\x1b[@-Z\\-_]")
SERVER_ALIAS = "renamed-bitbucket-smoke"


@dataclass(frozen=True)
class SmokeRequest:
    output_offset: int
    request_id: str
    tool_name: str


def result_marker(request_id, tool_name, decision):
    return f"CONSENT_SMOKE_RESULT:{request_id}:{tool_name}:{decision}"


def matches_result(output, request, decision):
    return result_marker(request.request_id, request.tool_name, decision) in output


def has_exact_pr_id(text, pull_request_id=42):
    return bool(re.search(rf"(?<![A-Za-z0-9])(?:Pull request:\s*#?{pull_request_id}|#{pull_request_id})(?![A-Za-z0-9])",
                         text))


def has_labeled_value(text, label, value):
    labels = re.compile(r"(?<![A-Za-z0-9])(?:Action|Server|Repository|Pull request):\s*")
    matches = list(labels.finditer(text))
    values = []
    for index, match in enumerate(matches):
        if match.group(0).startswith(label + ":"):
            end = matches[index + 1].start() if index + 1 < len(matches) else len(text)
            current = re.split(r"→\s*(?:Allow once|Deny)", text[match.end():end], maxsplit=1)[0]
            values.append(current.strip())
    return bool(values) and all(item == value for item in values)


def displayed_action(text, tool_name):
    current_text = re.sub(r"CONSENT_SMOKE_RESULT:\S+", "", text)
    actions = set(re.findall(r"(?<![A-Za-z0-9])Action:\s*(Approve pull request|Withdraw approval)(?![A-Za-z0-9_])",
                             current_text))
    expected = {"approve_pull_request": "Approve pull request",
                "withdraw_pull_request_approval": "Withdraw approval"}.get(tool_name)
    return expected is not None and actions == {expected}


def prompt_is_valid(text, tool_name):
    return (has_labeled_value(text, "Server", SERVER_ALIAS)
            and has_labeled_value(text, "Repository", "demo")
            and has_labeled_value(text, "Pull request", "#42")
            and displayed_action(text, tool_name))


def validate_prompt(text, tool_name):
    assert has_labeled_value(text, "Repository", "demo"), "consent prompt omitted exact repository demo"
    assert (has_exact_pr_id(text) and has_labeled_value(text, "Pull request", "#42")), (
        "consent prompt omitted exact PR 42")
    assert has_labeled_value(text, "Server", SERVER_ALIAS), "consent prompt omitted the exact renamed server alias"
    assert displayed_action(text, tool_name), "consent prompt showed the wrong or ambiguous action"


class Terminal:
    def __init__(self, command, cwd, env):
        self.master = None
        slave = None
        self.process = None
        try:
            self.master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 150, 0, 0))
            self.process = subprocess.Popen(command, cwd=cwd, env=env, stdin=slave,
                                            stdout=slave, stderr=slave, start_new_session=True)
            os.close(slave)
            slave = None
            self.output = ""
        except BaseException:
            if slave is not None:
                os.close(slave)
            if self.master is not None:
                os.close(self.master)
            raise

    def send(self, value):
        os.write(self.master, value.encode())

    def wait(self, predicate, start=0, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            current = ANSI.sub("", self.output[start:]).replace("\r", "")
            if predicate(current):
                return current
            if self.process.poll() is not None:
                raise AssertionError("Pi exited early:\n" + current[-4000:])
            readable, _, _ = select.select([self.master], [], [], 0.1)
            if not readable:
                continue
            try:
                block = os.read(self.master, 65536)
            except OSError as error:
                if error.errno == errno.EIO:
                    raise AssertionError("Pi closed its terminal:\n" + current[-4000:]) from error
                raise
            if not block:
                raise AssertionError("Pi closed its terminal:\n" + current[-4000:])
            if b"\x1b[6n" in block:
                os.write(self.master, b"\x1b[1;1R")
            self.output += block.decode("utf-8", errors="replace")
        raise AssertionError("Timed out waiting for Pi UI:\n" + current[-4000:])

    def command(self, tool_name):
        if tool_name not in {"get_pull_request", "approve_pull_request", "withdraw_pull_request_approval"}:
            raise ValueError(f"unsupported smoke tool: {tool_name}")
        request = SmokeRequest(len(self.output), uuid.uuid4().hex, tool_name)
        self.send("/consent-smoke " + request.request_id + " " + tool_name + "\r")
        return request

    def wait_result(self, request, decision, timeout=30):
        marker = result_marker(request.request_id, request.tool_name, decision)
        return self.wait(lambda output: matches_result(output, request, decision), request.output_offset, timeout)

    def allow(self, screen):
        options = re.findall(r"\b(Allow once|Deny|Yes|No)(?= {2,}|$)", screen)
        options = options[-2:]
        choices = [index for index, option in enumerate(options) if option in {"Allow once", "Yes"}]
        assert len(choices) == 1, "expected exactly one single-call approval option"
        self.send("k" * 8 + "j" * choices[0] + "\r")

    def close(self):
        try:
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                pass
            try:
                os.killpg(self.process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            self.process.wait(timeout=2)
        finally:
            if self.master is not None:
                os.close(self.master)
                self.master = None


def check_pi_version(pi):
    try:
        result = subprocess.run([pi, "--version"], check=True, capture_output=True, text=True)
    except (OSError, subprocess.CalledProcessError) as error:
        raise AssertionError("could not determine Pi version; smoke driver requires Pi 1.5.0") from error
    version = (result.stdout or result.stderr).strip()
    assert version == "1.5.0", f"unsupported Pi version {version!r}; smoke driver supports 1.5.0"
    return version


def main():
    pi = shutil.which("pi")
    assert pi, "Pi must be installed and on PATH"
    pi_version = check_pi_version(pi)
    companion = ROOT / ".pi/extensions/bitbucket-consent.ts"
    broker = ROOT / "node_modules/pi-mcp-adapter/tool-approval.ts"
    assert companion.is_file(), "Pi companion extension is missing"
    assert broker.is_file(), "run npm ci first to install the pinned adapter"
    helper_source = '''import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { requestBrokerApproval } from BROKER_PATH;
export default function (pi: ExtensionAPI) {
  pi.on("session_start", async (_event, ctx) => { ctx.ui.notify("CONSENT_SMOKE_READY"); });
  pi.registerCommand("consent-smoke", {
    handler: async (args, ctx) => {
      const [requestId, originalName, extra] = args.trim().split(/\\s+/);
      const validTools = new Set(["get_pull_request", "approve_pull_request", "withdraw_pull_request_approval"]);
      if (!requestId || !validTools.has(originalName) || extra) {
        ctx.ui.notify("CONSENT_SMOKE_INVALID_REQUEST");
        return;
      }
      const result = await requestBrokerApproval(pi.events, "renamed-bitbucket-smoke",
        { name: "renamed_" + originalName, originalName },
        { repo_slug: "demo", pull_request_id: 42 }, "direct");
      const decision = typeof result === "string" ? result : result.decision;
      ctx.ui.notify("CONSENT_SMOKE_RESULT:" + requestId + ":" + originalName + ":" + decision);
    }
  });
}
'''.replace("BROKER_PATH", json.dumps(str(broker)))
    with tempfile.TemporaryDirectory(prefix="bitbucket-pi-smoke-") as temporary:
        directory = Path(temporary)
        helper = directory / "smoke-helper.ts"
        helper.write_text(helper_source)
        agent = directory / "agent"
        agent.mkdir()
        (agent / "settings.json").write_text('{"packages":[],"extensions":[]}')
        env = {key: value for key, value in os.environ.items()
               if key in {"PATH", "LANG", "LC_ALL", "SHELL"}}
        env.update(HOME=str(directory), PI_CODING_AGENT_DIR=str(agent),
                   TERM="xterm-256color", COLORTERM="truecolor")
        command = [pi, "--offline", "--no-session", "--no-tools", "--no-mcp",
                   "--no-extensions", "--no-skills", "--no-prompt-templates",
                   "--no-context-files", "-e", str(companion), "-e", str(helper)]
        terminal = Terminal(command, directory, env)
        try:
            terminal.wait(lambda output: "CONSENT_SMOKE_READY" in output)
            read_request = terminal.command("get_pull_request")
            terminal.wait_result(read_request, "abstain")

            approval = terminal.command("approve_pull_request")
            prompt = terminal.wait(
                lambda output: prompt_is_valid(output, approval.tool_name)
                and ("Allow once" in output or "Yes" in output), approval.output_offset)
            validate_prompt(prompt, approval.tool_name)
            terminal.allow(prompt)
            terminal.wait_result(approval, "allow_once")

            for tool_name in ("approve_pull_request", "withdraw_pull_request_approval"):
                request = terminal.command(tool_name)
                prompt = terminal.wait(
                    lambda output: prompt_is_valid(output, tool_name)
                    and ("Deny" in output or "No" in output), request.output_offset)
                validate_prompt(prompt, tool_name)
                terminal.send("\x1b")
                terminal.wait_result(request, "deny")
        finally:
            terminal.close()
        print(f"PASS: Pi {pi_version} TUI allows once, prompts again, denies cancel/withdrawal, leaves reads alone")


if __name__ == "__main__":
    main()
