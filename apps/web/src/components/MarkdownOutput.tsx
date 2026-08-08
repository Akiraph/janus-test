import DOMPurify from "dompurify";
import { marked } from "marked";
import { createEffect, onCleanup } from "solid-js";

interface MarkdownOutputProps {
  text: string;
}

export function MarkdownOutput(props: MarkdownOutputProps) {
  let root!: HTMLDivElement;
  let frame: number | undefined;
  let timer: number | undefined;

  createEffect(() => {
    const text = props.text;
    if (frame !== undefined) cancelAnimationFrame(frame);
    if (timer !== undefined) window.clearTimeout(timer);
    frame = undefined;
    timer = undefined;

    const render = () => {
      frame = undefined;
      timer = undefined;
      try {
        const html = marked.parse(text, { async: false, gfm: true }) as string;
        const content = DOMPurify.sanitize(html, { RETURN_DOM_FRAGMENT: true });
        root.replaceChildren(content);
      } catch {
        root.replaceChildren(document.createTextNode(text));
      }
    };

    // Coalesce bursty SSE deltas into one parse per paint. This keeps the
    // rendered Markdown live without parsing the same growing document for
    // every individual token.
    if (typeof requestAnimationFrame === "function") {
      frame = requestAnimationFrame(render);
    } else {
      timer = window.setTimeout(render, 0);
    }

    onCleanup(() => {
      if (frame !== undefined) cancelAnimationFrame(frame);
      if (timer !== undefined) window.clearTimeout(timer);
      frame = undefined;
      timer = undefined;
    });
  });

  return <div class="session-markdown" ref={root} />;
}
