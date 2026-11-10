// Shared plumbing for the live Paseo/jcode v1 acceptance harness.
//
// Every ACP or daemon byte in these harnesses is produced by the real jcode
// binary. Only the model backend is scripted: a local OpenAI-compatible HTTP
// server whose responses the harness releases deliberately, so turn order is
// deterministic and no provider tokens are spent.
import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { connect, type Socket } from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";
import { createInterface } from "node:readline";

export const SKILL_MARKER = "LIVE_ACCEPTANCE_SKILL_MARKER_7c1e";
export const DEFAULT_MODEL = "gpt-5.2";
export const ALT_MODEL = "gpt-5.2-mini";

export const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

export async function waitFor<T>(
  label: string,
  probe: () => T | undefined | null | false | Promise<T | undefined | null | false>,
  timeoutMs = 20_000,
  intervalMs = 50,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  let last: unknown;
  while (Date.now() < deadline) {
    try {
      const value = await probe();
      if (value) return value as T;
      last = value;
    } catch (error) {
      last = error;
    }
    await sleep(intervalMs);
  }
  throw new Error(
    `timed out after ${timeoutMs}ms waiting for ${label}; last=${safeJson(last).slice(0, 800)}`,
  );
}

export function safeJson(value: unknown): string {
  if (value === undefined) return "undefined";
  try {
    return JSON.stringify(value) ?? String(value);
  } catch {
    return String(value);
  }
}

/// The daemon wire carries turn identity as `{runtime_generation, turn_id}`
/// while the ACP adapter renders the same identity as `"<generation>:<id>"`.
/// Compare the two by a single normalized key, never by raw shape.
export function turnIdentityKey(identity: unknown): string {
  if (typeof identity === "string") return identity;
  if (identity && typeof identity === "object") {
    const value = identity as { runtime_generation?: unknown; turn_id?: unknown };
    if (value.runtime_generation !== undefined && value.turn_id !== undefined) {
      return `${String(value.runtime_generation)}:${String(value.turn_id)}`;
    }
  }
  return safeJson(identity);
}

export function messageText(content: unknown): string {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content
      .map((part) =>
        typeof part === "string"
          ? part
          : typeof part === "object" && part
            ? String((part as Record<string, unknown>).text ?? "")
            : "",
      )
      .join("");
  }
  return "";
}

export function bodyText(body: unknown): string {
  const messages = (body as { messages?: Array<{ content?: unknown }> } | undefined)?.messages;
  return Array.isArray(messages) ? messages.map((m) => messageText(m.content)).join("\n") : "";
}

export function lastUserText(body: unknown): string {
  const messages = (body as { messages?: Array<{ role?: string; content?: unknown }> } | undefined)
    ?.messages;
  if (!Array.isArray(messages)) return "";
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    if (messages[index].role === "user") return messageText(messages[index].content);
  }
  return "";
}

/** Finds a nested key/value that looks like an effort setting. Returns [key, value]. */
export function findEffort(value: unknown, depth = 0): [string, string] | null {
  if (depth > 6 || value === null || typeof value !== "object") return null;
  for (const [key, child] of Object.entries(value as Record<string, unknown>)) {
    if (
      typeof child === "string" &&
      /effort|reasoning|verbosity/i.test(key) &&
      child.length > 0
    ) {
      return [key, child];
    }
  }
  for (const child of Object.values(value as Record<string, unknown>)) {
    const found = findEffort(child, depth + 1);
    if (found) return found;
  }
  return null;
}

export interface ModelRequest {
  body: Record<string, unknown>;
  response: ServerResponse;
  /** True once a reply/abort has been written. */
  settled: boolean;
}

export interface MockUsage {
  promptTokens: number;
  completionTokens: number;
  cachedTokens?: number;
}

export interface MockModel {
  baseUrl: string;
  requests: ModelRequest[];
  pending: ModelRequest[];
  close(): Promise<void>;
  takeContaining(text: string, timeoutMs?: number): Promise<ModelRequest>;
  replyText(request: ModelRequest, text: string, usage?: MockUsage): void;
  replyToolCall(request: ModelRequest, options: {
    id: string;
    name: string;
    arguments: Record<string, unknown>;
  }): void;
  abort(request: ModelRequest): void;
}

