import { type ChildProcess, spawn, spawnSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const CONTROL_ORIGIN = "http://127.0.0.1:4317";
const FINISH_STREAM = [
  'data: {"choices":[{"index":0,"delta":{"role":"assistant","content":"Live fixture reply"}}]}',
  "",
  'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_f","type":"function","function":{"name":"finish","arguments":""}}]}}]}',
  "",
  'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\\"summary\\":\\"fixture complete\\"}"}}]}}]}',
  "",
  'data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":9,"completion_tokens":4}}',
  "",
  "data: [DONE]",
  "",
  "",
].join("\n");

type DataResponse<T> = { data: T };

interface OperationView {
  id: string;
  status: string;
  target_id?: string | null;
  error?: unknown;
}

interface SessionView {
  id: string;
  version: string;
}

export interface LiveJanusEnvironment {
  projectId: string;
  sessionId: string;
  sessionTitle: string;
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
    runGit(["-C", workRepo, "commit", "--allow-empty", "-m", "fixture"]);
    runGit(["clone", "--bare", workRepo, bareRepo]);

    const executable = resolve(
      repoRoot,
      "target",
      "debug",
      process.platform === "win32" ? "janus-server.exe" : "janus-server",
    );
    server = spawn(executable, [], {
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
    };
    server.stdout?.on("data", collectLog);
    server.stderr?.on("data", collectLog);

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

    const operation = await requestJson<DataResponse<OperationView>>("/api/v1/projects", {
      method: "POST",
      headers: { "idempotency-key": `web-live-${Date.now()}` },
      body: JSON.stringify({
        name: "Live project",
        repository: {
          access: "public_https",
          url: pathToFileURL(bareRepo).href,
          branch: "main",
        },
      }),
    });
    const projectId = await waitForProject(operation.data.id);
    const sessionTitle = "Live supervisor";
    const session = await requestJson<DataResponse<SessionView>>(
      `/api/v1/projects/${projectId}/sessions`,
      {
        method: "POST",
        body: JSON.stringify({ title: sessionTitle }),
      },
    );

    return {
      projectId,
      sessionId: session.data.id,
      sessionTitle,
      stop: async () => {
        await stopProcess(server);
        await closeServer(provider.server);
        await rm(fixtureRoot, { recursive: true, force: true });
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

async function startProvider(): Promise<{ origin: string; server: Server }> {
  const server = createServer((request, response) => {
    if (request.method !== "POST" || request.url !== "/v1/chat/completions") {
      response.writeHead(404).end();
      return;
    }
    response.writeHead(200, { "content-type": "text/event-stream" });
    response.end(FINISH_STREAM);
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
  return { origin: `http://127.0.0.1:${address.port}`, server };
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

async function waitForProject(operationId: string): Promise<string> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const operation = await requestJson<DataResponse<OperationView>>(
      `/api/v1/operations/${operationId}`,
    );
    if (operation.data.status === "succeeded" && operation.data.target_id) {
      return operation.data.target_id;
    }
    if (["failed", "needs_attention", "canceled"].includes(operation.data.status)) {
      throw new Error(
        `fixture project creation ${operation.data.status}: ${JSON.stringify(operation.data.error)}`,
      );
    }
    await delay(100);
  }
  throw new Error("fixture project creation timed out");
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
