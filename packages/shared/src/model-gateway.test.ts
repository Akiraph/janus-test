import { describe, expect, test } from "bun:test";
import {
  modelConfigModelId,
  modelConfigReasoningEffort,
  modelMapSchema,
  resolveModelConfig,
  shouldUseReasoningEffort,
  upsertModelProviderRequestSchema,
} from "./model-gateway";

describe("modelMapSchema", () => {
  test("accepts legacy string model entries", () => {
    const models = modelMapSchema.parse({
      default: "claude-3-5-sonnet-latest",
    });

    const config = resolveModelConfig(models, "default");

    expect(config).toBe("claude-3-5-sonnet-latest");
    expect(config === undefined ? undefined : modelConfigModelId(config)).toBe(
      "claude-3-5-sonnet-latest",
    );
    expect(
      config === undefined ? undefined : modelConfigReasoningEffort(config),
    ).toBeUndefined();
  });

  test("accepts object model entries with custom reasoning effort", () => {
    const models = modelMapSchema.parse({
      planner: {
        model: "gpt-5",
        reasoningEffort: "max",
      },
    });

    const config = resolveModelConfig(models, "planner");

    expect(config === undefined ? undefined : modelConfigModelId(config)).toBe(
      "gpt-5",
    );
    expect(
      config === undefined ? undefined : modelConfigReasoningEffort(config),
    ).toBe("max");
    expect(shouldUseReasoningEffort("none")).toBe(false);
    expect(shouldUseReasoningEffort("max")).toBe(true);
  });
});

describe("upsertModelProviderRequestSchema", () => {
  test("requires Codex providers to use the Responses wire API", () => {
    const base = {
      client: "codex",
      name: "Codex",
      upstreamBaseUrl: "https://api.openai.com/v1",
      apiKey: "sk-test",
      authMode: "bearer",
      models: { default: "gpt-5-codex" },
      enabled: true,
      discussionEnabled: false,
      priority: 0,
    } as const;

    expect(
      upsertModelProviderRequestSchema.safeParse({
        ...base,
        wireApi: "chat",
      }).success,
    ).toBe(false);
    expect(
      upsertModelProviderRequestSchema.safeParse({
        ...base,
        wireApi: "responses",
      }).success,
    ).toBe(true);
  });
});
