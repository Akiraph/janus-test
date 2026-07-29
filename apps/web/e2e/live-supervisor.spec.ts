import { expect, test } from "@playwright/test";
import { type LiveJanusEnvironment, startLiveJanus } from "./support/liveJanus";

type DataResponse<T> = { data: T };

interface LiveSession {
  id: string;
  state: string;
  active_turn_id?: string | null;
  version: string;
}

interface LiveTurn {
  handoff_from_turn_id?: string | null;
  handoff_to_turn_id?: string | null;
  id: string;
  status: string;
}

interface MessageRoute {
  handoff_from_turn_id?: string | null;
  route: string;
  session_version: string;
  turn_id: string;
}

interface TimelineItem {
  kind: string;
  projection: unknown;
  turn_id?: string | null;
}

interface TimelinePage {
  items: TimelineItem[];
}

interface CancelResult {
  session_version: string;
  to_status: string;
  turn_id: string;
}

interface LiveTerminal {
  id: string;
  project_id: string;
  status: string;
}

test.describe.configure({ mode: "serial" });

let live: LiveJanusEnvironment;

test.beforeAll(async () => {
  live = await startLiveJanus();
});

test.afterAll(async () => {
  await live?.stop();
});

test("a browser message completes through the live supervisor", async ({ page }) => {
  test.setTimeout(90_000);

  await page.goto(`/projects/${live.projectId}?view=sessions`, {
    waitUntil: "domcontentloaded",
  });

  const sessionRow = page.getByRole("button", { name: live.sessionTitle, exact: true });
  await expect(sessionRow).toBeVisible({ timeout: 15_000 });
  await sessionRow.click();

  await expect(
    page.getByRole("tablist", { name: "Session views" }).getByRole("tab", { name: "Terminal" }),
  ).toHaveCount(0);

  const composer = page.getByPlaceholder(/Send a message/i);
  await expect(composer).toBeVisible();
  await composer.fill("Complete this through the live supervisor");
  await page.getByRole("button", { name: "Send message" }).click();

  await expect(page.getByText("Complete this through the live supervisor")).toBeVisible();
  await expect(page.getByText("Live fixture reply")).toBeVisible({ timeout: 30_000 });

  const session = await page.request.get(`/api/v1/sessions/${live.sessionId}`);
  expect(session.ok()).toBe(true);
  expect((await session.json()).data.state).toBe("ready");

  const timeline = await page.request.get(`/api/v1/sessions/${live.sessionId}/timeline`);
  expect(timeline.ok()).toBe(true);
  const timelineBody = await timeline.json();
  expect(timelineBody.data.items).toEqual(
    expect.arrayContaining([
      expect.objectContaining({ kind: "user_message" }),
      expect.objectContaining({ kind: "assistant_message" }),
    ]),
  );
});

test("Main Terminal runs in the Project workspace through CLI and WebSocket", async ({ page }) => {
  test.setTimeout(60_000);
  const created = live.cli<DataResponse<LiveTerminal>>(["terminal", "create", live.projectId]);
  expect(created.data.project_id).toBe(live.projectId);
  expect(created.data.status).toBe("running");

  try {
    await page.goto(`/projects/${live.projectId}`, { waitUntil: "domcontentloaded" });
    await page.getByRole("button", { name: "Terminal", exact: true }).click();

    const terminal = page.getByRole("application", { name: "Main Terminal" });
    await expect(terminal).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText("live", { exact: true })).toBeVisible({ timeout: 15_000 });
    await terminal.click();
    await page.keyboard.type("pwd");
    await page.keyboard.press("Enter");

    const rows = terminal.locator(".xterm-rows");
    await expect(rows).toContainText(live.projectId, { timeout: 15_000 });
    expect(await rows.textContent()).not.toContain(live.sessionId);
  } finally {
    live.cli(["terminal", "close", created.data.id]);
  }
});

test("Project files keep drafts across real SCM navigation and close cleanly", async ({
  page,
}, testInfo) => {
  await page.goto(`/projects/${live.projectId}`, { waitUntil: "domcontentloaded" });

  await page.getByRole("button", { name: "Explorer", exact: true }).click();
  const readme = page.getByRole("button", { name: "README.md", exact: true });
  await expect(readme).toBeVisible({ timeout: 15_000 });
  await readme.click();

  const editor = page.getByRole("textbox", { name: "File content README.md" });
  await expect(editor).toHaveValue("# Live fixture\n");
  await editor.fill("unsaved live draft");

  await page.getByRole("button", { name: "Source Control", exact: true }).click();
  await expect(page.locator(".scm-graph-row").first()).toContainText("fixture", {
    timeout: 15_000,
  });
  await page.screenshot({
    path: testInfo.outputPath("live-workspace-desktop.png"),
    fullPage: true,
  });

  await page.getByRole("button", { name: "Explorer", exact: true }).click();
  await expect(editor).toHaveValue("unsaved live draft");
  await page.getByRole("button", { name: "Close README.md" }).click();
  await expect(editor).toHaveCount(0);
  await expect(page.locator(".ide-tab")).toHaveCount(0);
});

