// Live ACP companion probe, independently checking capability advertisement,
// prompt-response correlation,
// explicit skill selection and `null` clear, `_jcode/detach`, and config
// aliases.
//
// This talks to the real `jcode acp` process built from the same binary the
// Paseo connector run uses, on its own isolated daemon socket/home. It is live
// binary evidence, not a scripted ACP fixture, but it is NOT Paseo-connector
// proof; the final report labels it that way.
import path from "node:path";
import { spawn, type ChildProcess } from "node:child_process";
import { createRequire } from "node:module";
import { Readable, Writable } from "node:stream";
import { pathToFileURL } from "node:url";
import {
  ALT_MODEL,
  DEFAULT_MODEL,
  SKILL_MARKER,
  binaryProvenance,
  bodyText,
  connectPeer,
  createWorkspace,
  daemonEnv,
  failureLines,
  findEffort,
  holdModelUntilAdmitted,
  lastUserText,
  newEvidence,
  requireBinary,
  resolveConsumerRoot,
  runScenario,
  safeJson,
  sleep,
  startJcodeDaemon,
  startMockModel,
  summaryLine,
  turnIdentityKey,
  waitFor,
  waitForPeerAdmission,
  waitForPeerInjection,
  writeEvidence,
  type JcodeDaemon,
  type LivePeer,
  type MockModel,
} from "./jcode-live-common.ts";

const CONSUMER_ROOT = resolveConsumerRoot();

async function loadSdk(): Promise<any> {
  // Resolve the ACP SDK from the consumer checkout so the probe and the
  // connector use the same protocol implementation.
  const requireFromConsumer = createRequire(path.join(CONSUMER_ROOT, "package.json"));
  return import(pathToFileURL(requireFromConsumer.resolve("@agentclientprotocol/sdk")).href);
}

