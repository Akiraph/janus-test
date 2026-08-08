/** Placeholder data keeps a query mounted while its key changes. It is only
 * valid for rendering after the returned entity matches the requested key. */
export function visibleTurnData<T extends { id: string }>(
  data: T | undefined,
  turnId: string | undefined,
): T | undefined {
  return data?.id === turnId ? data : undefined;
}