// Live v1 acceptance through the real Paseo ACP connector.
//
// The consumer modules are imported from the consumer-owned checkout
// (PASEO_CONSUMER_ROOT, supplied by run-live-acceptance.sh). No
// consumer source is copied or stubbed: the real Paseo daemon creates the real
// `jcode acp` child and this harness drives it exactly as the app does.
//
// The jcode side is a real binary on an isolated socket/home. Only the model is
// scripted (local OpenAI-compatible HTTP server) so turn order is deterministic.
import path from "node:path";
import { createRequire } from "node:module";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";
import {
  ALT_MODEL,
  DEFAULT_MODEL,
  SKILL_MARKER,
  binaryProvenance,
  bodyText,
  connectPeer,
  createWorkspace,
  failureLines,
  findEffort,
  holdModelUntilAdmitted,
  lastUserText,
  linkBinary,
  newEvidence,
  peerTerminals,
  peerTurnStarts,
  requireBinary,
  runScenario,
  safeJson,
  startJcodeDaemon,
  startMockModel,
  summaryLine,
  turnIdentityKey,
  waitFor,
  waitForPeerAdmission,
  waitForPeerInjection,
  waitForPeerTerminal,
  writeEvidence,
  type JcodeDaemon,
  type LivePeer,
  type MockModel,
} from "./jcode-live-common.ts";

const CONSUMER_ROOT =
  process.env.PASEO_CONSUMER_ROOT ?? process.cwd();

async function loadConsumer(relativePath: string): Promise<any> {
  return import(pathToFileURL(path.join(CONSUMER_ROOT, relativePath)).href);
}

function gitValue(root: string, args: string[]): string {
  const result = spawnSync("git", ["-C", root, ...args], { encoding: "utf8" });
  return (result.stdout ?? "").trim();
}

function consumerWorktreeDigest(): string {
  const digest = createHash("sha256");
  digest.update(gitValue(CONSUMER_ROOT, ["diff", "HEAD"]));
  digest.update(gitValue(CONSUMER_ROOT, ["status", "--porcelain"]));
  const untracked = gitValue(CONSUMER_ROOT, ["ls-files", "--others", "--exclude-standard"])
    .split("\n")
    .filter(Boolean)
    .sort();
  for (const file of untracked) {
    digest.update(file);
    try {
      digest.update(readFileSync(path.join(CONSUMER_ROOT, file)));
    } catch {
      digest.update("<unreadable>");
    }
  }
  return digest.digest("hex");
}

const CONSUMER_SOURCE_FILES = [
  "packages/plugin/src/server/acp-internal/connection.ts",
  "packages/plugin/src/server/acp.ts",
  "packages/plugin/src/server/provider.ts",
  "packages/server/src/server/session.ts",
  "packages/server/src/server/agent/agent-manager.ts",
  "packages/server/src/server/agent/plugin-provider.ts",
  "packages/server/src/server/agent/provider-registry.ts",
  "packages/protocol/src/messages.ts",
  "plugins/jcode-provider/index.server.ts",
];

function consumerSourceDigest(): string {
  const digest = createHash("sha256");
  for (const file of CONSUMER_SOURCE_FILES) {
    digest.update(file);
    try {
      digest.update(readFileSync(path.join(CONSUMER_ROOT, file)));
    } catch {
      digest.update("<missing>");
    }
  }
  return digest.digest("hex");
}

function consumerProvenance() {
  const require = createRequire(path.join(CONSUMER_ROOT, "package.json"));
  let pluginEntry: string | null = null;
  try {
    pluginEntry = require.resolve("@getpaseo/plugin/server");
  } catch {
    pluginEntry = null;
  }
  return {
    root: CONSUMER_ROOT,
    head: gitValue(CONSUMER_ROOT, ["rev-parse", "HEAD"]),
    tree: gitValue(CONSUMER_ROOT, ["rev-parse", "HEAD^{tree}"]),
    worktreeSha256: consumerWorktreeDigest(),
    sourceSha256: consumerSourceDigest(),
    sourceFiles: CONSUMER_SOURCE_FILES,
    branch: gitValue(CONSUMER_ROOT, ["branch", "--show-current"]),
    dirty: gitValue(CONSUMER_ROOT, ["status", "--porcelain"]).split("\n").filter(Boolean).length,
    // Record the module the daemon actually imported. Under the default
    // conditions this is compiled dist, which can lag the checkout; the
    // runner passes --conditions=source so the true source tree is exercised.
    pluginEntry,
    nodeConditions: process.execArgv.filter((arg) => arg.includes("conditions")),
    pluginDist: path.join(CONSUMER_ROOT, "packages/plugin/dist/server/acp-internal/connection.js"),
    pluginSource: path.join(CONSUMER_ROOT, "packages/plugin/src/server/acp-internal/connection.ts"),
  };
}

function userTexts(entries: Array<{ item: any }>): string[] {
  return entries
    .map((entry) => entry.item)
    .filter((item) => item?.type === "user_message")
    .map((item) => String(item.text ?? ""));
}

function peerTurnIdentities(peer: LivePeer) {
  return {
    started: peer.turnStarts().map((event) => ({ identity: event.identity, content: event.user_content, origin: event.origin })),
    completed: peer.turnEnds("completed").map((event) => event.identity),
    cancelled: peer.turnEnds("cancelled").map((event) => event.identity),
  };
}

function requestSummary(request: { body: Record<string, unknown> } | undefined) {
  if (!request) return null;
  return {
    model: request.body.model ?? null,
    effort: findEffort(request.body),
    stream: request.body.stream ?? null,
    hasTools: Array.isArray(request.body.tools) && request.body.tools.length > 0,
  };
}

