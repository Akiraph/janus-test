import type { TerminalOutputViewModel } from "../types";

export function parseTerminalOutputText(
  text: string,
): TerminalOutputViewModel | undefined {
  const match =
    /^exit_code:\s*(-?\d+)\nstdout:(?:\n([\s\S]*?))?\nstderr:(?:\n([\s\S]*))?$/.exec(
      text.trim().replace(/\r\n/g, "\n"),
    );

  if (match === null) {
    return undefined;
  }

  const exitCode = Number(match[1]);

  if (!Number.isInteger(exitCode)) {
    return undefined;
  }

  return {
    exitCode,
    stdout: match[2] ?? "",
    stderr: match[3] ?? "",
  };
}

export function formatTerminalStreamText(
  text: string | undefined,
): string | undefined {
  const trimmed = text?.trim();

  if (trimmed === undefined || trimmed.length === 0) {
    return undefined;
  }

  const lines = trimmed
    .split(/\r?\n/)
    .map((line) => parseCliOutputLine(line.trim()))
    .filter((line): line is string => line !== undefined && line.length > 0);

  return lines.length === 0 ? undefined : lines.join("\n");
}

export function parseCliOutputLine(line: string): string | undefined {
  if (line.length === 0) {
    return undefined;
  }

  const parsed = parseJsonObject(line);

  if (parsed === undefined) {
    return line;
  }

  return (
    parseClaudeStreamJsonMessage(parsed) ??
    parseCodexItemJsonMessage(parsed) ??
    parseGenericJsonMessage(parsed)
  );
}

function parseClaudeStreamJsonMessage(
  value: Record<string, unknown>,
): string | undefined {
  const type = readString(value, "type");

  if (type === "assistant") {
    return parseMessageContent(readRecord(value, "message"));
  }

  if (type === "result") {
    return readString(value, "result") ?? readString(value, "subtype");
  }

  if (type === "system") {
    const subtype = readString(value, "subtype");
    return subtype === undefined ? undefined : `System: ${subtype}`;
  }

  return undefined;
}

function parseGenericJsonMessage(
  value: Record<string, unknown>,
): string | undefined {
  return (
    readString(value, "message") ??
    readString(value, "text") ??
    readString(value, "result") ??
    readString(value, "delta") ??
    readString(value, "output") ??
    parseMessageContent(value)
  );
}

function parseCodexItemJsonMessage(
  value: Record<string, unknown>,
): string | undefined {
  const eventType = readString(value, "type") ?? readString(value, "event");

  if (eventType?.startsWith("item.") !== true) {
    return undefined;
  }

  const item = readRecord(value, "item");

  if (item === undefined) {
    return undefined;
  }

  if (eventType.endsWith(".started")) {
    return undefined;
  }

  return parseResponseItem(item);
}

function parseResponseItem(item: Record<string, unknown>): string | undefined {
  const itemType = readString(item, "type");

  if (itemType === "message") {
    return parseMessageContent(item);
  }

  if (itemType === "function_call" || itemType === "tool_call") {
    const name = readString(item, "name") ?? readString(item, "tool_name");
    return name === undefined ? undefined : `Tool use: ${name}`;
  }

  if (itemType === "function_call_output" || itemType === "tool_result") {
    return readString(item, "output") ?? readString(item, "result");
  }

  return (
    readString(item, "message") ??
    readString(item, "text") ??
    readString(item, "result") ??
    readString(item, "output") ??
    parseMessageContent(item)
  );
}

function parseMessageContent(
  message: Record<string, unknown> | undefined,
): string | undefined {
  if (message === undefined) {
    return undefined;
  }

  const content = message.content;

  if (typeof content === "string") {
    return content;
  }

  if (!Array.isArray(content)) {
    return undefined;
  }

  const parts = content
    .map((item) => parseContentItem(item))
    .filter((part): part is string => part !== undefined && part.length > 0);

  return parts.length === 0 ? undefined : parts.join("\n");
}

function parseContentItem(item: unknown): string | undefined {
  if (typeof item === "string") {
    return item;
  }

  if (!isRecord(item)) {
    return undefined;
  }

  const text = readString(item, "text");

  if (text !== undefined) {
    return text;
  }

  const type = readString(item, "type");
  const name = readString(item, "name");

  if (
    (type === "tool_use" || type === "function_call" || type === "tool_call") &&
    name !== undefined
  ) {
    return `Tool use: ${name}`;
  }

  return (type === "output_text" || type === "input_text") && text !== undefined
    ? text
    : undefined;
}

function parseJsonObject(value: string): Record<string, unknown> | undefined {
  try {
    const parsed: unknown = JSON.parse(value);
    return isRecord(parsed) ? parsed : undefined;
  } catch {
    return undefined;
  }
}

function readRecord(
  value: Record<string, unknown>,
  key: string,
): Record<string, unknown> | undefined {
  const property = value[key];
  return isRecord(property) ? property : undefined;
}

function readString(
  value: Record<string, unknown>,
  key: string,
): string | undefined {
  const property = value[key];
  return typeof property === "string" && property.length > 0
    ? property
    : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
