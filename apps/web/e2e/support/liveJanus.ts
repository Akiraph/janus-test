import { type ChildProcess, spawn, spawnSync } from "node:child_process";
import { appendFileSync } from "node:fs";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const CONTROL_ORIGIN = "http://127.0.0.1:4317";

function jsonSseFrame(value: unknown): string {
  return `data: ${JSON.stringify(value)}\n\n`;
}

const COMPLETED_FRAME = {
  choices: [{ index: 0, delta: {}, finish_reason: "stop" }],
  usage: { prompt_tokens: 9, completion_tokens: 4 },
};
const FINISH_STREAM = `${jsonSseFrame({
  choices: [{ index: 0, delta: { role: "assistant", content: "Live fixture reply" } }],
})}${jsonSseFrame(COMPLETED_FRAME)}data: [DONE]\n\n`;

const DELAYED_MARKDOWN_START = jsonSseFrame({
  choices: [{ index: 0, delta: { role: "assistant", content: "## Live stream" } }],
});
const DELAYED_MARKDOWN_END = `${jsonSseFrame({
  choices: [{ index: 0, delta: { content: "\n\n- Live fixture reply" } }],
})}${jsonSseFrame(COMPLETED_FRAME)}data: [DONE]\n\n`;

function toolStream(callId: string, name: string, argumentsValue: unknown): string {
  const frames = [
    {
      choices: [{ index: 0, delta: { role: "assistant", content: "" } }],
    },
    {
      choices: [
        {
          index: 0,
          delta: {
            tool_calls: [
              {
                index: 0,
                id: callId,
                type: "function",
                function: { name, arguments: "" },
              },
            ],
          },
        },
      ],
    },
    {
      choices: [
        {
          index: 0,
          delta: {
            tool_calls: [
              {
                index: 0,
                function: { arguments: JSON.stringify(argumentsValue) },
              },
            ],
          },
        },
      ],
    },
    {
      choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }],
      usage: { prompt_tokens: 9, completion_tokens: 4 },
    },
  ];
  return `${frames.map(jsonSseFrame).join("")}data: [DONE]\n\n`;
}

type DataResponse<T> = { data: T };

interface OperationView {
  id: string;
  status: string;
  target_id?: string | null;
  problem?: unknown;
}

interface SessionView {
  id: string;
  version: string;
}

export interface LiveJanusEnvironment {
  projectId: string;
  sessionId: string;
  sessionTitle: string;
  cli: <T>(args: string[]) => T;
  request: <T>(path: string, init?: RequestInit) => Promise<T>;
  providerRequestCount: () => number;
  restart: () => Promise<void>;
  serverLog: () => string;
  stop: () => Promise<void>;
}

