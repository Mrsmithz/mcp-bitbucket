import test from "node:test";
import assert from "node:assert/strict";
import { createEventBus } from "@earendil-works/pi-coding-agent";
import { ensureToolCallApproved, requestBrokerApproval } from "../node_modules/pi-mcp-adapter/tool-approval.ts";
import { getServerApprovalIdentity, getToolApprovalIdentity } from "../node_modules/pi-mcp-adapter/session-approvals.ts";

const EVENT = "pi-mcp-adapter:tool-approval-request";
const { default: registerConsent } = await import("../.pi/extensions/bitbucket-consent.ts");

function eventBus() {
  return createEventBus();
}

function piFixture() {
  const events = eventBus();
  const handlers = new Map();
  const pi = {
    events,
    on(name, handler) { handlers.set(name, handler); return () => handlers.delete(name); },
  };
  registerConsent(pi);
  return { events, handlers };
}

function approvalFixture({ tool = "approve_pull_request", args = { repo_slug: "core", pull_request_id: 17 }, ui } = {}) {
  const definition = { command: "bitbucket-mcp", approveTools: ["approve_pull_request", "withdraw_pull_request_approval"] };
  const toolMeta = { name: `mcp__renamed__${tool}`, originalName: tool, inputSchema: { type: "object" } };
  const state = {
    config: { mcpServers: { renamed: definition }, settings: {} },
    approvedServers: new Map(),
    approvedToolCalls: new Map(),
    approvalEvents: eventBus(),
    ...(ui ? { ui } : {}),
  };
  const identity = getServerApprovalIdentity(state, "renamed");
  state.approvedServers.set("renamed", identity);
  const { cacheKey } = getToolApprovalIdentity("renamed", toolMeta, args);
  state.approvedToolCalls.set(cacheKey, true);
  return { state, toolMeta, args };
}

async function approve(fixture, args = fixture.args, signal) {
  return ensureToolCallApproved(
    fixture.state,
    "renamed",
    fixture.toolMeta,
    args,
    signal,
  );
}

function context(select, { hasUI = true } = {}) {
  return { hasUI, ui: { select } };
}

function startSession(handlers, ctx) {
  handlers.get("session_start")({}, ctx);
}

function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

function timeout(ms) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`did not settle within ${ms}ms`)), ms);
    timer.unref?.();
  });
}

test("the populated adapter caches really allow without UI before the companion is installed", async () => {
  let uiCalls = 0;
  const fixture = approvalFixture({ ui: { select: async () => { uiCalls++; return "Deny"; } } });
  assert.equal(fixture.state.config.mcpServers.renamed.approveTools.includes("approve_pull_request"), true);
  assert.equal(fixture.state.approvedServers.size, 1);
  assert.equal(fixture.state.approvedToolCalls.size, 1);
  assert.deepEqual(await approve(fixture), { ok: true });
  assert.equal(uiCalls, 0);
});

test("the real broker invokes the default factory handler and returns allow-once", async () => {
  const { events, handlers } = piFixture();
  const fixture = approvalFixture();
  let prompt;
  startSession(handlers, context(async (title, options) => {
    prompt = { title, options };
    return "Allow once";
  }));
  assert.equal(await requestBrokerApproval(
    events,
    "renamed",
    { name: fixture.toolMeta.name, originalName: fixture.toolMeta.originalName },
    fixture.args,
    "proxy",
  ), "allow_once");
  assert.deepEqual(prompt.options, ["Allow once", "Deny"]);
  assert.match(prompt.title, /Action: Approve pull request/);
});

test("concurrent valid requests each require and receive their own decision", async () => {
  const { events, handlers } = piFixture();
  const firstDialog = deferred();
  const secondDialog = deferred();
  const dialogs = [firstDialog, secondDialog];
  const prompts = [];
  startSession(handlers, context((title) => {
    prompts.push(title);
    return dialogs[prompts.length - 1].promise;
  }));
  const first = approvalFixture();
  const second = approvalFixture();
  first.state.approvalEvents = events;
  second.state.approvalEvents = events;

  const firstCall = approve(first);
  await new Promise((resolve) => setImmediate(resolve));
  const secondCall = approve(second);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(prompts.length, 1);

  firstDialog.resolve("Deny");
  assert.deepEqual(await firstCall, { ok: false, reason: "denied" });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(prompts.length, 2);
  secondDialog.resolve("Allow once");
  assert.deepEqual(await secondCall, { ok: true });
  assert.equal(prompts.length, 2);
});