function sse(res: ServerResponse, events: Array<Record<string, unknown> | "[DONE]">) {
  res.writeHead(200, {
    "Content-Type": "text/event-stream",
    "Cache-Control": "no-cache",
    Connection: "keep-alive",
  });
  for (const event of events) {
    res.write(`data: ${typeof event === "string" ? event : JSON.stringify(event)}\n\n`);
  }
  res.end();
}

function chunk(
  model: string,
  delta: Record<string, unknown>,
  finishReason: string | null,
  usage?: MockUsage,
): Record<string, unknown> {
  const frame: Record<string, unknown> = {
    id: "live-acceptance",
    object: "chat.completion.chunk",
    model,
    choices: [{ index: 0, delta, finish_reason: finishReason }],
  };
  if (usage) {
    frame.usage = {
      prompt_tokens: usage.promptTokens,
      completion_tokens: usage.completionTokens,
      total_tokens: usage.promptTokens + usage.completionTokens,
      ...(usage.cachedTokens === undefined
        ? {}
        : { prompt_tokens_details: { cached_tokens: usage.cachedTokens } }),
    };
  }
  return frame;
}

export async function startMockModel(
  models: string[] = [DEFAULT_MODEL, ALT_MODEL],
): Promise<MockModel> {
  const requests: ModelRequest[] = [];
  const pending: ModelRequest[] = [];

  const handler = async (request: IncomingMessage, response: ServerResponse) => {
    let raw = "";
    for await (const part of request) raw += part;
    const url = request.url ?? "";
    if (url.includes("/models")) {
      response.setHeader("Content-Type", "application/json");
      response.end(
        JSON.stringify({ object: "list", data: models.map((id) => ({ id, object: "model" })) }),
      );
      return;
    }
    let body: Record<string, unknown> = {};
    try {
      body = raw ? (JSON.parse(raw) as Record<string, unknown>) : {};
    } catch {
      body = {};
    }
    const model = typeof body.model === "string" ? body.model : DEFAULT_MODEL;
    const tools = Array.isArray(body.tools) ? body.tools : [];
    // jcode also asks for background session summaries. Those tool-free
    // completions must not consume a foreground turn gate.
    if (tools.length === 0) {
      if (body.stream === false) {
        response.setHeader("Content-Type", "application/json");
        response.end(
          JSON.stringify({
            id: "auxiliary",
            object: "chat.completion",
            model,
            choices: [
              { index: 0, message: { role: "assistant", content: "E2E session" }, finish_reason: "stop" },
            ],
          }),
        );
      } else {
        sse(response, [
          chunk(model, { role: "assistant", content: "E2E session" }, null),
          chunk(model, {}, "stop"),
          "[DONE]",
        ]);
      }
      return;
    }
    const entry: ModelRequest = { body, response, settled: false };
    requests.push(entry);
    pending.push(entry);
  };

  const server: Server = createServer((request, response) => {
    void handler(request, response).catch((error) => {
      try {
        response.destroy(error instanceof Error ? error : undefined);
      } catch {
        // already gone
      }
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("mock model has no TCP port");

  const take = (entry: ModelRequest) => {
    entry.settled = true;
    const index = pending.indexOf(entry);
    if (index >= 0) pending.splice(index, 1);
  };

  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
    requests,
    pending,
    async close() {
      for (const entry of pending.splice(0)) {
        try {
          entry.response.destroy();
        } catch {
          // already gone
        }
      }
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
    async takeContaining(text, timeoutMs = 20_000) {
      return waitFor(
        `model request containing ${JSON.stringify(text)}`,
        () => pending.find((entry) => bodyText(entry.body).includes(text)),
        timeoutMs,
      );
    },
    replyText(entry, text, usage) {
      if (entry.settled) throw new Error("model request already settled");
      take(entry);
      const model = typeof entry.body.model === "string" ? entry.body.model : DEFAULT_MODEL;
      sse(entry.response, [
        chunk(model, { role: "assistant", content: text }, null),
        chunk(model, {}, "stop", usage),
        "[DONE]",
      ]);
    },
    replyToolCall(entry, options) {
      if (entry.settled) throw new Error("model request already settled");
      take(entry);
      const model = typeof entry.body.model === "string" ? entry.body.model : DEFAULT_MODEL;
      sse(entry.response, [
        chunk(
          model,
          {
            role: "assistant",
            tool_calls: [
              {
                index: 0,
                id: options.id,
                type: "function",
                function: { name: options.name, arguments: JSON.stringify(options.arguments) },
              },
            ],
          },
          null,
        ),
        chunk(model, {}, "tool_calls"),
        "[DONE]",
      ]);
    },
    abort(entry) {
      if (entry.settled) return;
      take(entry);
      try {
        entry.response.destroy();
      } catch {
        // already gone
      }
    },
  };
}

export interface LiveWorkspace {
  root: string;
  cwd: string;
  home: string;
  runtime: string;
  socketPath: string;
  binDir: string;
  cleanup(): Promise<void>;
}

export async function createWorkspace(prefix = "jcode-live-"): Promise<LiveWorkspace> {
  const root = await mkdtemp(path.join(tmpdir(), prefix));
  const cwd = path.join(root, "work");
  const home = path.join(root, "home");
  const runtime = path.join(root, "runtime");
  const binDir = path.join(root, "bin");
  await Promise.all([mkdir(cwd, { recursive: true }), mkdir(home, { recursive: true }), mkdir(runtime, { recursive: true }), mkdir(binDir, { recursive: true })]);
  const skillDir = path.join(cwd, ".jcode", "skills", "acceptance-skill");
  await mkdir(skillDir, { recursive: true });
  await writeFile(
    path.join(skillDir, "SKILL.md"),
    `---\nname: acceptance-skill\ndescription: Live acceptance probe skill.\n---\n\nWhen this skill is active, include the marker ${SKILL_MARKER} in your answer.\n`,
    "utf8",
  );
  return {
    root,
    cwd,
    home,
    runtime,
    socketPath: path.join(root, "jcode.sock"),
    binDir,
    async cleanup() {
      await rm(root, { recursive: true, force: true });
    },
  };
}

export interface JcodeDaemon {
  child: ChildProcess;
  socketPath: string;
  logs: string[];
  env: NodeJS.ProcessEnv;
  stop(): Promise<void>;
}

export function daemonEnv(workspace: LiveWorkspace, modelBaseUrl: string, model = DEFAULT_MODEL): NodeJS.ProcessEnv {
  return {
    ...process.env,
    HOME: process.env.HOME,
    JCODE_NO_TELEMETRY: "1",
    JCODE_HOME: workspace.home,
    JCODE_RUNTIME_DIR: workspace.runtime,
    JCODE_SOCKET: workspace.socketPath,
    JCODE_OPENAI_COMPAT_API_BASE: modelBaseUrl,
    JCODE_OPENAI_COMPAT_DEFAULT_MODEL: model,
    OPENAI_COMPAT_API_KEY: "isolated-live-acceptance-key",
    PATH: `${workspace.binDir}${path.delimiter}${process.env.PATH ?? ""}`,
  };
}

export async function startJcodeDaemon(options: {
  binary: string;
  workspace: LiveWorkspace;
  modelBaseUrl: string;
  model?: string;
}): Promise<JcodeDaemon> {
  const env = daemonEnv(options.workspace, options.modelBaseUrl, options.model);
  const logs: string[] = [];
  const child = spawn(
    options.binary,
    [
      "--socket",
      options.workspace.socketPath,
      "--provider",
      "openai-compatible",
      "--model",
      options.model ?? DEFAULT_MODEL,
      "serve",
    ],
    { cwd: options.workspace.cwd, env, stdio: ["ignore", "pipe", "pipe"] },
  );
  child.stdout?.on("data", (data) => logs.push(String(data)));
  child.stderr?.on("data", (data) => logs.push(String(data)));
  await waitFor(
    "jcode daemon socket",
    () => {
      if (child.exitCode !== null) {
        throw new Error(`jcode daemon exited ${child.exitCode}\n${logs.join("").slice(-4000)}`);
      }
      return existsSync(options.workspace.socketPath);
    },
    30_000,
  );
  return {
    child,
    socketPath: options.workspace.socketPath,
    logs,
    env,
    async stop() {
      if (child.exitCode !== null || child.signalCode !== null) return;
      const closed = new Promise<void>((resolve) => child.once("close", () => resolve()));
      child.kill("SIGTERM");
      const settled = await Promise.race([
        closed.then(() => true),
        sleep(3_000).then(() => false),
      ]);
      if (!settled) {
        child.kill("SIGKILL");
        await Promise.race([closed, sleep(2_000)]);
      }
    },
  };
}

export interface PeerEvent {
  [key: string]: any;
}

export interface PeerStateSnapshot {
  id: number;
  sessionId: string;
  isProcessing: boolean;
  pendingSoftInterrupts: number;
}

/// Harness-only inbound delivery gate. Holding a predicate buffers matching
/// raw peer lines, so a regression test can reproduce delivery inversion (for
/// example a scoped terminal that the consumer has already moved past but the
/// raw attachment has not delivered yet) without touching the daemon. Scenarios
/// never hold the gate; they use the positive barriers below.
export interface PeerGate {
  hold(predicate: (event: PeerEvent) => boolean): void;
  /// Delivers buffered lines and stops holding. Returns how many were buffered.
  release(): number;
  buffered(): number;
  holding(): boolean;
}

export interface LivePeer {
  socket: Socket;
  events: PeerEvent[];
  send(message: Record<string, unknown>): void;
  subscribe(workingDir: string, clientInstanceId: string, targetSessionId?: string): Promise<PeerEvent>;
  message(id: number, content: string): void;
  softInterrupt(id: number, content: string, urgent?: boolean, clientId?: string): void;
  cancel(id: number): void;
  /// Protocol request that mutates shared config and fans out to other attachments.
  setReasoningEffort(id: number, effort: string): void;
  setModel(id: number, model: string): void;
  /// `{type:"state"}` round-trip. `pendingSoftInterrupts` is the daemon's
  /// authoritative admission count: a steer that no attachment has handed to
  /// the session queue yet is not in it.
  state(timeoutMs?: number): Promise<PeerStateSnapshot>;
  /// The daemon's `{type:"ack", id}` for a soft interrupt this peer sent.
  ackFor(id: number): PeerEvent | undefined;
  gate: PeerGate;
  destroy(): void;
  turnStarts(): PeerEvent[];
  turnEnds(end: "completed" | "cancelled" | "failed"): PeerEvent[];
  identityFor(content: string): string | undefined;
}

export async function connectPeer(socketPath: string): Promise<LivePeer> {
  const socket = connect(socketPath);
  await new Promise<void>((resolve, reject) => {
    socket.once("connect", resolve);
    socket.once("error", reject);
  });
  const events: PeerEvent[] = [];
  const gateState: {
    predicate: ((event: PeerEvent) => boolean) | null;
    buffered: string[];
  } = { predicate: null, buffered: [] };
  const gate: PeerGate = {
    hold(predicate) {
      gateState.predicate = predicate;
    },
    release() {
      gateState.predicate = null;
      const buffered = gateState.buffered.splice(0);
      for (const line of buffered) events.push(parsePeerLine(line));
      return buffered.length;
    },
    buffered() {
      return gateState.buffered.length;
    },
    holding() {
      return gateState.predicate !== null;
    },
  };
  function parsePeerLine(line: string): PeerEvent {
    try {
      return JSON.parse(line) as PeerEvent;
    } catch {
      return { type: "unparsed", line };
    }
  }
  const lines = createInterface({ input: socket });
  lines.on("line", (line) => {
    const event = parsePeerLine(line);
    if (gateState.predicate?.(event)) {
      gateState.buffered.push(line);
      return;
    }
    events.push(event);
  });
  let requestId = 1000;
  const peer: LivePeer = {
    socket,
    events,
    send(message) {
      socket.write(`${JSON.stringify(message)}\n`);
    },
    async subscribe(workingDir, clientInstanceId, targetSessionId) {
      peer.send({
        type: "subscribe",
        id: 1,
        supports_turn_identity: true,
        working_dir: workingDir,
        client_instance_id: clientInstanceId,
        ...(targetSessionId ? { target_session_id: targetSessionId } : {}),
      });
      return waitFor(
        "peer session id",
        () => events.find((event) => event.type === "session"),
        15_000,
      );
    },
    message(id, content) {
      peer.send({ type: "message", id, content });
    },
    softInterrupt(id, content, urgent = true, clientId) {
      peer.send({
        type: "soft_interrupt",
        id,
        content,
        urgent,
        ...(clientId === undefined ? {} : { client_id: clientId }),
      });
    },
    cancel(id) {
      peer.send({ type: "cancel", id });
    },
    setReasoningEffort(id, effort) {
      peer.send({ type: "set_reasoning_effort", id, effort });
    },
    setModel(id, model) {
      peer.send({ type: "set_model", id, model });
    },
    async state(timeoutMs = 15_000) {
      const id = requestId++;
      peer.send({ type: "state", id });
      const reply = await waitFor(
        `peer state reply ${id}`,
        () => events.find((event) => event.type === "state" && event.id === id),
        timeoutMs,
        25,
      );
      return {
        id,
        sessionId: String(reply.session_id ?? ""),
        isProcessing: Boolean(reply.is_processing),
        pendingSoftInterrupts: Number(reply.pending_soft_interrupts ?? 0),
      };
    },
    ackFor(id) {
      return events.find((event) => event.type === "ack" && event.id === id);
    },
    gate,
    destroy() {
      lines.close();
      socket.destroy();
    },
    turnStarts() {
      return events.filter((event) => event.type === "turn_started");
    },
    turnEnds(end) {
      return events.filter((event) => event.type === "turn_event" && event.end === end);
    },
    identityFor(content) {
      return events.find(
        (event) => event.type === "turn_started" && event.user_content === content,
      )?.identity;
    },
  };
  return peer;
}

/// Turn-scoped raw-attachment views. Every barrier and assertion below reads
/// through these so one turn's terminal can never be answered by another
/// turn's event, a previous scenario's late terminal, or an unscoped reply.
export function peerTerminals(
  peer: LivePeer,
  identity: unknown,
  end: "completed" | "cancelled" | "failed",
): PeerEvent[] {
  const key = turnIdentityKey(identity);
  return peer
    .turnEnds(end)
    .filter((event) => turnIdentityKey(event.identity) === key);
}

export function peerTurnStarts(peer: LivePeer, identity: unknown): PeerEvent[] {
  const key = turnIdentityKey(identity);
  return peer
    .turnStarts()
    .filter((event) => turnIdentityKey(event.identity) === key);
}

/// Scoped `soft_interrupt_injected` events for one turn, optionally narrowed to
/// one steer text. This is the daemon's own statement that the steer entered
/// this execution rather than opening another one.
export function peerInjections(
  peer: LivePeer,
  identity: unknown,
  content?: string,
): any[] {
  const key = turnIdentityKey(identity);
  return peer.events
    .filter(
      (event) =>
        event.type === "turn_event" &&
        turnIdentityKey(event.identity) === key &&
        event.event?.type === "soft_interrupt_injected" &&
        (content === undefined || event.event?.content === content),
    )
    .map((event) => event.event);
}

/// Positive barrier: wait until the daemon itself has published this turn's
/// scoped terminal. A consumer-side idle flag does not imply the raw
/// attachment has received the terminal yet, so callers must not sample the
/// event list once after `waitIdle`.
export async function waitForPeerTerminal(
  peer: LivePeer,
  identity: unknown,
  end: "completed" | "cancelled" | "failed",
  timeoutMs = 15_000,
): Promise<PeerEvent[]> {
  await waitFor(
    `peer ${end} terminal for turn ${turnIdentityKey(identity)}`,
    () => {
      const matches = peerTerminals(peer, identity, end);
      return matches.length > 0 ? matches : undefined;
    },
    timeoutMs,
  );
  return peerTerminals(peer, identity, end);
}

/// Positive barrier for the injection witness, scoped to the exact turn.
export async function waitForPeerInjection(
  peer: LivePeer,
  identity: unknown,
  content: string,
  timeoutMs = 15_000,
): Promise<any> {
  return waitFor(
    `daemon injection of ${JSON.stringify(content)} into turn ${turnIdentityKey(identity)}`,
    () => peerInjections(peer, identity, content)[0],
    timeoutMs,
  );
}

export interface AdmissionWitness {
  state: PeerStateSnapshot;
  /// Present when the caller sent the steer itself and supplied `ackId`.
  ack: PeerEvent | null;
}

/// Positively witness daemon admission of a steer. Admission means the session
/// queue holds it (`pending_soft_interrupts`) and, for a steer this peer sent,
/// the daemon acknowledged that request id. An ACP prompt response, a timeline
/// echo, or consumer idle is not admission.
export async function waitForPeerAdmission(
  peer: LivePeer,
  options: { ackId?: number; minPending?: number; label?: string; timeoutMs?: number } = {},
): Promise<AdmissionWitness> {
  const minPending = options.minPending ?? 1;
  const label = options.label ?? "steer";
  return waitFor(
    `daemon admission of ${label}`,
    async () => {
      const state = await peer.state();
      if (state.pendingSoftInterrupts < minPending) return undefined;
      const ack = options.ackId === undefined ? null : peer.ackFor(options.ackId) ?? null;
      if (options.ackId !== undefined && ack === null) return undefined;
      return { state, ack };
    },
    options.timeoutMs ?? 15_000,
    25,
  );
}

/// The causal order the repaired steering scenarios require: the held origin
/// model call is released only after the daemon admission promise resolves.
/// `inspect` runs while the origin is still held, so a caller can assert that
/// the steer did not open a second execution. There is no sleep, retry, or
/// timeout-to-green path here: a missing admission keeps the origin held and
/// fails the scenario.
export async function holdModelUntilAdmitted<T>(options: {
  admission: () => Promise<T>;
  release: () => void;
  inspect?: (witness: T) => void;
  label?: string;
}): Promise<T> {
  let witness: T;
  try {
    witness = await options.admission();
  } catch (error) {
    throw new Error(
      `held ${options.label ?? "origin"} model call was not released: admission never arrived (${error instanceof Error ? error.message : String(error)})`,
    );
  }
  if (witness === undefined || witness === null || (witness as unknown) === false) {
    throw new Error(
      `held ${options.label ?? "origin"} model call was not released: admission produced no witness`,
    );
  }
  options.inspect?.(witness);
  options.release();
  return witness;
}

export function binaryProvenance(binary: string) {
  const version = spawnSync(binary, ["--version"], { encoding: "utf8" });
  const digest = spawnSync("sha256sum", [binary], { encoding: "utf8" });
  return {
    binary,
    resolved: existsSync(binary) ? binary : null,
    version: (version.stdout ?? "").trim() || (version.stderr ?? "").trim(),
    sha256: (digest.stdout ?? "").split(/\s+/)[0] ?? null,
    stat: version.status,
  };
}

export function linkBinary(binary: string, binDir: string, name = "jcode"): string {
  const target = path.join(binDir, name);
  spawnSync("ln", ["-sf", binary, target]);
  return target;
}

export interface Evidence {
  startedAt: string;
  provenance: Record<string, unknown>;
  environment: Record<string, unknown>;
  scenarios: Record<string, { status: "pass" | "fail" | "skip"; detail?: unknown; error?: string }>;
  notes: string[];
}

export function newEvidence(): Evidence {
  return {
    startedAt: new Date().toISOString(),
    provenance: {},
    environment: {},
    scenarios: {},
    notes: [],
  };
}

export function recordScenario(
  evidence: Evidence,
  name: string,
  status: "pass" | "fail" | "skip",
  payload?: { detail?: unknown; error?: unknown },
) {
  evidence.scenarios[name] = {
    status,
    ...(payload?.detail !== undefined ? { detail: payload.detail } : {}),
    ...(payload?.error !== undefined ? { error: String(payload.error) } : {}),
  };
}

export async function runScenario(
  evidence: Evidence,
  name: string,
  body: () => Promise<unknown>,
): Promise<boolean> {
  const selected = (process.env.JCODE_ACCEPTANCE_SCENARIOS ?? "")
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean);
  if (selected.length > 0 && !selected.some((entry) => name === entry || name.startsWith(entry))) {
    recordScenario(evidence, name, "skip");
    persistIncremental(evidence);
    return true;
  }
  try {
    const detail = await body();
    recordScenario(evidence, name, "pass", { detail });
    persistIncremental(evidence);
    return true;
  } catch (error) {
    const message =
      error instanceof Error
        ? error.stack ?? error.message
        : `${safeJson(error)}${(error as { code?: number })?.code !== undefined ? ` code=${(error as { code?: number }).code}` : ""}`;
    recordScenario(evidence, name, "fail", { error: message });
    // Fail fast on the console as well: CI keeps the job log even when the
    // evidence file on the runner is discarded.
    console.error(boundedFailureLine(name, message));
    persistIncremental(evidence);
    return false;
  }
}

/// Bounded so a stack repeated across scenarios cannot flood a CI log.
export function boundedFailureLine(name: string, raw: unknown, limit = 1200): string {
  const text = String(raw ?? "failed without an error")
    .replace(/\s+/g, " ")
    .trim();
  const suffix = text.length > limit ? ` … [${text.length - limit} chars truncated]` : "";
  return `FAILED ${name}: ${text.slice(0, limit)}${suffix}`;
}

/// One line per failure plus one line for skips, with a total budget.
export function failureLines(evidence: Evidence, perError = 1200, total = 6000): string[] {
  const failed = Object.entries(evidence.scenarios).filter(([, value]) => value.status === "fail");
  const skipped = Object.entries(evidence.scenarios)
    .filter(([, value]) => value.status === "skip")
    .map(([name]) => name);
  const lines: string[] = [];
  let budget = total;
  let emitted = 0;
  for (const [name, value] of failed) {
    if (budget <= 0) break;
    const limit = Math.max(160, Math.min(perError, budget));
    const line = boundedFailureLine(name, value.error, limit);
    lines.push(line);
    budget -= line.length;
    emitted += 1;
  }
  if (emitted < failed.length) {
    lines.push(`... ${failed.length - emitted} more failed scenarios were truncated`);
  }
  if (skipped.length > 0) lines.push(`SKIPPED ${skipped.join(", ")}`);
  return lines;
}

/// Keep per-scenario evidence durable even when a later floating promise or
/// shutdown path kills the process before the final write.
function persistIncremental(evidence: Evidence) {
  const file = process.env.JCODE_ACCEPTANCE_OUT;
  if (!file) return;
  try {
    writeEvidence(file, evidence);
  } catch {
    // Evidence must never change scenario behavior.
  }
}

export function writeEvidence(file: string, evidence: Evidence) {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
}

export function summaryLine(evidence: Evidence): string {
  const entries = Object.entries(evidence.scenarios);
  const passed = entries.filter(([, value]) => value.status === "pass").length;
  const failed = entries.filter(([, value]) => value.status === "fail").length;
  const skipped = entries.filter(([, value]) => value.status === "skip").length;
  return `${passed} passed, ${failed} failed, ${skipped} skipped`;
}

export function requireBinary(): string {
  const binary = process.env.JCODE_E2E_BINARY;
  if (!binary || !path.isAbsolute(binary)) {
    throw new Error(
      "Set JCODE_E2E_BINARY to an absolute, current jcode executable (real build, not target/debug from Nov 9)",
    );
  }
  if (!existsSync(binary)) throw new Error(`JCODE_E2E_BINARY does not exist: ${binary}`);
  return binary;
}
