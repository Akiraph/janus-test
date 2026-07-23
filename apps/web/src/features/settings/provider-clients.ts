import type { ModelGatewayClient, ModelProviderAuthMode } from "@janus/shared";

/** Display labels for each configurable model-gateway client. */
export const CLIENT_LABELS: Record<ModelGatewayClient, string> = {
  supervisor: "Supervisor",
  "claude-code": "Claude Code",
  codex: "Codex",
};

/** Short helper text shown under each client segment. */
export const CLIENT_DESCRIPTIONS: Record<ModelGatewayClient, string> = {
  supervisor: "Janus's own tool-loop model with provider/model aliases.",
  "claude-code": "Anthropic provider with opus/sonnet/haiku alias mapping.",
  codex: "OpenAI-shaped provider (Codex). No alias mapping needed.",
};

/** The order segments are presented in the settings UI. */
export const CLIENT_ORDER: readonly ModelGatewayClient[] = [
  "supervisor",
  "claude-code",
  "codex",
];

/**
 * Derive the upstream auth header style from the wire API.
 * Anthropic uses `x-api-key`; OpenAI-shaped APIs use bearer tokens.
 * This removes the need for a manual auth-mode field in the UI.
 */
export function deriveAuthMode(wireApi: string): ModelProviderAuthMode {
  return wireApi === "chat" || wireApi === "responses" ? "bearer" : "x-api-key";
}
