const DEFAULT_FOLLOW_THRESHOLD_PX = 80;

export interface ScrollViewport {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}

export function isNearLatest(
  scrollHeight: number,
  scrollTop: number,
  clientHeight: number,
  threshold = DEFAULT_FOLLOW_THRESHOLD_PX,
): boolean {
  return scrollHeight - scrollTop - clientHeight <= threshold;
}

/** Return the next scroll position without disturbing a reader who left the
 * latest content. The browser clamps the result, but using the exact bottom
 * keeps the intent clear and makes smooth scrolling consistent across engines.
 */
export function scrollTopForContentChange(
  currentScrollTop: number,
  scrollHeight: number,
  clientHeight: number,
  followLatest: boolean,
): number {
  if (!followLatest) return currentScrollTop;
  return Math.max(0, scrollHeight - clientHeight);
}

/** Keep a streaming conversation pinned without leaving an in-flight smooth
 * animation behind to be interrupted by the next content update. */
export function keepLatestContentVisible(viewport: ScrollViewport, followLatest: boolean): void {
  if (!followLatest) return;
  viewport.scrollTop = scrollTopForContentChange(
    viewport.scrollTop,
    viewport.scrollHeight,
    viewport.clientHeight,
    true,
  );
}