// The SDK is loaded from the consumer checkout at runtime. Untyped calls
// return JSON objects, while explicitly typed calls keep their result type.
function withTimeout<T = Record<string, any>>(promise: Promise<T>, ms: number, label: string): Promise<T> {
  return Promise.race([
    promise,
    sleep(ms).then(() => {
      throw new Error(`timed out after ${ms}ms waiting for ${label}`);
    }),
  ]);
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
  };

  const workspace = await createWorkspace("jcode-acp-probe-");
  const model = await startMockModel();
  let daemon: JcodeDaemon | undefined;
  let peer: LivePeer | undefined;
  let acp: ChildProcess | undefined;
  let connection: any;
  const stderr: string[] = [];

  try {
    daemon = await startJcodeDaemon({ binary, workspace, modelBaseUrl: model.baseUrl });
    peer = await connectPeer(daemon.socketPath);
    evidence.environment.isolated = {
      root: workspace.root,
      socket: workspace.socketPath,
      home: workspace.home,
      runtime: workspace.runtime,
    };

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

    const updates: any[] = [];
    const notifications: Array<{ method: string; params: any }> = [];
    connection = new sdk.ClientSideConnection(
      () => ({
        async sessionUpdate(params: any) {
          updates.push(params);
        },
        async requestPermission(params: any) {
          const allow = (params.options ?? []).find((option: any) =>
            String(option.kind ?? "").startsWith("allow"),
          );
          return { outcome: { outcome: "selected", optionId: allow?.optionId } };
        },
        async extNotification(method: string, params: any) {
          notifications.push({ method, params });
        },
      }),
      stream,
    );

    const initialize = await withTimeout(
      connection.initialize({ protocolVersion: 1 }),
      20_000,
      "ACP initialize",
    );
    const connectionMeta = initialize.agentCapabilities?._meta?.jcode;
    const created = await withTimeout(
      connection.newSession({ cwd: workspace.cwd, mcpServers: [] }),
      20_000,
      "ACP session/new",
    );
    const attachmentMeta = created._meta?.jcode;
    const protocolSessionId: string = created.sessionId;
    evidence.environment.protocolSessionId = protocolSessionId;
    // The peer is the TUI stand-in and must share the same daemon session, so
    // attach it only after the ACP attachment has created that session.
    const session = await peer.subscribe(workspace.cwd, "probe-peer", protocolSessionId);
    evidence.environment.attachedSessionId = session.session_id;
    if (session.session_id !== protocolSessionId) {
      throw new Error(
        `peer attached to ${safeJson(session.session_id)} instead of ${protocolSessionId}`,
      );
    }

    const prompt = async (text: string, meta?: Record<string, unknown>) => {
      const params: any = {
        sessionId: protocolSessionId,
        prompt: [{ type: "text", text }],
      };
      if (meta) params._meta = meta;
      return withTimeout(connection.prompt(params), 30_000, `prompt ${JSON.stringify(text)}`);
    };

    // --- p01: capability and attachment advertisement ---------------------
    await runScenario(evidence, "p01-capability-advertisement", async () => {
      if (!connectionMeta) throw new Error("initialize has no agentCapabilities._meta.jcode");
      if (connectionMeta.interfaceVersion !== 1) {
        throw new Error(`interfaceVersion ${safeJson(connectionMeta.interfaceVersion)}`);
      }
      const required = ["shared_control", "peer_turns", "turn_correlation", "skill_selection"];
      const missing = required.filter((name) => !(connectionMeta.extensions ?? []).includes(name));
      if (missing.length > 0) throw new Error(`missing extensions: ${missing.join(", ")}`);
      if (connectionMeta.sessionMcpServers !== false) {
        throw new Error(`sessionMcpServers ${safeJson(connectionMeta.sessionMcpServers)}`);
      }
      if (!attachmentMeta) throw new Error("session/new has no _meta.jcode");
      if (typeof attachmentMeta.clientInstanceId !== "string") {
        throw new Error("attachment has no clientInstanceId");
      }
      const expected = {
        observe: true,
        stopObservedTurn: true,
        detach: true,
        implicitSteering: true,
      };
      for (const [field, value] of Object.entries(expected)) {
        if (attachmentMeta.sharedControl?.[field] !== value) {
          throw new Error(
            `sharedControl.${field} is ${safeJson(attachmentMeta.sharedControl?.[field])}, expected ${value}`,
          );
        }
      }
      if ("interfaceVersion" in attachmentMeta) {
        throw new Error("attachment metadata repeats interfaceVersion");
      }
      return {
        connectionMeta,
        attachmentMeta,
        configOptions: created.configOptions,
      };
    });

    const markerMessages = (body: Record<string, unknown>) =>
      (Array.isArray(body.messages) ? (body.messages as Array<Record<string, unknown>>) : []).filter(
        (message) => JSON.stringify(message).includes(SKILL_MARKER),
      ).length;

    // --- p02: explicit skill select, sticky, and null clear ---------------
    await runScenario(evidence, "p02-skill-select-and-clear", async () => {
      const before = model.requests.length;
      const selectedPending = prompt("SKILL-SELECT", { jcode: { skill: "acceptance-skill" } });
      const selectedRequest = await model.takeContaining("SKILL-SELECT");
      const selectedReminders = markerMessages(selectedRequest.body);
      if (selectedReminders !== 1) {
        throw new Error(`expected one skill reminder on selection, got ${selectedReminders}`);
      }
      model.replyText(selectedRequest, "SKILL-SELECT-ANSWER");
      const selected = await selectedPending;
      const selectedCorrelation = selected._meta?.jcode;
      if (selectedCorrelation?.role !== "origin" || !selectedCorrelation?.turnId) {
        throw new Error(`unexpected correlation ${safeJson(selectedCorrelation)}`);
      }

      const stickyPending = prompt("STICKY-SELECTED");
      const stickyRequest = await model.takeContaining("STICKY-SELECTED");
      const stickyReminders = markerMessages(stickyRequest.body);
      model.replyText(stickyRequest, "STICKY-ANSWER");
      await stickyPending;
      if (stickyReminders !== 1) {
        throw new Error(`sticky prompt appended ${stickyReminders - 1} extra skill reminders`);
      }

      const clearedPromise = prompt("CLEARED-TEXT", { jcode: { skill: null } });
      const clearedRequest = await model.takeContaining("CLEARED-TEXT");
      const clearedReminders = markerMessages(clearedRequest.body);
      const clearedLastUser = lastUserText(clearedRequest.body);
      model.replyText(clearedRequest, "CLEARED-ANSWER");
      const clearedText = await clearedPromise;
      if (clearedReminders !== 1) {
        throw new Error(`clearing prompt appended a skill reminder (${clearedReminders})`);
      }
      if (!clearedLastUser.endsWith("CLEARED-TEXT")) {
        throw new Error(`cleared prompt text was modified: ${safeJson(clearedLastUser)}`);
      }
      if (clearedText._meta?.jcode?.role !== "origin") {
        throw new Error(`cleared text correlation ${safeJson(clearedText._meta?.jcode)}`);
      }

      // Re-selecting the same skill after the clear must append a new hidden
      // reminder. If the clear had not changed the attachment state, this
      // selection would be a no-op and the count would stay at one.
      const reselectPending = prompt("RESELECT-A", { jcode: { skill: "acceptance-skill" } });
      const reselectRequest = await model.takeContaining("RESELECT-A");
      const reselectReminders = markerMessages(reselectRequest.body);
      model.replyText(reselectRequest, "RESELECT-ANSWER");
      await reselectPending;
      if (reselectReminders !== 2) {
        throw new Error(
          `re-selection did not append a reminder (${reselectReminders}); the clear did not clear the attachment skill`,
        );
      }

      const requestsBeforeClear = model.requests.length;
      const cleared = await prompt("", { jcode: { skill: null } });
      await sleep(300);
      if (model.requests.length !== requestsBeforeClear) {
        throw new Error("selection-only clear started a model turn");
      }
      const clearCorrelation = cleared._meta?.jcode;
      if (clearCorrelation?.role !== "none" || clearCorrelation?.turnId !== null) {
        throw new Error(`selection-only clear correlation ${safeJson(clearCorrelation)}`);
      }

      const afterClearPending = prompt("AFTER-CLEAR");
      const afterClearRequest = await model.takeContaining("AFTER-CLEAR");
      const afterClearReminders = markerMessages(afterClearRequest.body);
      model.replyText(afterClearRequest, "AFTER-CLEAR-ANSWER");
      await afterClearPending;
      if (afterClearReminders !== 2) {
        throw new Error(`prompt after clear appended a skill reminder (${afterClearReminders})`);
      }
      return {
        selectionTurnId: selectedCorrelation.turnId,
        reminderMessages: {
          afterSelect: selectedReminders,
          afterSticky: stickyReminders,
          afterClearWithText: clearedReminders,
          afterReselect: reselectReminders,
          afterSelectionOnlyClear: afterClearReminders,
        },
        clearedLastUser,
        selectionOnlyClearRole: clearCorrelation.role,
        requestCountDelta: model.requests.length - before,
      };
    });

    // --- p03: peer turn observation and steer correlation -----------------
    await runScenario(evidence, "p03-peer-observe-and-steer-correlation", async () => {
      peer!.message(30, "PEER-BASE");
      const baseRequest = await model.takeContaining("PEER-BASE");
      const started = await waitFor(
        "peer _jcode/turn started",
        () => notifications.find((entry) => entry.method === "_jcode/turn" && entry.params?.state === "started"),
        15_000,
      );
      const turnId = started.params.turnId;
      const beforeSteer = await peer!.state();
      if (beforeSteer.pendingSoftInterrupts !== 0) {
        throw new Error(`session queue was not empty before the steer: ${safeJson(beforeSteer)}`);
      }
      const joinPromise = prompt("ACP-JOIN");
      // The prompt write is not admission. Hold the origin model call until the
      // daemon's own state reports the steer queued on this session, so the
      // origin cannot finish first and turn the steer into a second origin.
      const admission = await holdModelUntilAdmitted({
        label: "PEER-BASE",
        admission: () => waitForPeerAdmission(peer!, { label: "ACP-JOIN steer" }),
        inspect: (witness) => {
          if (witness.state.sessionId !== protocolSessionId) {
            throw new Error(
              `steer was admitted to ${safeJson(witness.state.sessionId)} instead of ${protocolSessionId}`,
            );
          }
          if (witness.state.pendingSoftInterrupts < 1) {
            throw new Error(`admission reported no pending steer: ${safeJson(witness)}`);
          }
          if (model.pending.length !== 1 || model.pending[0] !== baseRequest) {
            throw new Error(
              `the steer opened a second model call before admission: ${safeJson(
                model.pending.map((entry) => bodyText(entry.body).slice(0, 120)),
              )}`,
            );
          }
        },
        release: () => model.replyText(baseRequest, "PEER-BASE-ANSWER"),
      });
      const joinRequest = await waitFor(
        "provider request containing steer text",
        () => model.pending.find((entry) => bodyText(entry.body).includes("ACP-JOIN")),
        20_000,
      );
      model.replyText(joinRequest, "JOIN-ANSWER");
      const joined = await joinPromise;
      const correlation = joined._meta?.jcode;
      if (correlation?.role !== "steer") {
        throw new Error(`steer response role ${safeJson(correlation)}`);
      }
      if (correlation?.turnId !== turnId) {
        throw new Error(
          `steer turnId ${safeJson(correlation?.turnId)} != notification turnId ${safeJson(turnId)}`,
        );
      }
      // The daemon's own statement that the steer entered this execution.
      const injection = await waitForPeerInjection(peer!, turnId, "ACP-JOIN");
      const extraStarts = peer!.turnStarts().filter((event) => event.user_content === "ACP-JOIN");
      if (extraStarts.length !== 0) {
        throw new Error(
          `the steer opened a separate daemon turn: ${safeJson(extraStarts.map((event) => event.identity))}`,
        );
      }
      await waitFor(
        "peer turn completed notification",
        () =>
          notifications.find(
            (entry) =>
              entry.method === "_jcode/turn" &&
              entry.params?.turnId === turnId &&
              entry.params?.state === "completed",
          ),
        15_000,
      );
      const starts = notifications.filter(
        (entry) => entry.method === "_jcode/turn" && entry.params?.turnId === turnId && entry.params?.state === "started",
      );
      if (starts.length !== 1) throw new Error(`expected one started notification, got ${starts.length}`);
      await waitFor(
        "peer completed terminal",
        () =>
          peer!
            .turnEnds("completed")
            .some((event) => turnIdentityKey(event.identity) === turnIdentityKey(turnId)),
        10_000,
      );
      const peerStarts = peer!
        .turnStarts()
        .filter((event) => turnIdentityKey(event.identity) === turnIdentityKey(turnId));
      const peerCompleted = peer!
        .turnEnds("completed")
        .filter((event) => turnIdentityKey(event.identity) === turnIdentityKey(turnId));
      if (peerStarts.length !== 1 || peerCompleted.length !== 1) {
        throw new Error(
          `daemon terminal mismatch: starts=${peerStarts.length} completed=${peerCompleted.length} turnId=${safeJson(turnId)} observedStarts=${safeJson(peer!.turnStarts().map((event) => ({ identity: event.identity, content: event.user_content })))}`,
        );
      }
      return {
        observedTurnId: turnId,
        steerRole: correlation.role,
        steerTurnId: correlation.turnId,
        admission: {
          sessionId: admission.state.sessionId,
          pendingSoftInterrupts: admission.state.pendingSoftInterrupts,
        },
        injection: {
          content: injection.content ?? null,
          clientIds: injection.client_ids ?? null,
        },
        peerTerminals: {
          completed: peer!.turnEnds("completed").map((event) => event.identity),
          cancelled: peer!.turnEnds("cancelled").map((event) => event.identity),
        },
      };
    });

    // --- p04: detach leaves inserted work running -------------------------
    await runScenario(evidence, "p04-detach", async () => {
      const promptPromise = prompt("DETACH-BASE");
      const request = await model.takeContaining("DETACH-BASE");
      const turnId = await waitFor(
        "detach turn identity",
        () => peer!.identityFor("DETACH-BASE"),
        15_000,
      );
      const detach = await withTimeout(
        connection.extMethod("_jcode/detach", { sessionId: protocolSessionId }),
        10_000,
        "detach",
      );
      if (safeJson(detach) !== "{}") throw new Error(`detach result ${safeJson(detach)}`);
      const settled = await promptPromise;
      const correlation = settled._meta?.jcode;
      if (settled.stopReason !== "cancelled") {
        throw new Error(`detached prompt stopReason ${safeJson(settled.stopReason)}`);
      }
      if (
        turnIdentityKey(correlation?.turnId) !== turnIdentityKey(turnId) ||
        correlation?.role !== "origin"
      ) {
        throw new Error(
          `detached prompt correlation ${safeJson({ correlation, peerIdentity: turnId, peerStarts: peer!.turnStarts().map((event) => ({ identity: event.identity, content: event.user_content })) })}`,
        );
      }
      if (
        peer!
          .turnEnds("cancelled")
          .some((event) => turnIdentityKey(event.identity) === turnIdentityKey(turnId))
      ) {
        throw new Error("detach cancelled inserted work");
      }
      const updatesAfterDetach = updates.length;
      await sleep(300);
      if (updates.length !== updatesAfterDetach) {
        throw new Error("session updates continued after the detach response");
      }
      model.replyText(request, "DETACH-ANSWER");
      await waitFor(
        "inserted work completed after detach",
        () =>
          peer!
            .turnEnds("completed")
            .some((event) => turnIdentityKey(event.identity) === turnIdentityKey(turnId)),
        20_000,
      );
      const repeated = await withTimeout(
        connection.extMethod("_jcode/detach", { sessionId: protocolSessionId }),
        10_000,
        "repeated detach",
      );
      if (safeJson(repeated) !== "{}") throw new Error(`repeated detach ${safeJson(repeated)}`);
      let malformedCode: number | null = null;
      try {
        await connection.extMethod("_jcode/detach", { sessionId: 123 });
      } catch (error) {
        malformedCode = (error as { code?: number }).code ?? null;
      }
      if (malformedCode !== -32602) {
        throw new Error(`malformed detach error code ${safeJson(malformedCode)}`);
      }
      // Resume by replaying history, then run a normal turn on the new attachment.
      const reloaded = await withTimeout(
        connection.loadSession({ sessionId: protocolSessionId, cwd: workspace.cwd, mcpServers: [] }),
        20_000,
        "session/load after detach",
      );
      const resumedPending = prompt("AFTER-DETACH");
      const resumedRequest = await model.takeContaining("AFTER-DETACH");
      model.replyText(resumedRequest, "AFTER-DETACH-ANSWER");
      const resumed = await resumedPending;
      if (resumed._meta?.jcode?.role !== "origin") {
        throw new Error(`resumed prompt correlation ${safeJson(resumed._meta?.jcode)}`);
      }
      return {
        detachedTurnId: turnId,
        detachedStopReason: settled.stopReason,
        insertedWorkContinued: true,
        repeatedDetachResult: repeated,
        malformedDetachCode: malformedCode,
        updatesAfterDetach: 0,
        reloadAttachment: reloaded._meta?.jcode ?? null,
        resumedRole: resumed._meta?.jcode?.role,
      };
    });

    // --- p05: model/effort config and legacy aliases ----------------------
    await runScenario(evidence, "p05-model-effort-config", async () => {
      const modelSet = await withTimeout(
        connection.setSessionConfigOption({
          sessionId: protocolSessionId,
          configId: "model",
          value: ALT_MODEL,
        }),
        15_000,
        "set model",
      );
      const modelOption = (modelSet.configOptions ?? []).find((option: any) => option.id === "model");
      if (modelOption?.currentValue !== ALT_MODEL) {
        throw new Error(`model option ${safeJson(modelOption?.currentValue)}`);
      }
      const effortSet = await withTimeout(
        connection.setSessionConfigOption({
          sessionId: protocolSessionId,
          configId: "reasoning_effort",
          value: "high",
        }),
        15_000,
        "set reasoning_effort",
      );
      const effortOption = (effortSet.configOptions ?? []).find(
        (option: any) => option.id === "reasoning_effort",
      );
      if (effortOption?.currentValue !== "high") {
        throw new Error(`effort option ${safeJson(effortOption)}`);
      }
      const configuredPending = prompt("CONFIG-PROBE");
      const configuredRequest = await model.takeContaining("CONFIG-PROBE");
      const summary = {
        model: configuredRequest.body.model,
        effort: findEffort(configuredRequest.body),
      };
      model.replyText(configuredRequest, "CONFIG-ANSWER");
      const configured = await configuredPending;
      if (summary.model !== ALT_MODEL) throw new Error(`provider model ${safeJson(summary.model)}`);
      if (!summary.effort || summary.effort[1] !== "high") {
        throw new Error(`provider effort ${safeJson(summary.effort)}`);
      }
      if (configured._meta?.jcode?.role !== "origin") {
        throw new Error(`config prompt correlation ${safeJson(configured._meta?.jcode)}`);
      }

      const legacyModel = await withTimeout(
        connection.extMethod("session/set_model", {
          sessionId: protocolSessionId,
          modelId: DEFAULT_MODEL,
        }),
        15_000,
        "legacy set_model",
      );
      const legacyOption = (legacyModel.configOptions ?? legacyModel.result?.configOptions ?? []).find(
        (option: any) => option.id === "model",
      );
      const legacyEffort = await withTimeout(
        connection.extMethod("session/set_reasoning_effort", {
          sessionId: protocolSessionId,
          effort: "low",
        }),
        15_000,
        "legacy set_reasoning_effort",
      );
      const legacyEffortOption = (
        legacyEffort.configOptions ??
        legacyEffort.result?.configOptions ??
        []
      ).find((option: any) => option.id === "reasoning_effort");
      const legacyPromptPending = prompt("LEGACY-ALIAS-PROBE");
      const legacyRequest = await model.takeContaining("LEGACY-ALIAS-PROBE");
      const legacySummary = {
        model: legacyRequest.body.model,
        effort: findEffort(legacyRequest.body),
        lastUser: lastUserText(legacyRequest.body),
      };
      model.replyText(legacyRequest, "LEGACY-ANSWER");
      await legacyPromptPending;
      if (legacySummary.model !== DEFAULT_MODEL) {
        throw new Error(`legacy model alias did not apply: ${safeJson(legacySummary.model)}`);
      }
      return {
        configOptions: { model: modelOption, effort: effortOption },
        providerRequest: summary,
        legacyModelOption: legacyOption ?? null,
        legacyEffortOption: legacyEffortOption ?? null,
        legacyProviderRequest: legacySummary,
      };
    });

    evidence.notes.push(
      "Probe covers capability advertisement, all four sharedControl booleans, correlation roles, explicit skill select plus null clear, _jcode/detach, and config aliases against the real binary.",
    );
    evidence.notes.push(
      "This is not Paseo-connector proof. Connector-level evidence lives in jcode-live-acceptance.ts.",
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
      path.join(process.env.JCODE_SCRATCH_DIR ?? "/tmp", "jcode-acp-probe.evidence.json");
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