test("mobile workspace switches between navigation and the active Session", async ({
  page,
}, testInfo) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`/projects/${live.projectId}`, { waitUntil: "domcontentloaded" });

  const sessionRow = page.getByRole("button", { name: live.sessionTitle, exact: true });
  await expect(sessionRow).toBeVisible({ timeout: 15_000 });
  await expect(page.locator(".ide-main")).toBeHidden();
  await sessionRow.click();

  const composer = page.getByPlaceholder(/Send a message/i);
  await expect(composer).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Workspace activity" })).toBeHidden();

  await page.screenshot({ path: testInfo.outputPath("live-session-mobile.png"), fullPage: true });

  await page.getByRole("button", { name: "Open workspace navigation" }).click();
  await expect(sessionRow).toBeVisible();
  await expect(page.locator(".ide-main")).toBeHidden();

  await sessionRow.click();
  await expect(composer).toBeVisible();
});

test("live deployment pages remain usable on mobile", async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 390, height: 844 });

  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Projects", exact: true })).toBeVisible();
  await expect(page.getByText("Live project", { exact: true })).toBeVisible();
  await expect(page.getByRole("link", { name: "Settings" })).toBeVisible();

  await page.goto("/settings", { waitUntil: "domcontentloaded" });
  const navigation = page.getByRole("navigation", { name: "Settings navigation" });
  await expect(navigation).toBeVisible();
  expect((await navigation.boundingBox())?.height).toBeLessThan(80);
  const provider = page.locator(".provider-card").filter({ hasText: "Live fixture" });
  await expect(provider).toContainText("Fixture model");
  await expect(page.getByRole("button", { name: /^Models/ })).toHaveCount(0);
  await page.screenshot({ path: testInfo.outputPath("live-settings-mobile.png"), fullPage: true });

  await page.goto("/system", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "System", level: 2 })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Capabilities" })).toBeVisible();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth > document.documentElement.clientWidth,
    ),
  ).toBe(false);
});

test("blocking Ask answers once and duplicate delivery is idempotent", async () => {
  test.setTimeout(60_000);
  const routed = postMessage("[fixture:ask] Ask before finishing");
  await waitForTurn(routed.turn_id, "waiting_for_ask");

  const timeline = await waitFor(
    () => readTimeline(),
    (page) => findStringProperty(page, "ask_id") !== undefined,
    "blocking Ask projection",
  );
  const askId = findStringProperty(timeline, "ask_id");
  expect(askId).toBeTruthy();

  live.cli(["sessions", "answer-ask", askId as string, "fixture answer"]);
  await waitForTurn(routed.turn_id, "completed");

  const beforeTimeline = await readTimeline();
  const beforeSession = await readSession();
  const beforeRequests = live.providerRequestCount();
  const duplicate = live.cli<DataResponse<{ route_or_status: string }>>([
    "sessions",
    "answer-ask",
    askId as string,
    "fixture answer",
  ]);
  expect(duplicate.data.route_or_status).toBe("completed");

  await delay(300);
  expect((await readTimeline()).items).toHaveLength(beforeTimeline.items.length);
  expect((await readSession()).version).toBe(beforeSession.version);
  expect(live.providerRequestCount()).toBe(beforeRequests);
});

test("best-effort Ask expires to its default and resumes", async () => {
  test.setTimeout(60_000);
  const routed = postMessage("[fixture:ask-expire] Use the default");
  await waitForTurn(routed.turn_id, "completed", 20_000);
  expect(JSON.stringify(await readTimeline())).toContain("fixture expiry default");
});

test("a completed Job settles its Tool Call and resumes exactly once", async () => {
  test.setTimeout(60_000);
  const beforeRequests = live.providerRequestCount();
  const routed = postMessage("[fixture:job-resume] Finish after the Job");
  await waitForTurn(routed.turn_id, "waiting_for_job", 20_000);
  await waitForTurn(routed.turn_id, "completed", 20_000);

  const toolCall = (await readTimeline()).items.find(
    (item) => item.turn_id === routed.turn_id && item.kind === "tool_call",
  );
  expect(toolCall?.projection).toMatchObject({
    kind: "tool_call",
    status: "succeeded",
    summary: expect.objectContaining({ status: "succeeded" }),
    tool_name: "job",
  });
  expect(live.providerRequestCount()).toBe(beforeRequests + 2);
});