async function main() {
  const evidence = newEvidence();
  evidence.provenance = {
    jcode: binaryProvenance(requireBinary()),
    consumer: consumerProvenance(),
  };
  evidence.environment = {
    node: process.version,
    platform: `${process.platform}/${process.arch}`,
    consumerRoot: CONSUMER_ROOT,
    isolatedWorkspacePrefix: "jcode-live-",
  };

  const binary = requireBinary();
  const workspace = await createWorkspace();
  const model = await startMockModel();
  linkBinary(binary, workspace.binDir);

  let daemon: JcodeDaemon | undefined;
  let peer: LivePeer | undefined;
  let paseo: any;
  let client: any;
  const restoreEnv: Array<() => void> = [];
  const setEnv = (key: string, value: string | undefined) => {
    const previous = process.env[key];
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
    restoreEnv.push(() => {
      if (previous === undefined) delete process.env[key];
      else process.env[key] = previous;
    });
  };

  try {
    daemon = await startJcodeDaemon({ binary, workspace, modelBaseUrl: model.baseUrl });
    peer = await connectPeer(daemon.socketPath);
    const session = await peer.subscribe(workspace.cwd, "acceptance-peer");
    const sessionId: string = session.session_id;
    evidence.environment.sessionId = sessionId;
    evidence.environment.isolated = {
      root: workspace.root,
      home: workspace.home,
      runtime: workspace.runtime,
      socket: workspace.socketPath,
    };

    // Seed the shared session from the peer (TUI stand-in), then reply.
    peer.message(2, "BASELINE");
    const baselineRequest = await model.takeContaining("BASELINE");
    model.replyText(baselineRequest, "BASELINE-ANSWER");
    await waitFor(
      "baseline peer turn completed",
      () => peer!.turnEnds("completed").length >= 1,
      15_000,
    );

    const { createTestPaseoDaemon } = await loadConsumer(
      "packages/server/src/server/test-utils/paseo-daemon.ts",
    );
    const { DaemonClient } = await loadConsumer(
      "packages/server/src/server/test-utils/daemon-client.ts",
    );

    // The daemon (in-process) passes its environment to `jcode acp`, so expose
    // the isolated jcode environment before the daemon starts.
    for (const [key, value] of Object.entries(daemon.env)) {
      if (key.startsWith("JCODE_") || key === "OPENAI_COMPAT_API_KEY" || key === "PATH") {
        setEnv(key, value === undefined ? undefined : String(value));
      }
    }
    setEnv("JCODE_SOCKET", workspace.socketPath);

    paseo = await createTestPaseoDaemon({
      pluginsEnabled: true,
      paseoHomeRoot: path.join(workspace.root, "paseo"),
    });
    client = new DaemonClient({ url: `ws://127.0.0.1:${paseo.port}/ws` });
    await client.connect();
    await client.fetchAgents({ subscribe: {} });
    await client.installDirectoryPlugin(path.join(CONSUMER_ROOT, "plugins/jcode-provider"));
    const imported = await client.importAgent({
      providerId: "jcode",
      providerHandleId: `plugin:${JSON.stringify({ version: 1, data: { sessionId } })}`,
      cwd: workspace.cwd,
    });
    let agentId: string = imported.id;

    const timeline = async () => {
      const page = await client.fetchAgentTimeline(agentId, {
        direction: "tail",
        limit: 0,
        projection: "canonical",
      });
      return page.entries as Array<{ item: any }>;
    };
    const agent = async () => (await client.fetchAgent({ agentId })).agent;
    const waitIdle = () =>
      waitFor("agent idle", async () => (await agent()).status === "idle", 30_000);
    const nextRequest = (text?: string) =>
      text ? model.takeContaining(text) : waitFor("next model request", () => model.pending[0], 20_000);
    const send = (text: string, options?: Record<string, unknown>) => {
      const promise: Promise<unknown> = client.sendMessage(agentId, text, options);
      // The harness releases the model response before awaiting the send.
      // Mark a rejection handled immediately so an early busy rejection cannot
      // kill the process before the scenario's await attaches.
      promise.catch(() => undefined);
      return promise;
    };

    // --- s01: real connector import and replay -----------------------------
    await runScenario(evidence, "s01-import-and-replay", async () => {
      const entries = await timeline();
      const texts = userTexts(entries);
      if (JSON.stringify(texts) !== JSON.stringify(["BASELINE"])) {
        throw new Error(`unexpected replayed user messages: ${safeJson(texts)}`);
      }
      const snapshot = await agent();
      const baselineIdentity = peer!.identityFor("BASELINE");
      if (!baselineIdentity) throw new Error("no daemon turn identity for BASELINE");
      return {
        replayedUserMessages: texts,
        baselineTurnIdentity: baselineIdentity,
        model: snapshot.model,
        capabilities: snapshot.capabilities,
        providerHandle: snapshot.persistence ?? null,
      };
    });

    // --- s02: forward steering through the connector -----------------------
    await runScenario(evidence, "s02-forward-steer", async () => {
      const ORIGIN = "PEER-TURN";
      const STEER = "FORWARD-STEER";
      peer!.message(20, ORIGIN);
      const peerRequest = await nextRequest(ORIGIN);
      const identity = await waitFor(
        "daemon identity for PEER-TURN",
        () => peer!.identityFor(ORIGIN),
        15_000,
      );
      if ((await peer!.state()).pendingSoftInterrupts !== 0) {
        throw new Error("a stray soft interrupt was already queued before the steer");
      }

      const steerPromise = send(STEER, {
        activeTurnBehavior: "steer",
      });
      // Keep the origin model call held until the daemon has actually admitted
      // the steer. Releasing as soon as the send returned would let the origin
      // finish first, so a consumer-side echo could be mistaken for steering.
      const admission = await holdModelUntilAdmitted({
        label: ORIGIN,
        admission: () => waitForPeerAdmission(peer!, { label: STEER }),
        inspect: () => {
          if (model.pending.length !== 1 || model.pending[0] !== peerRequest) {
            throw new Error(
              `the steer opened a second model call before admission: ${safeJson(
                model.pending.map((entry) => bodyText(entry.body).slice(0, 120)),
              )}`,
            );
          }
        },
        release: () => model.replyText(peerRequest, "PEER-INTERMEDIATE"),
      });
      await steerPromise;
      const steerRequest = await nextRequest(STEER);
      if (steerRequest === peerRequest) throw new Error("steer reused the origin model request");
      model.replyText(steerRequest, "FORWARD-ANSWER");
      await waitIdle();
      // Consumer idle is not the daemon terminal. Wait for this exact turn's
      // scoped terminal (and the scoped injection) before scoring anything.
      const completed = await waitForPeerTerminal(peer!, identity, "completed");
      const injection = await waitForPeerInjection(peer!, identity, STEER);

      const texts = userTexts(await timeline());
      const expected = ["BASELINE", ORIGIN, STEER];
      if (JSON.stringify(texts) !== JSON.stringify(expected)) {
        throw new Error(`unexpected user messages: ${safeJson(texts)}`);
      }
      const starts = peerTurnStarts(peer!, identity);
      const cancelled = peerTerminals(peer!, identity, "cancelled");
      if (starts.length !== 1) throw new Error(`expected one turn start, got ${starts.length}`);
      if (completed.length !== 1) throw new Error(`expected one completion, got ${completed.length}`);
      if (cancelled.length !== 0) throw new Error("forward steer cancelled the turn");
      const extraStarts = peer!.turnStarts().filter((event) => event.user_content === STEER);
      if (extraStarts.length !== 0) {
        throw new Error(
          `the steer opened a separate daemon turn: ${safeJson(extraStarts.map((event) => event.identity))}`,
        );
      }
      if (!Array.isArray(injection.client_ids) || injection.client_ids.length === 0) {
        throw new Error("the injected forward steer carried no client ids");
      }
      return {
        steerJoinedIdentity: identity,
        admission: {
          pendingSoftInterrupts: admission.state.pendingSoftInterrupts,
          originHeldUntilAdmitted: true,
        },
        turnStarts: starts.length,
        turnCompletions: completed.length,
        turnCancellations: cancelled.length,
        injection: {
          point: injection.point,
          clientIds: injection.client_ids.length,
          scopedToOrigin: true,
        },
        request: requestSummary(steerRequest),
        texts,
      };
    });

    // --- s03: reverse steering from the peer ------------------------------
    await runScenario(evidence, "s03-reverse-steer", async () => {
      const ORIGIN = "FOREGROUND";
      const STEER = "REVERSE-STEER";
      const STEER_REQUEST_ID = 21;
      const STEER_CLIENT_ID = "reverse-steer-21";
      const prompt = send(ORIGIN);
      const foregroundRequest = await nextRequest(ORIGIN);
      const identity = await waitFor(
        "daemon identity for the Paseo-origin turn",
        () => peer!.identityFor(ORIGIN),
        15_000,
      );
      if (!identity) throw new Error("no daemon identity for the Paseo-origin turn");
      if ((await peer!.state()).pendingSoftInterrupts !== 0) {
        throw new Error("a stray soft interrupt was already queued before the reverse steer");
      }

      peer!.softInterrupt(STEER_REQUEST_ID, STEER, false, STEER_CLIENT_ID);
      // The peer's own steer must be acknowledged and visible in the session
      // queue before the origin model call is released. A timeline echo alone
      // only proves the consumer displayed something.
      const admission = await holdModelUntilAdmitted({
        label: ORIGIN,
        admission: () =>
          waitForPeerAdmission(peer!, { ackId: STEER_REQUEST_ID, label: STEER }),
        inspect: (witness) => {
          if (!witness.ack) throw new Error(`no daemon ack for steer id ${STEER_REQUEST_ID}`);
          if (model.pending.length !== 1 || model.pending[0] !== foregroundRequest) {
            throw new Error(
              `the reverse steer opened a second model call before admission: ${safeJson(
                model.pending.map((entry) => bodyText(entry.body).slice(0, 120)),
              )}`,
            );
          }
        },
        release: () => model.replyText(foregroundRequest, "FOREGROUND-INTERMEDIATE"),
      });
      await prompt;
      await waitFor(
        "peer steer text in Paseo timeline",
        async () => userTexts(await timeline()).includes(STEER),
        15_000,
      );
      const steerRequest = await nextRequest(STEER);
      if (steerRequest === foregroundRequest) {
        throw new Error("steer reused the foreground model request");
      }
      model.replyText(steerRequest, "REVERSE-ANSWER");
      await waitIdle();
      const completed = await waitForPeerTerminal(peer!, identity, "completed");
      const injection = await waitForPeerInjection(peer!, identity, STEER);

      const texts = userTexts(await timeline());
      const expected = ["BASELINE", "PEER-TURN", "FORWARD-STEER", ORIGIN, STEER];
      if (JSON.stringify(texts) !== JSON.stringify(expected)) {
        throw new Error(`unexpected user messages: ${safeJson(texts)}`);
      }
      const starts = peerTurnStarts(peer!, identity);
      const cancelled = peerTerminals(peer!, identity, "cancelled");
      if (starts.length !== 1 || completed.length !== 1 || cancelled.length !== 0) {
        throw new Error(
          `turn terminal mismatch starts=${starts.length} completed=${completed.length} cancelled=${cancelled.length}`,
        );
      }
      const extraStarts = peer!.turnStarts().filter((event) => event.user_content === STEER);
      if (extraStarts.length !== 0) {
        throw new Error(
          `the reverse steer opened a separate daemon turn: ${safeJson(extraStarts.map((event) => event.identity))}`,
        );
      }
      if (!Array.isArray(injection.client_ids) || !injection.client_ids.includes(STEER_CLIENT_ID)) {
        throw new Error(
          `the injected reverse steer did not carry ${STEER_CLIENT_ID}: ${safeJson(injection.client_ids)}`,
        );
      }
      return {
        foregroundTurnIdentity: identity,
        admission: {
          pendingSoftInterrupts: admission.state.pendingSoftInterrupts,
          ackId: admission.ack?.id,
          originHeldUntilAdmitted: true,
        },
        injection: {
          point: injection.point,
          clientIds: injection.client_ids,
          scopedToOrigin: true,
        },
        texts,
        request: requestSummary(steerRequest),
      };
    });

    // --- s04: one execution, several prompt responses, settled once ------
    // The contract does not require the consumer to replace its internal
    // activeTurn id with the daemon identity. What it requires is that several
    // prompt responses naming one execution settle completion, display and
    // usage once. This drives an origin prompt and a concurrent steer into the
    // same execution while the model backend is deliberately held, so the
    // daemon evidence shows one turn identity taking the injected steer and
    // the consumer shows one visible terminal, one answer pair, and one usage
    // total covering both model calls.
    await runScenario(evidence, "s04-turn-correlation-single-settlement", async () => {
      const ORIGIN = "CORRELATED-TURN";
      const STEER = "CORRELATED-STEER";
      const originUsage = { promptTokens: 1000, completionTokens: 20 };
      const steerUsage = { promptTokens: 300, completionTokens: 7 };
      const expectedInput = originUsage.promptTokens + steerUsage.promptTokens;
      const expectedOutput = originUsage.completionTokens + steerUsage.completionTokens;
      const requestsBefore = model.requests.length;
      // A late idle from s03 can arrive after this subscription opens and
      // would otherwise become the first recorded transition. Establish the
      // causal baseline first: this execution's evidence starts at its own
      // running snapshot.
      const baseline = await agent();
      const baselineStatus = String(baseline.status ?? "");
      const baselineTurnId = baseline.activeTurn?.turnId ?? null;
      if (baselineStatus !== "idle" || baselineTurnId !== null) {
        throw new Error(
          `scenario did not start from an idle baseline: ${safeJson({ status: baselineStatus, activeTurnId: baselineTurnId })}`,
        );
      }
      const snapshots: Array<{
        lifecycle: string;
        activeTurnId: string | null;
        usage: Record<string, unknown> | null;
      }> = [];
      let executionObserved = false;
      let baselineSnapshotsIgnored = 0;
      const unsubscribe = paseo.daemon.agentManager.subscribe(
        (event: any) => {
          if (event.type !== "agent_state") return;
          if (event.agent.id !== agentId) return;
          const snapshot = {
            lifecycle: String(event.agent.lifecycle),
            activeTurnId: event.agent.activeTurnId ?? null,
            usage: event.agent.lastUsage ?? null,
          };
          if (!executionObserved) {
            // Leading non-running snapshots are baseline or a late idle from
            // the previous scenario, not this execution's transitions.
            if (snapshot.lifecycle !== "running") {
              baselineSnapshotsIgnored += 1;
              return;
            }
            executionObserved = true;
          }
          snapshots.push(snapshot);
        },
        { agentId, replayState: false },
      );
      try {
        const origin = send(ORIGIN);
        const originRequest = await nextRequest(ORIGIN);
        const daemonIdentity = await waitFor(
          "daemon identity for CORRELATED-TURN",
          () => peer!.identityFor(ORIGIN),
          15_000,
        );
        const during = await waitFor(
          "Paseo active turn during CORRELATED-TURN",
          async () => (await agent()).activeTurn,
          10_000,
        );

        // Queue the steer into the same execution while the origin's model
        // call is still held. Nothing has been answered yet.
        const steer = send(STEER, { activeTurnBehavior: "steer" });
        await waitFor(
          "Paseo accepted the steer into the active turn",
          async () => {
            const snapshot = await agent();
            const texts = userTexts(await timeline());
            return (
              snapshot.activeTurn?.turnId === during.turnId &&
              texts.includes(STEER) &&
              model.pending.length === 1 &&
              model.pending[0] === originRequest
            );
          },
          10_000,
        );

        // Paseo accepting the steer is not daemon admission. Keep the origin
        // model call held until the session queue holds the steer, then assert
        // it did not start an execution of its own.
        const admission = await holdModelUntilAdmitted({
          label: ORIGIN,
          admission: () => waitForPeerAdmission(peer!, { label: STEER }),
          inspect: () => {
            if (model.pending.length !== 1 || model.pending[0] !== originRequest) {
              throw new Error(
                `the steer opened a second model call before daemon admission: ${safeJson(
                  model.pending.map((entry) => bodyText(entry.body).slice(0, 120)),
                )}`,
              );
            }
          },
          release: () => model.replyText(originRequest, "CORRELATION-ANSWER", originUsage),
        });
        const steerRequest = await nextRequest(STEER);
        if (steerRequest === originRequest) {
          throw new Error("steer reused the origin model request");
        }
        // The first answer has streamed, but the execution is not terminal:
        // the injected steer still owes a model call and an answer.
        const midExecution = await agent();
        if (midExecution.status !== "running" || midExecution.activeTurn?.turnId !== during.turnId) {
          throw new Error(
            `Paseo settled after the first answer: ${safeJson({ status: midExecution.status, activeTurn: midExecution.activeTurn })}`,
          );
        }
        model.replyText(steerRequest, "STEER-ANSWER", steerUsage);
        await origin;
        await steer;
        await waitIdle();

        // Consumer idle is not the daemon terminal: wait for the exact shared
        // turn's scoped terminal and its scoped injection before scoring.
        const completed = await waitForPeerTerminal(peer!, daemonIdentity, "completed");
        const injection = await waitForPeerInjection(peer!, daemonIdentity, STEER);
        if (model.requests.length - requestsBefore !== 2) {
          throw new Error(
            `expected exactly two model calls in one execution, got ${model.requests.length - requestsBefore}`,
          );
        }
        if (completedFor(daemonIdentity) !== 1) {
          throw new Error("shared turn did not complete exactly once");
        }
        const otherStarts = peer!
          .turnStarts()
          .filter((event) => event.user_content === STEER);
        if (otherStarts.length !== 0) {
          throw new Error("the steer opened a second daemon turn");
        }
        if (!Array.isArray(injection.client_ids) || injection.client_ids.length === 0) {
          throw new Error("the injected steer carried no client ids");
        }

        const entries = await timeline();
        const answers = entries.filter(
          (entry) =>
            entry.item?.type === "assistant_message" &&
            (String(entry.item.text ?? "").includes("CORRELATION-ANSWER") ||
              String(entry.item.text ?? "").includes("STEER-ANSWER")),
        );
        const answerTexts = answers.map((entry) => String(entry.item.text ?? ""));
        const expectedAnswers = ["CORRELATION-ANSWER", "STEER-ANSWER"];
        if (JSON.stringify(answerTexts) !== JSON.stringify(expectedAnswers)) {
          throw new Error(`unexpected visible answers: ${safeJson(answerTexts)}`);
        }
        const premature = snapshots.filter(
          (entry) => entry.lifecycle !== "running" && entry.activeTurnId !== null,
        );
        if (premature.length > 0) {
          throw new Error(`Paseo kept a turn active outside running: ${safeJson(premature)}`);
        }
        const transitions = snapshots
          .map((entry) => entry.lifecycle)
          .filter((lifecycle, index, all) => index === 0 || lifecycle !== all[index - 1]);
        if (JSON.stringify(transitions) !== JSON.stringify(["running", "idle"])) {
          throw new Error(`unexpected Paseo lifecycle transitions: ${safeJson(transitions)}`);
        }
        const turnIds = Array.from(
          new Set(
            snapshots
              .map((entry) => entry.activeTurnId)
              .filter((turnId): turnId is string => turnId !== null),
          ),
        );
        if (turnIds.length !== 1 || turnIds[0] !== during.turnId) {
          throw new Error(`the execution used several local turn ids: ${safeJson(turnIds)}`);
        }
        const after = await agent();
        if (after.activeTurn) {
          throw new Error(`active turn survived completion: ${safeJson(after.activeTurn)}`);
        }
        const usage = after.lastUsage ?? null;
        if (
          usage?.inputTokens !== expectedInput ||
          usage?.outputTokens !== expectedOutput
        ) {
          throw new Error(
            `usage settled as ${safeJson(usage)}; expected ${expectedInput} input / ${expectedOutput} output once`,
          );
        }
        const doubled = snapshots.filter(
          (entry) =>
            entry.usage?.inputTokens === expectedInput * 2 ||
            entry.usage?.outputTokens === expectedOutput * 2,
        );
        if (doubled.length > 0) {
          throw new Error(`usage was applied twice: ${safeJson(doubled)}`);
        }
        return {
          paseoLocalTurnId: during.turnId,
          daemonTurnIdentity: daemonIdentity,
          daemonCompletions: completedFor(daemonIdentity),
          terminalBarrier: { completedEvents: completed.length },
          admission: {
            pendingSoftInterrupts: admission.state.pendingSoftInterrupts,
            originHeldUntilAdmitted: true,
          },
          baselineSnapshotsIgnored,
          modelCalls: model.requests.length - requestsBefore,
          injectedSteer: {
            point: injection.point,
            clientIds: injection.client_ids.length,
            scopedToOrigin: true,
          },
          visibleAnswers: answerTexts,
          lifecycle: snapshots,
          usage: {
            inputTokens: usage.inputTokens,
            outputTokens: usage.outputTokens,
            expectedInput,
            expectedOutput,
          },
          singleSettlement: true,
          peerTerminals: peerTurnIdentities(peer!),
        };
      } finally {
        unsubscribe();
      }
    });

    function completedFor(identity: string | undefined) {
      return peer!.turnEnds("completed").filter((event) => turnIdentityKey(event.identity) === turnIdentityKey(identity)).length;
    }

    // --- s05: Stop followed immediately by a new prompt -------------------
    await runScenario(evidence, "s05-stop-then-prompt", async () => {
      const stopping = send("STOPPED-PROMPT").catch((error) => {
        evidence.notes.push(`stop prompt rejected: ${String(error)}`);
      });
      const stopRequest = await nextRequest("STOPPED-PROMPT");
      model.replyToolCall(stopRequest, {
        id: "live-stop-tool",
        name: "bash",
        arguments: { command: "sleep 30" },
      });
      const stoppedIdentity = await waitFor(
        "stopped turn identity",
        () => peer!.identityFor("STOPPED-PROMPT"),
        15_000,
      );
      await waitFor(
        "tool execution for stopped turn",
        () =>
          peer!.events.some(
            (event) =>
              event.type === "turn_event" &&
              turnIdentityKey(event.identity) === turnIdentityKey(stoppedIdentity) &&
              event.event?.type === "tool_exec" &&
              event.event?.id === "live-stop-tool",
          ),
        10_000,
      );
      await client.cancelAgent(agentId);
      const successor = send("SUCCESSOR-PROMPT", {
        activeTurnBehavior: "interrupt",
      });
      const successorRequest = await nextRequest("SUCCESSOR-PROMPT");
      model.replyText(successorRequest, "SUCCESSOR-ANSWER");
      await successor;
      await waitIdle();
      const successorIdentity = await waitFor(
        "successor turn identity",
        () => peer!.identityFor("SUCCESSOR-PROMPT"),
        15_000,
      );
      const cancelled = peer!
        .turnEnds("cancelled")
        .map((event) => event.identity);
      const cancelledKeys = cancelled.map(turnIdentityKey);
      if (!cancelledKeys.includes(turnIdentityKey(stoppedIdentity))) {
        throw new Error(`stopped turn ${safeJson(stoppedIdentity)} was not cancelled; cancelled=${safeJson(cancelled)}`);
      }
      if (cancelledKeys.includes(turnIdentityKey(successorIdentity))) {
        throw new Error("a delayed stop cancelled the successor turn");
      }
      if (
        peer!
          .turnEnds("completed")
          .every((event) => turnIdentityKey(event.identity) !== turnIdentityKey(successorIdentity))
      ) {
        throw new Error("successor turn did not complete");
      }
      if (turnIdentityKey(successorIdentity) === turnIdentityKey(stoppedIdentity)) {
        throw new Error("successor reused the stopped turn identity");
      }
      void stopping;
      return {
        stoppedIdentity,
        successorIdentity,
        cancelled,
        successorCompleted: true,
        texts: userTexts(await timeline()),
      };
    });

    // --- s06: Stop an observed peer turn (stopObservedTurn) ---------------
    await runScenario(evidence, "s06-stop-observed-peer-turn", async () => {
      peer!.message(22, "PEER-STOP-TURN");
      const request = await nextRequest("PEER-STOP-TURN");
      model.replyToolCall(request, {
        id: "live-peer-stop-tool",
        name: "bash",
        arguments: { command: "sleep 30" },
      });
      const identity = await waitFor(
        "peer stop turn identity",
        () => peer!.identityFor("PEER-STOP-TURN"),
        15_000,
      );
      await waitFor(
        "peer tool execution",
        () =>
          peer!.events.some(
            (event) =>
              event.type === "turn_event" &&
              turnIdentityKey(event.identity) === turnIdentityKey(identity) &&
              event.event?.type === "tool_exec" &&
              event.event?.id === "live-peer-stop-tool",
          ),
        10_000,
      );
      await client.cancelAgent(agentId);
      await waitFor(
        "observed peer turn cancelled",
        () => peer!.turnEnds("cancelled").some((event) => turnIdentityKey(event.identity) === turnIdentityKey(identity)),
        10_000,
      );
      return {
        observedTurnIdentity: identity,
        cancelled: true,
        snapshotStatus: (await agent()).status,
      };
    });

    // --- s07: model and effort propagate both directions ------------------
    await runScenario(evidence, "s07-model-effort", async () => {
      await client.setAgentModel(agentId, ALT_MODEL);
      await waitFor(
        "peer model_changed",
        () => peer!.events.find((event) => event.type === "model_changed" && event.model === ALT_MODEL),
        10_000,
      );
      await client.setAgentThinkingOption(agentId, "high");
      await waitFor(
        "peer reasoning_effort_changed high",
        () =>
          peer!.events.find(
            (event) =>
              event.type === "reasoning_effort_changed" &&
              (event.effort === "high" || event.reasoning_effort === "high"),
          ),
        10_000,
      );
      const after = await agent();
      if (after.model !== ALT_MODEL) throw new Error(`Paseo model is ${safeJson(after.model)}`);
      const effortHigh = after.thinkingOptionId === "high" || after.effectiveThinkingOptionId === "high";
      if (!effortHigh) {
        throw new Error(
          `Paseo thinking option is ${safeJson({ thinkingOptionId: after.thinkingOptionId, effective: after.effectiveThinkingOptionId })}`,
        );
      }
      const prompt = send("EFFORT-MODEL-PROBE");
      const request = await nextRequest("EFFORT-MODEL-PROBE");
      const summary = requestSummary(request);
      model.replyText(request, "EFFORT-MODEL-ANSWER");
      await prompt;
      await waitIdle();
      if (summary?.model !== ALT_MODEL) {
        throw new Error(`provider request model ${safeJson(summary?.model)} != ${ALT_MODEL}`);
      }
      if (!summary?.effort || summary.effort[1] !== "high") {
        throw new Error(`provider request effort is ${safeJson(summary?.effort)}`);
      }

      // Reverse direction uses the real protocol setters, not a textual
      // `/effort` message: only the setters fan out to other attachments.
      peer!.setReasoningEffort(31, "low");
      const peerEffort = await waitFor(
        "peer set_reasoning_effort reply",
        () =>
          peer!.events.find(
            (event) =>
              event.type === "reasoning_effort_changed" &&
              event.id === 31 &&
              (event.effort === "low" || event.reasoning_effort === "low"),
          ),
        10_000,
      );
      const afterPeer = await waitFor(
        "Paseo reflects peer effort",
        async () => {
          const snapshot = await agent();
          return snapshot.effectiveThinkingOptionId === "low" || snapshot.thinkingOptionId === "low"
            ? snapshot
            : undefined;
        },
        10_000,
      );
      peer!.setModel(32, DEFAULT_MODEL);
      const peerModel = await waitFor(
        "peer set_model reply",
        () =>
          peer!.events.find(
            (event) =>
              event.type === "model_changed" &&
              event.id === 32 &&
              event.model === DEFAULT_MODEL,
          ),
        10_000,
      );
      const afterPeerModel = await waitFor(
        "Paseo reflects peer model",
        async () => {
          const snapshot = await agent();
          return snapshot.model === DEFAULT_MODEL ? snapshot : undefined;
        },
        10_000,
      );
      return {
        model: summary.model,
        effort: summary.effort,
        passthrough: { model: after.model, thinkingOptionId: after.thinkingOptionId },
        peerSetterEffort: peerEffort,
        peerSetterModel: peerModel,
        paseoAfterPeerSetters: {
          model: afterPeerModel.model,
          thinkingOptionId: afterPeer.thinkingOptionId,
          effectiveThinkingOptionId: afterPeer.effectiveThinkingOptionId,
        },
      };
    });

    // --- s08: textual skill selection through the connector ---------------
    await runScenario(evidence, "s08-skill-selection-textual", async () => {
      const prompt = send("/acceptance-skill SKILL-SELECT");
      const request = await nextRequest("SKILL-SELECT");
      const body = bodyText(request.body);
      if (!body.includes(SKILL_MARKER)) {
        throw new Error("skill instructions were not present in the provider request");
      }
      model.replyText(request, "SKILL-ANSWER");
      await prompt;
      await waitIdle();

      const sticky = send("STICKY-AFTER-SKILL");
      const stickyRequest = await nextRequest("STICKY-AFTER-SKILL");
      const stickyBody = bodyText(stickyRequest.body);
      model.replyText(stickyRequest, "STICKY-ANSWER");
      await sticky;
      await waitIdle();
      return {
        markerPresentOnSelection: true,
        markerPresentOnStickyPrompt: stickyBody.includes(SKILL_MARKER),
        note: "explicit selection and null clear are scored by s10-explicit-skill-select-clear and jcode-acp-probe.ts",
      };
    });

    // --- s09: owner-aware close (delete, not archive) ---------------------
    // Paseo archive intentionally cancels the active run before archiving, so
    // it is not the owner-aware close path. Deleting the agent closes the
    // provider session without a prior cancel: the ACP view must close while a
    // peer-owned turn keeps running.
    await runScenario(evidence, "s09-owner-aware-close", async () => {
      peer!.message(24, "CLOSE-PEER-TURN");
      const request = await nextRequest("CLOSE-PEER-TURN");
      model.replyToolCall(request, {
        id: "live-close-tool",
        name: "bash",
        arguments: { command: "sleep 2" },
      });
      const identity = await waitFor(
        "close peer turn identity",
        () => peer!.identityFor("CLOSE-PEER-TURN"),
        15_000,
      );
      await client.deleteAgent(agentId);
      // The peer turn must survive Paseo's close and complete afterwards.
      const completionRequest = await waitFor(
        "provider request after close",
        () => model.pending[0],
        20_000,
      );
      model.replyText(completionRequest, "PEER-SURVIVED-CLOSE");
      await waitFor(
        "peer turn completed after Paseo close",
        () => peer!.turnEnds("completed").some((event) => turnIdentityKey(event.identity) === turnIdentityKey(identity)),
        20_000,
      );
      if (peer!.turnEnds("cancelled").some((event) => turnIdentityKey(event.identity) === turnIdentityKey(identity))) {
        throw new Error("Paseo close cancelled the peer turn");
      }
      peer!.message(25, "AFTER-CLOSE");
      const afterRequest = await nextRequest("AFTER-CLOSE");
      model.replyText(afterRequest, "AFTER-CLOSE-ANSWER");
      await waitFor(
        "peer turn after close",
        () => peer!.identityFor("AFTER-CLOSE"),
        15_000,
      );
      await waitFor(
        "post-close peer completion",
        () =>
          peer!
            .turnEnds("completed")
            .some(
              (event) =>
                turnIdentityKey(event.identity) ===
                turnIdentityKey(peer!.identityFor("AFTER-CLOSE")),
            ),
        15_000,
      );
      const reopened = await client.importAgent({
        providerId: "jcode",
        providerHandleId: `plugin:${JSON.stringify({ version: 1, data: { sessionId } })}`,
        cwd: workspace.cwd,
      });
      agentId = reopened.id;
      return {
        deleted: true,
        peerTurnSurvivedClose: true,
        peerTurnIdentity: identity,
        sessionAliveAfterClose: true,
        reopenedAgentId: reopened.id,
      };
    });

    // --- s10: explicit skill selection and null clear --------------------
    await runScenario(evidence, "s10-explicit-skill-select-clear", async () => {
      const markerMessages = (body: Record<string, unknown>) =>
        (Array.isArray(body.messages) ? (body.messages as Array<Record<string, unknown>>) : []).filter(
          (message) => JSON.stringify(message).includes(SKILL_MARKER),
        ).length;

      // s08 selected the skill textually, so start from a known cleared state.
      const reset = send("RESET-SKILL", { skill: null });
      const resetRequest = await nextRequest("RESET-SKILL");
      const baseline = markerMessages(resetRequest.body);
      model.replyText(resetRequest, "RESET-SKILL-ANSWER");
      await reset;
      await waitIdle();

      const selected = send("EXPLICIT-SKILL", { skill: "acceptance-skill" });
      const selectedRequest = await nextRequest("EXPLICIT-SKILL");
      const afterSelect = markerMessages(selectedRequest.body);
      if (afterSelect !== baseline + 1) {
        throw new Error(
          `explicit skill selection did not append a reminder (${baseline} -> ${afterSelect})`,
        );
      }
      model.replyText(selectedRequest, "EXPLICIT-SKILL-ANSWER");
      await selected;
      await waitIdle();

      const sticky = send("EXPLICIT-STICKY");
      const stickyRequest = await nextRequest("EXPLICIT-STICKY");
      const afterSticky = markerMessages(stickyRequest.body);
      model.replyText(stickyRequest, "EXPLICIT-STICKY-ANSWER");
      await sticky;
      await waitIdle();
      if (afterSticky !== afterSelect) {
        throw new Error(`sticky explicit selection appended ${afterSticky - afterSelect} extra reminders`);
      }

      const cleared = send("EXPLICIT-CLEARED", { skill: null });
      const clearedRequest = await nextRequest("EXPLICIT-CLEARED");
      const afterClear = markerMessages(clearedRequest.body);
      const clearedText = lastUserText(clearedRequest.body);
      model.replyText(clearedRequest, "EXPLICIT-CLEARED-ANSWER");
      await cleared;
      await waitIdle();
      if (afterClear !== afterSelect) {
        throw new Error(`clear prompt appended ${afterClear - afterSelect} extra reminders`);
      }
      if (!clearedText.endsWith("EXPLICIT-CLEARED")) {
        throw new Error(`clear prompt text was modified: ${safeJson(clearedText)}`);
      }

      // Re-selecting after the clear must append a new reminder. If the clear
      // had not changed the attachment's sticky state, this would be a no-op.
      const reselect = send("EXPLICIT-RESELECT", { skill: "acceptance-skill" });
      const reselectRequest = await nextRequest("EXPLICIT-RESELECT");
      const afterReselect = markerMessages(reselectRequest.body);
      model.replyText(reselectRequest, "EXPLICIT-RESELECT-ANSWER");
      await reselect;
      await waitIdle();
      if (afterReselect !== afterSelect + 1) {
        throw new Error(
          `re-selection produced ${afterReselect} reminders; the connector clear did not clear the skill`,
        );
      }
      return {
        reminders: { baseline, afterSelect, afterSticky, afterClear, afterReselect },
        clearedText,
        explicitSelectAndClear: true,
      };
    });

    // --- s12: observer detach stops updates, not peer work ----------------
    // A peer-originated turn is already running when Paseo detaches. The view
    // never owned that turn, so detach must close the attachment without
    // cancelling the work, stop delivering updates to the closed record, and
    // leave the native session reloadable.
    await runScenario(evidence, "s12-observer-detach", async () => {
      const OBSERVED = "OBSERVED-TURN";
      const ANSWER = "OBSERVER-SURVIVED-DETACH";
      const handle = (await agent()).persistence ?? null;
      peer!.message(27, OBSERVED);
      const request = await nextRequest(OBSERVED);
      const identity = await waitFor(
        "observed peer turn identity",
        () => peer!.identityFor(OBSERVED),
        15_000,
      );
      // Hold the turn in a tool while the attachment is live, then detach.
      model.replyToolCall(request, {
        id: "live-observer-tool",
        name: "bash",
        arguments: { command: "sleep 3" },
      });
      const observed = await waitFor(
        "Paseo observes the peer turn",
        async () => {
          const snapshot = await agent();
          const turnId = String(snapshot.activeTurn?.turnId ?? "");
          return snapshot.status === "running" && turnId.startsWith("acp-vendor:")
            ? snapshot
            : null;
        },
        15_000,
      );

      // Watch the push channel, not a post-hoc timeline read: fetching a closed
      // record can hydrate history from the provider again.
      const pushed: any[] = [];
      const stopPushes = client.subscribeRawMessages((message: any) => pushed.push(message));
      await client.detachProviderSession(agentId);
      const pushBaseline = pushed.length;
      const closed = await agent().catch(() => null);
      if (closed && closed.status !== "closed") {
        throw new Error(`Paseo kept the observed agent live: ${safeJson(closed.status)}`);
      }
      const closedTimeline = await timeline().catch(() => null);

      // The next model call happens after the held tool, i.e. after detach.
      const completionRequest = await waitFor(
        "provider request after observer detach",
        () => model.pending[0],
        20_000,
      );
      model.replyText(completionRequest, ANSWER);
      await waitFor(
        "peer turn completed after observer detach",
        () =>
          peer!
            .turnEnds("completed")
            .some((event) => turnIdentityKey(event.identity) === turnIdentityKey(identity)),
        20_000,
      );
      if (
        peer!
          .turnEnds("cancelled")
          .some((event) => turnIdentityKey(event.identity) === turnIdentityKey(identity))
      ) {
        throw new Error("observer detach cancelled the peer turn");
      }

      const afterTimeline = await timeline().catch(() => null);
      stopPushes();
      const postDetachPushes = pushed.slice(pushBaseline);
      const leaked = postDetachPushes.filter((message) => {
        const encoded = JSON.stringify(message);
        return (
          message.type === "agent_stream" &&
          encoded.includes(agentId) &&
          encoded.includes(ANSWER)
        );
      });
      if (leaked.length > 0) {
        throw new Error("the detached attachment received post-detach updates");
      }
      const closedReadSurfacedAnswer = (afterTimeline ?? []).some((entry) =>
        JSON.stringify(entry).includes(ANSWER),
      );

      // Reload the same native session through a fresh agent record.
      const resumed = await client.resumeAgent(handle, { cwd: workspace.cwd });
      agentId = resumed.id;
      await waitFor(
        "resumed agent replays the peer turn and answer",
        async () => {
          const entries = await timeline();
          const text = JSON.stringify(userTexts(entries));
          return text.includes(OBSERVED) && JSON.stringify(entries).includes(ANSWER);
        },
        20_000,
      );
      const reloaded = send("AFTER-RELOAD");
      const reloadRequest = await nextRequest("AFTER-RELOAD");
      model.replyText(reloadRequest, "AFTER-RELOAD-ANSWER");
      await reloaded;
      await waitIdle();

      return {
        observedTurnIdentity: identity,
        observedLifecycle: observed.status,
        observedActiveTurnId: observed.activeTurn?.turnId ?? null,
        detachedStatus: closed?.status ?? null,
        closedTimelineLength: closedTimeline?.length ?? null,
        postDetachAgentStreams: postDetachPushes.filter(
          (message) => message.type === "agent_stream",
        ).length,
        // A fresh timeline read for the closed record can load provider
        // history; this is a read, not a delivered update.
        closedReadSurfacedAnswer,
        readTimelineLength: afterTimeline?.length ?? null,
        detachedAttachmentLeakedAnswer: leaked.length,
        peerCompletedAfterDetach: true,
        peerCancelledAfterDetach: false,
        resumedAgentId: resumed.id,
        reloadReplayedPeerWork: true,
        promptAfterReload: "AFTER-RELOAD-ANSWER",
      };
    });

    // --- s11: explicit detach preserves inserted work --------------------
    await runScenario(evidence, "s11-detach-preserves-inserted-work", async () => {
      const prompt = send("DETACH-INSERTED");
      const request = await nextRequest("DETACH-INSERTED");
      const identity = await waitFor(
        "detach turn identity",
        () => peer!.identityFor("DETACH-INSERTED"),
        15_000,
      );
      await client.detachProviderSession(agentId);
      // The inserted turn belongs to the shared daemon session and must finish.
      model.replyText(request, "DETACH-ANSWER");
      await waitFor(
        "inserted work completed after detach",
        () =>
          peer!
            .turnEnds("completed")
            .some((event) => turnIdentityKey(event.identity) === turnIdentityKey(identity)),
        20_000,
      );
      if (
        peer!
          .turnEnds("cancelled")
          .some((event) => turnIdentityKey(event.identity) === turnIdentityKey(identity))
      ) {
        throw new Error("connector detach cancelled inserted work");
      }
      await prompt.catch(() => undefined);
      const closedSnapshot = await agent().catch(() => null);
      if (closedSnapshot && closedSnapshot.status !== "closed") {
        throw new Error(`Paseo kept the detached agent live: ${safeJson(closedSnapshot.status)}`);
      }

      // Paseo keeps the closed record, so a second import of the same native
      // session is rejected by design. Prove survival the way the shared
      // session is actually used: another attached client keeps working.
      peer!.message(26, "AFTER-DETACH-PEER");
      const peerRequest = await nextRequest("AFTER-DETACH-PEER");
      model.replyText(peerRequest, "AFTER-DETACH-PEER-ANSWER");
      const afterIdentity = await waitFor(
        "peer turn after detach",
        () => peer!.identityFor("AFTER-DETACH-PEER"),
        15_000,
      );
      await waitFor(
        "peer completion after detach",
        () =>
          peer!
            .turnEnds("completed")
            .some((event) => turnIdentityKey(event.identity) === turnIdentityKey(afterIdentity)),
        15_000,
      );
      const replayed = userTexts(await timeline());
      return {
        detachedTurnIdentity: identity,
        insertedWorkCompleted: true,
        agentClosed: true,
        replayedUserMessages: replayed,
        peerContinuesAfterDetach: true,
      };
    });
  } catch (error) {
    evidence.notes.push(`setup or teardown failure: ${error instanceof Error ? error.stack ?? error.message : String(error)}`);
    throw error;
  } finally {
    try {
      await client?.close?.();
    } catch {
      // ignore
    }
    try {
      await paseo?.close?.();
    } catch {
      // ignore
    }
    peer?.destroy();
    try {
      await daemon?.stop();
    } catch {
      // ignore
    }
    try {
      await model.close();
    } catch {
      // ignore
    }
    for (const restore of restoreEnv.reverse()) restore();
    const evidencePath =
      process.env.JCODE_ACCEPTANCE_OUT ??
      path.join(process.env.JCODE_SCRATCH_DIR ?? "/tmp", "jcode-live-acceptance.evidence.json");
    evidence.notes.push(`daemon log tail: ${(daemon?.logs.join("").slice(-2000) ?? "").replace(/\s+/g, " ")}`);
    evidence.notes.push(`isolated workspace: ${workspace.root}`);
    writeEvidence(evidencePath, evidence);
    console.log(`evidence: ${evidencePath}`);
    console.log(`scenarios: ${summaryLine(evidence)}`);
    for (const line of failureLines(evidence)) console.error(line);
    if (process.env.JCODE_ACCEPTANCE_KEEP === "1") {
      console.log(`kept isolated workspace: ${workspace.root}`);
    } else {
      await workspace.cleanup().catch(() => undefined);
    }
    const failed = Object.values(evidence.scenarios).some((scenario) => scenario.status === "fail");
    if (failed) process.exitCode = 1;
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack ?? error.message : error);
  process.exitCode = 1;
});
