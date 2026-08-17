import { readFile } from "node:fs/promises";
import { expect, test } from "@playwright/test";

test("thinking content and nested tools share the thinking alignment", async ({ page }) => {
  await page.goto("/", { waitUntil: "domcontentloaded" });
  const sessionCss = await readFile(
    new URL("../src/features/session/session.css", import.meta.url),
    "utf8",
  );
  await page.addStyleTag({
    content: `
      :root {
        --space-2: 8px;
        --space-3: 12px;
        --text: #111;
        --text-muted: #555;
        --text-faint: #888;
        --border: #ddd;
        --leading-sm: 1.25;
        --text-sm: 14px;
        --dur: 200ms;
        --dur-fast: 100ms;
        --easing: linear;
      }
      ${sessionCss}
    `,
  });

  const positions = await page.evaluate(() => {
    const host = document.createElement("div");
    host.style.cssText = "position: fixed; left: 0; top: 0; width: 700px; visibility: hidden;";
    host.innerHTML = `
      <article class="session-event session-event--trailing">
        <button class="session-event__summary">
          <span class="session-event__dot"></span>
          <span class="session-event__title">Thought for a while</span>
        </button>
        <div class="session-event__body-wrap session-event__body-wrap--open">
          <div class="session-event__body-content">
            <div class="session-event__body">
              <div class="session-event__body-markdown">uncompressed reasoning</div>
              <div class="session-event__activity">
                <div class="session-event__activity-thought">
                  <div class="session-event__activity-heading">
                    <span class="session-event__dot"></span>
                    <span class="session-event__title">Thought</span>
                  </div>
                  <div class="session-event__activity-thought-body">thought detail</div>
                </div>
                <div class="session-event__activity-tool">
                  <article class="session-event">
                    <button class="session-event__summary">
                      <span class="session-event__dot"></span>
                      <span class="session-event__title">Ran pwd</span>
                    </button>
                  </article>
                </div>
              </div>
            </div>
          </div>
        </div>
      </article>`;
    document.body.append(host);
    const left = (selector: string) => {
      const element = host.querySelector<HTMLElement>(selector);
      if (!element) throw new Error(`missing ${selector}`);
      return Math.round(element.getBoundingClientRect().left);
    };
    const textLeft = (selector: string) => {
      const element = host.querySelector<HTMLElement>(selector);
      if (!element) throw new Error(`missing ${selector}`);
      return Math.round(
        element.getBoundingClientRect().left +
          Number.parseFloat(getComputedStyle(element).paddingLeft),
      );
    };
    const summaryTitle = left(
      ".session-event--trailing > .session-event__summary .session-event__title",
    );
    return {
      summaryTitle,
      bodyMarkdown: textLeft(".session-event__body-markdown"),
      thoughtBody: textLeft(".session-event__activity-thought-body"),
      thoughtDot: left(".session-event__activity-heading .session-event__dot"),
      toolDot: left(".session-event__activity-tool .session-event__dot"),
      thoughtTitle: left(".session-event__activity-heading .session-event__title"),
      toolTitle: left(".session-event__activity-tool .session-event__title"),
    };
  });

  expect(positions.bodyMarkdown).toBe(positions.summaryTitle);
  expect(positions.thoughtBody).toBe(positions.summaryTitle);
  expect(positions.toolDot).toBe(positions.thoughtDot);
  expect(positions.toolTitle).toBe(positions.thoughtTitle);
});