test("Handoff transfers a running Job and completes the successor", async () => {
  test.setTimeout(60_000);
  const beforeRequests = live.providerRequestCount();
  const predecessor = postMessage("[fixture:handoff-job] Start transferable work");
  await waitForTurn(predecessor.turn_id, "waiting_for_job", 20_000);

  const successor = postMessage("Take over while the existing Job finishes");
  expect(successor.route).toBe("handed_off");
  expect(successor.handoff_from_turn_id).toBe(predecessor.turn_id);
  await waitForTurn(predecessor.turn_id, "handed_off");
  await waitForTurn(successor.turn_id, "waiting_for_job");
  await waitForTurn(successor.turn_id, "completed", 20_000);

  const predecessorAfter = await readTurn(predecessor.turn_id);
  const successorAfter = await readTurn(successor.turn_id);
  expect(predecessorAfter.handoff_to_turn_id).toBe(successor.turn_id);
  expect(successorAfter.handoff_from_turn_id).toBe(predecessor.turn_id);
  expect((await readSession()).active_turn_id).toBeNull();

  const toolCall = (await readTimeline()).items.find(
    (item) => item.turn_id === predecessor.turn_id && item.kind === "tool_call",
  );
  expect(toolCall?.projection).toMatchObject({
    kind: "tool_call",
    status: "succeeded",
    summary: expect.objectContaining({ status: "succeeded" }),
    tool_name: "job",
  });
  expect(live.providerRequestCount()).toBe(beforeRequests + 3);
});

test("Cancel stops a running Job and is retryable with the original version", async () => {
  test.setTimeout(60_000);
  const routed = postMessage("[fixture:cancel-job] Start cancellable work");
  await waitForTurn(routed.turn_id, "waiting_for_job", 20_000);
  const cancelVersion = (await readSession()).version;

  const first = live.cli<DataResponse<CancelResult>>([
    "sessions",
    "cancel",
    live.sessionId,
    routed.turn_id,
    cancelVersion,
    "--reason",
    "fixture cancel",
  ]);
  expect(first.data.to_status).toBe("canceled");

  const repeated = live.cli<DataResponse<CancelResult>>([
    "sessions",
    "cancel",
    live.sessionId,
    routed.turn_id,
    cancelVersion,
    "--reason",
    "fixture cancel retry",
  ]);
  expect(repeated.data.to_status).toBe("canceled");
  expect((await readSession()).active_turn_id).toBeNull();
});

test("restart interrupts an active Turn without replaying the Provider", async () => {
  test.setTimeout(60_000);
  const routed = postMessage("[fixture:restart-ask] Wait across restart");
  await waitForTurn(routed.turn_id, "waiting_for_ask");
  const beforeRequests = live.providerRequestCount();

  await live.restart();
  await waitForTurn(routed.turn_id, "interrupted", 20_000);
  expect((await readSession()).active_turn_id).toBeNull();
  expect(live.providerRequestCount()).toBe(beforeRequests);
});

function postMessage(content: string): MessageRoute {
  const session = live.cli<DataResponse<LiveSession>>(["sessions", "get", live.sessionId]);
  return live.cli<DataResponse<MessageRoute>>([
    "sessions",
    "post-message",
    live.sessionId,
    content,
    session.data.version,
  ]).data;
}

async function readSession(): Promise<LiveSession> {
  return (await live.request<DataResponse<LiveSession>>(`/api/v1/sessions/${live.sessionId}`)).data;
}

async function readTurn(turnId: string): Promise<LiveTurn> {
  return (
    await live.request<DataResponse<LiveTurn>>(`/api/v1/sessions/${live.sessionId}/turns/${turnId}`)
  ).data;
}

async function readTimeline(): Promise<TimelinePage> {
  return (
    await live.request<DataResponse<TimelinePage>>(
      `/api/v1/sessions/${live.sessionId}/timeline?limit=100`,
    )
  ).data;
}

async function waitForTurn(
  turnId: string,
  status: string,
  timeoutMilliseconds = 15_000,
): Promise<LiveTurn> {
  return waitFor(
    () => readTurn(turnId),
    (turn) => turn.status === status,
    `Turn ${turnId} to become ${status}`,
    timeoutMilliseconds,
  );
}

async function waitFor<T>(
  read: () => Promise<T>,
  accepts: (value: T) => boolean,
  label: string,
  timeoutMilliseconds = 15_000,
): Promise<T> {
  const deadline = Date.now() + timeoutMilliseconds;
  let last: T | undefined;
  while (Date.now() < deadline) {
    last = await read();
    if (accepts(last)) return last;
    await delay(100);
  }
  throw new Error(
    `${label} timed out; last value: ${JSON.stringify(last)}\nJanus server output:\n${live.serverLog()}`,
  );
}

function findStringProperty(value: unknown, key: string): string | undefined {
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = findStringProperty(item, key);
      if (found !== undefined) return found;
    }
    return undefined;
  }
  if (typeof value !== "object" || value === null) return undefined;
  const direct = Reflect.get(value, key);
  if (typeof direct === "string") return direct;
  for (const nested of Object.values(value)) {
    const found = findStringProperty(nested, key);
    if (found !== undefined) return found;
  }
  return undefined;
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}