export async function startLiveJanus(): Promise<LiveJanusEnvironment> {
  const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../../..");
  const fixtureRoot = await mkdtemp(join(tmpdir(), "janus-web-live-"));
  const workRepo = join(fixtureRoot, "work");
  const bareRepo = join(fixtureRoot, "fixture.git");
  const dataRoot = join(fixtureRoot, "data");
  const provider = await startProvider();
  let server: ChildProcess | undefined;
  let serverLog = "";

  try {
    runGit(["init", "-b", "main", workRepo]);
    runGit(["-C", workRepo, "config", "user.email", "ui-e2e@example.invalid"]);
    runGit(["-C", workRepo, "config", "user.name", "Janus UI E2E"]);
    await writeFile(join(workRepo, "README.md"), "# Live fixture\n", "utf8");
    runGit(["-C", workRepo, "add", "README.md"]);
    runGit(["-C", workRepo, "commit", "-m", "fixture"]);
    runGit(["clone", "--bare", workRepo, bareRepo]);

    const executable = resolve(
      repoRoot,
      "target",
      "debug",
      process.platform === "win32" ? "janus-server.exe" : "janus-server",
    );
    const testCli = resolve(
      repoRoot,
      "target",
      "debug",
      process.platform === "win32" ? "janus-test.exe" : "janus-test",
    );
    const launchServer = () => {
      const child = spawn(executable, [], {
        cwd: repoRoot,
        env: {
          ...process.env,
          JANUS_DATA_ROOT: dataRoot,
          JANUS_DEV_AUTH: "true",
          JANUS_PUBLIC_ORIGIN: "http://localhost:4317",
          JANUS_WEBAUTHN_RP_ID: "localhost",
        },
        stdio: ["ignore", "pipe", "pipe"],
        windowsHide: true,
      });
      const collectLog = (chunk: Buffer) => {
        serverLog = `${serverLog}${chunk.toString("utf8")}`.slice(-16_000);
        // DIAGNOSTIC: also persist full server log to disk.
        try {
          appendFileSync(`${tmpdir()}/janus-live-server.log`, chunk.toString("utf8"));
        } catch {
          // ignore
        }
      };
      child.stdout?.on("data", collectLog);
      child.stderr?.on("data", collectLog);
      return child;
    };

    server = launchServer();
    await waitForReady(server, () => serverLog);
    await requestJson("/api/v1/model-providers", {
      method: "POST",
      body: JSON.stringify({
        kind: "openai_chat",
        display_name: "Live fixture",
        base_url: `${provider.origin}/v1`,
        api_key: "sk-test-only",
        models: [
          {
            display_name: "Fixture model",
            upstream_model_id: "fixture-model",
            supports_1m: false,
            supports_images: false,
            enabled: true,
          },
        ],
        enabled: true,
      }),
    });

    const projectOperation = runCliJson<DataResponse<OperationView>>(testCli, [
      "--base-url",
      CONTROL_ORIGIN,
      "projects",
      "create",
      "--name",
      "Live project",
      "--url",
      pathToFileURL(bareRepo).href,
      "--branch",
      "main",
      "--idempotency-key",
      `web-live-${Date.now()}`,
    ]);
    const project = runCliJson<DataResponse<OperationView>>(testCli, [
      "--base-url",
      CONTROL_ORIGIN,
      "operations",
      "wait",
      projectOperation.data.id,
      "--poll-millis",
      "50",
    ]);
    const projectId = requireOperationTarget(project.data, "project creation");
    const sessionTitle = "Live supervisor";
    const sessionOperation = runCliJson<DataResponse<OperationView>>(testCli, [
      "--base-url",
      CONTROL_ORIGIN,
      "sessions",
      "create",
      projectId,
      "--title",
      sessionTitle,
      "--idempotency-key",
      `web-live-session-${Date.now()}`,
    ]);
    const session = runCliJson<DataResponse<OperationView>>(testCli, [
      "--base-url",
      CONTROL_ORIGIN,
      "operations",
      "wait",
      sessionOperation.data.id,
      "--poll-millis",
      "50",
    ]);
    const sessionId = requireOperationTarget(session.data, "session creation");

    return {
      projectId,
      sessionId,
      sessionTitle,
      cli: <T>(args: string[]) => {
        try {
          return runCliJson<T>(testCli, ["--base-url", CONTROL_ORIGIN, ...args]);
        } catch (error) {
          const details = serverLog ? `\nJanus server output:\n${serverLog}` : "";
          throw new Error(`${error instanceof Error ? error.message : String(error)}${details}`);
        }
      },
      request: requestJson,
      providerRequestCount: provider.requestCount,
      restart: async () => {
        await stopProcess(server);
        serverLog = "";
        server = launchServer();
        await waitForReady(server, () => serverLog);
      },
      serverLog: () => serverLog,
      stop: async () => {
        try {
          deleteSessionWithCli(testCli, sessionId);
        } finally {
          await stopProcess(server);
          await closeServer(provider.server);
          await rm(fixtureRoot, { recursive: true, force: true });
        }
      },
    };
  } catch (error) {
    await stopProcess(server);
    await closeServer(provider.server);
    await rm(fixtureRoot, { recursive: true, force: true });
    const details = serverLog ? `\nJanus server output:\n${serverLog}` : "";
    throw new Error(`${error instanceof Error ? error.message : String(error)}${details}`);
  }
}

