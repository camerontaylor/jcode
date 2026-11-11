// Deterministic regression evidence for the two causal synchronization rules
// the live steering scenarios depend on:
//
//   b01  A consumer-side settle (here: the real `jcode acp` prompt response)
//        does not imply the raw daemon attachment has delivered that turn's
//        terminal. Assertions must wait for the exact turn's scoped terminal,
//        not sample the event list once after the consumer looks idle.
//
//   b02  A held origin model call must not be released until the daemon has
//        positively admitted the steer. Delivery/admission is delayed with a
//        gate (never a sleep), and the release ordering is asserted.
//
// Both checks run against the real daemon, the real `jcode acp` adapter and a
// real raw peer attachment. The only controlled part is the harness-side
// delivery gate, which simulates message reordering the daemon is allowed to
// produce; no daemon behavior is stubbed. This is deliberately separate from
// the 12-scenario connector run and the 5-check ACP probe so neither set is
// renamed, skipped or weakened.
import path from "node:path";
import { spawn, type ChildProcess } from "node:child_process";
import { createRequire } from "node:module";
import { Readable, Writable } from "node:stream";
import { pathToFileURL } from "node:url";
import {
  binaryProvenance,
  bodyText,
  connectPeer,
  createWorkspace,
  daemonEnv,
  failureLines,
  holdModelUntilAdmitted,
  newEvidence,
  peerTerminals,
  peerTurnStarts,
  requireBinary,
  resolveConsumerRoot,
  runScenario,
  safeJson,
  sleep,
  startJcodeDaemon,
  startMockModel,
  summaryLine,
  waitFor,
  waitForPeerAdmission,
  waitForPeerInjection,
  waitForPeerTerminal,
  writeEvidence,
  type JcodeDaemon,
  type LivePeer,
  type MockModel,
} from "./jcode-live-common.ts";

const CONSUMER_ROOT = resolveConsumerRoot();

async function loadSdk(): Promise<any> {
  const requireFromConsumer = createRequire(path.join(CONSUMER_ROOT, "package.json"));
  return import(pathToFileURL(requireFromConsumer.resolve("@agentclientprotocol/sdk")).href);
}

function withTimeout<T>(promise: Promise<T>, ms: number, label: string): Promise<T> {
  return Promise.race([
    promise,
    sleep(ms).then(() => {
      throw new Error(`timed out after ${ms}ms waiting for ${label}`);
    }),
  ]);
}

function deferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