test("a pre-aborted adapter request does not open the consent UI", async () => {
  const { events, handlers } = piFixture();
  let dialogs = 0;
  startSession(handlers, context(async () => { dialogs++; return "Allow once"; }));
  const fixture = approvalFixture();
  fixture.state.approvalEvents = events;
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(approve(fixture, fixture.args, controller.signal), { name: "AbortError" });
  assert.equal(dialogs, 0);
});

test("default factory makes every approval decision override the same proven adapter grant", async () => {
  const prompts = [];
  const decisions = ["Deny", "Allow once", "Allow once"];
  const { events, handlers } = piFixture();
  const fixture = approvalFixture();
  fixture.state.approvalEvents = events;
  startSession(handlers, context(async (title, options, opts) => {
    prompts.push({ title, options, signal: opts.signal });
    return decisions.shift();
  }));

  assert.deepEqual(await approve(fixture), { ok: false, reason: "denied" });
  assert.deepEqual(await approve(fixture), { ok: true });
  assert.equal(fixture.state.approvedServers.size, 1);
  assert.equal(fixture.state.approvedToolCalls.size, 1);
  assert.deepEqual(prompts.map(({ options }) => options), [["Allow once", "Deny"], ["Allow once", "Deny"]]);
  assert.match(prompts[0].title, /Action: Approve pull request\nServer: renamed\nRepository: core\nPull request: #17/);

  fixture.toolMeta.originalName = "withdraw_pull_request_approval";
  fixture.toolMeta.name = "mcp__renamed__withdraw_pull_request_approval";
  fixture.state.config.mcpServers.renamed.approveTools.push("withdraw_pull_request_approval");
  assert.deepEqual(await approve(fixture), { ok: true });
  assert.match(prompts[2].title, /^Withdraw pull request approval\?/);
  assert.match(prompts[2].title, /Action: Withdraw approval/);
});

test("only exact approval tools are claimed, independent of server alias", async () => {
  const { events, handlers } = piFixture();
  const prompts = [];
  startSession(handlers, context(async (title) => { prompts.push(title); return "Allow once"; }));
  const unclaimed = (tool) => {
    let claimed = false;
    events.emit(EVENT, {
      serverName: "any-alias",
      originalToolName: tool,
      prefixedToolName: `mcp__any_alias__${tool}`,
      args: { repo_slug: "core", pull_request_id: 3 },
      origin: "proxy",
      supportsDenyReason: true,
      claim() { claimed = true; return true; },
    });
    return claimed;
  };
  assert.equal(unclaimed("get_pull_request"), false);
  assert.equal(unclaimed("create_pull_request"), false);
  assert.equal(unclaimed("create_pull_request_comment"), false);
  assert.equal(unclaimed("approve_pull_request"), true);
  assert.equal(prompts.length, 0);
});

test("malformed or absent arguments are denied before adapter caches can grant access", async () => {
  const { events, handlers } = piFixture();
  startSession(handlers, context(async () => "Allow once"));
  const fixture = approvalFixture({ args: {} });
  fixture.state.approvalEvents = events;
  assert.equal(fixture.state.config.mcpServers.renamed.approveTools.includes("approve_pull_request"), true);
  assert.deepEqual(await approve(fixture, undefined), { ok: false, reason: "denied" });
  assert.deepEqual(await approve(fixture, null), { ok: false, reason: "denied" });
});

test("legacy workspace arguments fail closed before showing approval consent", async () => {
  const { events, handlers } = piFixture();
  let dialogs = 0;
  startSession(handlers, context(async () => { dialogs++; return "Allow once"; }));
  const fixture = approvalFixture();
  fixture.state.approvalEvents = events;
  assert.deepEqual(await approve(fixture, { ...fixture.args, workspace: "acme" }), { ok: false, reason: "denied" });
  assert.equal(dialogs, 0);
});

test("headless, canceled, and failed UI decisions deny despite valid adapter grants", async () => {
  const noUi = piFixture();
  const noUiFixture = approvalFixture();
  noUiFixture.state.approvalEvents = noUi.events;
  startSession(noUi.handlers, context(async () => "Allow once", { hasUI: false }));
  assert.deepEqual(await approve(noUiFixture), { ok: false, reason: "denied" });

  const canceled = piFixture();
  const canceledFixture = approvalFixture();
  canceledFixture.state.approvalEvents = canceled.events;
  startSession(canceled.handlers, context(async () => undefined));
  assert.deepEqual(await approve(canceledFixture), { ok: false, reason: "denied" });

  const broken = piFixture();
  const brokenFixture = approvalFixture();
  brokenFixture.state.approvalEvents = broken.events;
  startSession(broken.handlers, context(async () => { throw new Error("dialog failed"); }));
  assert.deepEqual(await approve(brokenFixture), { ok: false, reason: "denied" });
});

test("a canceled queued call settles while the first human dialog is still unresolved", async () => {
  const { events, handlers } = piFixture();
  const firstDialog = deferred();
  let dialogs = 0;
  startSession(handlers, context(() => { dialogs++; return firstDialog.promise; }));
  const first = approvalFixture();
  const second = approvalFixture();
  first.state.approvalEvents = events;
  second.state.approvalEvents = events;
  const firstCall = approve(first);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(dialogs, 1);

  const controller = new AbortController();
  const secondCall = approve(second, second.args, controller.signal).then(
    () => assert.fail("aborted request unexpectedly resolved"),
    (error) => { assert.equal(error.name, "AbortError"); return { ok: false, reason: "aborted" }; },
  );
  await new Promise((resolve) => setImmediate(resolve));
  controller.abort();
  assert.deepEqual(await Promise.race([secondCall, timeout(250)]), { ok: false, reason: "aborted" });
  assert.equal(dialogs, 1);

  firstDialog.resolve("Deny");
  assert.deepEqual(await firstCall, { ok: false, reason: "denied" });
});

test("session replacement gives new prompts a fresh queue and invalidates old decisions", async () => {
  const { events, handlers } = piFixture();
  const oldDialog = deferred();
  let newDialogCalls = 0;
  startSession(handlers, context(() => oldDialog.promise));
  const oldFixture = approvalFixture();
  oldFixture.state.approvalEvents = events;
  const oldCall = approve(oldFixture);
  await new Promise((resolve) => setImmediate(resolve));

  handlers.get("session_shutdown")();
  startSession(handlers, context(async () => { newDialogCalls++; return "Allow once"; }));
  const newFixture = approvalFixture();
  newFixture.state.approvalEvents = events;
  assert.deepEqual(await approve(newFixture), { ok: true });
  assert.equal(newDialogCalls, 1);
  oldDialog.resolve("Allow once");
  assert.deepEqual(await oldCall, { ok: false, reason: "denied" });
});

test("a vetoed session switch preserves consent for the following adapter call", async () => {
  const { events, handlers } = piFixture();
  let prompts = 0;
  startSession(handlers, context(async () => { prompts++; return "Allow once"; }));
  const fixture = approvalFixture();
  fixture.state.approvalEvents = events;

  handlers.get("session_before_switch")?.();
  assert.deepEqual(await approve(fixture), { ok: true });
  assert.equal(prompts, 1);
});

test("session shutdown invalidates an outstanding allow-once result", async () => {
  const { events, handlers } = piFixture();
  const dialog = deferred();
  startSession(handlers, context(() => dialog.promise));
  const fixture = approvalFixture();
  fixture.state.approvalEvents = events;
  const pending = approve(fixture);
  await new Promise((resolve) => setImmediate(resolve));
  handlers.get("session_shutdown")();
  dialog.resolve("Allow once");
  assert.deepEqual(await pending, { ok: false, reason: "denied" });
});

test("request cancellation while a dialog is open denies and releases the queue", async () => {
  const { events, handlers } = piFixture();
  const dialog = deferred();
  let dialogCalls = 0;
  startSession(handlers, context(() => { dialogCalls++; return dialog.promise; }));
  const fixture = approvalFixture();
  fixture.state.approvalEvents = events;
  const controller = new AbortController();
  const pending = approve(fixture, fixture.args, controller.signal).then(
    () => assert.fail("aborted request unexpectedly resolved"),
    (error) => { assert.equal(error.name, "AbortError"); return { ok: false, reason: "aborted" }; },
  );
  await new Promise((resolve) => setImmediate(resolve));
  controller.abort();
  assert.deepEqual(await Promise.race([pending, timeout(250)]), { ok: false, reason: "aborted" });

  const nextFixture = approvalFixture();
  nextFixture.state.approvalEvents = events;
  const next = approve(nextFixture);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(dialogCalls, 2);
  dialog.resolve("Allow once");
  assert.deepEqual(await next, { ok: true });
});