async function startProvider(): Promise<{
  origin: string;
  server: Server;
  requestCount: () => number;
}> {
  let requestCount = 0;
  const server = createServer((request, response) => {
    if (request.method !== "POST" || request.url !== "/v1/chat/completions") {
      response.writeHead(404).end();
      return;
    }
    let requestBody = "";
    request.setEncoding("utf8");
    request.on("data", (chunk: string) => {
      requestBody += chunk;
    });
    request.on("end", () => {
      requestCount += 1;
      let latestUser = "";
      let hasToolResult = false;
      let payload: unknown;
      try {
        payload = JSON.parse(requestBody);
        latestUser = latestUserContent(payload);
        hasToolResult = hasToolResultAfterLatestUser(payload);
      } catch {
        response.writeHead(400).end();
        return;
      }

      const callId = `fixture_call_${requestCount}`;
      const calledTools = toolNamesAfterLatestUser(payload);
      let stream = FINISH_STREAM;
      if (latestUser.includes("[fixture:attachments]")) {
        const attachmentIds = uuids(latestUser);
        if (!calledTools.includes("attachment_list")) {
          stream = toolStream(callId, "attachment_list", {});
        } else if (!calledTools.includes("attachment_read") && attachmentIds[0]) {
          stream = toolStream(callId, "attachment_read", {
            attachment_id: attachmentIds[0],
          });
        } else if (!calledTools.includes("attachment_save") && attachmentIds[1]) {
          stream = toolStream(callId, "attachment_save", {
            attachment_id: attachmentIds[1],
            path: "assets/logo.bin",
          });
        }
      } else if (latestUser.includes("[fixture:attachment-reuse]")) {
        const attachmentId = uuids(latestToolContent(payload))[0];
        if (!calledTools.includes("attachment_list")) {
          stream = toolStream(callId, "attachment_list", {});
        } else if (!calledTools.includes("attachment_read") && attachmentId) {
          stream = toolStream(callId, "attachment_read", { attachment_id: attachmentId });
        }
      } else if (latestUser.includes("[fixture:ask-expire]") && !calledTools.includes("ask_user")) {
        stream = toolStream(callId, "ask_user", {
          prompt: "Use the fixture default",
          mode: "best_effort",
          default: "fixture expiry default",
          expires_in_ms: 100,
        });
      } else if (
        (latestUser.includes("[fixture:ask]") || latestUser.includes("[fixture:restart-ask]")) &&
        !calledTools.includes("ask_user")
      ) {
        stream = toolStream(callId, "ask_user", {
          prompt: "Choose the fixture answer",
          mode: "blocking",
          choices: ["fixture answer"],
        });
      } else if (latestUser.includes("[fixture:cancel-job]") && !calledTools.includes("bash")) {
        stream = toolStream(callId, "bash", {
          command: fixtureSleepCommand(30),
          mode: "async",
          timeout_ms: 60_000,
        });
      } else if (latestUser.includes("[fixture:job-resume]") && !hasToolResult) {
        stream = toolStream(callId, "bash", {
          command: fixtureSleepCommand(1),
          mode: "async",
          timeout_ms: 30_000,
        });
      } else if (latestUser.includes("[fixture:handoff-job]") && !calledTools.includes("bash")) {
        stream = toolStream(callId, "bash", {
          command: fixtureSleepCommand(3),
          mode: "async",
          timeout_ms: 30_000,
        });
      }

      response.writeHead(200, { "content-type": "text/event-stream" });
      if (latestUser === "你好") {
        response.write(DELAYED_MARKDOWN_START);
        setTimeout(() => response.end(DELAYED_MARKDOWN_END), 1_000);
        return;
      }
      response.end(stream);
    });
  });
  await new Promise<void>((resolvePromise, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      resolvePromise();
    });
  });
  const address = server.address();
  if (!address || typeof address === "string") {
    await closeServer(server);
    throw new Error("fixture provider did not expose a TCP address");
  }
  return {
    origin: `http://127.0.0.1:${address.port}`,
    server,
    requestCount: () => requestCount,
  };
}

function latestUserContent(payload: unknown): string {
  if (typeof payload !== "object" || payload === null) return "";
  const messages = Reflect.get(payload, "messages");
  if (!Array.isArray(messages)) return "";
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (typeof message !== "object" || message === null) continue;
    if (Reflect.get(message, "role") !== "user") continue;
    const content = Reflect.get(message, "content");
    return typeof content === "string" ? content : JSON.stringify(content);
  }
  return "";
}

function hasToolResultAfterLatestUser(payload: unknown): boolean {
  if (typeof payload !== "object" || payload === null) return false;
  const messages = Reflect.get(payload, "messages");
  if (!Array.isArray(messages)) return false;
  let latestUserIndex = -1;
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (
      typeof message === "object" &&
      message !== null &&
      Reflect.get(message, "role") === "user"
    ) {
      latestUserIndex = index;
      break;
    }
  }
  return messages
    .slice(latestUserIndex + 1)
    .some(
      (message) =>
        typeof message === "object" && message !== null && Reflect.get(message, "role") === "tool",
    );
}

function toolNamesAfterLatestUser(payload: unknown): string[] {
  if (typeof payload !== "object" || payload === null) return [];
  const messages = Reflect.get(payload, "messages");
  if (!Array.isArray(messages)) return [];
  let latestUser = -1;
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message: unknown = messages[index];
    if (
      typeof message === "object" &&
      message !== null &&
      Reflect.get(message, "role") === "user"
    ) {
      latestUser = index;
      break;
    }
  }
  const names: string[] = [];
  for (const message of messages.slice(latestUser + 1)) {
    if (typeof message !== "object" || message === null) continue;
    const calls = Reflect.get(message, "tool_calls");
    if (!Array.isArray(calls)) continue;
    for (const call of calls) {
      if (typeof call !== "object" || call === null) continue;
      const fn = Reflect.get(call, "function");
      if (typeof fn !== "object" || fn === null) continue;
      const name = Reflect.get(fn, "name");
      if (typeof name === "string") names.push(name);
    }
  }
  return names;
}