async function main() {
  const evidence = newEvidence();
  const binary = requireBinary();
  evidence.provenance = { jcode: binaryProvenance(binary) };
  evidence.environment = {
    node: process.version,
    platform: `${process.platform}/${process.arch}`,
    consumerRoot: CONSUMER_ROOT,
    isolation: "dedicated daemon socket, JCODE_HOME, JCODE_RUNTIME_DIR; no shared daemon",
    purpose: "deterministic delivery-inversion and delayed-admission regression",
  };

  const workspace = await createWorkspace("jcode-live-barriers-");
  const model: MockModel = await startMockModel();
  let daemon: JcodeDaemon | undefined;
  let peer: LivePeer | undefined;
  let acp: ChildProcess | undefined;
  let connection: any;
  const stderr: string[] = [];

  try {
    daemon = await startJcodeDaemon({ binary, workspace, modelBaseUrl: model.baseUrl });
    peer = await connectPeer(daemon.socketPath);

    const sdk = await loadSdk();
    acp = spawn(binary, ["acp"], {
      cwd: workspace.cwd,
      env: daemonEnv(workspace, model.baseUrl),
      stdio: ["pipe", "pipe", "pipe"],
    });
    if (!acp.stdin || !acp.stdout) throw new Error("jcode acp did not expose stdio pipes");
    acp.stderr?.on("data", (data) => stderr.push(String(data)));
    const stream = sdk.ndJsonStream(
      Writable.toWeb(acp.stdin) as any,
      Readable.toWeb(acp.stdout) as any,
    );
    connection = new sdk.ClientSideConnection(
      () => ({
        async sessionUpdate() {},
        async requestPermission(params: any) {
          const allow = (params.options ?? []).find((option: any) =>
            String(option.kind ?? "").startsWith("allow"),
          );
          return { outcome: { outcome: "selected", optionId: allow?.optionId } };
        },
        async extNotification() {},
      }),
      stream,
    );

    await withTimeout(connection.initialize({ protocolVersion: 1 }), 20_000, "ACP initialize");
    const created: any = await withTimeout(
      connection.newSession({ cwd: workspace.cwd, mcpServers: [] }),
      20_000,
      "ACP session/new",
    );
    const sessionId: string = created.sessionId;
    evidence.environment.protocolSessionId = sessionId;
    const session = await peer.subscribe(workspace.cwd, "barrier-peer", sessionId);
    evidence.environment.attachedSessionId = session.session_id;
    if (session.session_id !== sessionId) {
      throw new Error(`peer attached to ${safeJson(session.session_id)} instead of ${sessionId}`);
    }

    // --- b01: a terminal delivered after the consumer settled --------------
    await runScenario(evidence, "b01-terminal-barrier-after-consumer-settle", async () => {
      const ORIGIN = "INVERTED-ORIGIN";
      const order: string[] = [];
      // Withhold only terminals from the raw attachment. Turn starts keep
      // flowing so the harness can recover the exact identity before release.
      peer!.gate.hold(
        (event) => event.type === "turn_event" && event.end !== undefined,
      );
      try {
        const prompt = connection.prompt({
          sessionId,
          prompt: [{ type: "text", text: ORIGIN }],
        });
        const request = await model.takeContaining(ORIGIN);
        model.replyText(request, "INVERTED-ANSWER");
        await withTimeout(prompt, 30_000, `prompt ${ORIGIN}`);
        order.push("consumer-settled");

        const identity = await waitFor(
          "raw peer turn identity",
          () => peer!.identityFor(ORIGIN),
          15_000,
        );
        order.push("identity-known");

        // The daemon publishes the terminal independently of the gated raw
        // attachment. Waiting for it to reach the gate proves the message
        // exists and is being withheld, rather than simply not sent yet.
        await waitFor(
          "terminal withheld behind the delivery gate",
          () => (peer!.gate.buffered() > 0 ? true : undefined),
          15_000,
        );
        order.push("terminal-withheld");

        // This is exactly the old defective check: a single sample after the
        // consumer settled sees no terminal.
        const sampledImmediately = peerTerminals(peer!, identity, "completed");
        if (sampledImmediately.length !== 0) {
          throw new Error(
            `delivery gate did not invert terminal delivery: ${safeJson(sampledImmediately)}`,
          );
        }

        let barrierResolved = false;
        const barrier = waitForPeerTerminal(peer!, identity, "completed").then((events) => {
          barrierResolved = true;
          order.push("terminal-barrier-resolved");
          return events;
        });
        // Let the barrier's immediate probe run while the terminal is still
        // withheld. This is an event-loop turn, not a sleep or retry.
        await Promise.resolve();
        const resolvedBeforeDelivery = barrierResolved;
        order.push("barrier-armed");
        if (resolvedBeforeDelivery) {
          throw new Error("terminal barrier resolved before the terminal was delivered");
        }

        const released = peer!.gate.release();
        if (released === 0) throw new Error("delivery gate held no terminal to release");
        order.push("gate-released");
        const completed = await barrier;
        if (completed.length !== 1) {
          throw new Error(`expected one scoped completion, got ${completed.length}`);
        }
        const starts = peerTurnStarts(peer!, identity);
        if (starts.length !== 1) {
          throw new Error(`expected one scoped turn start, got ${starts.length}`);
        }
        if (peer!.gate.buffered() !== 0) {
          throw new Error("delivery gate still holds terminal events");
        }
        const settledAt = order.indexOf("consumer-settled");
        const withheldAt = order.indexOf("terminal-withheld");
        const armedAt = order.indexOf("barrier-armed");
        const releasedAt = order.indexOf("gate-released");
        const resolvedAt = order.indexOf("terminal-barrier-resolved");
        if (
          settledAt !== 0 ||
          withheldAt < settledAt ||
          armedAt < withheldAt ||
          releasedAt < armedAt ||
          resolvedAt < releasedAt
        ) {
          throw new Error(`unexpected causal order: ${safeJson(order)}`);
        }
        return {
          consumerSettledBeforeTerminal: true,
          sampledTerminalsBeforeRelease: sampledImmediately.length,
          withheldTerminals: released,
          barrierWaitedForDelivery: true,
          peerTerminalIdentity: identity,
          order,
        };
      } finally {
        if (peer!.gate.holding()) peer!.gate.release();
      }
    });

    // --- b02: held origin waits for daemon admission ------------------------
    await runScenario(evidence, "b02-held-origin-waits-for-admission", async () => {
      const ORIGIN = "ADMISSION-ORIGIN";
      const STEER = "ADMISSION-STEER";
      const STEER_ID = 41;
      const STEER_CLIENT_ID = "admission-steer-41";
      const order: string[] = [];
      let admissionObserved = false;

      const originPrompt = connection.prompt({
        sessionId,
        prompt: [{ type: "text", text: ORIGIN }],
      });
      const originRequest = await model.takeContaining(ORIGIN);
      const identity = await waitFor(
        "origin turn identity",
        () => peer!.identityFor(ORIGIN),
        15_000,
      );
      const before = await peer!.state();
      if (before.pendingSoftInterrupts !== 0) {
        throw new Error(
          `session queue was not empty before the steer: ${safeJson(before)}`,
        );
      }

      // Adversarial ordering: the steer is ready but deliberately not handed
      // to the daemon yet. No sleep and no retry; the release path may only
      // proceed once the daemon has admitted it.
      const flush = deferred();
      const delivery = flush.promise.then(() => {
        order.push("steer-delivered");
        peer!.softInterrupt(STEER_ID, STEER, false, STEER_CLIENT_ID);
      });

      const held = holdModelUntilAdmitted({
        label: ORIGIN,
        admission: async () => {
          const witness = await waitForPeerAdmission(peer!, {
            ackId: STEER_ID,
            label: STEER,
          });
          admissionObserved = true;
          order.push("admission-observed");
          return witness;
        },
        inspect: (witness) => {
          if (!admissionObserved) {
            throw new Error("admission witness was not observed before inspect");
          }
          if (witness.state.pendingSoftInterrupts < 1) {
            throw new Error(`admission reported no pending steer: ${safeJson(witness)}`);
          }
          if (model.pending.length !== 1 || model.pending[0] !== originRequest) {
            throw new Error(
              `the steer opened a second model call before admission: ${safeJson(
                model.pending.map((entry) => bodyText(entry.body).slice(0, 120)),
              )}`,
            );
          }
          order.push("origin-held-at-admission");
        },
        release: () => {
          order.push("origin-released");
          model.replyText(originRequest, "ADMISSION-INTERMEDIATE");
        },
      });
      // Settle the held promise without awaiting it yet so we can inspect the
      // still-held state deterministically.
      void held.catch(() => undefined);

      // The steer is not in the daemon, so the origin must still be held. Two
      // protocol round-trips are used to prove the state rather than a sleep.
      const whileUndelivered = await peer!.state();
      if (whileUndelivered.pendingSoftInterrupts !== 0) {
        throw new Error(
          `steer was admitted before it was delivered: ${safeJson(whileUndelivered)}`,
        );
      }
      if (order.includes("origin-released")) {
        throw new Error("origin model call was released before admission");
      }
      if (model.pending.length !== 1 || model.pending[0] !== originRequest) {
        throw new Error("origin model call did not stay the only held request");
      }

      flush.resolve();
      await delivery;
      const admitted = await held;
      const releaseCalls = order.filter((entry) => entry === "origin-released").length;
      if (releaseCalls !== 1) {
        throw new Error(`expected exactly one release after admission, got ${releaseCalls}`);
      }
      if (
        order.indexOf("admission-observed") < 0 ||
        order.indexOf("origin-held-at-admission") < order.indexOf("admission-observed") ||
        order.indexOf("origin-released") < order.indexOf("origin-held-at-admission")
      ) {
        throw new Error(`unexpected release order: ${safeJson(order)}`);
      }

      const steerRequest = await waitFor(
        `model request containing ${JSON.stringify(STEER)}`,
        () => model.pending.find((entry) => bodyText(entry.body).includes(STEER)),
        20_000,
      );
      if (steerRequest === originRequest) {
        throw new Error("steer reused the origin model request");
      }
      model.replyText(steerRequest, "ADMISSION-STEER-ANSWER");
      await withTimeout(originPrompt, 30_000, `prompt ${ORIGIN}`);
      order.push("consumer-settled");

      // Even after the consumer settled, the assertions wait for the daemon's
      // scoped terminal and injection instead of sampling once.
      const completed = await waitForPeerTerminal(peer!, identity, "completed");
      const injection = await waitForPeerInjection(peer!, identity, STEER);
      const starts = peerTurnStarts(peer!, identity);
      const cancelled = peerTerminals(peer!, identity, "cancelled");
      if (completed.length !== 1) {
        throw new Error(`expected one scoped completion, got ${completed.length}`);
      }
      if (starts.length !== 1) {
        throw new Error(`expected one scoped turn start, got ${starts.length}`);
      }
      if (cancelled.length !== 0) {
        throw new Error(`the shared turn was cancelled: ${safeJson(cancelled)}`);
      }
      const extraStarts = peer!.turnStarts().filter((event) => event.user_content === STEER);
      if (extraStarts.length !== 0) {
        throw new Error(
          `the steer opened a separate daemon turn: ${safeJson(extraStarts.map((event) => event.identity))}`,
        );
      }
      if (!Array.isArray(injection.client_ids) || !injection.client_ids.includes(STEER_CLIENT_ID)) {
        throw new Error(
          `the injected steer did not carry ${STEER_CLIENT_ID}: ${safeJson(injection.client_ids)}`,
        );
      }
      const after = await peer!.state();
      if (after.pendingSoftInterrupts !== 0) {
        throw new Error(`the session queue did not drain: ${safeJson(after)}`);
      }
      return {
        steerDeliveredAfterBaseline: true,
        admissionPendingSoftInterrupts: admitted.state.pendingSoftInterrupts,
        admissionAckId: admitted.ack?.id ?? null,
        releaseCalls,
        releaseBeforeAdmission:
          order.indexOf("origin-released") < order.indexOf("admission-observed"),
        originHeldThroughStateRoundTrips: true,
        completionEvents: completed.length,
        turnStarts: starts.length,
        cancellations: cancelled.length,
        extraTurnStartsForSteer: extraStarts.length,
        injection: { point: injection.point, clientIds: injection.client_ids },
        queueDrained: true,
        order,
      };
    });

    evidence.notes.push(
      "b01 delays only raw-peer terminal delivery; b02 delays only steer delivery. Both gates are harness-side and deterministic, with no sleeps or retries.",
    );
    evidence.notes.push(
      "These are regression checks for the acceptance harness's causal barriers, not additional product scenarios.",
    );
  } catch (error) {
    evidence.notes.push(
      `setup failure: ${error instanceof Error ? error.stack ?? error.message : String(error)}`,
    );
    throw error;
  } finally {
    try {
      await connection?.close?.();
    } catch {
      // ignore
    }
    if (acp && acp.exitCode === null && acp.signalCode === null) {
      acp.kill("SIGTERM");
      await Promise.race([
        new Promise<void>((resolve) => acp!.once("close", () => resolve())),
        sleep(2_000),
      ]);
      if (acp.exitCode === null && acp.signalCode === null) acp.kill("SIGKILL");
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
    const evidencePath =
      process.env.JCODE_ACCEPTANCE_OUT ??
      path.join(
        process.env.JCODE_SCRATCH_DIR ?? "/tmp",
        "jcode-live-barriers.evidence.json",
      );
    evidence.notes.push(`acp stderr tail: ${stderr.join("").slice(-2000).replace(/\s+/g, " ")}`);
    writeEvidence(evidencePath, evidence);
    console.log(`evidence: ${evidencePath}`);
    console.log(`scenarios: ${summaryLine(evidence)}`);
    for (const line of failureLines(evidence)) console.error(line);
    await workspace.cleanup().catch(() => undefined);
    if (Object.values(evidence.scenarios).some((scenario) => scenario.status === "fail")) {
      process.exitCode = 1;
    }
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack ?? error.message : error);
  process.exitCode = 1;
});
