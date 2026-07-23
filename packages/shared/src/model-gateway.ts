import { z } from "zod";
import { isoDateTimeSchema } from "./common";

export const modelProviderAuthModes = ["x-api-key", "bearer"] as const;
export const modelProviderAuthModeSchema = z.enum(modelProviderAuthModes);
export type ModelProviderAuthMode = z.infer<typeof modelProviderAuthModeSchema>;

export const modelGatewayClients = [
  "supervisor",
  "claude-code",
  "codex",
] as const;
export const modelGatewayClientSchema = z.enum(modelGatewayClients);
export type ModelGatewayClient = z.infer<typeof modelGatewayClientSchema>;

export const openAiWireApis = ["responses", "chat"] as const;
export const openAiWireApiSchema = z.enum(openAiWireApis);
export type OpenAiWireApi = z.infer<typeof openAiWireApiSchema>;

function requireCodexResponsesWireApi(
  provider: {
    readonly client?: string | undefined;
    readonly wireApi?: string | undefined;
  },
  context: z.RefinementCtx,
): void {
  if (provider.client === "codex" && provider.wireApi !== "responses") {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["wireApi"],
      message: "Codex providers must use the OpenAI Responses API.",
    });
  }
}

export const modelProviderUpstreamBaseUrlSchema = z
  .string()
  .url()
  .refine(
    (value) => {
      try {
        const url = new URL(value);
        return url.protocol === "http:" || url.protocol === "https:";
      } catch {
        return false;
      }
    },
    {
      message: "Model provider base URL must use http or https.",
    },
  );

export const modelAliases = ["default", "haiku", "sonnet", "opus"] as const;
export const modelAliasSchema = z.enum(modelAliases);
export type ModelAlias = z.infer<typeof modelAliasSchema>;

const nonEmptyModelName = z.string().trim().min(1);
const nonEmptyReasoningEffort = z
  .string()
  .trim()
  .min(1)
  .max(64)
  .regex(/^[A-Za-z0-9._:-]+$/, {
    message:
      "Reasoning effort may contain only letters, numbers, dots, underscores, colons, and dashes.",
  });

export const modelReasoningEffortPresets = [
  "none",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
] as const;
export const modelReasoningEffortPresetSchema = z.enum(
  modelReasoningEffortPresets,
);
export type ModelReasoningEffortPreset = z.infer<
  typeof modelReasoningEffortPresetSchema
>;

export const modelConfigSchema = z.union([
  nonEmptyModelName,
  z
    .object({
      model: nonEmptyModelName,
      reasoningEffort: nonEmptyReasoningEffort.optional(),
      streamingDisabledAt: isoDateTimeSchema.optional(),
      streamingDisabledReason: z.string().trim().min(1).max(500).optional(),
    })
    .strict(),
]);

export type ModelConfig = z.infer<typeof modelConfigSchema>;

/**
 * A provider's models keyed by alias. Claude Code/Codex usually include
 * `default`; supervisor providers may expose only custom aliases.
 */
export const modelMapSchema = z
  .record(z.string().trim().min(1), modelConfigSchema)
  .refine((models) => Object.keys(models).length > 0, {
    message: "At least one model must be configured.",
  });

export type ModelMap = z.infer<typeof modelMapSchema>;

export function modelConfigModelId(config: ModelConfig): string {
  return typeof config === "string" ? config : config.model;
}

export function modelConfigReasoningEffort(
  config: ModelConfig,
): string | undefined {
  if (typeof config === "string") {
    return undefined;
  }

  return config.reasoningEffort;
}

export function modelConfigStreamingDisabledAt(
  config: ModelConfig,
): string | undefined {
  return typeof config === "string" ? undefined : config.streamingDisabledAt;
}

export function modelConfigStreamingDisabledReason(
  config: ModelConfig,
): string | undefined {
  return typeof config === "string"
    ? undefined
    : config.streamingDisabledReason;
}

export function shouldUseModelStreaming(config: ModelConfig): boolean {
  return modelConfigStreamingDisabledAt(config) === undefined;
}

export function shouldUseReasoningEffort(
  effort: string | undefined,
): effort is string {
  return effort !== undefined && effort !== "none";
}

export function resolveModelConfig(
  models: ModelMap,
  modelAlias: string | undefined,
): ModelConfig | undefined {
  const requestedModel =
    modelAlias === undefined ? undefined : models[modelAlias];
  return requestedModel ?? models.default ?? Object.values(models)[0];
}

/**
 * The alias referenced by the active route. A free string (not the fixed
 * `ModelAlias` enum) so supervisor's custom aliases can be selected globally.
 */
export const routeModelAliasSchema = z.string().trim().min(1);

export const modelProviderRecordSchema = z
  .object({
    id: z.string().min(1),
    client: modelGatewayClientSchema,
    name: z.string().trim().min(1).max(80),
    upstreamBaseUrl: modelProviderUpstreamBaseUrlSchema,
    /**
     * Whether an API key is stored for this provider. The raw key is never
     * returned; it is held encrypted in the key vault keyed by the provider id.
     */
    hasApiKey: z.boolean(),
    apiKeyPreview: z.string().min(1).optional(),
    authMode: modelProviderAuthModeSchema,
    wireApi: openAiWireApiSchema.optional(),
    models: modelMapSchema,
    enabled: z.boolean(),
    discussionEnabled: z.boolean().default(false),
    priority: z.number().int().nonnegative(),
    createdAt: isoDateTimeSchema,
    updatedAt: isoDateTimeSchema,
  })
  .superRefine(requireCodexResponsesWireApi);