function latestToolContent(payload: unknown): string {
  if (typeof payload !== "object" || payload === null) return "";
  const messages = Reflect.get(payload, "messages");
  if (!Array.isArray(messages)) return "";
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (typeof message !== "object" || message === null) continue;
    if (Reflect.get(message, "role") !== "tool") continue;
    const content = Reflect.get(message, "content");
    return typeof content === "string" ? content : JSON.stringify(content);
  }
  return "";
}

function uuids(value: string): string[] {
  return [...value.matchAll(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi)]
    .map((match) => match[0])
    .filter((id, index, values) => values.indexOf(id) === index);
}

function fixtureSleepCommand(seconds: number): string {
  return process.platform === "win32"
    ? `powershell -NoProfile -NonInteractive -Command "Start-Sleep -Seconds ${seconds}"`
    : `sleep ${seconds}`;
}

async function waitForReady(server: ChildProcess, getLog: () => string): Promise<void> {
  for (let attempt = 0; attempt < 120; attempt += 1) {
    if (server.exitCode !== null) {
      throw new Error(`Janus server exited before readiness (${server.exitCode})\n${getLog()}`);
    }
    try {
      const response = await fetch(`${CONTROL_ORIGIN}/health/ready`);
      if (response.ok) return;
    } catch {
      // Startup is expected to refuse connections until the listener binds.
    }
    await delay(250);
  }
  throw new Error(`Janus server did not become ready\n${getLog()}`);
}

function requireOperationTarget(operation: OperationView, label: string): string {
  if (operation.status !== "succeeded" || !operation.target_id) {
    throw new Error(`${label} did not return a target: ${JSON.stringify(operation)}`);
  }
  return operation.target_id;
}

function deleteSessionWithCli(executable: string, sessionId: string): void {
  const session = runCliJson<DataResponse<SessionView>>(executable, [
    "--base-url",
    CONTROL_ORIGIN,
    "sessions",
    "get",
    sessionId,
  ]);
  const deletion = runCliJson<DataResponse<OperationView>>(executable, [
    "--base-url",
    CONTROL_ORIGIN,
    "sessions",
    "delete",
    sessionId,
    "--expected-version",
    session.data.version,
  ]);
  runCliJson<DataResponse<OperationView>>(executable, [
    "--base-url",
    CONTROL_ORIGIN,
    "operations",
    "wait",
    deletion.data.id,
    "--poll-millis",
    "50",
  ]);
  const missing = spawnSync(
    executable,
    ["--base-url", CONTROL_ORIGIN, "sessions", "get", sessionId],
    { encoding: "utf8", windowsHide: true },
  );
  const output = `${missing.stdout ?? ""}\n${missing.stderr ?? ""}`;
  if (missing.status === 0 || !output.includes("404")) {
    throw new Error(`deleted Session remained readable: ${output}`);
  }
}

function runCliJson<T>(executable: string, args: string[]): T {
  const result = spawnSync(executable, args, { encoding: "utf8", windowsHide: true });
  if (result.status !== 0) {
    const diagnostics = [result.stderr, result.stdout].filter(Boolean).join("\n");
    throw new Error(`janus-test ${args.join(" ")} failed (${result.status}): ${diagnostics}`);
  }
  try {
    return JSON.parse(result.stdout) as T;
  } catch (error) {
    throw new Error(`janus-test returned invalid JSON: ${result.stdout}`, { cause: error });
  }
}

async function requestJson<T = unknown>(path: string, init: RequestInit = {}): Promise<T> {
  const response = await fetch(`${CONTROL_ORIGIN}${path}`, {
    ...init,
    headers: {
      ...(init.body ? { "content-type": "application/json" } : {}),
      ...(init.headers ?? {}),
    },
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`${init.method ?? "GET"} ${path} returned ${response.status}: ${text}`);
  }
  return (text ? JSON.parse(text) : undefined) as T;
}

function runGit(args: string[]): void {
  const result = spawnSync("git", args, { encoding: "utf8", windowsHide: true });
  if (result.status !== 0) {
    throw new Error(`git ${args.join(" ")} failed: ${result.stderr || result.stdout}`);
  }
}

async function stopProcess(process: ChildProcess | undefined): Promise<void> {
  if (!process || process.exitCode !== null) return;
  const exited = new Promise<void>((resolvePromise) => {
    process.once("exit", () => resolvePromise());
  });
  process.kill();
  await Promise.race([exited, delay(5_000)]);
  if (process.exitCode === null) process.kill("SIGKILL");
}

async function closeServer(server: Server): Promise<void> {
  if (!server.listening) return;
  await new Promise<void>((resolvePromise, reject) => {
    server.close((error) => (error ? reject(error) : resolvePromise()));
  });
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}
