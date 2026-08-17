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
  id: string;
  status: string;
}

interface MessageRoute {
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

test("a browser message completes through live execution", async ({ page }) => {
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
  await composer.fill("你好");
  await page.getByRole("button", { name: "Send message" }).click();

  const userBubble = page.locator(".session-message__bubble").filter({ hasText: "你好" }).last();
  await expect(userBubble).toBeVisible();
  // The bubble swaps from a provisional optimistic rendering to the durable
  // projection as soon as the server commits the message (instant under SSE
  // convergence), so boundingBox() can hit a transient gap mid-swap. Poll until
  // the durable bubble reports a box instead of measuring once.
  await expect
    .poll(
      async () => {
        const box = await userBubble.boundingBox();
        return box ? box.width > box.height : false;
      },
      { timeout: 5_000 },
    )
    .toBe(true);

  // Streaming provisional text renders as plain text (not parsed as markdown
  // while deltas arrive), then settles to the durable markdown once the Round
  // completes. The durable assistant message contains the heading + list item,
  // so "Live fixture reply" is the stable visible signal across both phases.
  await expect(page.locator(".session-message--status")).toContainText(
    /^(Working \d+[smh]|Reconnecting \(|Worked for \d+[smh])/,
    { timeout: 10_000 },
  );
  await expect(page.getByText("Live fixture reply")).toBeVisible({ timeout: 30_000 });

  const settledSession = await waitFor(
    () => readSession(),
    (session) => session.state === "ready",
    "Session to settle after visible provisional output",
  );
  expect(settledSession.active_turn_id).toBeNull();

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

test("Session attachments remain reusable and can become workspace files", async ({
  page,
}, testInfo) => {
  test.setTimeout(90_000);
  await page.goto(`/projects/${live.projectId}?view=sessions`, {
    waitUntil: "domcontentloaded",
  });
  await page.getByRole("button", { name: live.sessionTitle, exact: true }).click();

  await page.locator('input[type="file"]').setInputFiles([
    {
      name: "session.log",
      mimeType: "text/plain",
      buffer: Buffer.from("attachment log survives later turns\n", "utf8"),
    },
    {
      name: "logo.bin",
      mimeType: "application/vnd.janus.asset",
      buffer: Buffer.from([0, 1, 2, 127, 128, 255]),
    },
  ]);
  await expect(page.getByText("session.log", { exact: true })).toBeVisible();
  await expect(page.getByText("logo.bin", { exact: true })).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath("attachments-desktop.png"), fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: testInfo.outputPath("attachments-mobile.png"), fullPage: true });
  await page.setViewportSize({ width: 1440, height: 900 });

  const composer = page.getByPlaceholder(/Send a message/i);
  await composer.fill("[fixture:attachments] Inspect the log and save the binary asset");
  await page.getByRole("button", { name: "Send message" }).click();
  await waitFor(
    () => readTimeline(),
    (timeline) => JSON.stringify(timeline).includes('"tool_name":"attachment_save"'),
    "attachment save tool",
    30_000,
  );

  const savedFile = await live.request<DataResponse<{ path: string; size: number }>>(
    `/api/v1/projects/${live.projectId}/files/meta?path=${encodeURIComponent("assets/logo.bin")}`,
  );
  expect(savedFile.data).toMatchObject({ path: "assets/logo.bin", size: 6 });

  await waitFor(
    () => readSession(),
    (session) => session.state === "ready" && session.active_turn_id === null,
    "attachment turn to settle before reuse",
  );
  const reused = postMessage("[fixture:attachment-reuse] Read a previous Session attachment");
  await waitForTurn(reused.turn_id, "completed", 30_000);
  const attachmentReads = JSON.stringify(await readTimeline()).match(
    /"tool_name":"attachment_read"/g,
  );
  expect(attachmentReads?.length).toBeGreaterThanOrEqual(2);
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
    // The "Send interrupt" button is enabled exactly when the terminal's
    // WebSocket is live. A refactor removed the connection-status badge this
    // test used to match on (`{status()}`), so gate on the enabled button
    // instead of a label that no longer exists.
    await expect(page.getByRole("button", { name: "Send interrupt" })).toBeEnabled({
      timeout: 15_000,
    });
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
  test.setTimeout(60_000);
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
  test.setTimeout(60_000);
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
  test.setTimeout(60_000);
  await page.setViewportSize({ width: 390, height: 844 });

  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Projects", exact: true })).toBeVisible({
    timeout: 15_000,
  });
  await expect(page.getByText("Live project", { exact: true })).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole("link", { name: "Settings" })).toBeVisible({ timeout: 15_000 });

  await page.goto("/settings", { waitUntil: "domcontentloaded" });
  const navigation = page.getByRole("navigation", { name: "Settings navigation" });
  await expect(navigation).toBeVisible();
  expect((await navigation.boundingBox())?.height).toBeLessThan(80);
  const provider = page.locator(".provider-card").filter({ hasText: "Live fixture" });
  await expect(provider).toContainText("Fixture model");
  await expect(page.getByRole("button", { name: /^Models/ })).toHaveCount(0);
  await page.screenshot({ path: testInfo.outputPath("live-settings-mobile.png"), fullPage: true });

  await page.goto("/system", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "System", level: 2 })).toBeVisible({
    timeout: 15_000,
  });
  await expect(page.getByRole("heading", { name: "Service" })).toBeVisible({ timeout: 15_000 });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth > document.documentElement.clientWidth,
    ),
  ).toBe(false);
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

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}