export type ModelProviderRecord = z.output<typeof modelProviderRecordSchema>;

export const upsertModelProviderRequestSchema = z
  .object({
    id: z.string().trim().min(1).optional(),
    client: modelGatewayClientSchema.default("claude-code"),
    name: z.string().trim().min(1).max(80),
    upstreamBaseUrl: modelProviderUpstreamBaseUrlSchema,
    /**
     * The API key for this provider. Required when creating a provider; optional
     * when editing (omit to keep the existing key).
     */
    apiKey: z.string().trim().min(1).optional(),
    authMode: modelProviderAuthModeSchema.default("x-api-key"),
    wireApi: openAiWireApiSchema.optional(),
    models: modelMapSchema,
    enabled: z.boolean().default(true),
    discussionEnabled: z.boolean().default(false),
    priority: z.number().int().nonnegative().default(0),
  })
  .superRefine(requireCodexResponsesWireApi);

export type UpsertModelProviderInput = z.input<
  typeof upsertModelProviderRequestSchema
>;

export type UpsertModelProviderRequest = z.output<
  typeof upsertModelProviderRequestSchema
>;

export const testModelProviderRequestSchema =
  upsertModelProviderRequestSchema.extend({
    modelAlias: routeModelAliasSchema.optional(),
  });

export type TestModelProviderInput = z.input<
  typeof testModelProviderRequestSchema
>;

export type TestModelProviderRequest = z.output<
  typeof testModelProviderRequestSchema
>;

export const testModelProviderResponseSchema = z.object({
  success: z.literal(true),
});

export type TestModelProviderResponse = z.infer<
  typeof testModelProviderResponseSchema
>;

export const modelProviderResponseSchema = z.object({
  provider: modelProviderRecordSchema,
});

export type ModelProviderResponse = z.infer<typeof modelProviderResponseSchema>;

export const listModelProvidersResponseSchema = z.object({
  providers: z.array(modelProviderRecordSchema),
});

export type ListModelProvidersResponse = z.infer<
  typeof listModelProvidersResponseSchema
>;

export const activeModelGatewayRouteRecordSchema = z.object({
  app: modelGatewayClientSchema,
  providerId: z.string().min(1),
  modelAlias: routeModelAliasSchema,
  updatedAt: isoDateTimeSchema,
});

export type ActiveModelGatewayRouteRecord = z.infer<
  typeof activeModelGatewayRouteRecordSchema
>;

export const activeModelGatewayRoutesByClientSchema = z.object({
  supervisor: activeModelGatewayRouteRecordSchema.optional(),
  "claude-code": activeModelGatewayRouteRecordSchema.optional(),
  codex: activeModelGatewayRouteRecordSchema.optional(),
});

export type ActiveModelGatewayRoutesByClient = z.infer<
  typeof activeModelGatewayRoutesByClientSchema
>;

export const setActiveModelGatewayRouteRequestSchema = z.object({
  app: modelGatewayClientSchema.default("claude-code"),
  providerId: z.string().trim().min(1),
  modelAlias: routeModelAliasSchema.default("default"),
});

export type SetActiveModelGatewayRouteInput = z.input<
  typeof setActiveModelGatewayRouteRequestSchema
>;

export type SetActiveModelGatewayRouteRequest = z.output<
  typeof setActiveModelGatewayRouteRequestSchema
>;

export const activeModelGatewayRouteResponseSchema = z.object({
  route: activeModelGatewayRouteRecordSchema,
});

export type ActiveModelGatewayRouteResponse = z.infer<
  typeof activeModelGatewayRouteResponseSchema
>;

export const modelProviderHealthStatusSchema = z.enum([
  "unknown",
  "healthy",
  "degraded",
]);

export type ModelProviderHealthStatus = z.infer<
  typeof modelProviderHealthStatusSchema
>;

export const modelProviderHealthRecordSchema = z.object({
  providerId: z.string().min(1),
  status: modelProviderHealthStatusSchema,
  failureCount: z.number().int().nonnegative(),
  lastCheckedAt: isoDateTimeSchema.optional(),
  lastError: z.string().min(1).optional(),
});

export type ModelProviderHealthRecord = z.infer<
  typeof modelProviderHealthRecordSchema
>;

export const modelGatewayStatusResponseSchema = z.object({
  activeRoute: activeModelGatewayRouteRecordSchema.optional(),
  activeRoutes: activeModelGatewayRoutesByClientSchema.default({}),
  providers: z.array(
    z.object({
      provider: modelProviderRecordSchema,
      health: modelProviderHealthRecordSchema,
    }),
  ),
});

export type ModelGatewayStatusResponse = z.infer<
  typeof modelGatewayStatusResponseSchema
>;
