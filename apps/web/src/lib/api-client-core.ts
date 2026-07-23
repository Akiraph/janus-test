import type { ZodType } from "zod";

const apiBaseUrl = import.meta.env.VITE_API_BASE_URL ?? "";

export function buildApiUrl(path: string): string {
  return `${apiBaseUrl}${path}`;
}

export function buildApiHeaders(
  initHeaders?: HeadersInit,
  options: { readonly includeJsonContentType?: boolean } = {},
): Headers {
  const headers = new Headers(initHeaders);
  const includeJsonContentType = options.includeJsonContentType ?? true;

  if (includeJsonContentType && !headers.has("content-type")) {
    headers.set("content-type", "application/json");
  }

  return headers;
}

export async function requestJson<TResponse>(
  path: string,
  schema: ZodType<TResponse>,
  init?: RequestInit,
): Promise<TResponse> {
  const response = await fetch(buildApiUrl(path), {
    ...init,
    credentials: "include",
    headers: buildApiHeaders(init?.headers),
  });

  const payload: unknown = await response.json().catch(() => undefined);

  if (!response.ok) {
    throw new Error(extractErrorMessage(payload));
  }

  return schema.parse(payload);
}

export async function requestVoid(
  path: string,
  init: RequestInit,
  options: {
    readonly ignoreNotFound?: boolean;
    readonly fallbackMessage: string;
  },
): Promise<void> {
  const response = await fetch(buildApiUrl(path), {
    ...init,
    credentials: "include",
    headers: buildApiHeaders(init.headers, { includeJsonContentType: false }),
  });

  if (options.ignoreNotFound === true && response.status === 404) {
    return;
  }

  if (!response.ok) {
    const payload: unknown = await response.json().catch(() => undefined);
    throw new Error(extractErrorMessage(payload) || options.fallbackMessage);
  }
}

export function subscribeEventStream<TEvent>(
  request: {
    readonly path: string;
    readonly eventName: string;
    readonly schema: ZodType<TEvent>;
  },
  onEvent: (event: TEvent) => void,
  onError?: (error: unknown) => void,
): () => void {
  const source = new EventSource(buildApiUrl(request.path), {
    withCredentials: true,
  });
  const handleEvent = (event: Event) => {
    if (!(event instanceof MessageEvent)) {
      return;
    }

    try {
      onEvent(request.schema.parse(JSON.parse(event.data)));
    } catch (error) {
      onError?.(error);
    }
  };

  source.addEventListener(request.eventName, handleEvent);
  source.onerror = (error) => {
    onError?.(error);
  };

  return () => {
    source.removeEventListener(request.eventName, handleEvent);
    source.close();
  };
}

export function extractErrorMessage(payload: unknown): string {
  if (typeof payload === "object" && payload !== null && "error" in payload) {
    const error = payload.error;

    if (
      typeof error === "object" &&
      error !== null &&
      "message" in error &&
      typeof error.message === "string"
    ) {
      return error.message;
    }
  }

  return "Request failed.";
}
