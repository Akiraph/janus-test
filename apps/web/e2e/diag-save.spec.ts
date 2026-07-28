import { test } from "@playwright/test";

const PROJECT_ID = "019f91f6-1687-7a03-9be5-ed8df3502e79";

test("diag save + git", async ({ page }) => {
  test.setTimeout(90000);
  const apiCalls: { method: string; url: string; status: number; body?: string }[] = [];
  page.on("response", async (resp) => {
    const u = resp.url();
    if (u.includes("/api/v1/")) {
      const req = resp.request();
      let body: string | undefined;
      try {
        const pd = req.postData();
        if (pd) body = pd.slice(0, 300);
      } catch {}
      const call: (typeof apiCalls)[number] = {
        method: req.method(),
        url: u.replace(/^https?:\/\/[^/]+/, ""),
        status: resp.status(),
      };
      if (body !== undefined) call.body = body;
      apiCalls.push(call);
    }
  });
  page.on("console", (msg) => {
    if (msg.type() === "error") console.log(`[BROWSER ERR]`, msg.text());
  });

  await page.goto(`http://127.0.0.1:5173/projects/${PROJECT_ID}`, {
    waitUntil: "domcontentloaded",
  });
  await page.waitForSelector(".ide-shell", { timeout: 15000 });
  await page.waitForTimeout(1000);

  // Open a small editable text file. Prefer AGENTS.md or TODO.
  const items = page.locator(".ide-tree-item");
  let opened = false;
  for (let i = 0; i < (await items.count()); i++) {
    const cls = await items.nth(i).getAttribute("class");
    if (cls?.includes("ide-tree-item--dir")) continue;
    const text = (await items.nth(i).innerText()).trim();
    // pick a small-ish text file
    if (/^(AGENTS\.md|TODO|LICENSE|README\.md|\.npmrc)$/.test(text)) {
      await items.nth(i).click();
      opened = true;
      break;
    }
  }
  if (!opened) {
    for (let i = 0; i < (await items.count()); i++) {
      const cls = await items.nth(i).getAttribute("class");
      if (cls?.includes("ide-tree-item--dir")) continue;
      await items.nth(i).click();
      break;
    }
  }
  await page.waitForTimeout(1500);

  const textarea = page.locator(".files-textarea").first();
  const taVisible = await textarea.isVisible().catch(() => false);
  console.log("textarea visible:", taVisible);
  if (!taVisible) {
    console.log("NO TEXTAREA - file not editable. API calls so far:");
    console.log(JSON.stringify(apiCalls, null, 2));
    return;
  }

  const before = await textarea.inputValue();
  const marker = `<!-- repro ${Date.now()} -->\n`;
  await textarea.click();
  await textarea.fill(marker + before);
  await page.waitForTimeout(400);

  // click Save
  const saveBtn = page
    .locator("button")
    .filter({ hasText: /^Save$/ })
    .first();
  await saveBtn.click();
  await page.waitForTimeout(2500);

  // Switch to SCM and read
  await page.locator('.ide-activity-btn[aria-label="Source Control"]').click();
  await page.waitForTimeout(1500);
  const scmText = await page
    .locator(".scm-panel")
    .innerText()
    .catch(() => "<none>");
  console.log("=== SCM AFTER SAVE ===");
  console.log(scmText.slice(0, 800));

  console.log("=== API CALLS ===");
  console.log(JSON.stringify(apiCalls, null, 2));
});
