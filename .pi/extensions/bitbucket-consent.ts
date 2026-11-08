import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import {
  MCP_TOOL_APPROVAL_REQUEST_EVENT,
  type McpToolApprovalHandler,
  type McpToolApprovalRequest,
} from "pi-mcp-adapter/types";

const actions = new Map([
  ["approve_pull_request", { label: "Approve pull request", title: "Approve pull request?" }],
  ["withdraw_pull_request_approval", { label: "Withdraw approval", title: "Withdraw pull request approval?" }],
]);
const choices = ["Allow once", "Deny"];
const segment = /^[A-Za-z0-9._~-]{1,128}$/;

type RawRequest = {
  originalToolName: string;
  serverName: unknown;
  args: unknown;
  signal?: unknown;
  claim: McpToolApprovalRequest["claim"];
};

type Target = { repo: string; pullRequestId: number };
type SessionState = {
  context: ExtensionContext;
  lifetime: AbortController;
  queue: Promise<void>;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isRawRequest(value: unknown): value is RawRequest {
  return isRecord(value)
    && typeof value.originalToolName === "string"
    && typeof value.claim === "function";
}

function safeLabel(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const cleaned = value.replace(/[\u0000-\u001f\u007f-\u009f]/g, "").slice(0, 128);
  return cleaned.length > 0 ? cleaned : undefined;
}

function parseTarget(args: unknown): Target | undefined {
  if (!isRecord(args)) return undefined;
  if (Object.hasOwn(args, "workspace")) return undefined;
  const { repo_slug: repo, pull_request_id: id } = args;
  if (typeof repo !== "string" || !segment.test(repo)) return undefined;
  if (typeof id !== "number" || !Number.isSafeInteger(id) || id <= 0) return undefined;
  return { repo, pullRequestId: id };
}

function abortable<T>(promise: Promise<T>, signal: AbortSignal): Promise<T | undefined> {
  if (signal.aborted) return Promise.resolve(undefined);
  return new Promise((resolve) => {
    const onAbort = () => resolve(undefined);
    signal.addEventListener("abort", onAbort, { once: true });
    promise.then(resolve, () => resolve(undefined)).finally(() => signal.removeEventListener("abort", onAbort));
  });
}

function combineSignals(first: AbortSignal, second?: AbortSignal): { signal: AbortSignal; dispose: () => void } {
  const controller = new AbortController();
  const abort = () => controller.abort();
  first.addEventListener("abort", abort, { once: true });
  second?.addEventListener("abort", abort, { once: true });
  if (first.aborted || second?.aborted) controller.abort();
  return {
    signal: controller.signal,
    dispose: () => {
      first.removeEventListener("abort", abort);
      second?.removeEventListener("abort", abort);
    },
  };
}

function isAbortSignal(value: unknown): value is AbortSignal | undefined {
  return value === undefined || value instanceof AbortSignal;
}

async function decide(
  state: SessionState,
  isCurrent: (state: SessionState) => boolean,
  toolName: string,
  serverValue: unknown,
  args: unknown,
  requestSignalValue: unknown,
): Promise<"allow_once" | "deny"> {
  if (!isCurrent(state) || state.lifetime.signal.aborted || !state.context.hasUI) return "deny";
  if (!isAbortSignal(requestSignalValue)) return "deny";
  const action = actions.get(toolName);
  const server = safeLabel(serverValue);
  const target = parseTarget(args);
  if (!action || !server || !target) return "deny";

  const combined = combineSignals(
    state.lifetime.signal,
    requestSignalValue,
  );
  const queued = state.queue.then(async () => {
    if (combined.signal.aborted || !isCurrent(state)) return "deny";
    const prompt = [
      `Action: ${action.label}`,
      `Server: ${server}`,
      `Repository: ${target.repo}`,
      `Pull request: #${target.pullRequestId}`,
    ].join("\n");
    const choice = await abortable(state.context.ui.select(
      `${action.title}\n\n${prompt}`,
      choices,
      { signal: combined.signal },
    ), combined.signal);
    return choice === "Allow once" && !combined.signal.aborted && isCurrent(state)
      ? "allow_once"
      : "deny";
  });
  state.queue = queued.then(() => undefined, () => undefined);
  try {
    return await abortable(queued, combined.signal) ?? "deny";
  } catch {
    return "deny";
  } finally {
    combined.dispose();
  }
}

export default function registerBitbucketConsent(pi: ExtensionAPI): void {
  let current: SessionState | undefined;
  const isCurrent = (state: SessionState) => current === state;

  pi.on("session_start", (_event, context) => {
    current?.lifetime.abort();
    current = { context, lifetime: new AbortController(), queue: Promise.resolve() };
  });
  pi.on("session_shutdown", () => {
    current?.lifetime.abort();
    current = undefined;
  });

  pi.events.on(MCP_TOOL_APPROVAL_REQUEST_EVENT, (event: unknown) => {
    if (!isRawRequest(event)) return;
    if (!actions.has(event.originalToolName)) return;
    const state = current;
    const handler: McpToolApprovalHandler = () => state
      ? decide(state, isCurrent, event.originalToolName, event.serverName, event.args, event.signal)
      : "deny";
    event.claim(handler);
  });
}
